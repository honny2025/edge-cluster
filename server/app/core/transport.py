"""连接层抽象（对应任务书 §6「连接层抽象」）。

设计目标：把「发现 / 心跳 / 指令通道」做成可替换模块。
MVP 只有一种实现 —— Agent 主动拨出的 WebSocket（LocalWsTransport）。
后续若要换 P2P 或中继服务器，只需新增一个 Transport 实现并注册到
ConnectionManager，业务层代码完全不用动。

契约：
- `send(payload)`：发送一条 JSON 消息；成功返回 True，失败返回 False（不抛异常）。
- `is_alive()`：连接是否仍然可用。
- `close()`：主动关闭。
- `kind`：传输类型标识，便于诊断。
"""

from __future__ import annotations

import abc
import asyncio
from typing import Any

from .logging_conf import get_logger


logger = get_logger("core.transport")


class Transport(abc.ABC):
    """节点通信通道抽象基类。"""

    #: 传输类型标识（local_ws / p2p / relay ...）
    kind: str = "abstract"

    @property
    @abc.abstractmethod
    def node_id(self) -> str:
        """这条通道属于哪个节点。"""

    @abc.abstractmethod
    async def send(self, payload: dict[str, Any]) -> bool:
        """发送消息。失败返回 False，绝不抛异常打断业务。"""

    @abc.abstractmethod
    async def close(self) -> None:
        """关闭通道。"""

    @abc.abstractmethod
    def is_alive(self) -> bool:
        """通道是否可用。"""

    @property
    def peer(self) -> str:
        """对端标识，用于日志。"""
        return self.node_id


class LocalWsTransport(Transport):
    """MVP 实现：基于 Agent 主动拨出的 WebSocket 连接。

    平台永远不主动连手机 —— 手机可能在 NAT / 省电策略后，
    只有 Agent 拨出方向可靠。平台→Agent 的指令全部复用这条连接。
    """

    kind = "local_ws"

    def __init__(self, node_id: str, ws: Any, ip: str = "") -> None:
        self._node_id = node_id
        self._ws = ws
        self.ip = ip
        self._alive = True
        # 发送串行化，避免并发写同一个 websocket
        self._send_lock = asyncio.Lock()

    @property
    def node_id(self) -> str:
        return self._node_id

    @property
    def peer(self) -> str:
        return f"{self._node_id}@{self.ip}" if self.ip else self._node_id

    def is_alive(self) -> bool:
        return self._alive

    async def send(self, payload: dict[str, Any]) -> bool:
        if not self._alive:
            return False
        try:
            async with self._send_lock:
                await self._ws.send_json(payload)
            return True
        except Exception as exc:  # 单点发送失败不能拖垮进程
            self._alive = False
            logger.warning("指令下发失败 node=%s err=%s", self._node_id, exc)
            return False

    async def close(self) -> None:
        self._alive = False
        try:
            await self._ws.close()
        except Exception:
            pass

    def mark_dead(self) -> None:
        self._alive = False
