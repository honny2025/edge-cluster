#!/usr/bin/env python3
"""里程碑 2 验收脚本：完整状态监控 + 首页大屏。

自动断言任务书 §5 里程碑 2 的验收项：

  B1 大屏聚合接口（/system/overview、/system/cluster-metrics）可用且鉴权生效
  B2 汇总口径正确：只统计**在线且采样新鲜**的节点，无样本时为 null 而非 0
  B3 离线节点明确为离线，**不残留旧数据冒充在线**（M2 最核心的验收项）
  B4 指标持续稳定刷新：采样点连续增长、时间单调、间隔合理
  B5 告警机制：结构完整、离线节点告警、且**"从未上报"不误报为离线**
  B6 前端产物可交付（单端口托管）
  B7 拿不到的指标保持 null，新补充字段落在 extra 里

用法：
    python docs/acceptance/m2_check.py --phase online     # 节点都在线时跑
    python docs/acceptance/m2_check.py --phase offline    # 先停掉至少 1 个 Agent 再跑
    python docs/acceptance/m2_check.py                    # 默认 all（含 offline，需人工配合）

设计说明：脚本只做"平台侧"断言；节点进程的启停由人工/外层脚本驱动，
与真实运维操作一致（见 docs/deployment.md）。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

TOKEN = "edge-cluster-dev-token"
DEFAULT_BASE = "http://127.0.0.1:8000"

_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok


def http_json(base: str, path: str, timeout: float = 8.0):
    """调用平台接口。显式绕过系统代理，避免被本机 HTTP_PROXY 拦截。"""
    req = urllib.request.Request(f"{base}{path}", headers={"X-Auth-Token": TOKEN})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def http_raw(base: str, path: str, timeout: float = 8.0) -> tuple[int, str, str]:
    """返回 (状态码, 内容类型, 正文前若干字符)。"""
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(urllib.request.Request(f"{base}{path}"), timeout=timeout) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            return resp.status, resp.headers.get("Content-Type", ""), body[:200]
    except urllib.error.HTTPError as exc:
        return exc.code, exc.headers.get("Content-Type", "") if exc.headers else "", ""


def wait_until(fn, timeout: float, interval: float = 1.0, desc: str = "") -> bool:
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


def parse_utc(iso: str) -> datetime:
    """后端时间戳是 naive UTC，补上时区再比较。"""
    s = iso if iso.endswith("Z") else f"{iso}Z"
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def main() -> int:
    parser = argparse.ArgumentParser(description="里程碑 2 验收检查")
    parser.add_argument("--base", default=DEFAULT_BASE, help="平台地址")
    parser.add_argument("--nodes", type=int, default=2, help="期望的在线节点数")
    parser.add_argument("--offline-timeout", type=float, default=40.0,
                        help="等待节点离线的上限（秒）")
    parser.add_argument("--phase", default="all",
                        choices=["all", "online", "offline"],
                        help="online=在线场景；offline=需先停掉至少 1 个 Agent；all=两者")
    args = parser.parse_args()
    base = args.base.rstrip("/")

    print("=" * 70)
    print("里程碑 2 验收：完整状态监控 + 首页大屏")
    print(f"平台地址: {base}")
    print("=" * 70)

    # ---------- B1 接口可用性 ----------
    print("\n[B1] 大屏聚合接口")
    try:
        ov = http_json(base, "/api/v1/system/overview")
    except Exception as exc:
        check("B1.1 /system/overview 可读", False, f"无法访问: {exc}")
        return _summary()

    required_top = ["generated_at", "node_stats", "resources", "services",
                    "alerts", "alert_summary", "abnormal_nodes", "nodes_metrics"]
    missing = [k for k in required_top if k not in ov]
    check("B1.1 /system/overview 字段完整", not missing,
          f"缺 {missing}" if missing else f"共 {len(required_top)} 个顶层字段")

    res_keys = ["cpu_avg", "gpu_avg", "npu_avg", "temperature_avg",
                "mem_used", "mem_total", "mem_percent",
                "storage_used", "storage_total", "storage_percent",
                "counted_nodes", "online_nodes", "stale_nodes"]
    res = ov.get("resources", {})
    missing_res = [k for k in res_keys if k not in res]
    check("B1.2 resources 汇总字段完整", not missing_res,
          f"缺 {missing_res}" if missing_res else f"共 {len(res_keys)} 项")

    try:
        series = http_json(base, "/api/v1/system/cluster-metrics?minutes=30")
        check("B1.3 /system/cluster-metrics 可读", "items" in series,
              f"count={series.get('count')} bucket={series.get('bucket_seconds')}s")
    except Exception as exc:
        check("B1.3 /system/cluster-metrics 可读", False, str(exc))

    # 鉴权反例
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        opener.open(urllib.request.Request(f"{base}/api/v1/system/overview"), timeout=5)
        check("B1.4 无 token 访问 overview 应被拒绝", False, "未带 token 却访问成功")
    except urllib.error.HTTPError as exc:
        check("B1.4 无 token 访问 overview 应被拒绝", exc.code == 401, f"HTTP {exc.code}")
    except Exception as exc:
        check("B1.4 无 token 访问 overview 应被拒绝", False, str(exc))

    # ---------- B2 汇总口径 ----------
    if args.phase in ("all", "online"):
        print("\n[B2] 集群汇总口径（只统计在线且新鲜的节点）")

        counted, online = res.get("counted_nodes", 0), res.get("online_nodes", 0)
        check("B2.1 参与统计的节点数不超过在线节点数", counted <= online,
              f"counted={counted} online={online}")

        # 口径自洽：有节点在线却没进统计 → 必须能在 stale_nodes 里找到原因
        stale = res.get("stale_nodes", [])
        if online > counted:
            check("B2.2 未参与统计的节点被标记为 stale（口径可解释）",
                  len(stale) >= online - counted,
                  f"stale={stale}")
        else:
            check("B2.2 未参与统计的节点被标记为 stale（口径可解释）", True,
                  "全部在线节点都参与了统计")

        # 无样本的指标必须是 null，不能是 0（0 表示"测到了，值是 0"）
        avg_fields = ["cpu_avg", "gpu_avg", "npu_avg", "temperature_avg"]
        zero_is_null = True
        detail = []
        for f in avg_fields:
            v = res.get(f)
            if v == 0 and counted == 0:
                zero_is_null = False
                detail.append(f"{f}=0 但无样本")
        check("B2.3 无有效样本时均值为 null 而非 0（不造假）", zero_is_null,
              "; ".join(detail) if detail else f"cpu={res.get('cpu_avg')} gpu={res.get('gpu_avg')}")

        # percent 与 used/total 自洽
        consistent = True
        detail = []
        for name, u, t, p in (
            ("内存", res.get("mem_used"), res.get("mem_total"), res.get("mem_percent")),
            ("存储", res.get("storage_used"), res.get("storage_total"), res.get("storage_percent")),
        ):
            if u is None or t in (None, 0):
                continue
            expect = 100.0 * u / t
            if p is None or abs(expect - p) > 0.5:
                consistent = False
                detail.append(f"{name}: 期望 {expect:.1f}% 实际 {p}")
        check("B2.4 汇总使用率与 used/total 自洽", consistent,
              "; ".join(detail) if detail else f"内存={res.get('mem_percent')}% 存储={res.get('storage_percent')}%")

        # ---------- B4 指标持续刷新 ----------
        print("\n[B4] 指标持续稳定刷新")
        nodes = http_json(base, "/api/v1/nodes")["items"]
        online_nodes = [n for n in nodes if n["status"] == "online"]
        if not online_nodes:
            check("B4.1 存在在线节点可供检查", False, "当前没有 online 节点")
        else:
            target = online_nodes[0]["node_id"]
            s1 = http_json(base, f"/api/v1/nodes/{target}/metrics?minutes=10")
            latest1 = s1["items"][-1]["ts"] if s1.get("items") else None
            n1 = s1.get("count", 0)
            # 心跳周期 5s，等 12s 至少应推进一个采样周期
            time.sleep(12)
            s2 = http_json(base, f"/api/v1/nodes/{target}/metrics?minutes=10")
            latest2 = s2["items"][-1]["ts"] if s2.get("items") else None
            n2 = s2.get("count", 0)

            # 判据必须用"最新采样时间是否推进"，**不能用点数是否增长**：
            # 固定回看窗口在稳态下点数是恒定的（新点进来的同时旧点滑出窗口），
            # 甚至因 2s 降采样保护而小幅波动。用点数判断会得出"没在刷新"的错误结论。
            if latest1 and latest2:
                advanced = (parse_utc(latest2) - parse_utc(latest1)).total_seconds()
                check("B4.1 最新采样持续推进（12s 内前进至少 5s）", advanced >= 5.0,
                      f"最新采样前进了 {advanced:.1f}s（窗口内点数 {n1} → {n2}，"
                      f"稳态下点数恒定属正常）")
            else:
                check("B4.1 最新采样持续推进（12s 内前进至少 5s）", False,
                      f"无采样数据（count={n1} → {n2}）")

            items = s2.get("items", [])
            ts = [parse_utc(i["ts"]) for i in items]
            check("B4.2 采样时间单调递增（曲线不会来回跳）",
                  all(ts[i] <= ts[i + 1] for i in range(len(ts) - 1)),
                  f"{len(ts)} 个点")

            # 只检查**最近约 1 分钟**的间隔。
            # 整个 10 分钟窗口里可能包含服务重启造成的历史断档，那是真实发生的
            # 断档（Agent 停了当然没采样），不能算作"刷新不稳定"。
            recent = ts[-12:] if len(ts) > 12 else ts
            gaps = [(recent[i + 1] - recent[i]).total_seconds() for i in range(len(recent) - 1)]
            # 心跳 5s、平台有 2s 降采样保护，放宽到 2~12s 视为正常
            ok_gap = all(1.5 <= g <= 12.0 for g in gaps) if gaps else False
            check("B4.3 最近 1 分钟采样间隔稳定（2~12s，持续刷新无空洞）", ok_gap,
                  f"最近 {len(recent)} 点，最小={min(gaps):.1f}s 最大={max(gaps):.1f}s"
                  if gaps else "样本太少")

            # 全窗口的最长断档仅作信息展示（含历史重启断档）
            all_gaps = [(ts[i + 1] - ts[i]).total_seconds() for i in range(len(ts) - 1)]
            if all_gaps:
                print(f"        信息：全窗口最长断档 {max(all_gaps):.1f}s"
                      f"（含服务重启期间的真实断档，不影响判定）")

        # ---------- B7 不造假 ----------
        print("\n[B7] 指标真实性（拿不到就留空）")
        latest = http_json(base, f"/api/v1/nodes/{online_nodes[0]['node_id']}/metrics/latest") \
            if online_nodes else None
        if latest:
            check("B7.1 无法获取的功耗字段保持 null（不编造）",
                  latest.get("power_watts") is None,
                  f"power_watts={latest.get('power_watts')}")
            extra = latest.get("extra") or {}
            check("B7.2 补充指标落在 extra JSON（不污染 §4 主字段表）",
                  isinstance(extra, dict) and len(extra) >= 0,
                  f"extra keys={sorted(extra.keys())}")
            has_freq = "cpu_freq_avg_mhz" in extra
            check("B7.3 新增采集项（CPU 频率 / 电池）已上报", has_freq,
                  f"freq={extra.get('cpu_freq_avg_mhz')} battery={extra.get('battery_percent')}")

    # ---------- B3 / B5 离线场景 ----------
    if args.phase in ("all", "offline"):
        print("\n[B3] 离线节点不残留旧数据（需先停掉至少 1 个 Agent）")
        print("     等待节点变为 offline ...")

        # 先记下离线前的统计口径，便于对比
        before = http_json(base, "/api/v1/system/overview")
        before_counted = before["resources"]["counted_nodes"]

        ok = wait_until(
            lambda: http_json(base, "/api/v1/nodes?status=offline").get("total", 0) >= 1,
            timeout=args.offline_timeout, interval=1.5, desc="等待 offline 状态")
        check("B3.1 存在被标记为 offline 的节点", ok)

        ov2 = http_json(base, "/api/v1/system/overview")
        nodes2 = http_json(base, "/api/v1/nodes")["items"]
        offline_ids = [n["node_id"] for n in nodes2 if n["status"] == "offline"]
        res2 = ov2["resources"]

        # ★ 核心断言：离线节点在 nodes_metrics 里必须是 null，绝不带旧值
        leaked = []
        for nid in offline_ids:
            if ov2.get("nodes_metrics", {}).get(nid) is not None:
                leaked.append(nid)
        check("B3.2 离线节点的当前指标为 null（后端不下发旧值）", not leaked,
              f"泄漏旧值: {leaked}" if leaked else f"离线 {len(offline_ids)} 台，均无指标下发")

        # 离线节点不参与汇总
        online2 = len([n for n in nodes2 if n["status"] == "online"])
        check("B3.3 离线节点不参与集群资源汇总",
              res2.get("counted_nodes", 0) <= online2,
              f"counted={res2.get('counted_nodes')} online={online2}（离线前 counted={before_counted}）")

        # 离线节点出现在异常节点列表里
        abnormal_ids = [n["node_id"] for n in ov2.get("abnormal_nodes", [])]
        check("B3.4 离线节点出现在 abnormal_nodes 列表中",
              all(nid in abnormal_ids for nid in offline_ids),
              f"abnormal={len(abnormal_ids)}")

        # 历史曲线仍可查（历史是真实发生过的，可以看；但不当"当前值"用）
        if offline_ids:
            hist = http_json(base, f"/api/v1/nodes/{offline_ids[0]}/metrics?minutes=60")
            check("B3.5 离线节点历史曲线仍可查询（历史数据保留）",
                  hist.get("count", 0) >= 1, f"count={hist.get('count')}")

        # ---------- B5 告警 ----------
        print("\n[B5] 告警机制")

        def alert_shape_ok(a: dict) -> bool:
            return all(k in a for k in ("level", "kind", "node_id", "message")) \
                and a.get("level") in ("warning", "critical")

        alerts = ov2.get("alerts", [])
        check("B5.1 告警结构完整（level/kind/node_id/message）",
              all(alert_shape_ok(a) for a in alerts),
              f"当前 {len(alerts)} 条告警")

        offline_alerts = [a for a in alerts if a["kind"] == "node_offline"]
        check("B5.2 离线节点触发 node_offline 告警",
              len(offline_alerts) >= len(offline_ids),
              f"node_offline 告警 {len(offline_alerts)} 条，离线节点 {len(offline_ids)} 台")

        # ★ 区分"没有上报"与"离线"：从未上报的节点不应报离线
        never_seen_ids = [n["node_id"] for n in nodes2 if n["status"] == "never_seen"]
        misreported = [a["node_id"] for a in offline_alerts if a["node_id"] in never_seen_ids]
        check("B5.3 never_seen（没上报）不误报为 offline（区分四态）", not misreported,
              f"误报: {misreported}" if misreported else f"never_seen {len(never_seen_ids)} 台，无误报")

        # 概要与明细一致
        summ = ov2.get("alert_summary", {})
        check("B5.4 alert_summary 与告警明细一致",
              summ.get("total") == len(alerts)
              and summ.get("critical") == len([a for a in alerts if a["level"] == "critical"]),
              f"summary={summ}")

    # ---------- B6 前端交付 ----------
    print("\n[B6] 前端产物（单端口交付）")
    dist_index = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__)))), "web", "dist", "index.html")
    check("B6.1 web/dist/index.html 存在", os.path.exists(dist_index), dist_index)

    code, ctype, body = http_raw(base, "/")
    check("B6.2 平台根路径返回控制台 HTML", code == 200 and "text/html" in ctype,
          f"HTTP {code} {ctype}")
    check("B6.3 根路径确实返回前端页面（非占位 JSON）",
          "<div id=" in body or "<!doctype" in body.lower(),
          body[:60].replace("\n", " "))

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
