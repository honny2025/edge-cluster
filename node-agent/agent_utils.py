"""工具函数：结构化日志（带大小滚动）+ 通用小工具。

日志同时输出到控制台与文件，文件按 log_max_bytes 滚动，保留 log_backup_count 份。
Termux 下长时间无人值守运行也只会占用有限磁盘。
"""

from __future__ import annotations

import logging
import os
import sys
from pathlib import Path

_FMT = "%(asctime)s | %(levelname)-7s | %(name)-16s | %(message)s"
_DATE = "%Y-%m-%d %H:%M:%S"


class _SizedRotatingHandler(logging.Handler):
    """按大小滚动的简化实现（不依赖 logging.handlers 在 Termux 的行为差异）。"""

    def __init__(self, path: Path, max_bytes: int, backup_count: int) -> None:
        super().__init__()
        self.path = path
        self.max_bytes = max(1024, max_bytes)
        self.backup_count = max(0, backup_count)
        self._stream = None
        self._open()

    def _open(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._stream = open(self.path, "a", encoding="utf-8")

    def emit(self, record: logging.LogRecord) -> None:
        try:
            msg = self.format(record) + "\n"
            self._stream.write(msg)
            self._stream.flush()
            if self._stream.tell() >= self.max_bytes:
                self._rotate()
        except Exception:
            self.handleError(record)

    def _rotate(self) -> None:
        try:
            self._stream.close()
        except Exception:
            pass
        # agent.log.2 <- agent.log.1 <- agent.log
        for i in range(self.backup_count, 0, -1):
            src = self.path if i == 1 else self.path.with_suffix(self.path.suffix + f".{i - 1}")
            dst = self.path.with_suffix(self.path.suffix + f".{i}")
            try:
                if src.exists():
                    if dst.exists():
                        dst.unlink()
                    os.replace(src, dst)
            except Exception:
                continue
        self._open()

    def close(self) -> None:
        try:
            self._stream.close()
        except Exception:
            pass
        super().close()


_configured = False


def setup_logging(log_dir: Path, level: str = "INFO", max_bytes: int = 5 * 1024 * 1024,
                  backup_count: int = 3, tag: str = "agent") -> Path:
    global _configured
    log_file = log_dir / f"{tag}.log"
    if _configured:
        return log_file

    root = logging.getLogger()
    root.setLevel(getattr(logging, level.upper(), logging.INFO))
    root.handlers.clear()

    console = logging.StreamHandler(sys.stdout)
    console.setFormatter(logging.Formatter(_FMT, datefmt=_DATE))
    root.addHandler(console)

    try:
        fh = _SizedRotatingHandler(log_file, max_bytes, backup_count)
        fh.setFormatter(logging.Formatter(_FMT, datefmt=_DATE))
        root.addHandler(fh)
    except Exception as exc:
        root.warning("文件日志初始化失败，仅控制台输出: %s", exc)

    _configured = True
    return log_file


def get_logger(name: str) -> logging.Logger:
    return logging.getLogger(name)


def human_bytes(n: int | float | None) -> str:
    """字节转可读字符串，用于日志。"""
    if n is None:
        return "N/A"
    v = float(n)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if abs(v) < 1024:
            return f"{v:.1f}{unit}"
        v /= 1024
    return f"{v:.1f}PB"
