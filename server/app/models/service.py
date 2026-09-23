"""模型服务（Service）ORM 模型 —— 对应任务书 §4「服务 Service」。

`gateway_meta` 是为 §6「API Gateway 预留」保留的 JSON 字段：
MVP 不实现转发，但注册数据结构上先把调度所需的元信息位置留出来。
"""

from __future__ import annotations

from datetime import datetime

from sqlalchemy import Boolean, DateTime, ForeignKey, Integer, JSON, String, Text
from sqlalchemy.orm import Mapped, mapped_column

from ..database import Base
from .enums import ApiType, ServiceStatus
from .node import utcnow


class Service(Base):
    __tablename__ = "services"

    service_id: Mapped[str] = mapped_column(String(64), primary_key=True)
    node_id: Mapped[str] = mapped_column(
        String(64), ForeignKey("nodes.node_id", ondelete="CASCADE"), index=True
    )
    model_id: Mapped[str | None] = mapped_column(String(64), nullable=True, index=True)

    name: Mapped[str] = mapped_column(String(128), default="")             # 服务显示名
    status: Mapped[str] = mapped_column(
        String(16), default=ServiceStatus.STOPPED.value, index=True
    )
    port: Mapped[int] = mapped_column(Integer, default=0)
    endpoint: Mapped[str] = mapped_column(String(512), default="")
    api_type: Mapped[str] = mapped_column(String(16), default=ApiType.OTHER.value)

    pid: Mapped[int | None] = mapped_column(Integer, nullable=True)
    started_at: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)
    last_error: Mapped[str] = mapped_column(Text, default="")

    # 平台侧探活
    last_probe_at: Mapped[datetime | None] = mapped_column(DateTime, nullable=True)
    probe_ok: Mapped[int] = mapped_column(Integer, default=0)     # 0/1

    # 启动配置（平台可改，Agent 按此拉起进程）
    command: Mapped[str] = mapped_column(String(512), default="")
    args: Mapped[list | None] = mapped_column(JSON, nullable=True)
    health_url: Mapped[str] = mapped_column(String(512), default="")
    log_path: Mapped[str] = mapped_column(String(512), default="")
    auto_restart: Mapped[int] = mapped_column(Integer, default=1)

    # ★ §6 预留：Gateway 调度所需元信息（协议/路径模板/权重/限流位等）
    gateway_meta: Mapped[dict | None] = mapped_column(JSON, nullable=True)

    created_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow, onupdate=utcnow)

    def __repr__(self) -> str:  # pragma: no cover
        return f"<Service {self.service_id} {self.name} {self.status} :{self.port}>"
