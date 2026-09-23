"""节点（Node）ORM 模型 —— 对应任务书 §4「节点 Node」字段表。"""

from __future__ import annotations

from datetime import datetime, timezone

from sqlalchemy import DateTime, Index, Integer, String, Text
from sqlalchemy.orm import Mapped, mapped_column

from ..database import Base
from .enums import NodeStatus


def utcnow() -> datetime:
    """统一使用带时区的 UTC 时间；DB 里存 naive UTC 以便 SQLite 比较。"""
    return datetime.now(timezone.utc).replace(tzinfo=None)


class Node(Base):
    __tablename__ = "nodes"

    node_id: Mapped[str] = mapped_column(String(64), primary_key=True)
    name: Mapped[str] = mapped_column(String(128), default="")
    model: Mapped[str] = mapped_column(String(128), default="")            # 手机型号
    soc: Mapped[str] = mapped_column(String(128), default="")              # SoC
    android_version: Mapped[str] = mapped_column(String(64), default="")
    agent_version: Mapped[str] = mapped_column(String(32), default="")
    ip: Mapped[str] = mapped_column(String(64), default="")

    # 三态：never_seen / online / offline
    status: Mapped[str] = mapped_column(
        String(16), default=NodeStatus.NEVER_SEEN.value, index=True
    )

    last_heartbeat: Mapped[datetime | None] = mapped_column(DateTime, nullable=True, index=True)
    online_since: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)
    uptime_seconds: Mapped[int] = mapped_column(Integer, default=0)

    # 防抖用：连续超时次数（心跳巡检维护，不对外暴露语义）
    missed_heartbeats: Mapped[int] = mapped_column(Integer, default=0)

    # 模拟器标记，便于 Dashboard 区分真机与模拟节点
    is_simulated: Mapped[int] = mapped_column(Integer, default=0)

    note: Mapped[str] = mapped_column(Text, default="")

    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    __table_args__ = (Index("ix_nodes_status_hb", "status", "last_heartbeat"),)

    def refresh_uptime(self) -> None:
        """根据 online_since 重算在线时长（秒）。"""
        if self.status == NodeStatus.ONLINE.value and self.online_since:
            self.uptime_seconds = max(0, int((utcnow() - self.online_since).total_seconds()))

    def __repr__(self) -> str:  # pragma: no cover
        return f"<Node {self.node_id} {self.name} {self.status}>"
