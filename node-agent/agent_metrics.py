"""设备信息与指标采集。

真机（Android/Termux）优先，逐级降级；**拿不到的指标一律返回 None，不造假**：
1. `/proc`（Linux 通用，Termux 一定能读）
2. `termux-api`（需要 Termux:API 应用，缺了不影响其他指标）
3. `sysfs` / `dumpsys` / `getprop`（Android 系统信息）

主字段表（§4）之外的补充信息（CPU 频率、电池电量与充电状态）统一放进
`extra` JSON，不污染三端共用的字段契约。

模拟模式（--simulate）下生成"合理"的假数据：CPU/NPU 随负载波动、
温度跟随 CPU 变化、内存缓慢漂移、网络流量单调递增、电池缓慢放电并在
低电量时转入充电。
"""

from __future__ import annotations

import os
import random
import re
import shutil
import subprocess
import time
from typing import Any

_IS_ANDROID = "ANDROID_ROOT" in os.environ or os.path.exists("/system/build.prop")


def _run(cmd: list[str], timeout: float = 3.0) -> str:
    """执行命令，失败一律返回空串（采集失败不能影响 Agent 主流程）。"""
    try:
        out = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout, check=False
        )
        return (out.stdout or "").strip()
    except Exception:
        return ""


def _has(cmd: str) -> bool:
    return shutil.which(cmd) is not None


# ============================================================== 设备信息（静态）


def device_info(simulate: bool, sim_cfg: Any) -> dict[str, Any]:
    """采集设备静态信息：型号 / SoC / Android 版本。"""
    if simulate:
        return {
            "model": str(getattr(sim_cfg, "model", "") or "Xiaomi 13"),
            "soc": str(getattr(sim_cfg, "soc", "") or "Snapdragon 8 Gen 2"),
            "android_version": str(getattr(sim_cfg, "android_version", "") or "14"),
        }

    model = _run(["getprop", "ro.product.model"]) or _read_first(
        ["/system/build.prop", "/vendor/build.prop"], r"ro\.product\.model=(.+)"
    )
    soc = _run(["getprop", "ro.soc.model"]) or _run(["getprop", "ro.board.platform"])
    if not soc:
        soc = _read_first(["/proc/cpuinfo"], r"Hardware\s*:\s*(.+)") or _cpuinfo_hardware()
    android = _run(["getprop", "ro.build.version.release"])
    if not android:
        android = _read_first(["/system/build.prop"], r"ro\.build\.version\.release=(.+)")

    return {
        "model": model or "Unknown",
        "soc": soc or "Unknown",
        "android_version": android or "Unknown",
    }


def _read_text(path: str) -> str:
    """读一行文本。读不到返回空串。"""
    try:
        with open(path, encoding="utf-8", errors="ignore") as f:
            return f.read().strip()
    except Exception:
        return ""


def _read_float(path: str) -> float | None:
    """读一个数值。读不到或解析失败返回 None（不造假）。"""
    raw = _read_text(path)
    if not raw:
        return None
    try:
        return round(float(raw), 1)
    except ValueError:
        return None


def _read_first(paths: list[str], pattern: str) -> str:
    for p in paths:
        try:
            if not os.path.exists(p):
                continue
            with open(p, encoding="utf-8", errors="ignore") as f:
                for line in f:
                    m = re.search(pattern, line)
                    if m:
                        return m.group(1).strip()
        except Exception:
            continue
    return ""


def _cpuinfo_hardware() -> str:
    try:
        with open("/proc/cpuinfo", encoding="utf-8", errors="ignore") as f:
            for line in f:
                if line.lower().startswith("hardware"):
                    return line.split(":", 1)[1].strip()
    except Exception:
        pass
    return ""


# ============================================================== 指标（动态）


class MetricsCollector:
    """周期性采集节点指标。

    CPU 使用率基于 `/proc/stat` 两次采样的差值计算，需要保留上一帧状态。
    内存 / 存储 / 温度 / 网络同样尽量从 /proc 与 sysfs 读；
    读不到就留 None（前端显示"暂无数据"，绝不用假值顶替）。
    """

    def __init__(self, simulate: bool = False) -> None:
        self.simulate = simulate
        self._prev_cpu: tuple[int, int] | None = None
        self._prev_net: tuple[int, int] | None = None
        self._sim_state: dict[str, float] = {
            "cpu": 28.0,
            "npu": 12.0,
            "mem_ratio": 0.45,
            "temp": 38.0,
            "rx": 0.0,
            "tx": 0.0,
            "battery": 86.0,
            "charging": 0.0,  # 0=放电 1=充电，用 float 保持状态表类型统一
        }
        self._sim_started = time.time()

    # ------------------------------------------------------------------ 主入口

    def collect(self) -> dict[str, Any]:
        if self.simulate:
            return self._collect_simulated()
        try:
            return self._collect_real()
        except Exception:
            # 采集崩了也不能让 Agent 掉线，返回空指标让平台"留空"
            return {}

    # ------------------------------------------------------------------ 真机

    def _collect_real(self) -> dict[str, Any]:
        m: dict[str, Any] = {}
        # termux-battery-status 只调一次：温度与电量/充电状态都从它解析
        battery_raw = self._termux_battery_raw()

        m.update(self._cpu_mem_real())
        m.update(self._storage_real())
        m.update(self._net_real())
        m.update(self._temp_real(battery_raw))
        # GPU / NPU：Android 无通用接口，尝试常见 sysfs 与 termux-api
        m["gpu_usage"] = self._gpu_real()
        m["npu_usage"] = self._npu_real()
        # 功耗：需要硬件/内核支持，普通手机拿不到 → 留空（不造假）
        m["power_watts"] = None
        m["extra"] = self._extra_real(battery_raw)
        return m

    @staticmethod
    def _termux_battery_raw() -> str:
        """调用一次 termux-battery-status，供温度与电池两处复用。

        没装 Termux:API 时命令不存在，直接返回空串，不影响其他指标。
        """
        if _has("termux-battery-status"):
            return _run(["termux-battery-status"], timeout=4)
        return ""

    def _extra_real(self, battery_raw: str = "") -> dict[str, Any]:
        """补充性指标：CPU 频率、电池。

        平台侧主字段表（§4）没有这些列，统一塞进 `extra` JSON，
        既不影响三端契约，又能额外承载信息。
        """
        extra: dict[str, Any] = {"cpu_cores": os.cpu_count() or 1}
        freq = self._cpu_freq_avg()
        if freq is not None:
            extra["cpu_freq_avg_mhz"] = freq
        extra.update(self._battery_real(battery_raw))
        return extra

    def _cpu_freq_avg(self) -> float | None:
        """各核当前频率的平均值（MHz）。

        路径 `/sys/devices/system/cpu/cpuN/cpufreq/scaling_cur_freq` 在 Android 上
        通常可读（无需 root）；部分内核只暴露 `cpuinfo_cur_freq`。
        """
        vals: list[float] = []
        for i in range(32):
            got = False
            for name in ("scaling_cur_freq", "cpuinfo_cur_freq"):
                p = f"/sys/devices/system/cpu/cpu{i}/cpufreq/{name}"
                try:
                    with open(p, encoding="utf-8") as f:
                        vals.append(float(f.read().strip()) / 1000.0)  # kHz → MHz
                    got = True
                    break
                except Exception:
                    continue
            if not got and vals:
                break  # 已经读到若干核，说明 cpu{i} 不存在了，无需继续扫
        if not vals:
            return None
        return round(sum(vals) / len(vals), 1)

    def _battery_real(self, battery_raw: str = "") -> dict[str, Any]:
        """电池信息：termux-api 优先，其次 sysfs 降级。拿不到返回空 dict。"""
        out: dict[str, Any] = {}

        if battery_raw:
            pct = re.search(r'"percentage"\s*:\s*([\d.]+)', battery_raw)
            st = re.search(r'"status"\s*:\s*"([^"]+)"', battery_raw)
            temp = re.search(r'"temperature"\s*:\s*([\d.]+)', battery_raw)
            if pct:
                out["battery_percent"] = round(float(pct.group(1)), 1)
            if st:
                out["battery_status"] = st.group(1)
            if temp:
                out["battery_temperature"] = round(float(temp.group(1)), 1)
            if out:
                return out

        pct = _read_float("/sys/class/power_supply/battery/capacity")
        if pct is not None:
            out["battery_percent"] = pct
        status = _read_text("/sys/class/power_supply/battery/status")
        if status:
            out["battery_status"] = status
        return out

    def _cpu_mem_real(self) -> dict[str, Any]:
        out: dict[str, Any] = {}
        nproc = os.cpu_count() or 1

        # --- CPU：/proc/stat 差值法 ---
        try:
            with open("/proc/stat", encoding="utf-8") as f:
                first = f.readline()
            parts = [int(x) for x in first.split()[1:]]
            idle = parts[3] + (parts[4] if len(parts) > 4 else 0)
            total = sum(parts)
            if self._prev_cpu is not None:
                d_idle = idle - self._prev_cpu[0]
                d_total = total - self._prev_cpu[1]
                if d_total > 0:
                    out["cpu_usage"] = round(max(0.0, min(100.0, 100.0 * (1 - d_idle / d_total))), 2)
            self._prev_cpu = (idle, total)
            if "cpu_usage" not in out:
                out["cpu_usage"] = None  # 首次采样无法计算
        except Exception:
            out["cpu_usage"] = None

        # --- 内存：/proc/meminfo ---
        try:
            mem: dict[str, int] = {}
            with open("/proc/meminfo", encoding="utf-8") as f:
                for line in f:
                    k, _, v = line.partition(":")
                    num = re.search(r"(\d+)", v)
                    if num:
                        mem[k.strip()] = int(num.group(1)) * 1024  # kB → bytes
            total = mem.get("MemTotal")
            avail = mem.get("MemAvailable")
            if avail is None:
                free = mem.get("MemFree", 0)
                cache = mem.get("Cached", 0) + mem.get("Buffers", 0)
                avail = free + cache
            if total:
                out["mem_total"] = total
                out["mem_used"] = max(0, total - avail)
            else:
                out["mem_total"] = out["mem_used"] = None
        except Exception:
            out["mem_total"] = out["mem_used"] = None

        # --- 负载 ---
        try:
            with open("/proc/loadavg", encoding="utf-8") as f:
                out["load_avg"] = round(float(f.read().split()[0]), 2)
        except Exception:
            out["load_avg"] = None

        out["cpu_cores"] = nproc
        return out

    def _storage_real(self) -> dict[str, Any]:
        """存储：statvfs 读数据分区，/data 优先，退回 Termux 目录。"""
        for path in ("/data", os.path.expanduser("~")):
            try:
                st = os.statvfs(path)
                total = st.f_blocks * st.f_frsize
                free = st.f_bavail * st.f_frsize
                if total > 0:
                    return {"storage_total": total, "storage_used": max(0, total - free)}
            except Exception:
                continue
        return {"storage_total": None, "storage_used": None}

    def _net_real(self) -> dict[str, Any]:
        """网络：/proc/net/dev 累计字节。"""
        try:
            rx = tx = 0
            with open("/proc/net/dev", encoding="utf-8") as f:
                for line in f:
                    if ":" not in line:
                        continue
                    iface, _, data = line.partition(":")
                    iface = iface.strip()
                    if iface in ("lo",) or iface.startswith(("dummy", "ifb")):
                        continue
                    cols = data.split()
                    if len(cols) >= 9:
                        rx += int(cols[0])
                        tx += int(cols[8])
            return {"net_rx_bytes": rx, "net_tx_bytes": tx}
        except Exception:
            return {"net_rx_bytes": None, "net_tx_bytes": None}

    def _temp_real(self, battery_raw: str = "") -> dict[str, Any]:
        """温度：优先 termux-api（电池温度），其次 sysfs 热区。

        注意 sysfs 有多个 thermal_zone，不同 zone 可能是电池/主板/CPU，
        取其中较高者作为"机身温度"的近似，避免只读到某个温度偏低的 zone。
        """
        if battery_raw:
            m = re.search(r'"temperature"\s*:\s*([\d.]+)', battery_raw)
            if m:
                try:
                    return {"temperature": round(float(m.group(1)), 1)}
                except ValueError:
                    pass
        # sysfs 热区（部分设备可读）
        best: float | None = None
        for zone in range(0, 12):
            p = f"/sys/class/thermal/thermal_zone{zone}/temp"
            try:
                with open(p, encoding="utf-8") as f:
                    val = float(f.read().strip())
                if val > 1000:  # 毫摄氏度
                    val /= 1000.0
                if 0 < val < 200 and (best is None or val > best):
                    best = val
            except Exception:
                continue
        if best is not None:
            return {"temperature": round(best, 1)}
        return {"temperature": None}

    def _gpu_real(self) -> float | None:
        """GPU 使用率：Android 无通用接口。

        覆盖 Adreno（kgsl）与 Mali 两条常见路径；`..._percentage` 类节点直接是
        百分比，`gpubusy` 类节点是 "busy total" 两个数需要相除。
        """
        candidates = [
            "/sys/class/kgsl/kgsl-3d0/gpu_busy_percentage",
            "/sys/class/kgsl/kgsl-3d0/gpubusy",
            "/sys/class/misc/mali0/device/utilization",
            "/sys/kernel/gpu/gpu_busy",
            "/sys/class/devfreq/gpufreq/load",
        ]
        for p in candidates:
            try:
                with open(p, encoding="utf-8") as f:
                    raw = f.read().strip()
            except Exception:
                continue
            nums = [float(x) for x in re.findall(r"[\d.]+", raw)[:2]]
            if not nums:
                continue
            if len(nums) == 2:
                # "busy total" 形式：busy/total，total 为 0 时视为无效
                if nums[1] > 0:
                    return round(100.0 * nums[0] / nums[1], 2)
                continue
            v = nums[0]
            if v <= 100:
                return round(v, 2)
            return round(v / 10.0, 2)  # 千分比形式
        return None

    def _npu_real(self) -> float | None:
        """NPU 使用率：绝大多数设备不暴露 → 返回 None（不造假）。"""
        for p in ("/sys/kernel/npu/load", "/sys/class/npu/npu_busy"):
            try:
                with open(p, encoding="utf-8") as f:
                    return round(float(re.findall(r"[\d.]+", f.read())[0]), 2)
            except Exception:
                continue
        return None

    # ------------------------------------------------------------------ 模拟

    def _collect_simulated(self) -> dict[str, Any]:
        """生成合理的假数据：随机游走 + 相互关联，看起来像真机。"""
        s = self._sim_state
        # CPU：带惯性的随机游走，偶尔来一次负载尖峰
        s["cpu"] = _clamp(s["cpu"] + random.uniform(-9, 9), 5.0, 96.0, center=28.0)
        if random.random() < 0.04:
            s["cpu"] = min(99.0, s["cpu"] + random.uniform(25, 45))

        # NPU：多数时间低负载，随 CPU 轻微相关
        s["npu"] = _clamp(
            s["npu"] * 0.6 + (s["cpu"] * 0.25) + random.uniform(-3, 6), 0.0, 95.0, center=12.0
        )

        # 内存：缓慢漂移，和 CPU 正相关
        target = 0.42 + s["cpu"] / 400.0
        s["mem_ratio"] = _clamp(s["mem_ratio"] * 0.9 + target * 0.1, 0.25, 0.92, center=0.45)

        # 温度：跟随 CPU 挂一阶惯性
        s["temp"] = _clamp(
            s["temp"] * 0.93 + (30.0 + s["cpu"] * 0.28) * 0.07, 28.0, 85.0, center=38.0
        )

        # 网络：单调递增的累计字节（模拟持续小流量）
        s["rx"] += random.uniform(2_000, 90_000)
        s["tx"] += random.uniform(1_500, 60_000)

        # 电池：缓慢放电，低于 25% 转入充电，充到 95% 再放电
        if s["charging"] >= 1.0:
            s["battery"] = min(100.0, s["battery"] + random.uniform(0.15, 0.5))
            if s["battery"] >= 95.0:
                s["charging"] = 0.0
        else:
            s["battery"] = max(0.0, s["battery"] - random.uniform(0.05, 0.2))
            if s["battery"] <= 25.0:
                s["charging"] = 1.0

        mem_total = 12 * 1024**3  # 12GB
        storage_total = 256 * 1024**3
        return {
            "cpu_usage": round(s["cpu"], 2),
            "gpu_usage": round(_clamp(s["cpu"] * 0.55 + random.uniform(-5, 8), 0, 99, 25), 2),
            "npu_usage": round(s["npu"], 2),
            "mem_used": int(mem_total * s["mem_ratio"]),
            "mem_total": mem_total,
            "storage_used": int(storage_total * 0.62),
            "storage_total": storage_total,
            "temperature": round(s["temp"], 1),
            "power_watts": None,  # 模拟器同样不编造功耗
            "net_rx_bytes": int(s["rx"]),
            "net_tx_bytes": int(s["tx"]),
            "load_avg": round(s["cpu"] / 100.0 * (os.cpu_count() or 4) * 1.1, 2),
            "cpu_cores": os.cpu_count() or 8,
            "extra": {
                "simulated": True,
                "sim_uptime": round(time.time() - self._sim_started, 1),
                "cpu_cores": os.cpu_count() or 8,
                # 频率随负载在 1.0~2.8GHz 之间浮动，与 CPU 使用率正相关
                "cpu_freq_avg_mhz": round(1000 + s["cpu"] * 18, 1),
                "battery_percent": round(s["battery"], 1),
                "battery_status": "CHARGING" if s["charging"] >= 1.0 else "DISCHARGING",
            },
        }


def _clamp(v: float, lo: float, hi: float, center: float | None = None) -> float:
    """夹取并可选地向中心值轻微回拉（模拟真实指标的均值回归）。"""
    if center is not None:
        v = v * 0.97 + center * 0.03
    return max(lo, min(hi, v))
