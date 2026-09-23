"""数据库连接与会话管理。

MVP 用 SQLite 单文件；开启 WAL 提升并发读写表现（心跳写入 + Dashboard 读取）。
"""

from __future__ import annotations

from collections.abc import Iterator

from sqlalchemy import create_engine, event
from sqlalchemy.orm import DeclarativeBase, Session, sessionmaker

from .config import settings


class Base(DeclarativeBase):
    """所有 ORM 模型的基类。"""


engine = create_engine(
    f"sqlite:///{settings.db_file.as_posix()}",
    echo=False,
    future=True,
    # SQLite 在多线程（后台任务 + 请求处理）下需要放开线程检查，配合连接池使用
    connect_args={"check_same_thread": False, "timeout": 15},
)


@event.listens_for(engine, "connect")
def _set_sqlite_pragma(dbapi_connection, _connection_record) -> None:
    """每条连接建立时设置 pragma：WAL + 外键 + 忙等待。"""
    cursor = dbapi_connection.cursor()
    try:
        cursor.execute("PRAGMA journal_mode=WAL")
        cursor.execute("PRAGMA synchronous=NORMAL")
        cursor.execute("PRAGMA foreign_keys=ON")
        cursor.execute("PRAGMA busy_timeout=15000")
    except Exception:
        pass
    finally:
        cursor.close()


SessionLocal = sessionmaker(bind=engine, autoflush=False, autocommit=False, future=True)


def init_db() -> None:
    """建表。幂等，可重复调用。"""
    settings.ensure_dirs()
    # 导入以触发模型注册（必须在使用前 import 所有模型模块）
    from .models import deployment, metric, model, node, service  # noqa: F401

    Base.metadata.create_all(bind=engine)


def get_session() -> Iterator[Session]:
    """FastAPI 依赖注入用的会话生成器。"""
    session = SessionLocal()
    try:
        yield session
    finally:
        session.close()
