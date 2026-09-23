"""模型库（Model）ORM 模型 —— 对应任务书 §4「模型 Model」。"""

from __future__ import annotations

from datetime import datetime

from sqlalchemy import BigInteger, DateTime, String, Text, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from ..database import Base
from .enums import Framework
from .node import utcnow


class Model(Base):
    __tablename__ = "models"

    model_id: Mapped[str] = mapped_column(String(64), primary_key=True)
    name: Mapped[str] = mapped_column(String(200), index=True)
    version: Mapped[str] = mapped_column(String(64), default="v1")
    file_name: Mapped[str] = mapped_column(String(255), default="")
    file_size: Mapped[int] = mapped_column(BigInteger, default=0)          # bytes
    framework: Mapped[str] = mapped_column(String(32), default=Framework.OTHER.value)
    format: Mapped[str] = mapped_column(String(32), default="")            # gguf / onnx / ...
    sha256: Mapped[str] = mapped_column(String(64), default="", index=True)
    description: Mapped[str] = mapped_column(Text, default="")
    uploaded_at: Mapped[datetime] = mapped_column(DateTime, default=utcnow)
    storage_path: Mapped[str] = mapped_column(String(512), default="")     # 相对 server/data
    download_count: Mapped[int] = mapped_column(default=0)

    __table_args__ = (UniqueConstraint("name", "version", name="uq_model_name_version"),)

    def __repr__(self) -> str:  # pragma: no cover
        return f"<Model {self.name}:{self.version} {self.file_size}B>"
