"""节点指标采样（MetricSample）—— 对应任务书 §4「节点指标 Metric」。

原则：拿不到的指标一律留 NULL，**不造假**。
"""

from __future__ import annotations

from datetime import datetime

from sqlalchemy import BigInteger, DateTime, Float, ForeignKey, Index, Integer, JSON, String
from sqlalchemy.orm import Mapped, mapped_column

from ..database import Base
from .node import utcnow


class MetricSample(Base):
    __tablename__ = "metric_samples"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    node_id: Mapped[str] = mapped_column(
        String(64), ForeignKey("nodes.node_id", ondelete="CASCADE"), index=True
    )
    ts: Mapped[datetime] = mapped_column(DateTime, default=utcnow, index=True)

    cpu_usage: Mapped[float | None] = mapped_column(Float, nullable=True)     # %
    gpu_usage: Mapped[float | None] = mapped_column(Float, nullable=True)     # %
    npu_usage: Mapped[float | None] = mapped_column(Float, nullable=True)     # %
    mem_used: Mapped[int | None] = mapped_column(BigInteger, nullable=True)   # bytes
    mem_total: Mapped[int | None] = mapped_column(BigInteger, nullable=True)  # bytes
    storage_used: Mapped[int | None] = mapped_column(BigInteger, nullable=True)
    storage_total: Mapped[int | None] = mapped_column(BigInteger, nullable=True)
    temperature: Mapped[float | None] = mapped_column(Float, nullable=True)   # ℃
    power_watts: Mapped[float | None] = mapped_column(Float, nullable=True)   # W（拿不到留空）
    net_rx_bytes: Mapped[int | None] = mapped_column(BigInteger, nullable=True)  # 累计
    net_tx_bytes: Mapped[int | None] = mapped_column(BigInteger, nullable=True)  # 累计
    load_avg: Mapped[float | None] = mapped_column(Float, nullable=True)      # 1 分钟负载
    extra: Mapped[dict | None] = mapped_column(JSON, nullable=True)           # 扩展位

    __table_args__ = (Index("ix_metric_node_ts", "node_id", "ts"),)

    def __repr__(self) -> str:  # pragma: no cover
        return f"<MetricSample {self.node_id} @{self.ts} cpu={self.cpu_usage}>"
