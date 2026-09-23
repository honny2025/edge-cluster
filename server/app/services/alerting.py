"""告警判定（里程碑 2）。

规则全部来自配置 `[alerts]`，代码里不含任何硬编码阈值。

三条硬纪律：

1. **没有数据 ≠ 异常**。指标为 `None` 时一律不产生告警。手机拿不到 NPU 使用率是
   常态，不能因此刷屏告警。
2. **`never_seen` 不报离线告警**。任务书 §4 明确要求区分"没有上报"和"节点离线"：
   从未上报过的节点只是还没接入，不等于掉线。只有曾经在线过、现在心跳超时的才告警。
3. **离线节点的旧指标不参与指标类告警**。否则一台断了 3 小时的机器会因为
   离线前温度偏高而持续报警，属噪声。
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from datetime import datetime
from typing import Any

from ..config import settings
from ..core.logging_conf import get_logger
from ..models import NodeStatus
from .metrics_query import percent

logger = get_logger("services.alerting")

# 告警级别（前端按此着色）
LEVEL_WARNING = "warning"
LEVEL_CRITICAL = "critical"

# 告警类型
KIND_CPU = "cpu"
KIND_MEMORY = "memory"
KIND_STORAGE = "storage"
KIND_TEMPERATURE = "temperature"
KIND_NODE_OFFLINE = "node_offline"


@dataclass
class Alert:
    """一条告警。字段故意保持扁平，方便直接塞进 JSON 给前端。"""

    level: str
    kind: str
    node_id: str
    node_name: str
    message: str
    value: float | None = None
    threshold: float | None = None
    unit: str = ""

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class AlertService:
    """根据节点列表 + 最新指标算出当前活跃告警。无状态，可随时调用。"""

    def evaluate(
        self,
        nodes: list[dict[str, Any]],
        latest: dict[str, dict[str, Any]],
        now: datetime | None = None,
    ) -> list[dict[str, Any]]:
        cfg = settings.alerts
        if not cfg.enabled:
            return []

        alerts: list[Alert] = []
        for node in nodes:
            node_id = node["node_id"]
            name = node.get("name") or node_id
            status = node.get("status")

            # ---- 离线告警：只针对"曾经在线过"的节点 ----
            if status == NodeStatus.OFFLINE.value and cfg.alarm_on_offline:
                hb = node.get("last_heartbeat")
                alerts.append(
                    Alert(
                        level=LEVEL_CRITICAL,
                        kind=KIND_NODE_OFFLINE,
                        node_id=node_id,
                        node_name=name,
                        message=f"节点离线（最后心跳 {hb or '未知'}）",
                        value=None,
                        threshold=None,
                    )
                )
                # 离线节点不再评估指标类告警（旧数据不参与，避免噪声）
                continue

            # never_seen / online 之外的异常状态不在这里管
            if status != NodeStatus.ONLINE.value:
                continue

            sample = latest.get(node_id)
            if not sample:
                # 在线但还没收到指标：只说明数据未到，不是异常
                continue

            self._check_cpu(alerts, node_id, name, sample, cfg)
            self._check_memory(alerts, node_id, name, sample, cfg)
            self._check_storage(alerts, node_id, name, sample, cfg)
            self._check_temperature(alerts, node_id, name, sample, cfg)

        # critical 在前，同级按节点名排序，保证前端顺序稳定不跳动
        alerts.sort(key=lambda a: (a.level != LEVEL_CRITICAL, a.node_name, a.kind))
        if alerts:
            logger.debug("当前活跃告警 %d 条", len(alerts))
        return [a.to_dict() for a in alerts]

    # ------------------------------------------------------------------ 各条规则

    @staticmethod
    def _check_cpu(alerts: list[Alert], node_id: str, name: str, sample: dict, cfg: Any) -> None:
        v = sample.get("cpu_usage")
        if v is None or v < cfg.cpu_usage_warn:
            return
        alerts.append(
            Alert(
                level=LEVEL_WARNING,
                kind=KIND_CPU,
                node_id=node_id,
                node_name=name,
                message=f"CPU 使用率偏高 {v:.1f}%（阈值 {cfg.cpu_usage_warn}%）",
                value=round(float(v), 2),
                threshold=float(cfg.cpu_usage_warn),
                unit="%",
            )
        )

    @staticmethod
    def _check_memory(alerts: list[Alert], node_id: str, name: str, sample: dict, cfg: Any) -> None:
        p = percent(sample.get("mem_used"), sample.get("mem_total"))
        if p is None or p < cfg.mem_usage_warn:
            return
        alerts.append(
            Alert(
                level=LEVEL_WARNING,
                kind=KIND_MEMORY,
                node_id=node_id,
                node_name=name,
                message=f"内存使用率 {p:.1f}%（阈值 {cfg.mem_usage_warn}%）",
                value=p,
                threshold=float(cfg.mem_usage_warn),
                unit="%",
            )
        )

    @staticmethod
    def _check_storage(alerts: list[Alert], node_id: str, name: str, sample: dict, cfg: Any) -> None:
        p = percent(sample.get("storage_used"), sample.get("storage_total"))
        if p is None or p < cfg.storage_usage_warn:
            return
        alerts.append(
            Alert(
                level=LEVEL_WARNING,
                kind=KIND_STORAGE,
                node_id=node_id,
                node_name=name,
                message=f"存储使用率 {p:.1f}%（阈值 {cfg.storage_usage_warn}%）",
                value=p,
                threshold=float(cfg.storage_usage_warn),
                unit="%",
            )
        )

    @staticmethod
    def _check_temperature(
        alerts: list[Alert], node_id: str, name: str, sample: dict, cfg: Any
    ) -> None:
        v = sample.get("temperature")
        if v is None:
            return
        if v >= cfg.temperature_critical:
            alerts.append(
                Alert(
                    level=LEVEL_CRITICAL,
                    kind=KIND_TEMPERATURE,
                    node_id=node_id,
                    node_name=name,
                    message=f"温度过高 {v:.1f}℃（严重阈值 {cfg.temperature_critical}℃）",
                    value=round(float(v), 1),
                    threshold=float(cfg.temperature_critical),
                    unit="℃",
                )
            )
        elif v >= cfg.temperature_warn:
            alerts.append(
                Alert(
                    level=LEVEL_WARNING,
                    kind=KIND_TEMPERATURE,
                    node_id=node_id,
                    node_name=name,
                    message=f"温度偏高 {v:.1f}℃（阈值 {cfg.temperature_warn}℃）",
                    value=round(float(v), 1),
                    threshold=float(cfg.temperature_warn),
                    unit="℃",
                )
            )


alerting = AlertService()
