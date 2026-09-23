"""进程内事件总线：业务服务 → Dashboard 实时推送。

用 asyncio 队列实现广播。单个订阅者阻塞/报错不影响其他人（长期无人值守要求）。
"""

from __future__ import annotations

import asyncio
from typing import Any

from ..core.logging_conf import get_logger

logger = get_logger("services.event_bus")

# 每个订阅者的队列上限；满了丢最旧的，避免慢客户端把内存吃满
_QUEUE_MAX = 200


class EventBus:
    def __init__(self) -> None:
        self._subscribers: set[asyncio.Queue] = set()
        self._lock = asyncio.Lock()
        self._loop: asyncio.AbstractEventLoop | None = None

    def bind_loop(self, loop: asyncio.AbstractEventLoop) -> None:
        """记录事件循环，便于从非异步线程（如 UDP 线程）安全投递。"""
        self._loop = loop

    async def subscribe(self) -> asyncio.Queue:
        q: asyncio.Queue = asyncio.Queue(maxsize=_QUEUE_MAX)
        async with self._lock:
            self._subscribers.add(q)
        logger.debug("Dashboard 订阅者加入，当前 %d 个", len(self._subscribers))
        return q

    async def unsubscribe(self, q: asyncio.Queue) -> None:
        async with self._lock:
            self._subscribers.discard(q)
        logger.debug("Dashboard 订阅者退出，当前 %d 个", len(self._subscribers))

    async def publish(self, event: str, data: dict[str, Any]) -> None:
        """异步投递事件。"""
        message = {"type": "event", "event": event, "data": data}
        async with self._lock:
            targets = list(self._subscribers)
        for q in targets:
            try:
                q.put_nowait(message)
            except asyncio.QueueFull:
                # 慢客户端：丢掉一条最旧的再放新的，保证不阻塞发布方
                try:
                    q.get_nowait()
                    q.put_nowait(message)
                except Exception:
                    pass
            except Exception as exc:
                logger.debug("事件投递失败（忽略） err=%s", exc)

    def publish_threadsafe(self, event: str, data: dict[str, Any]) -> None:
        """从非事件循环线程（UDP 监听线程）安全投递。"""
        if self._loop is None:
            return
        try:
            asyncio.run_coroutine_threadsafe(self.publish(event, data), self._loop)
        except Exception as exc:
            logger.debug("跨线程事件投递失败（忽略） err=%s", exc)


bus = EventBus()
