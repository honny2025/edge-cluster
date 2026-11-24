#!/usr/bin/env python3
"""指标分层保留验收（原始 / 分钟级 / 小时级）。

**不需要平台在跑，也不需要真机** —— 脚本自己在临时目录建一个 SQLite 库，
塞进构造好的采样，调聚合服务，然后断言结果。跑完删干净。

为什么要有这个脚本
------------------
分层保留最危险的失败不是"聚合算错"，而是**顺序错**：

    先删原始、后聚合   →  历史永久断档，而且不报错，几个月后才发现

所以下面有一组专门针对顺序的断言（E 段），包括"原始已经没了、重跑聚合
不能把已有的分钟级数据误删"这条 —— 它对应的正是"删掉整段再插"这种写法。

覆盖：
  A 桶对齐      时间戳必须落在分钟/小时边界上
  B 聚合正确性  均值 / min / max / samples 计数
  C 不造假      整桶都没采到的字段必须是 None，不是 0
  D 特殊字段    累计计数器取最后一个值；枚举状态取最后一个非空值
  E 顺序与幂等  重跑幂等；原始被清理后重跑不得破坏已有聚合
  F 分层清理    原始按 raw 保留期剪、分钟级按 minute 保留期剪
  G 保留期为 0  表示不清理，而不是"立刻删光"

用法：
    python docs/acceptance/metric_tiers_check.py
"""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> bool:
    _results.append((name, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""))
    return ok


# ---------------------------------------------------------------------------
# 环境隔离：必须在 import app.config 之前设置 —— settings 是模块级单例，
# 导入那一刻就把环境变量读完了，晚一步就全落在真实库上。
# ---------------------------------------------------------------------------
TMP = Path(tempfile.mkdtemp(prefix="edge-tiers-"))
os.environ["EDGE_DATA__DB_PATH"] = str(TMP / "edge.db")
os.environ["EDGE_DATA__MODELS_DIR"] = str(TMP / "models")
os.environ["EDGE_DATA__LOGS_DIR"] = str(TMP / "logs")
os.environ["EDGE_WEB__SERVE_STATIC"] = "false"
os.environ["EDGE_SERVER__METRIC_RAW_RETENTION_DAYS"] = "2"
os.environ["EDGE_SERVER__METRIC_MINUTE_RETENTION_DAYS"] = "30"
os.environ["EDGE_SERVER__METRIC_HOUR_RETENTION_DAYS"] = "365"

sys.path.insert(0, str(ROOT / "server"))

from app.config import settings  # noqa: E402
from app.database import SessionLocal, init_db  # noqa: E402
from app.models import KIND_HOUR, KIND_MINUTE, MetricRollup, MetricSample, Node  # noqa: E402
from app.services import metric_rollup, metrics_query  # noqa: E402


def _seed_node(db, node_id: str = "tiers-01") -> None:
    if db.get(Node, node_id) is None:
        db.add(Node(node_id=node_id, name=node_id, status="online"))
        db.commit()


def _add(db, node_id: str, ts: datetime, **kw) -> None:
    db.add(MetricSample(node_id=node_id, ts=ts, **kw))


def _utcnow() -> datetime:
    """naive UTC —— 与 app.models.utcnow() 一致。

    Python 3.12+ 起 datetime.utcnow() 已废弃；直接用它会刷一屏
    DeprecationWarning，把真正的失败信息淹掉。
    """
    return datetime.now(timezone.utc).replace(tzinfo=None)


def _rollups(db, kind: str):
    return list(
        db.query(MetricRollup)
        .filter(MetricRollup.kind == kind)
        .order_by(MetricRollup.ts.asc())
        .all()
    )


# ---------------------------------------------------------------------------


def main() -> int:
    print("=" * 70)
    print("指标分层保留验收")
    print(f"临时库：{settings.db_file}")
    print("=" * 70)

    init_db()

    # 基时刻对齐到整分钟，方便手算期望值
    base = _utcnow().replace(second=0, microsecond=0) - timedelta(hours=1)

    with SessionLocal() as db:
        _seed_node(db)
        # 第 1 分钟：3 条采样
        _add(db, "tiers-01", base + timedelta(seconds=5),
             cpu_usage=10.0, mem_used=1000, mem_total=10000,
             net_rx_bytes=100, battery_status="charging", gpu_usage=None)
        _add(db, "tiers-01", base + timedelta(seconds=20),
             cpu_usage=20.0, mem_used=2000, mem_total=10000,
             net_rx_bytes=200, battery_status=None, gpu_usage=None)
        _add(db, "tiers-01", base + timedelta(seconds=55),
             cpu_usage=30.0, mem_used=3000, mem_total=10000,
             net_rx_bytes=300, battery_status="discharging", gpu_usage=None)
        # 第 2 分钟：1 条采样
        _add(db, "tiers-01", base + timedelta(minutes=1, seconds=10),
             cpu_usage=40.0, mem_used=4000, mem_total=10000,
             net_rx_bytes=400, battery_status="discharging", gpu_usage=None)
        db.commit()

        # ---------------------------------------------------------------
        print("\nA 桶对齐")
        # ---------------------------------------------------------------
        n_min = metric_rollup.rollup_range(
            db, KIND_MINUTE, base - timedelta(minutes=1), base + timedelta(minutes=5)
        )
        rows = _rollups(db, KIND_MINUTE)
        check("分钟级写出了 2 个桶", n_min == 2 and len(rows) == 2,
              f"written={n_min} rows={len(rows)}")
        check("桶起点对齐到整分钟（秒/微秒为 0）",
              all(r.ts.second == 0 and r.ts.microsecond == 0 for r in rows),
              str([r.ts.isoformat() for r in rows]))
        if rows:
            check("第 1 个桶的起点 == 期望值", rows[0].ts == base,
                  f"{rows[0].ts} vs {base}")

        # ---------------------------------------------------------------
        print("\nB 聚合正确性")
        # ---------------------------------------------------------------
        if len(rows) == 2:
            b0, b1 = rows
            check("cpu 均值 (10+20+30)/3 = 20", b0.cpu_usage == 20.0, str(b0.cpu_usage))
            check("cpu max = 30", b0.extra["stats"]["cpu_usage"]["max"] == 30.0,
                  str(b0.extra["stats"]["cpu_usage"]["max"]))
            check("cpu min = 10", b0.extra["stats"]["cpu_usage"]["min"] == 10.0,
                  str(b0.extra["stats"]["cpu_usage"]["min"]))
            check("samples 计数 = 3", b0.samples == 3, str(b0.samples))
            check("mem_used 均值取整 = 2000（BigInteger 列不回退成浮点）",
                  b0.mem_used == 2000 and isinstance(b0.mem_used, int), repr(b0.mem_used))
            check("未跨桶污染：第 2 桶 cpu = 40", b1.cpu_usage == 40.0, str(b1.cpu_usage))
            check("第 2 桶 samples = 1", b1.samples == 1, str(b1.samples))
            check("field_samples 如实记录参与平均的条数",
                  b0.extra["rollup"]["field_samples"].get("cpu_usage") == 3,
                  str(b0.extra["rollup"]["field_samples"].get("cpu_usage")))

        # ---------------------------------------------------------------
        print("\nC 不造假：拿不到就是 None")
        # ---------------------------------------------------------------
        if len(rows) == 2:
            check("整桶都没采到的 gpu_usage 必须是 None（不是 0.0）",
                  rows[0].gpu_usage is None, repr(rows[0].gpu_usage))
            check("gpu_usage 不进入 stats（没有 min/max 可言）",
                  "gpu_usage" not in rows[0].extra["stats"],
                  str(list(rows[0].extra["stats"].keys())))
            check("整桶没采到的 npu_usage 同样是 None",
                  rows[0].npu_usage is None, repr(rows[0].npu_usage))

        # ---------------------------------------------------------------
        print("\nD 特殊字段语义")
        # ---------------------------------------------------------------
        if len(rows) == 2:
            check("net_rx_bytes 取桶内最后一个值 300（不是均值 200）",
                  rows[0].net_rx_bytes == 300, repr(rows[0].net_rx_bytes))
            check("battery_status 取最后一个非空值 discharging（跳过中间的 None）",
                  rows[0].battery_status == "discharging", repr(rows[0].battery_status))

        # ---------------------------------------------------------------
        print("\nE 幂等与顺序安全")
        # ---------------------------------------------------------------
        before = [(r.ts.isoformat(), r.cpu_usage, r.samples) for r in _rollups(db, KIND_MINUTE)]
        metric_rollup.rollup_range(
            db, KIND_MINUTE, base - timedelta(minutes=1), base + timedelta(minutes=5)
        )
        after = [(r.ts.isoformat(), r.cpu_usage, r.samples) for r in _rollups(db, KIND_MINUTE)]
        check("重跑聚合：不产生重复桶，值也不变", before == after,
              f"{len(before)} → {len(after)}")

        # 关键：模拟"原始数据已被清理"，再重跑聚合。已有的分钟级必须原样保留。
        db.query(MetricSample).delete()
        db.commit()
        wrote = metric_rollup.rollup_range(
            db, KIND_MINUTE, base - timedelta(minutes=1), base + timedelta(minutes=5)
        )
        survived = _rollups(db, KIND_MINUTE)
        check("原始数据删光后重跑聚合：写入 0 个桶", wrote == 0, f"written={wrote}")
        check("且已有的分钟级数据一条都不能少（先删后插写法会在这里全灭）",
              len(survived) == 2, f"剩 {len(survived)} 桶")

        # ---------------------------------------------------------------
        print("\nF 分层清理")
        # ---------------------------------------------------------------
        now = _utcnow()
        # 3 天前的原始（超过 raw 保留 2 天）；30 天前的分钟级（未超 minute 保留 30 天）；
        # 400 天前的小时级（超过 hour 保留 365 天）
        _add(db, "tiers-01", now - timedelta(days=3), cpu_usage=1.0)
        db.add(MetricRollup(node_id="tiers-01", kind=KIND_MINUTE,
                            ts=now - timedelta(days=30) + timedelta(hours=1),
                            samples=12, cpu_usage=5.0))
        db.add(MetricRollup(node_id="tiers-01", kind=KIND_HOUR,
                            ts=now - timedelta(days=400), samples=720, cpu_usage=6.0))
        db.commit()

        stats = metric_rollup.run_once()
        check("run_once 返回 ok=True", stats.get("ok") is True, str(stats.get("error")))
        if stats.get("ok"):
            check("原始层清理到了过期数据（raw=2 天）",
                  stats["pruned"]["raw"] >= 1, str(stats["pruned"]["raw"]))
            check("分钟级层清理未误删 30 天内的数据（minute=30 天）",
                  stats["pruned"][KIND_MINUTE] == 0, str(stats["pruned"][KIND_MINUTE]))
            check("小时级层清掉了 400 天前的数据（hour=365 天）",
                  stats["pruned"][KIND_HOUR] >= 1, str(stats["pruned"][KIND_HOUR]))

        left_raw_old = db.query(MetricSample).filter(
            MetricSample.ts < now - timedelta(days=2)
        ).count()
        check("清理后库里确实没有超过 2 天的原始采样", left_raw_old == 0, str(left_raw_old))

        # 顺序：原始被删之前，聚合必须已经产出（否则 3 天前那条就永久丢了）
        exists = db.query(MetricRollup).filter(
            MetricRollup.kind == KIND_MINUTE,
            MetricRollup.ts <= now - timedelta(days=3),
            MetricRollup.ts > now - timedelta(days=3, minutes=5),
        ).count()
        check("被清理的那条原始采样，其分钟级聚合已先落库（证明顺序是先聚合后删除）",
              exists >= 1, f"命中 {exists} 桶")

        # ---------------------------------------------------------------
        print("\nG 保留期为 0 = 不清理")
        # ---------------------------------------------------------------
        settings.server.metric_hour_retention_days = 0
        before_hour = db.query(MetricRollup).filter(MetricRollup.kind == KIND_HOUR).count()
        db.add(MetricRollup(node_id="tiers-01", kind=KIND_HOUR,
                            ts=now - timedelta(days=900), samples=1, cpu_usage=9.0))
        db.commit()
        st2 = metric_rollup.run_once()
        after_hour = db.query(MetricRollup).filter(MetricRollup.kind == KIND_HOUR).count()
        check("hour 保留期设为 0 时不清任何小时级数据",
              st2.get("ok") and after_hour == before_hour + 1,
              f"{before_hour} → {after_hour}, pruned={st2.get('pruned', {}).get(KIND_HOUR)}")
        settings.server.metric_hour_retention_days = 365

        # ---------------------------------------------------------------
        print("\nH 现状汇报接口")
        # ---------------------------------------------------------------
        st = metric_rollup.tier_status()
        check("tier_status 覆盖三层", set(st["tiers"]) == {"raw", KIND_MINUTE, KIND_HOUR},
              str(list(st["tiers"])))
        check("每层都带保留期与行数",
              all({"retention_days", "rows", "oldest", "newest"} <= set(v)
                  for v in st["tiers"].values()),
              str({k: v["rows"] for k, v in st["tiers"].items()}))

        # ---------------------------------------------------------------
        print("\nI 查询分层：不重叠、不留缝、不重复计数")
        # ---------------------------------------------------------------
        # 这一段盯的是"查询时用哪一层"。切错了的后果不是报错，而是：
        #   * 两段重叠 → 同一时刻被算两遍，曲线数值虚高（不容易看出来）
        #   * 中间留缝 → 曲线出现一段凭空消失的空洞
        # 两种都只在肉眼看图时才发现，所以必须由断言钉死。
        now2 = _utcnow()
        s = settings.server
        s.metric_raw_retention_days, s.metric_minute_retention_days = 2, 30
        s.metric_hour_retention_days = 365

        def _segs(minutes: int):
            since = now2 - timedelta(minutes=minutes)
            return metrics_query.tier_plan(since, now2, now2)

        def _no_overlap(segs) -> bool:
            """按时间排序后，任意两段不得相交；相邻两段必须首尾相接（不留缝）。"""
            ss = sorted(segs, key=lambda x: x[1])
            for i in range(len(ss) - 1):
                if ss[i][2] > ss[i + 1][1]:
                    return False  # 重叠
                if ss[i][2] < ss[i + 1][1]:
                    return False  # 中间有缝
            return True

        def _ordered(segs) -> bool:
            return all(a < b for _t, a, b in segs)

        seg30 = _segs(30)
        check("30 分钟窗口只读原始层（最细的够用就用最细的）",
              [t for t, _a, _b in seg30] == ["raw"], str([t for t, *_ in seg30]))

        seg7d = _segs(60 * 24 * 7)
        check("7 天窗口 = minute + raw 两层",
              sorted(t for t, _a, _b in seg7d) == ["minute", "raw"],
              str([t for t, *_ in seg7d]))
        check("7 天窗口两层首尾相接、不重叠、不留缝", _no_overlap(seg7d),
              str([(t, a.isoformat(), b.isoformat()) for t, a, b in seg7d]))
        check("所有段的起点都早于终点", _ordered(seg7d))

        seg1y = _segs(60 * 24 * 365)
        check("365 天窗口三层齐全：hour + minute + raw",
              sorted(t for t, _a, _b in seg1y) == ["hour", "minute", "raw"],
              str([t for t, *_ in seg1y]))
        check("365 天窗口三层首尾相接、不重叠、不留缝", _no_overlap(seg1y),
              str([(t, a.isoformat(), b.isoformat()) for t, a, b in seg1y]))

        # 最容易写错的一条：原始层不清理（保留期 0）时，它覆盖整个窗口，
        # 于是 minute/hour 两段**必须为空**。若只按"自己那层的下界"切，
        # 这三段会同时覆盖整个窗口 —— 同一时刻被算三遍。
        s.metric_raw_retention_days = 0
        seg_zero = _segs(60 * 24 * 365)
        check("原始层保留期设为 0（不清理）时，只剩 raw 一段，聚合层不得再参与",
              [t for t, _a, _b in seg_zero] == ["raw"],
              str([t for t, *_ in seg_zero]))
        s.metric_raw_retention_days = 2

        # 反向：分钟级不清理时，hour 段必须为空（否则与 minute 重复）
        s.metric_minute_retention_days = 0
        seg_m0 = _segs(60 * 24 * 365)
        check("分钟级保留期设为 0 时，hour 段必须为空（不得与 minute 重复）",
              "hour" not in [t for t, _a, _b in seg_m0],
              str([t for t, *_ in seg_m0]))
        check("此时剩下两层仍不重叠", _no_overlap(seg_m0))
        s.metric_minute_retention_days = 30

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
    try:
        code = main()
    finally:
        shutil.rmtree(TMP, ignore_errors=True)
    sys.exit(code)
