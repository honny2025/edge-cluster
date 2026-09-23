"""首页大屏聚合（里程碑 2）。

两个数据源：

- `build_overview()`        —— 当前时刻的集群快照（汇总卡 + 告警 + 异常节点）
- `build_cluster_series()`  —— 集群资源时间序列（大屏趋势图）

**核心纪律**：汇总只统计 `online` 且**采样新鲜**的节点。
离线节点即使库里还留着最后一条采样，也不参与任何汇总 —— 否则就是
"用旧数据冒充在线"，正是任务书 §4 要禁止的行为。
"""

from __future__ import annotations

from datetime import datetime, timedelta
from typing import Any

from sqlalchemy import func, select

from ..core.logging_conf import get_logger
from ..database import SessionLocal
from ..models import MetricSample, Node, NodeStatus, Service, ServiceStatus, utcnow
from .alerting import alerting
from .metrics_query import is_fresh, latest_samples, percent, sample_age_seconds
from .node_registry import registry

logger = get_logger("services.overview")

# 趋势图最多返回多少个数据点（超出则自动加大分桶粒度）
MAX_SERIES_POINTS = 180


class OverviewService:
    """大屏数据装配。无状态。"""

    # ------------------------------------------------------------------ 快照

    def build_overview(self) -> dict[str, Any]:
        now = utcnow()
        nodes = registry.list()
        node_ids = [n["node_id"] for n in nodes]

        with SessionLocal() as db:
            latest = latest_samples(db, node_ids)
            services_running = int(
                db.scalar(
                    select(func.count(Service.service_id)).where(
                        Service.status == ServiceStatus.RUNNING.value
                    )
                )
                or 0
            )
            services_total = int(db.scalar(select(func.count(Service.service_id))) or 0)

        stats = {
            "total": len(nodes),
            "online": 0,
            "offline": 0,
            "never_seen": 0,
        }
        for n in nodes:
            s = n.get("status")
            if s in stats:
                stats[s] += 1

        # ---- 集群资源汇总：只算 online + 采样新鲜的节点 ----
        cpu_vals: list[float] = []
        gpu_vals: list[float] = []
        npu_vals: list[float] = []
        mem_used_sum = mem_total_sum = 0
        sto_used_sum = sto_total_sum = 0
        temp_vals: list[float] = []
        counted_ids: list[str] = []
        stale_ids: list[str] = []

        online_nodes = [n for n in nodes if n["status"] == NodeStatus.ONLINE.value]
        for n in online_nodes:
            nid = n["node_id"]
            sample = latest.get(nid)
            if not sample:
                # 在线但一次指标都没收到，不进汇总（也不报错）
                stale_ids.append(nid)
                continue
            if not is_fresh(sample, now):
                # 心跳还在、采集却断了 —— 不拿旧值顶替
                stale_ids.append(nid)
                continue

            counted_ids.append(nid)
            if sample.get("cpu_usage") is not None:
                cpu_vals.append(float(sample["cpu_usage"]))
            if sample.get("gpu_usage") is not None:
                gpu_vals.append(float(sample["gpu_usage"]))
            if sample.get("npu_usage") is not None:
                npu_vals.append(float(sample["npu_usage"]))
            if sample.get("temperature") is not None:
                temp_vals.append(float(sample["temperature"]))
            if sample.get("mem_used") is not None:
                mem_used_sum += int(sample["mem_used"])
            if sample.get("mem_total") is not None:
                mem_total_sum += int(sample["mem_total"])
            if sample.get("storage_used") is not None:
                sto_used_sum += int(sample["storage_used"])
            if sample.get("storage_total") is not None:
                sto_total_sum += int(sample["storage_total"])

        resources = {
            # 平均值类：无有效样本时为 None（前端显示"—"，不显示 0）
            "cpu_avg": _avg(cpu_vals),
            "gpu_avg": _avg(gpu_vals),
            "npu_avg": _avg(npu_vals),
            "temperature_avg": _avg(temp_vals),
            "temperature_max": max(temp_vals) if temp_vals else None,
            # 累加类：手机内存/存储可以跨节点求和，表示集群总池子
            "mem_used": mem_used_sum if mem_total_sum else None,
            "mem_total": mem_total_sum or None,
            "mem_percent": percent(mem_used_sum, mem_total_sum),
            "storage_used": sto_used_sum if sto_total_sum else None,
            "storage_total": sto_total_sum or None,
            "storage_percent": percent(sto_used_sum, sto_total_sum),
            # 统计口径透明化：前端可显示"参与统计 2/3 台"
            "counted_nodes": len(counted_ids),
            "online_nodes": len(online_nodes),
            "stale_nodes": stale_ids,
        }

        alerts = alerting.evaluate(nodes, latest, now)

        # 各节点最新指标：**离线或采样陈旧的节点一律给 None**。
        # 从数据源头就不把旧值发给前端，前端也就不可能"用旧数据冒充在线"。
        nodes_metrics: dict[str, dict[str, Any] | None] = {}
        for n in nodes:
            nid = n["node_id"]
            sample = latest.get(nid)
            usable = n["status"] == NodeStatus.ONLINE.value and is_fresh(sample, now)
            nodes_metrics[nid] = _metric_public(sample) if (usable and sample) else None

        # 异常节点：离线 + 从未上报分开列，前端文案不同（"离线" vs "未接入"）
        abnormal = [
            {
                "node_id": n["node_id"],
                "name": n["name"],
                "status": n["status"],
                "last_heartbeat": n["last_heartbeat"],
                "ip": n["ip"],
                "is_simulated": n["is_simulated"],
            }
            for n in nodes
            if n["status"] != NodeStatus.ONLINE.value
        ]

        return {
            "generated_at": now.isoformat(),
            "node_stats": stats,
            "resources": resources,
            "services": {"running": services_running, "total": services_total},
            "alerts": alerts,
            "alert_summary": {
                "critical": sum(1 for a in alerts if a["level"] == "critical"),
                "warning": sum(1 for a in alerts if a["level"] == "warning"),
                "total": len(alerts),
            },
            "abnormal_nodes": abnormal,
            "nodes_metrics": nodes_metrics,
            # 里程碑 3 起填充；先给空数组，前端无需改结构
            "recent_deployments": [],
            "stale_nodes": stale_ids,
        }

    # ------------------------------------------------------------------ 趋势

    def build_cluster_series(self, minutes: int = 30) -> dict[str, Any]:
        """集群资源时间序列：每个时间桶内，对所有节点的采样做平均/累加。

        分桶在 Python 里做（SQLite 对 DATETIME 分桶不顺手），
        桶宽随回看窗口自适应，保证点数不超过 MAX_SERIES_POINTS。
        时间差一律用 datetime 相减，**不用 timestamp()** —— 避免 naive datetime
        被按本地时区解释导致整体偏移。
        """
        now = utcnow()
        since = now - timedelta(minutes=minutes)

        with SessionLocal() as db:
            online_ids = [
                n["node_id"]
                for n in registry.list()
                if n["status"] == NodeStatus.ONLINE.value
            ]
            if not online_ids:
                return {"minutes": minutes, "bucket_seconds": 0, "count": 0, "items": []}

            rows = list(
                db.scalars(
                    select(MetricSample)
                    .where(MetricSample.node_id.in_(online_ids), MetricSample.ts >= since)
                    .order_by(MetricSample.ts.asc())
                )
            )

        if not rows:
            return {"minutes": minutes, "bucket_seconds": 0, "count": 0, "items": []}

        span = max((now - since).total_seconds(), 1.0)
        bucket = max(5, int(span / MAX_SERIES_POINTS))

        base = rows[0].ts
        buckets: dict[int, dict[str, list[float]]] = {}
        for r in rows:
            idx = int((r.ts - base).total_seconds() // bucket)
            b = buckets.setdefault(
                idx,
                {"ts": r.ts, "cpu": [], "gpu": [], "npu": [], "temp": [],
                 "mem_used": [], "mem_total": [], "sto_used": [], "sto_total": [],
                 "nodes": []},
            )
            b["ts"] = r.ts  # 桶内取最后一个时间戳，作为该点的时间
            for key, val in (
                ("cpu", r.cpu_usage), ("gpu", r.gpu_usage), ("npu", r.npu_usage),
                ("temp", r.temperature), ("mem_used", r.mem_used),
                ("mem_total", r.mem_total), ("sto_used", r.storage_used),
                ("sto_total", r.storage_total),
            ):
                if val is not None:
                    b[key].append(float(val))
            b["nodes"].append(r.node_id)

        items = []
        for idx in sorted(buckets):
            b = buckets[idx]
            mu, mt = sum(b["mem_used"]), sum(b["mem_total"])
            su, st = sum(b["sto_used"]), sum(b["sto_total"])
            items.append(
                {
                    "ts": b["ts"].isoformat(),
                    "cpu_avg": _avg(b["cpu"]),
                    "gpu_avg": _avg(b["gpu"]),
                    "npu_avg": _avg(b["npu"]),
                    "temperature_avg": _avg(b["temp"]),
                    "mem_used": mu if mt else None,
                    "mem_total": mt or None,
                    "mem_percent": percent(mu, mt),
                    "storage_used": su if st else None,
                    "storage_total": st or None,
                    "storage_percent": percent(su, st),
                    # 该桶内有多少个节点的采样参与了统计
                    "node_count": len({n for n in b["nodes"]}),
                }
            )

        return {
            "minutes": minutes,
            "bucket_seconds": bucket,
            "online_nodes": len(online_ids),
            "count": len(items),
            "items": items,
        }

    # ------------------------------------------------------------------ 单节点详情辅助

    def node_metric_status(self, node: dict[str, Any]) -> dict[str, Any]:
        """给前端用：该节点当前指标是否可展示。

        离线节点返回 `displayable=False`，前端据此显示"离线"而不是最后一次旧值。
        """
        nid = node["node_id"]
        with SessionLocal() as db:
            latest = latest_samples(db, [nid]).get(nid)

        online = node.get("status") == NodeStatus.ONLINE.value
        fresh = is_fresh(latest)
        return {
            "displayable": bool(online and fresh),
            "online": online,
            "fresh": fresh,
            "age_seconds": sample_age_seconds(latest),
        }


def _avg(vals: list[float]) -> float | None:
    """平均值。没有有效样本返回 None —— 前端显示"—"，绝不用 0 顶替。"""
    if not vals:
        return None
    return round(sum(vals) / len(vals), 2)


def _metric_public(sample: dict[str, Any] | None) -> dict[str, Any] | None:
    """指标 → 可直接 JSON 化的 dict。

    丢弃内部的 `ts`（datetime 对象），只保留 `ts_iso` 字符串，
    避免 pydantic 在宽松 dict 字段里遇到 datetime 时的序列化歧义。
    """
    if not sample:
        return None
    return {k: v for k, v in sample.items() if k != "ts"}


overview = OverviewService()
