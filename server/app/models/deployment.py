"""部署任务（DeployTask / DeployTaskNode）ORM 模型 —— 对应任务书 §4「部署任务 DeployTask」。

设计要点：任务与「逐节点状态」拆两张表，保证「允许部分成功部分失败」，
且单节点重试不影响同一任务里其他已成功的节点。
"""

from __future__ import annotations

from datetime import datetime

from sqlalchemy import (
    BigInteger,
    DateTime,
    ForeignKey,
    Index,
    Integer,
    String,
    Text,
    UniqueConstraint,
)
from sqlalchemy.orm import Mapped, mapped_column, relationship

from ..database import Base
from .enums import DeployNodeStatus, DeployTaskStatus
from .node import utcnow


class DeployTask(Base):
    __tablename__ = "deploy_tasks"

    task_id: Mapped[str] = mapped_column(String(64), primary_key=True)
    model_id: Mapped[str] = mapped_column(String(64), index=True)
    model_name: Mapped[str] = mapped_column(String(200), default="")   # 冗余快照，便于列表展示
    target_count: Mapped[int] = mapped_column(Integer, default=0)
    status: Mapped[str] = mapped_column(
        String(16), default=DeployTaskStatus.PENDING.value, index=True
    )
    note: Mapped[str] = mapped_column(Text, default="")
    created_by: Mapped[str] = mapped_column(String(64), default="platform")
    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, index=True)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)

    nodes: Mapped[list["DeployTaskNode"]] = relationship(
        back_populates="task", cascade="all, delete-orphan", lazy="selectin"
    )

    def __repr__(self) -> str:  # pragma: no cover
        return f"<DeployTask {self.task_id} {self.model_name} {self.status}>"


class DeployTaskNode(Base):
    __tablename__ = "deploy_task_nodes"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    task_id: Mapped[str] = mapped_column(
        String(64), ForeignKey("deploy_tasks.task_id", ondelete="CASCADE"), index=True
    )
    node_id: Mapped[str] = mapped_column(String(64), index=True)
    node_name: Mapped[str] = mapped_column(String(128), default="")     # 冗余快照

    status: Mapped[str] = mapped_column(
        String(16), default=DeployNodeStatus.PENDING.value, index=True
    )
    progress: Mapped[int] = mapped_column(Integer, default=0)           # 0-100，真实进度
    error: Mapped[str] = mapped_column(Text, default="")
    retry_count: Mapped[int] = mapped_column(Integer, default=0)
    bytes_downloaded: Mapped[int] = mapped_column(BigInteger, default=0)
    bytes_total: Mapped[int] = mapped_column(BigInteger, default=0)

    started_at: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    task: Mapped[DeployTask] = relationship(back_populates="nodes")

    __table_args__ = (
        UniqueConstraint("task_id", "node_id", name="uq_task_node"),
        Index("ix_dtn_status", "task_id", "status"),
    )

    def __repr__(self) -> str:  # pragma: no cover
        return f"<DeployTaskNode {self.task_id}/{self.node_id} {self.status} {self.progress}%>"
