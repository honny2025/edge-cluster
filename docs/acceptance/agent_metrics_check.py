#!/usr/bin/env python3
"""Agent 指标采集降级链验收（无需真机）。

真机上 `/proc/stat`、`/proc/loadavg`、`/proc/net/dev` 会被 Android 的 SELinux
封掉（免 root 读不到）。这件事在开发机（Windows）上没法自然复现，所以本脚本
**用合成文件系统造假环境**：把 `agent_metrics.PROC` / `SYS` 指向临时目录，
按场景摆出不同能读到的文件，再断言采集结果。

覆盖场景：
  A 完整 Linux：能算出真 CPU%             → cpu_usage 有值，来源 proc_stat
  B Android 封锁：/proc/stat 读不到        → cpu_usage 必须是 None（不造假），
                                            且 extra 里给出频率占比等替代值
  C 首次采样：差值还无法计算               → None + 来源 proc_stat_first_sample
  D 网络三级降级：sysfs / /proc/self/net/dev → 来源标注必须正确
  E 一等字段：分项温度 / GPU 频率 / 电池     → 能读就读进主字段，读不到必须 None
  F 热区无 type 标签：机身温度仍有值，分项温度必须是 None（不按编号猜）
  G 设备能力探测：capabilities / is_root / qnn_runtime 必须是明确三态

用法：
    python docs/acceptance/agent_metrics_check.py
"""

from __future__ import annotations

import shutil
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "node-agent"))

import agent_metrics  # noqa: E402
from agent_metrics import MetricsCollector  # noqa: E402

_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok


def w(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def cpu_stat(user: int, idle: int) -> str:
    """构造一行 /proc/stat：cpu user nice system idle iowait irq softirq steal"""
    return f"cpu {user} 0 {user} {idle} 0 0 0 0\ncpu0 {user} 0 {user} {idle} 0 0 0 0\n"


def meminfo(total_kb: int = 12_000_000, avail_kb: int = 4_500_000) -> str:
    return (
        f"MemTotal:       {total_kb} kB\n"
        f"MemFree:        2_000_000 kB\n"
        f"MemAvailable:   {avail_kb} kB\n"
        "Buffers:         100000 kB\n"
        "Cached:          500000 kB\n"
    )


def self_stat(utime: int, stime: int) -> str:
    """构造 /proc/self/stat。

    comm 里故意放一个空格（`(python test)`）—— 真机上进程名带空格很常见，
    解析必须从**最后一个右括号**之后开始，否则 utime/stime 会整体错位。
    """
    # 字段位次（与 proc(5) 一致）：右括号后第 0 个是 state(字段3)，
    # ppid..cmajflt 共 10 个填充占字段 4~13，utime/stime 落在下标 11/12。
    return (
        "1234 (python test) S 1 2 3 4 5 6 7 8 9 10 "
        f"{utime} {stime} 0 0 0 0 0 0\n"
    )


def build_tree(root: Path, *, proc_stat: str | None, freq: bool = False,
               thermal: bool = False) -> Path:
    """按场景摆文件。proc_stat=None 表示这个文件"读不到"（模拟 SELinux 拒绝）。"""
    proc = root / "proc"
    sys_dir = root / "sys"
    proc.mkdir(parents=True, exist_ok=True)
    sys_dir.mkdir(parents=True, exist_ok=True)

    if proc_stat is not None:
        w(proc / "stat", proc_stat)
        w(proc / "loadavg", "0.42 0.51 0.48 2/512 12345\n")
    w(proc / "meminfo", meminfo())
    w(proc / "self/stat", self_stat(1000, 500))

    if freq:
        for i in (0, 4):
            w(sys_dir / f"devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq", "1800000\n")
            w(sys_dir / f"devices/system/cpu/cpu{i}/cpufreq/cpuinfo_max_freq", "2400000\n")
    if thermal:
        w(sys_dir / "class/thermal/thermal_zone0/temp", "37000\n")
    return root


def with_paths(root: Path):
    agent_metrics.PROC = str(root / "proc")
    agent_metrics.SYS = str(root / "sys")


def main() -> int:
    print("=" * 70)
    print("Agent 指标采集降级链验收（合成文件系统，无需真机）")
    print("=" * 70)
    tmp = Path(tempfile.mkdtemp(prefix="edge-metrics-check-"))
    real_proc, real_sys = agent_metrics.PROC, agent_metrics.SYS

    try:
        # ---------------- A 完整 Linux ----------------
        print("\n[A] 完整 Linux：/proc/stat 可读 → 应算出真 CPU%")
        root_a = build_tree(tmp / "a", proc_stat=cpu_stat(100, 800), freq=True, thermal=True)
        with_paths(root_a)
        c = MetricsCollector(simulate=False)
        first = c.collect()
        check("首次采样 cpu_usage 为 None（差值还不够算）", first.get("cpu_usage") is None,
              str(first.get("cpu_usage")))
        check("首次采样来源标注为 proc_stat_first_sample",
              (first.get("extra") or {}).get("cpu_source") == "proc_stat_first_sample",
              str((first.get("extra") or {}).get("cpu_source")))

        # 换一组计数：d_idle=600, d_total=800 → 使用率 25.0%
        w(root_a / "proc/stat", cpu_stat(200, 1400))
        second = c.collect()
        check("第二次采样 cpu_usage == 25.0", second.get("cpu_usage") == 25.0,
              str(second.get("cpu_usage")))
        check("来源标注为 proc_stat", (second.get("extra") or {}).get("cpu_source") == "proc_stat",
              str((second.get("extra") or {}).get("cpu_source")))
        check("内存总量解析正确（1200万 kB → 字节）",
              second.get("mem_total") == 12_000_000 * 1024, str(second.get("mem_total")))
        check("可用内存正确扣减（used < total）",
              0 < (second.get("mem_used") or 0) < (second.get("mem_total") or 0),
              f"used={second.get('mem_used')}")
        check("loadavg 可读时有值", isinstance(second.get("load_avg"), float),
              str(second.get("load_avg")))

        # ---------------- B Android 封锁 ----------------
        print("\n[B] Android 封锁：/proc/stat 读不到 → 必须留空，不能造假")
        root_b = build_tree(tmp / "b", proc_stat=None, freq=True, thermal=True)
        with_paths(root_b)
        c = MetricsCollector(simulate=False)
        c.collect()
        b = c.collect()
        check("cpu_usage 为 None（不拿替代指标冒充）", b.get("cpu_usage") is None,
              str(b.get("cpu_usage")))
        check("来源标注为 unavailable", (b.get("extra") or {}).get("cpu_source") == "unavailable",
              str((b.get("extra") or {}).get("cpu_source")))
        check("load_avg 同步为 None（同一批被封的文件）", b.get("load_avg") is None,
              str(b.get("load_avg")))
        check("替代指标 cpu_freq_avg_mhz 可用（1800MHz）",
              (b.get("extra") or {}).get("cpu_freq_avg_mhz") == 1800.0,
              str((b.get("extra") or {}).get("cpu_freq_avg_mhz")))
        check("替代指标 cpu_freq_ratio 可用（1800/2400 = 75%）",
              (b.get("extra") or {}).get("cpu_freq_ratio") == 75.0,
              str((b.get("extra") or {}).get("cpu_freq_ratio")))
        check("内存不受 CPU 封锁影响，仍然有值", b.get("mem_total") is not None,
              str(b.get("mem_total")))

        # 进程自身 CPU：真机上唯一稳的 CPU 真值
        time.sleep(0.25)
        w(root_b / "proc/self/stat", self_stat(1025, 500))
        b2 = c.collect()
        selfcpu = (b2.get("extra") or {}).get("agent_cpu_percent")
        check("agent_cpu_percent 可算（读自己的进程不受限）",
              isinstance(selfcpu, float) and 0 < selfcpu < 1000, str(selfcpu))

        # ---------------- C 网络三级降级 ----------------
        print("\n[C] 网络：三级来源必须依次降级并标注")
        root_c = build_tree(tmp / "c", proc_stat=None)
        with_paths(root_c)
        c = MetricsCollector(simulate=False)

        c.collect()
        c3 = c.collect()
        check("全无网络来源时 net 为 None + unavailable",
              c3.get("net_rx_bytes") is None
              and (c3.get("extra") or {}).get("net_source") == "unavailable",
              str((c3.get("extra") or {}).get("net_source")))

        w(root_c / "proc/self/net/dev",
          "Inter-|   Receive                                                |  Transmit\n"
          " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets\n"
          "wlan0: 111111 0 0 0 0 0 0 0 222222 0 0 0 0 0 0 0\n")
        c4 = c.collect()
        check("第二级 /proc/self/net/dev 生效且数值正确",
              c4.get("net_rx_bytes") == 111111 and c4.get("net_tx_bytes") == 222222
              and (c4.get("extra") or {}).get("net_source") == "proc_self_net_dev",
              f"{c4.get('net_rx_bytes')}/{c4.get('net_tx_bytes')} "
              f"src={(c4.get('extra') or {}).get('net_source')}")

        w(root_c / "proc/net/dev",
          "wlan0: 999999 0 0 0 0 0 0 0 888888 0 0 0 0 0 0 0\n")
        c5 = c.collect()
        check("第一级 /proc/net/dev 优先于第二级",
              c5.get("net_rx_bytes") == 999999
              and (c5.get("extra") or {}).get("net_source") == "proc_net_dev",
              f"src={(c5.get('extra') or {}).get('net_source')}")

        root_d = build_tree(tmp / "d", proc_stat=None)
        with_paths(root_d)
        w(root_d / "sys/class/net/wlan0/statistics/rx_bytes", "123456\n")
        w(root_d / "sys/class/net/wlan0/statistics/tx_bytes", "654321\n")
        c6 = MetricsCollector(simulate=False)
        c6.collect()
        c6.collect()
        check("第三级 sysfs 生效",
              c6.collect().get("net_rx_bytes") == 123456
              and (c6.collect().get("extra") or {}).get("net_source") == "sysfs_iface",
              str(c6.collect().get("net_rx_bytes")))

        # ---------------- D 契约一致性 ----------------
        print("\n[D] 契约：新增字段必须落在 extra，主字段保持 0~100 语义")
        root_a2 = build_tree(tmp / "a2", proc_stat=cpu_stat(100, 800), freq=True)
        with_paths(root_a2)
        c = MetricsCollector(simulate=False)
        c.collect()
        d = c.collect()
        extra = d.get("extra") or {}
        check("cpu_source / net_source 在 extra 内，不污染主字段",
              "cpu_source" in extra and "net_source" in extra)
        check("cpu_usage 仍是 0~100 的百分比（不是别的口径）",
              d.get("cpu_usage") is None or 0 <= d["cpu_usage"] <= 100,
              str(d.get("cpu_usage")))

        # ---------------- E 一等字段 ----------------
        print()
        print("[E] 一等字段：分项温度 / GPU 频率 / 电池必须进主字段")
        root_e = build_tree(tmp / "e", proc_stat=cpu_stat(100, 800), freq=True)
        with_paths(root_e)
        sys_e = root_e / "sys"
        # zone0 故意不带 type（沿用老场景的写法），zone1/zone2 带语义标签
        w(sys_e / "class/thermal/thermal_zone0/temp", "37000")
        w(sys_e / "class/thermal/thermal_zone1/type", "cpu-0-0")
        w(sys_e / "class/thermal/thermal_zone1/temp", "41000")
        w(sys_e / "class/thermal/thermal_zone2/type", "gpu-thermal")
        w(sys_e / "class/thermal/thermal_zone2/temp", "39000")
        w(sys_e / "class/devfreq/gpufreq/cur_freq", "490000000")
        w(sys_e / "class/power_supply/battery/capacity", "77")
        w(sys_e / "class/power_supply/battery/status", "Discharging")
        c = MetricsCollector(simulate=False)
        e = c.collect()
        check("temperature 取全部热区最高者（41.0，含无标签的 zone0）",
              e.get("temperature") == 41.0, str(e.get("temperature")))
        check("cpu_temp 按 type 标签命中（41.0）",
              e.get("cpu_temp") == 41.0, str(e.get("cpu_temp")))
        check("gpu_temp 按 type 标签命中（39.0）",
              e.get("gpu_temp") == 39.0, str(e.get("gpu_temp")))
        check("cpu_freq_mhz 进主字段且与 extra 同源（1800.0）",
              e.get("cpu_freq_mhz") == 1800.0
              and (e.get("extra") or {}).get("cpu_freq_avg_mhz") == 1800.0,
              str(e.get("cpu_freq_mhz")))
        check("gpu_freq_mhz 由 Hz 换算成 MHz（490.0）",
              e.get("gpu_freq_mhz") == 490.0, str(e.get("gpu_freq_mhz")))
        check("battery_percent / battery_status 进主字段",
              e.get("battery_percent") == 77.0 and e.get("battery_status") == "Discharging",
              f"{e.get('battery_percent')}/{e.get('battery_status')}")

        # ---------------- F 拿不到 ≠ 0 ----------------
        print()
        print("[F] 拿不到 ≠ 0：一等字段读不到时必须是 None")
        root_f = build_tree(tmp / "f", proc_stat=None)
        with_paths(root_f)
        f = MetricsCollector(simulate=False).collect()
        for key in ("cpu_temp", "gpu_temp", "cpu_freq_mhz", "gpu_freq_mhz",
                    "battery_percent", "battery_temp", "battery_status"):
            v = f.get(key)
            check(f"{key} 为 None（而不是 0 / 空串）", v is None, repr(v))
        check("一等字段全部出现在返回值里（键存在，值可为 None）",
              all(key in f for key in ("cpu_temp", "gpu_temp", "gpu_freq_mhz",
                                       "battery_percent", "battery_temp", "battery_status")))

        # 只有无标签热区时：机身温度有值，分项温度必须留空（不按编号猜）
        root_g = build_tree(tmp / "g", proc_stat=None, thermal=True)
        with_paths(root_g)
        g = MetricsCollector(simulate=False).collect()
        check("热区无 type 标签 → 机身温度有值（37.0）而 cpu_temp 为 None",
              g.get("temperature") == 37.0 and g.get("cpu_temp") is None,
              f"temperature={g.get('temperature')} cpu_temp={g.get('cpu_temp')}")

        # ---------------- G 设备能力 ----------------
        print()
        print("[G] 设备能力：capabilities / is_root / qnn_runtime 必须是明确三态")
        with_paths(root_e)
        info = agent_metrics.device_info(False, None)
        caps = info.get("capabilities") or {}
        check("capabilities.can_proc_stat=True（合成环境能读 /proc/stat）",
              caps.get("can_proc_stat") is True, str(caps.get("can_proc_stat")))
        check("capabilities.can_thermal=True（有热区）",
              caps.get("can_thermal") is True, str(caps.get("can_thermal")))
        check("capabilities.can_proc_net=False（合成环境没有 net/dev，判否要说得出理由）",
              caps.get("can_proc_net") is False, str(caps.get("can_proc_net")))
        check("is_root 落在三态 {-1,0,1}", info.get("is_root") in (-1, 0, 1),
              str(info.get("is_root")))
        check("qnn_runtime 落在三态 {-1,0,1}（-1=目录都打不开，未知）",
              caps.get("qnn_runtime") in (-1, 0, 1), str(caps.get("qnn_runtime")))
        check("型号字段缺失时一律 Unknown，不编造",
              all(info.get(k) == "Unknown" for k in ("model", "soc", "cpu", "gpu", "npu")),
              str({k: info.get(k) for k in ("model", "soc", "cpu")}))

    finally:
        agent_metrics.PROC, agent_metrics.SYS = real_proc, real_sys
        shutil.rmtree(tmp, ignore_errors=True)

    passed = sum(1 for _, ok, _ in _results if ok)
    total = len(_results)
    print("\n" + "=" * 70)
    print(f"结果：{passed}/{total} 通过")
    for name, ok, detail in _results:
        if not ok:
            print(f"  ✗ {name}" + (f" — {detail}" if detail else ""))
    print("=" * 70)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
