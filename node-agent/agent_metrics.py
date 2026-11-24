"""设备信息与指标采集。

真机（Android/Termux）优先，逐级降级；**拿不到的指标一律返回 None，不造假**：
1. `/proc`（Linux 通用，Termux 一定能读）
2. `termux-api`（需要 Termux:API 应用，缺了不影响其他指标）
3. `sysfs` / `dumpsys` / `getprop`（Android 系统信息）

主字段表（§4）之外的补充信息统一放进 `extra` JSON，不污染三端共用的字段契约。
温度/频率/电池曾只活在 `extra` 里，前端要显示就得先知道键名，历史数据也没法画趋势；
现已提升为**一等字段**（cpu_temp / gpu_temp / cpu_freq_mhz / gpu_freq_mhz / battery_*）。
`extra` 里保留同名副本，兼容老前端与老验收脚本。

模拟模式（--simulate）下生成"合理"的假数据：CPU/NPU 随负载波动、
温度跟随 CPU 变化、内存缓慢漂移、网络流量单调递增、电池缓慢放电并在
低电量时转入充电。
"""

from __future__ import annotations

import glob
import os
import random
import re
import shutil
import subprocess
import time
from typing import Any

_IS_ANDROID = "ANDROID_ROOT" in os.environ or os.path.exists("/system/build.prop")

# 路径前缀做成模块常量，只为一件事：**可测试**。
# 采集逻辑要按"Android 封锁了什么"分支，而在开发机（Windows）上没有 /proc，
# 只有把前缀留成变量，验收脚本才能用合成文件造出"Android 环境"来跑。
PROC = "/proc"
SYS = "/sys"


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
    """采集设备静态信息（需求 §3「设备能力」）。

    分两类，语义完全不同，不要混：

    - **型号类**（model/soc/cpu/gpu/npu）：能读到就填，读不到一律 "Unknown"。
      这是部署前兼容性判断的依据，所以宁可明确写 Unknown，**也不猜一个看起来
      合理的值** —— 猜错会让不兼容的模型装到设备上，比返回 Unknown 糟得多。
    - **能力类**（is_root / capabilities / qnn_version）：描述"这台设备能做什么"，
      决定平台能不能给它下发某类任务。能力一律三态：True 有 / False 没有 /
      -1 或 None 未知。**未知 ≠ 没有**：平台遇到未知应当"允许但提示"，
      遇到 False 才拒绝。
    """
    if simulate:
        return {
            "model": str(getattr(sim_cfg, "model", "") or "Xiaomi 13"),
            "soc": str(getattr(sim_cfg, "soc", "") or "Snapdragon 8 Gen 2"),
            "android_version": str(getattr(sim_cfg, "android_version", "") or "14"),
            "cpu": "Qualcomm Kryo",
            "gpu": "Adreno 740",
            "npu": "Qualcomm Hexagon NPU",
            "qnn_version": "2.28.0",
            # 模拟节点刻意**不声明 root**：真机 Termux 场景默认就是无 root，
            # 模拟环境必须能代表最常见的那种设备，否则测不出降级路径。
            "is_root": 0,
            "capabilities": _capabilities(simulate=True, is_root=0, qnn_version="2.28.0"),
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

    is_root = _root_state()
    qnn = _qnn_version()
    return {
        "model": model or "Unknown",
        "soc": soc or "Unknown",
        "android_version": android or "Unknown",
        "cpu": _cpu_model(soc),
        "gpu": _gpu_model(),
        "npu": _npu_model(soc, qnn),
        "qnn_version": qnn,
        "is_root": is_root,
        "capabilities": _capabilities(simulate=False, is_root=is_root, qnn_version=qnn),
    }


# ------------------------------------------------------------ 设备能力探测
# 这一段的共同纪律：**只报告能证实的**。任何探测失败都不抛异常，
# 而是退化成"未知" —— 能力探测跑在注册路径上，抛异常等于设备直接上不了线。

# QNN / QAIRT。版本号只认属性和 sysfs，**绝不从 so 文件名推**：
# `libQnnHtpV73Stub.so` 里的 V73 是 HTP 架构版本，不是 QNN 版本，
# 混用会直接把部署兼容性判断带偏。
_QNN_VERSION_PROPS = (
    "ro.vendor.qti.qnn.version",
    "vendor.qti.qnn.version",
    "ro.vendor.qti.hexagon.version",
    "ro.vendor.hexagon.version",
)
_QNN_LIB_DIRS = (
    "/vendor/lib64", "/vendor/lib", "/odm/lib64", "/odm/lib",
    "/system/lib64", "/system/lib",
)
_QNN_LIB_NAMES = (
    "libQnnHtp.so", "libQnnSystem.so", "libQnnCpu.so", "libQnnHtpPrepare.so",
)


def _root_state() -> int:
    """是否有 root：1 有 / 0 没有 / -1 未知。

    只做**非侵入式**探测。第二步要调 `su -c id`，这里的 2 秒超时不是随手写的：
    部分设备的 su 会弹授权框，Agent 卡在注册流程上，比"不知道有没有 root"严重得多。

    需求原文「Root 是能力，不是架构」—— 这里就是那句话的落点：
    我们只**记录**这个能力，绝不因为它让任何功能变成"必须有 root"。
    """
    try:
        if os.geteuid() == 0:
            return 1
    except Exception:
        pass
    if not _has("su"):
        # Android 上 PATH 里没有 su，基本可以断定没 root；
        # 开发机上没有 su 完全正常，不能据此判断设备能力 → 未知。
        return 0 if _IS_ANDROID else -1
    out = _run(["su", "-c", "id"], timeout=2.0)
    return 1 if "uid=0" in out else 0


def _qnn_version() -> str:
    """QNN / QAIRT 版本号。读不到返回空串（= 未知，不是"没装"）。

    空串与"没装"的区别由 `capabilities.qnn_runtime` 承担，
    所以这里不必造一个 "unknown" 字符串塞进版本列 —— 那会让
    "版本号恰好叫 unknown"永远无法区分。
    """
    for prop in _QNN_VERSION_PROPS:
        v = _run(["getprop", prop])
        if v:
            return v[:64]
    return ""


def _qnn_runtime_present() -> int:
    """QNN 运行时是否就位：1 有 / 0 没有 / -1 未知。

    **-1 是必需的，不能省。** Android 11+ 连 /vendor 都不让普通 App 读，
    目录打不开时"看起来像没装"，但那是错觉 —— 所以逐个目录记录能不能打开，
    一个都打不开才回 -1。
    """
    opened = False
    for d in _QNN_LIB_DIRS:
        try:
            if not os.path.isdir(d):
                continue
            os.listdir(d)
            opened = True
        except Exception:
            continue
        for name in _QNN_LIB_NAMES:
            if os.path.exists(f"{d}/{name}"):
                return 1
    return 0 if opened else -1


def _readable(path: str) -> bool:
    """文件存在且真的能读出内容。

    Android 上"文件在但读被拒"是常态，所以不能用 os.path.exists 判断可读性 ——
    那会把"权限不足"误判成"有数据"。
    """
    try:
        with open(path, "rb") as f:
            return bool(f.read(1))
    except Exception:
        return False


def _cpu_model(soc: str) -> str:
    """CPU 型号。

    优先 `/proc/cpuinfo` 的 model name（Linux 上有，Android 上通常没有）；
    退到 `ro.product.board` / `ro.hardware`。后两个是**平台代号**不是 CPU 型号，
    所以原样返回，**不做 "kalama → Snapdragon 8 Gen 2" 这类映射**：
    映射表一旦过时，兼容性判断会静默出错。
    """
    return (
        _read_first([f"{PROC}/cpuinfo"], r"model name\s*:\s*(.+)")
        or _run(["getprop", "ro.product.board"])
        or _run(["getprop", "ro.hardware"])
        or soc
        or "Unknown"
    )


def _gpu_model() -> str:
    """GPU 型号：先 sysfs（kgsl / Mali），再 getprop 兜底。"""
    for rel in (
        "class/kgsl/kgsl-3d0/gpu_model",
        "class/kgsl/kgsl-3d0/devfreq/device/uevent",
        "class/misc/mali0/device/gpuinfo",
    ):
        v = _read_text(f"{SYS}/{rel}")
        if not v:
            continue
        m = re.search(r"(Adreno[^\s\"]*|Mali[-\w]*|PowerVR[^\s\"]*)", v, re.I)
        return (m.group(1) if m else v.splitlines()[0])[:64]
    for prop in ("ro.hardware.vulkan", "ro.hardware.egl", "ro.board.platform"):
        v = _run(["getprop", prop])
        if v:
            return v[:64]
    return "Unknown"


def _npu_model(soc: str, qnn_version: str = "") -> str:
    """NPU 型号。

    **只报告能证实的**：要么属性 / sysfs 里直接写了，要么装了 QNN 运行时
    （QNN 是 Qualcomm AI Engine Direct，只跑高通 HTP，所以能反推）。
    单凭 SoC 代号猜市场名（sm8550 → "Hexagon 780"）会随新平台不断过期，这里不做。
    """
    blob = " ".join(
        [
            soc,
            _run(["getprop", "ro.hardware.npu"]),
            _run(["getprop", "ro.vendor.qti.hardware.npu"]),
            _read_text(f"{SYS}/kernel/npu/type"),
            _read_text(f"{SYS}/class/npu/name"),
        ]
    ).lower()
    if "hexagon" in blob or "htp" in blob:
        return "Qualcomm Hexagon NPU"
    if "apu" in blob:
        return "MediaTek APU"
    if "npu" in blob:
        return (_read_text(f"{SYS}/class/npu/name") or "Generic NPU")[:64]
    if qnn_version:
        return "Qualcomm Hexagon NPU"
    return "Unknown"


def _capabilities(*, simulate: bool, is_root: int, qnn_version: str = "") -> dict[str, Any]:
    """这台设备"具备什么能力"，供平台决定下发哪类任务。

    平台侧的用法是**取决于能力，而不是取决于设备型号**：
    能读 /proc/stat 才画 CPU 曲线，装了 QNN 才允许部署 HTP 模型。
    能力项三态，未知绝不能当成"没有"。
    """
    if simulate:
        return {
            "can_proc_stat": True,
            "can_proc_net": True,
            "can_thermal": True,
            "termux_api": True,
            "gpu_counters": True,
            "qnn_runtime": 1 if qnn_version else 0,
            "root_state": is_root,
            "root": is_root == 1,
        }
    return {
        "can_proc_stat": _readable(f"{PROC}/stat"),
        "can_proc_net": _readable(f"{PROC}/net/dev") or _readable(f"{PROC}/self/net/dev"),
        "can_thermal": bool(_thermal_zones()),
        "termux_api": _has("termux-battery-status"),
        "gpu_counters": any(os.path.exists(f"{SYS}/{r}") for r in _GPU_COUNTER_PATHS),
        "qnn_runtime": _qnn_runtime_present(),
        "root_state": is_root,
        "root": is_root == 1,
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
        with open(f"{PROC}/cpuinfo", encoding="utf-8", errors="ignore") as f:
            for line in f:
                if line.lower().startswith("hardware"):
                    return line.split(":", 1)[1].strip()
    except Exception:
        pass
    return ""


# ------------------------------------------------------------ 硬件读取路径
# 把路径集中成常量，只为一件事：**能力探测与采集走同一份定义**。
# 如果 _capabilities() 说"没有 GPU 计数器"而 _gpu_real() 却去读另一批路径，
# 两者迟早会漂移成互相矛盾的结论。

# 功耗/占用类计数器（读得到说明这台设备开放了 GPU 计数）
_GPU_COUNTER_PATHS = (
    "class/kgsl/kgsl-3d0/gpu_busy_percentage",
    "class/kgsl/kgsl-3d0/gpubusy",
    "class/misc/mali0/device/utilization",
    "kernel/gpu/gpu_busy",
    "class/devfreq/gpufreq/load",
)

# 频率类（cur_freq 单位是 Hz；gpuclk 是 kgsl 的另一种写法）
_GPU_FREQ_PATHS = (
    "class/devfreq/gpufreq/cur_freq",
    "class/devfreq/*gpu*/cur_freq",
    "class/kgsl/kgsl-3d0/devfreq/cur_freq",
    "class/kgsl/kgsl-3d0/gpuclk",
)

# thermal_zone 的 type 标签匹配词。匹配的是**内核给的语义标签**，不是我们猜的编号。
_CPU_ZONE_HINTS = ("cpu", "soc", "tsens", "cluster", "big", "little")
_GPU_ZONE_HINTS = ("gpu", "kgsl", "gpuss", "mali", "adreno")


def _thermal_zones() -> list[tuple[str, float]]:
    """读全部 sysfs thermal_zone，返回 [(类型标签, 摄氏度)]。

    类型标签读不到就留空串 —— **温度本身仍然有效**，只是没法判定它属于
    CPU 还是 GPU，所以不丢弃：机身温度（取最高者）不需要标签，
    分项温度（cpu_temp/gpu_temp）才需要。
    """
    out: list[tuple[str, float]] = []
    for zone in range(0, 16):
        base = f"{SYS}/class/thermal/thermal_zone{zone}"
        val = _read_float(f"{base}/temp")
        if val is None:
            continue
        if val > 1000:  # 毫摄氏度（部分内核的写法）
            val /= 1000.0
        if 0 < val < 200:
            out.append((_read_text(f"{base}/type"), round(val, 1)))
    return out


def _pick_zone(zones: list[tuple[str, float]], hints: tuple[str, ...]) -> float | None:
    """按 type 标签挑一个温度；有多个命中取最高（同类型多传感器的常见情形）。"""
    hits = [t for name, t in zones if name and any(h in name.lower() for h in hints)]
    return max(hits) if hits else None


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
        # 进程自身 CPU（/proc/self/stat 差值），Android 上唯一还读得到的 CPU 真值
        self._prev_self: tuple[int, float] | None = None
        # 每个指标"这份值是从哪儿来的"——拿不到时前端要能说清原因，不能只给一个 —
        self._cpu_source = "unknown"
        self._net_source = "unknown"
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

        # CPU 使用率单独取：拿不到时必须是 None（Android 封锁 /proc/stat），
        # 由 _extra_real 补充"为什么拿不到 + 有什么替代值"
        cpu_usage, self._cpu_source = self._cpu_usage_real()
        m["cpu_usage"] = cpu_usage

        m.update(self._cpu_mem_real())
        m.update(self._storage_real())
        m.update(self._net_real())
        m.update(self._temp_real(battery_raw))
        # GPU / NPU：Android 无通用接口，尝试常见 sysfs 与 termux-api
        m["gpu_usage"] = self._gpu_real()
        m["npu_usage"] = self._npu_real()
        # 功耗：需要硬件/内核支持，普通手机拿不到 → 留空（不造假）
        m["power_watts"] = None
        # ---- 一等字段：分项温度 / 频率 / 电池 ----
        # 这些值以前只活在 extra 里：前端要显示得先知道 extra 的键名，
        # 而且进不了 metric_samples 的列，历史趋势图无从画起。
        # **拿不到仍然是 None，不是 0** —— 这条纪律在一等字段上同样成立。
        freq = self._cpu_freq_stats()
        battery = self._battery_real(battery_raw)
        cpu_temp, gpu_temp = self._cpu_gpu_temp()
        m["cpu_temp"] = cpu_temp
        m["gpu_temp"] = gpu_temp
        m["cpu_freq_mhz"] = freq.get("cpu_freq_avg_mhz")
        m["gpu_freq_mhz"] = self._gpu_freq_mhz()
        m["battery_percent"] = battery.get("battery_percent")
        m["battery_temp"] = battery.get("battery_temperature")
        m["battery_status"] = battery.get("battery_status")
        m["extra"] = self._extra_real(battery_raw, freq=freq, battery=battery)
        return m

    @staticmethod
    def _termux_battery_raw() -> str:
        """调用一次 termux-battery-status，供温度与电池两处复用。

        没装 Termux:API 时命令不存在，直接返回空串，不影响其他指标。
        """
        if _has("termux-battery-status"):
            return _run(["termux-battery-status"], timeout=4)
        return ""

    def _extra_real(
        self,
        battery_raw: str = "",
        *,
        freq: dict[str, float] | None = None,
        battery: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """补充性指标：CPU 频率、电池。

        `freq` / `battery` 由调用方传入已算好的结果：它们同时是一等字段的来源，
        在 `_collect_real` 里已经算过一次。传进来而不是重算，是为了让
        **一等字段和 extra 里的副本永远出自同一次采样** —— 重算会出现
        "主字段 37.2℃、extra 37.4℃"这种自相矛盾，且白读两遍 sysfs。
        """
        extra: dict[str, Any] = {
            "cpu_cores": os.cpu_count() or 1,
            # 数据来源标注：前端据此把 "—" 解释成"这台设备拿不到"，
            # 而不是让用户以为是采集坏了
            "cpu_source": self._cpu_source,
            "net_source": self._net_source,
        }
        # 频率均值 + 频率占比（UI 的「扩展信息」会显示 cpu_freq_avg_mhz）
        extra.update(freq if freq is not None else self._cpu_freq_stats())
        pressure = self._cpu_pressure_avg10()
        if pressure is not None:
            extra["cpu_pressure_avg10"] = pressure
        self_cpu = self._agent_self_cpu()
        if self_cpu is not None:
            extra["agent_cpu_percent"] = self_cpu
        extra.update(battery if battery is not None else self._battery_real(battery_raw))
        return extra

    def _cpu_usage_real(self) -> tuple[float | None, str]:
        """系统 CPU 使用率。返回 (值, 来源)。

        来源只认 `/proc/stat` 差值法 —— 那才是真·系统使用率。
        Android 8+ 起 SELinux 对普通 App 封了 `/proc/stat`（免 root 读不到），
        此时返回 `(None, "unavailable")`，**不拿别的指标冒充使用率**；
        能拿到的替代指标放 `extra` 并明确标注语义（见 `_extra_real`）。
        """
        try:
            with open(f"{PROC}/stat", encoding="utf-8") as f:
                first = f.readline()
            parts = [int(x) for x in first.split()[1:]]
            idle = parts[3] + (parts[4] if len(parts) > 4 else 0)
            total = sum(parts)
            value = None
            if self._prev_cpu is not None:
                d_idle = idle - self._prev_cpu[0]
                d_total = total - self._prev_cpu[1]
                if d_total > 0:
                    value = round(max(0.0, min(100.0, 100.0 * (1 - d_idle / d_total))), 2)
            self._prev_cpu = (idle, total)
            if value is None:
                return None, "proc_stat_first_sample"  # 首次采样无法算差值，属正常
            return value, "proc_stat"
        except Exception:
            self._prev_cpu = None
            return None, "unavailable"

    def _cpu_freq_stats(self) -> dict[str, float]:
        """CPU 频率：各核当前频率均值，以及相对最高频的占比。

        `scaling_cur_freq` 在 Android 上通常可读（这就是真机上唯一还拿得到的
        CPU 侧信号）。**频率占比 ≠ 使用率**（一个满载的低频核和一个空闲核可能同频），
        所以它只作为替代指标放进 extra，绝不写进 cpu_usage。
        """
        cur: list[float] = []
        mx: list[float] = []
        for i in range(32):
            c = _read_float(f"{SYS}/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq")
            if c is None:
                c = _read_float(f"{SYS}/devices/system/cpu/cpu{i}/cpufreq/cpuinfo_cur_freq")
            if c is None:
                if cur:
                    break  # 已读到若干核，说明 cpu{i} 不存在了
                continue
            cur.append(c)
            m = _read_float(f"{SYS}/devices/system/cpu/cpu{i}/cpufreq/cpuinfo_max_freq")
            if m is None:
                m = _read_float(f"{SYS}/devices/system/cpu/cpu{i}/cpufreq/scaling_max_freq")
            if m:
                mx.append(m)
        if not cur:
            return {}
        avg = sum(cur) / len(cur)
        out: dict[str, float] = {"cpu_freq_avg_mhz": round(avg / 1000.0, 1)}
        if mx:
            out["cpu_freq_ratio"] = round(100.0 * avg / (sum(mx) / len(mx)), 1)
        return out

    def _cpu_pressure_avg10(self) -> float | None:
        """PSI：`/proc/pressure/cpu` 的 some avg10。

        语义是"有任务因 CPU 不足而停顿的时间占比"，**不是使用率**（可能使用率不高
        但被限频限核时压力很高）。仅作 extra 里的替代参考。
        """
        try:
            with open(f"{PROC}/pressure/cpu", encoding="utf-8") as f:
                m = re.search(r"some\s+avg10=([\d.]+)", f.read())
            return round(float(m.group(1)), 2) if m else None
        except Exception:
            return None

    def _agent_self_cpu(self) -> float | None:
        """本进程 CPU 占用（单核百分比），来自 `/proc/self/stat` 的 utime+stime 差值。

        读自己的进程不受 Android 限制，所以这是真机上唯一稳定可得的 CPU 侧真值。
        它只代表 Agent 自己，不代表整机 —— 因此放 extra，不冒充 cpu_usage。
        """
        try:
            with open(f"{PROC}/self/stat", encoding="utf-8") as f:
                raw = f.read()
            fields = raw[raw.rfind(")") + 2:].split()
            ticks = int(fields[11]) + int(fields[12])
        except Exception:
            return None

        now = time.monotonic()
        prev, self._prev_self = self._prev_self, (ticks, now)
        if prev is None:
            return None
        d_ticks = ticks - prev[0]
        d_time = now - prev[1]
        if d_time <= 0 or d_ticks < 0:
            return None
        try:
            hz = float(os.sysconf("SC_CLK_TCK"))
        except Exception:
            hz = 100.0  # Android/Linux 默认 100；Windows 没有 sysconf
        return round(100.0 * (d_ticks / hz) / d_time, 2)

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

        pct = _read_float(f"{SYS}/class/power_supply/battery/capacity")
        if pct is not None:
            out["battery_percent"] = pct
        status = _read_text(f"{SYS}/class/power_supply/battery/status")
        if status:
            out["battery_status"] = status
        return out

    def _cpu_mem_real(self) -> dict[str, Any]:
        """内存与负载。

        CPU 使用率不在这里 —— 它在 `_cpu_usage_real()`，原因是：
        真机上这个来源会**整体缺失**，必须能被单独判定并标注来源，
        不能和"一定有值"的内存混在一个方法里。
        """
        out: dict[str, Any] = {}
        nproc = os.cpu_count() or 1

        # --- 内存：/proc/meminfo ---
        try:
            mem: dict[str, int] = {}
            with open(f"{PROC}/meminfo", encoding="utf-8") as f:
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

        # --- 负载（/proc/loadavg 与 /proc/stat 同属被 Android 封锁的一批）---
        try:
            with open(f"{PROC}/loadavg", encoding="utf-8") as f:
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

    @staticmethod
    def _int_of(path: str) -> int | None:
        txt = _read_text(path)
        if not txt:
            return None
        try:
            return int(float(txt.split()[0]))
        except (ValueError, IndexError):
            return None

    def _net_real(self) -> dict[str, Any]:
        """网络累计字节。三级降级，全都拿不到才留 None。

        Android 上 `/proc/net/dev` 与 `/proc/stat` 属同一批被封锁的全局文件，
        所以真机上常常三级全落空 —— 那就老实返回 None（前端显示 —）。
        """
        for reader, source in (
            (self._net_from_proc_file, "proc_net_dev"),
            (self._net_from_proc_file_self, "proc_self_net_dev"),
            (self._net_from_sysfs, "sysfs_iface"),
        ):
            try:
                rx, tx = reader()
            except Exception:
                continue
            if rx is not None and tx is not None:
                self._net_source = source
                return {"net_rx_bytes": rx, "net_tx_bytes": tx}
        self._net_source = "unavailable"
        return {"net_rx_bytes": None, "net_tx_bytes": None}

    @staticmethod
    def _parse_net_dev(path: str) -> tuple[int | None, int | None]:
        """/proc/net/dev 格式：`iface: rx_bytes ... tx_bytes(第 9 列)`。"""
        rx = tx = 0
        seen = False
        with open(path, encoding="utf-8") as f:
            for line in f:
                if ":" not in line:
                    continue
                iface, _, data = line.partition(":")
                iface = iface.strip()
                if iface == "lo" or iface.startswith(("dummy", "ifb")):
                    continue
                cols = data.split()
                if len(cols) >= 9:
                    rx += int(cols[0])
                    tx += int(cols[8])
                    seen = True
        return (rx, tx) if seen else (None, None)

    def _net_from_proc_file(self) -> tuple[int | None, int | None]:
        return self._parse_net_dev(f"{PROC}/net/dev")

    def _net_from_proc_file_self(self) -> tuple[int | None, int | None]:
        # 同一份内容的另一个入口：部分内核只放行 /proc/self/net/dev
        return self._parse_net_dev(f"{PROC}/self/net/dev")

    def _net_from_sysfs(self) -> tuple[int | None, int | None]:
        rx = tx = 0
        seen = False
        for p in glob.glob(f"{SYS}/class/net/*/statistics/rx_bytes"):
            iface = p.split("/")[-3]
            if iface == "lo":
                continue
            r = self._int_of(p)
            t = self._int_of(p.replace("rx_bytes", "tx_bytes"))
            if r is not None and t is not None:
                rx += r
                tx += t
                seen = True
        return (rx, tx) if seen else (None, None)

    def _temp_real(self, battery_raw: str = "") -> dict[str, Any]:
        """机身温度：优先 termux-api（电池温度），其次 sysfs 热区最高者。

        注意 sysfs 有多个 thermal_zone，不同 zone 可能是电池/主板/CPU，
        取其中较高者作为"机身温度"的近似，避免只读到某个温度偏低的 zone。
        **分项温度**（cpu_temp / gpu_temp）不在这里，见 `_cpu_gpu_temp()`。
        """
        if battery_raw:
            m = re.search(r'"temperature"\s*:\s*([\d.]+)', battery_raw)
            if m:
                try:
                    return {"temperature": round(float(m.group(1)), 1)}
                except ValueError:
                    pass
        zones = _thermal_zones()
        if zones:
            return {"temperature": max(t for _, t in zones)}
        return {"temperature": None}

    def _cpu_gpu_temp(self) -> tuple[float | None, float | None]:
        """CPU / GPU 分项温度。

        靠 thermal_zone 的 `type` 标签匹配。标签读不到时返回 None，
        **不做"zone 编号靠前就是 CPU"这类推测** —— 猜错会把 GPU 温度显示成
        CPU 温度，那比显示"—"更糟：用户会据此判断能不能满载跑推理。
        """
        zones = _thermal_zones()
        return _pick_zone(zones, _CPU_ZONE_HINTS), _pick_zone(zones, _GPU_ZONE_HINTS)

    def _gpu_freq_mhz(self) -> float | None:
        """GPU 当前频率（MHz）。devfreq 的 cur_freq 单位是 Hz，必须除 1e6。"""
        for rel in _GPU_FREQ_PATHS:
            for path in glob.glob(f"{SYS}/{rel}"):
                hz = _read_float(path)
                if hz and hz > 0:
                    # 有的节点已经是 MHz（gpuclk 在部分内核上是），用数量级区分：
                    # 手机 GPU 频率一定落在 100~2000MHz，超过 10000 才可能是 Hz。
                    return round(hz / 1_000_000.0, 1) if hz > 10_000 else round(hz, 1)
        return None

    def _gpu_real(self) -> float | None:
        """GPU 使用率：Android 无通用接口。

        覆盖 Adreno（kgsl）与 Mali 两条常见路径；`..._percentage` 类节点直接是
        百分比，`gpubusy` 类节点是 "busy total" 两个数需要相除。
        """
        candidates = [f"{SYS}/{rel}" for rel in _GPU_COUNTER_PATHS]
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
        for p in (f"{SYS}/kernel/npu/load", f"{SYS}/class/npu/npu_busy"):
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
            # 分项温度/频率/电池：模拟数据也走**同一套一等字段契约**，
            # 否则前端在 --simulate 下跑得好好的，接真机就全是 undefined。
            "cpu_temp": round(min(95.0, s["temp"] * 1.06), 1),
            "gpu_temp": round(min(95.0, s["temp"] * 0.96), 1),
            "cpu_freq_mhz": round(1000 + s["cpu"] * 18, 1),
            "gpu_freq_mhz": round(305.0 + s["cpu"] * 6.2, 1),
            "battery_percent": round(s["battery"], 1),
            "battery_temp": round(max(20.0, min(45.0, 26.0 + s["cpu"] * 0.12)), 1),
            "battery_status": "CHARGING" if s["charging"] >= 1.0 else "DISCHARGING",
            "power_watts": None,  # 模拟器同样不编造功耗
            "net_rx_bytes": int(s["rx"]),
            "net_tx_bytes": int(s["tx"]),
            "load_avg": round(s["cpu"] / 100.0 * (os.cpu_count() or 4) * 1.1, 2),
            "cpu_cores": os.cpu_count() or 8,
            "extra": {
                "simulated": True,
                "sim_uptime": round(time.time() - self._sim_started, 1),
                "cpu_cores": os.cpu_count() or 8,
                # 与真机保持同一套契约：模拟数据也标明来源
                "cpu_source": "simulated",
                "net_source": "simulated",
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
