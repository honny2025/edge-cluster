"""指标聚合查询（里程碑 2）。

职责很窄，只做两件事：

1. 取每个节点的**最新一条**指标采样（一次 SQL 拿全，不做 N+1 查询）；
2. 用「新鲜度」过滤陈旧采样，避免把几分钟前的旧值当作当前值展示。

设计纪律（对应任务书 §4 最核心的那条要求）：

- 集群资源汇总**只统计 online 节点**。离线节点即使库里还留着最后一条采样，
  也绝不参与汇总 —— 否则就是"用旧数据冒充在线"。
- 采样时间若比心跳周期落后太多（判定为 stale），同样排除在外。
  场景：Agent 主机 CPU 打满，心跳还在但采集卡住，此时指标是断层的，
  不能拿最后一条旧值顶上去。
"""

from __future__ import annotations

from datetime import datetime
from typing import Any, Iterable

from sqlalchemy import func, select
from sqlalchemy.orm import Session, aliased

from ..config import settings
from ..models import MetricSample

# 采样新鲜度容忍倍数：采样时间落后超过 heartbeat_interval * 该倍数即视为陈旧。
# 取 3 是为了容忍一次丢包 + 一次调度延迟。
STALE_FACTOR = 3


def _row_to_dict(r: MetricSample) -> dict[str, Any]:
    """ORM 行 → 纯 dict（避免换 session 后对象失效）。"""
    return {
        "ts": r.ts,
        "ts_iso": r.ts.isoformat() if r.ts else None,
        "cpu_usage": r.cpu_usage,
        "gpu_usage": r.gpu_usage,
        "npu_usage": r.npu_usage,
        "mem_used": r.mem_used,
        "mem_total": r.mem_total,
        "storage_used": r.storage_used,
        "storage_total": r.storage_total,
        "temperature": r.temperature,
        "power_watts": r.power_watts,
        "net_rx_bytes": r.net_rx_bytes,
        "net_tx_bytes": r.net_tx_bytes,
        "load_avg": r.load_avg,
        "extra": r.extra,
    }


def latest_samples(db: Session, node_ids: Iterable[str] | None = None) -> dict[str, dict[str, Any]]:
    """取每个节点的最新一条采样。

    用窗口函数 `ROW_NUMBER() PARTITION BY node_id ORDER BY ts DESC`，
    一次查询解决；避免按节点循环查库（节点多了就是 N+1）。
    """
    ids = list(node_ids) if node_ids is not None else None
    if ids is not None and not ids:
        return {}

    inner = select(
        MetricSample,
        func.row_number()
        .over(partition_by=MetricSample.node_id, order_by=MetricSample.ts.desc())
        .label("rn"),
    )
    if ids is not None:
        inner = inner.where(MetricSample.node_id.in_(ids))
    sub = inner.subquery()

    # 把子查询当作 MetricSample 的替身，只取每个节点 rn = 1 的那一行
    sample_alias = aliased(MetricSample, sub)
    rows = list(db.scalars(select(sample_alias).where(sub.c.rn == 1)))
    return {r.node_id: _row_to_dict(r) for r in rows}


def is_fresh(sample: dict[str, Any] | None, now: datetime | None = None) -> bool:
    """采样是否足够新鲜。拿不到时间戳一律视为不新鲜。"""
    if not sample or not sample.get("ts"):
        return False
    now = now or datetime.utcnow()
    age = (now - sample["ts"]).total_seconds()
    return age <= float(settings.server.heartbeat_interval) * STALE_FACTOR


def percent(used: int | float | None, total: int | float | None) -> float | None:
    """计算使用率百分比。任一为空或分母为 0 时返回 None（不编造 0%）。"""
    if used is None or total in (None, 0):
        return None
    try:
        return round(100.0 * float(used) / float(total), 2)
    except (TypeError, ValueError, ZeroDivisionError):
        return None


def sample_age_seconds(sample: dict[str, Any] | None, now: datetime | None = None) -> float | None:
    """采样距今多少秒。用于前端显示"数据已陈旧 42s"这类提示。"""
    if not sample or not sample.get("ts"):
        return None
    now = now or datetime.utcnow()
    return round((now - sample["ts"]).total_seconds(), 1)
