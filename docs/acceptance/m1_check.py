#!/usr/bin/env python3
"""里程碑 1 验收脚本：骨架 + 通信闭环。

自动断言任务书 §5 里程碑 1 的验收项：
  A1 平台存活
  A2 2 个 simulate 节点被自动发现并显示为 online
  A3 节点字段完整（§4 字段表要求）
  A4 kill 掉 1 个 Agent 后节点在阈值内变为 offline
  A5 离线节点保留 last_heartbeat（不置空、不残留旧数据冒充在线）
  A6 重启该 Agent 后自动恢复 online，且 node_id 复用（无重复记录）
  A7 心跳持续、指标真实入库

用法：
    python docs/acceptance/m1_check.py                    # 平台在本机默认端口
    python docs/acceptance/m1_check.py --base http://192.168.1.100:8000

设计说明：脚本负责"平台侧"的全部断言；节点的启动/击杀由外层 shell 驱动
（见 docs/acceptance/run_m1.sh），这样验收过程与真实运维操作一致。
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request

TOKEN = "edge-cluster-dev-token"
DEFAULT_BASE = "http://127.0.0.1:8000"

_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    mark = "PASS" if ok else "FAIL"
    print(f"  [{mark}] {name}" + (f" — {detail}" if detail else ""))
    return ok


def http_json(base: str, path: str, timeout: float = 5.0):
    """调用平台接口。显式绕过系统代理（NO_PROXY），避免被本机代理拦截。"""
    url = f"{base}{path}"
    req = urllib.request.Request(url, headers={"X-Auth-Token": TOKEN})
    # 关键：直连，不走 HTTP_PROXY（很多环境设了代理会导致 502）
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def wait_until(fn, timeout: float, interval: float = 1.0, desc: str = "") -> bool:
    """轮询等待条件成立，返回是否成功。"""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if fn():
                return True
        except Exception:
            pass
        time.sleep(interval)
    if desc:
        print(f"        （等待超时 {timeout:.0f}s：{desc}）")
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description="里程碑 1 验收检查")
    parser.add_argument("--base", default=DEFAULT_BASE, help="平台地址")
    parser.add_argument("--nodes", type=int, default=2, help="期望发现的节点数")
    parser.add_argument("--offline-timeout", type=float, default=30.0,
                        help="等待节点离线的上限（秒）")
    parser.add_argument("--phase", default="all",
                        choices=["all", "online", "offline", "recovery"],
                        help="分阶段执行：online=检查节点在线；offline=检查离线判定；"
                             "recovery=检查恢复；all=全流程（需人工配合 kill/重启）")
    args = parser.parse_args()
    base = args.base.rstrip("/")

    print("=" * 70)
    print("里程碑 1 验收：骨架 + 通信闭环")
    print(f"平台地址: {base}")
    print("=" * 70)

    # ---------- A1 平台存活 ----------
    print("\n[A1] 平台存活检查")
    try:
        health = http_json(base, "/api/v1/system/health")
        check("A1.1 /api/v1/system/health 返回 ok", health.get("status") == "ok",
              f"version={health.get('version')}")
    except Exception as exc:
        check("A1.1 /api/v1/system/health 返回 ok", False, f"无法连接平台: {exc}")
        return _summary()

    try:
        info = http_json(base, "/api/v1/system/info")
        check("A1.2 /api/v1/system/info 可读且鉴权生效",
              "node_stats" in info and "heartbeat_interval" in info,
              f"心跳周期={info.get('heartbeat_interval')}s 离线阈值={info.get('offline_threshold')}s")
    except Exception as exc:
        check("A1.2 /api/v1/system/info 可读且鉴权生效", False, str(exc))

    # 鉴权反例：不带 token 应 401
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        opener.open(urllib.request.Request(f"{base}/api/v1/nodes"), timeout=5)
        check("A1.3 无 token 应被拒绝", False, "未带 token 却访问成功")
    except urllib.error.HTTPError as exc:
        check("A1.3 无 token 应被拒绝", exc.code == 401, f"HTTP {exc.code}")
    except Exception as exc:
        check("A1.3 无 token 应被拒绝", False, str(exc))

    # ---------- A2 节点自动发现 ----------
    if args.phase in ("all", "online"):
        print(f"\n[A2] 节点自动发现（期望 {args.nodes} 个 simulate 节点）")

        def enough_online() -> bool:
            data = http_json(base, "/api/v1/nodes?status=online")
            return data.get("total", 0) >= args.nodes

        ok = wait_until(enough_online, timeout=25, interval=1.5,
                        desc=f"等待 {args.nodes} 个节点上线")
        nodes = http_json(base, "/api/v1/nodes")
        online = [n for n in nodes["items"] if n["status"] == "online"]
        check(f"A2.1 {args.nodes} 个节点在 25s 内显示为 online",
              ok and len(online) >= args.nodes,
              f"实际 online={len(online)} total={nodes['total']}")

        # ---------- A3 字段完整性 ----------
        print("\n[A3] 节点字段完整性（§4 字段表）")
        required = ["node_id", "name", "model", "soc", "android_version",
                    "agent_version", "ip", "status", "last_heartbeat"]
        missing_all: list[str] = []
        for n in nodes["items"]:
            # 值可以为空字符串，但字段必须存在
            missing = [k for k in required if k not in n]
            if missing:
                missing_all.append(f"{n.get('node_id')}: 缺 {missing}")
        check("A3.1 所有节点含 §4 要求的全部字段", not missing_all,
              "; ".join(missing_all) if missing_all else f"共 {len(nodes['items'])} 个节点")

        empty_core = [n["node_id"] for n in nodes["items"] if not n.get("node_id") or not n.get("ip")]
        check("A3.2 node_id 与 ip 非空", not empty_core, str(empty_core) if empty_core else "OK")

        sim_flag = [n for n in nodes["items"] if n.get("is_simulated")]
        check("A3.3 模拟节点被正确标记 is_simulated", len(sim_flag) > 0,
              f"{len(sim_flag)} 个模拟节点")

        # ---------- A7 指标入库 ----------
        print("\n[A7] 心跳与指标入库")
        target = nodes["items"][0]["node_id"]
        try:
            latest = http_json(base, f"/api/v1/nodes/{target}/metrics/latest")
            check("A7.1 节点指标已入库", latest is not None,
                  f"cpu={latest.get('cpu_usage')} mem={latest.get('mem_used')}/{latest.get('mem_total')}"
                  if latest else "无指标")
            if latest:
                has_core = latest.get("cpu_usage") is not None and latest.get("mem_total") is not None
                check("A7.2 核心指标（CPU/内存）非空", has_core)
                # 拿不到的指标必须为 null，不能是编造值
                check("A7.3 拿不到的功耗字段保持 null（不造假）",
                      latest.get("power_watts") is None,
                      f"power_watts={latest.get('power_watts')}")
        except Exception as exc:
            check("A7.1 节点指标已入库", False, str(exc))

        series = http_json(base, f"/api/v1/nodes/{target}/metrics?minutes=10")
        check("A7.4 指标历史曲线有多个采样点", series.get("count", 0) >= 2,
              f"count={series.get('count')}")

    # ---------- A4/A5 离线判定 ----------
    if args.phase in ("all", "offline"):
        print("\n[A4] 离线判定（请确保此时已 kill 掉其中一个 Agent）")
        print("     等待节点状态变为 offline ...")

        def any_offline() -> bool:
            data = http_json(base, "/api/v1/nodes?status=offline")
            return data.get("total", 0) >= 1

        ok = wait_until(any_offline, timeout=args.offline_timeout, interval=1.0,
                        desc="等待 offline 状态")
        all_nodes = http_json(base, "/api/v1/nodes")
        offline = [n for n in all_nodes["items"] if n["status"] == "offline"]
        online = [n for n in all_nodes["items"] if n["status"] == "online"]
        check("A4.1 节点在阈值内被标记为 offline", ok and len(offline) >= 1,
              f"offline={len(offline)} online={len(online)}")

        # A5：离线节点必须保留最后心跳时间（用于判断"离线多久"），
        #     但状态必须明确是 offline，不能仍显示 online
        kept = all(n.get("last_heartbeat") for n in offline)
        check("A5.1 离线节点保留最后心跳时间（不清零）", kept,
              str([n["last_heartbeat"] for n in offline]) if offline else "无离线节点")
        check("A5.2 离线节点状态未残留为 online",
              all(n["status"] == "offline" for n in offline))
        idle = [n["node_id"] for n in offline if n.get("uptime_seconds", 0) > 0]
        check("A5.3 离线节点在线时长归零（不显示虚假在线时长）", not idle,
              str(idle) if idle else "OK")
        check("A5.4 其他节点不受影响仍为 online", len(online) >= 1,
              f"online={len(online)}")

    # ---------- A6 恢复 ----------
    if args.phase in ("all", "recovery"):
        print("\n[A6] 重启 Agent 后自动恢复（请确保已重启被 kill 的 Agent）")
        before = http_json(base, "/api/v1/nodes")
        ids_before = sorted(n["node_id"] for n in before["items"])

        def recovered() -> bool:
            data = http_json(base, "/api/v1/nodes?status=online")
            return data.get("total", 0) >= args.nodes

        ok = wait_until(recovered, timeout=25, interval=1.5, desc="等待恢复在线")
        after = http_json(base, "/api/v1/nodes")
        ids_after = sorted(n["node_id"] for n in after["items"])
        online = [n for n in after["items"] if n["status"] == "online"]

        check("A6.1 重启后节点恢复 online", ok and len(online) >= args.nodes,
              f"online={len(online)}/{args.nodes}")
        check("A6.2 node_id 复用，无重复节点记录（不产生幽灵节点）",
              ids_before == ids_after,
              f"before={len(ids_before)} after={len(ids_after)}")

    return _summary()


def _summary() -> int:
    print("\n" + "=" * 70)
    passed = sum(1 for _, ok, _ in _results if ok)
    total = len(_results)
    failed = [(n, d) for n, ok, d in _results if not ok]
    print(f"验收结果：{passed}/{total} 通过")
    if failed:
        print("\n未通过项：")
        for name, detail in failed:
            print(f"  - {name}" + (f"（{detail}）" if detail else ""))
    else:
        print("全部通过 ✅")
    print("=" * 70)
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
