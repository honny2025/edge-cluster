"""Pydantic 出入参模型。

字段命名与 docs/design/tech-decisions.md §2 字段表严格一致，前端 types.ts 与之对应。
"""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field


# --------------------------------------------------------------------- 节点


class NodeOut(BaseModel):
    node_id: str
    name: str = ""
    model: str = ""
    soc: str = ""
    android_version: str = ""
    agent_version: str = ""
    ip: str = ""
    status: str
    last_heartbeat: str | None = None
    last_heartbeat_ts: float | None = None
    online_since: str | None = None
    uptime_seconds: int = 0
    is_simulated: bool = False
    note: str = ""
    created_at: str | None = None
    updated_at: str | None = None


class NodeUpdateIn(BaseModel):
    name: str = Field(min_length=1, max_length=128)
    note: str | None = None


class NodeListOut(BaseModel):
    items: list[NodeOut]
    total: int


# --------------------------------------------------------------------- 指标


class MetricOut(BaseModel):
    ts: str
    cpu_usage: float | None = None
    gpu_usage: float | None = None
    npu_usage: float | None = None
    mem_used: int | None = None
    mem_total: int | None = None
    storage_used: int | None = None
    storage_total: int | None = None
    temperature: float | None = None
    power_watts: float | None = None
    net_rx_bytes: int | None = None
    net_tx_bytes: int | None = None
    load_avg: float | None = None
    extra: dict[str, Any] | None = None


class MetricSeriesOut(BaseModel):
    node_id: str
    count: int
    items: list[MetricOut] = []


# --------------------------------------------------------------------- 系统


class HealthOut(BaseModel):
    status: str = "ok"
    version: str
    server_time: str


class NodeStats(BaseModel):
    total: int = 0
    online: int = 0
    offline: int = 0
    never_seen: int = 0


class SystemInfoOut(BaseModel):
    version: str
    server_time: str
    started_at: str
    uptime_seconds: int
    connected_nodes: int
    node_stats: NodeStats
    heartbeat_interval: int
    offline_threshold: int
    discovery_port: int


class ServerLogOut(BaseModel):
    lines: list[str]
    total_lines: int
    log_file: str


# ------------------------------------------------------- 里程碑 2：大屏聚合


class AlertOut(BaseModel):
    """一条告警。`level` 为 warning / critical。"""

    level: str
    kind: str            # cpu / memory / storage / temperature / node_offline
    node_id: str
    node_name: str = ""
    message: str
    value: float | None = None
    threshold: float | None = None
    unit: str = ""


class AlertSummary(BaseModel):
    critical: int = 0
    warning: int = 0
    total: int = 0


class ResourceSummary(BaseModel):
    """集群资源汇总。平均值类无样本时为 null，前端显示"—"。

    `counted_nodes` 是**真正参与统计**的节点数（在线且采样新鲜），
    `online_nodes` 是在线节点数。两者不等说明有节点在线但指标采不到。
    """

    cpu_avg: float | None = None
    gpu_avg: float | None = None
    npu_avg: float | None = None
    temperature_avg: float | None = None
    temperature_max: float | None = None
    mem_used: int | None = None
    mem_total: int | None = None
    mem_percent: float | None = None
    storage_used: int | None = None
    storage_total: int | None = None
    storage_percent: float | None = None
    counted_nodes: int = 0
    online_nodes: int = 0
    stale_nodes: list[str] = []


class ServicesSummary(BaseModel):
    running: int = 0
    total: int = 0


class AbnormalNodeOut(BaseModel):
    node_id: str
    name: str = ""
    status: str
    last_heartbeat: str | None = None
    ip: str = ""
    is_simulated: bool = False


class OverviewOut(BaseModel):
    """首页大屏一次性数据源，避免前端拼多个接口。"""

    generated_at: str
    node_stats: NodeStats
    resources: ResourceSummary
    services: ServicesSummary
    alerts: list[AlertOut] = []
    alert_summary: AlertSummary = AlertSummary()
    abnormal_nodes: list[AbnormalNodeOut] = []
    # 各节点最新指标；**离线或采样陈旧时为 null**，前端据此显示"离线"而非旧值
    nodes_metrics: dict[str, dict[str, Any] | None] = {}
    recent_deployments: list[dict[str, Any]] = []
    stale_nodes: list[str] = []


class ClusterSeriesPoint(BaseModel):
    ts: str
    cpu_avg: float | None = None
    gpu_avg: float | None = None
    npu_avg: float | None = None
    temperature_avg: float | None = None
    mem_used: int | None = None
    mem_total: int | None = None
    mem_percent: float | None = None
    storage_used: int | None = None
    storage_total: int | None = None
    storage_percent: float | None = None
    node_count: int = 0


class ClusterSeriesOut(BaseModel):
    minutes: int
    bucket_seconds: int = 0
    online_nodes: int = 0
    count: int = 0
    items: list[ClusterSeriesPoint] = []
