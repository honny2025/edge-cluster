"""结构化日志配置。

- 控制台 + 文件双输出，文件按大小轮转。
- 格式统一：时间 | 级别 | 模块 | 消息，便于 grep 和 Dashboard 展示。
- 任何模块用 get_logger("xxx") 拿 logger。
"""

from __future__ import annotations

import logging
import sys
from logging.handlers import RotatingFileHandler

from ..config import settings

_LOG_FORMAT = "%(asctime)s | %(levelname)-7s | %(name)-22s | %(message)s"
_DATE_FORMAT = "%Y-%m-%d %H:%M:%S"
_configured = False


class _ColorFormatter(logging.Formatter):
    """控制台彩色输出，文件里不带着色码。"""

    COLORS = {
        "DEBUG": "\033[36m",
        "INFO": "\033[32m",
        "WARNING": "\033[33m",
        "ERROR": "\033[31m",
        "CRITICAL": "\033[35m",
    }
    RESET = "\033[0m"

    def format(self, record: logging.LogRecord) -> str:
        text = super().format(record)
        color = self.COLORS.get(record.levelname)
        return f"{color}{text}{self.RESET}" if color else text


def setup_logging() -> None:
    global _configured
    if _configured:
        return

    settings.ensure_dirs()
    level = getattr(logging, str(settings.server.log_level).upper(), logging.INFO)

    root = logging.getLogger()
    root.setLevel(level)
    root.handlers.clear()

    fmt = logging.Formatter(_LOG_FORMAT, datefmt=_DATE_FORMAT)

    console = logging.StreamHandler(sys.stdout)
    console.setFormatter(_ColorFormatter(_LOG_FORMAT, datefmt=_DATE_FORMAT))
    root.addHandler(console)

    log_file = settings.logs_path / "server.log"
    try:
        file_handler = RotatingFileHandler(
            log_file, maxBytes=5 * 1024 * 1024, backupCount=3, encoding="utf-8"
        )
        file_handler.setFormatter(fmt)
        root.addHandler(file_handler)
    except Exception as exc:  # 文件日志失败不能影响主流程
        root.warning("文件日志初始化失败，仅输出到控制台: %s", exc)

    # 降噪：uvicorn access 日志交给自己的 logger 管
    logging.getLogger("uvicorn.access").setLevel(logging.WARNING)
    logging.getLogger("watchfiles").setLevel(logging.WARNING)

    _configured = True


def get_logger(name: str) -> logging.Logger:
    setup_logging()
    return logging.getLogger(name)
