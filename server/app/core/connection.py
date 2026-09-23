"""连接管理器：维护 node_id → Transport 的映射，并向 Agent 下发指令。

业务层只依赖 Transport 抽象，所以换 P2P / 中继时这里不用改。

指令幂等（任务书 M5 要求）：平台→Agent 的每条指令都带全局唯一 `cmd_id`，
Agent 侧对已处理过的 cmd_id 做去重，重复下发不会产生副作用。
"""

from __future__ import annotations

import asyncio
import time
import uuid
from typing import Any

from .logging_conf import get_logger
from .transport import LocalWsTransport, Transport


logger = get_logger("core.connection")


class ConnectionManager:
    """进程内连接注册表。"""

    def __init__(self) -> None:
        self._transports: dict[str, Transport] = {}
        self._lock = asyncio.Lock()
        # cmd_id → 未来结果（MVP 用于等待 Agent 回执，超时丢弃）
        self._pending: dict[str, asyncio.Future] = {}

    # ------------------------------------------------------------------ 注册

    async def register(self, node_id: str, ws: Any, ip: str = "") -> LocalWsTransport:
        """注册（或替换）一条节点连接。旧连接会被主动关闭，避免双连接互相干扰。"""
        async with self._lock:
            old = self._transports.get(node_id)
            if old is not None:
                logger.info("节点重连，替换旧连接 node=%s kind=%s", node_id, old.kind)
                await old.close()
            transport = LocalWsTransport(node_id, ws, ip=ip)
            self._transports[node_id] = transport
        logger.info(
            "连接已建立 node=%s ip=%s kind=%s 当前连接数=%d",
            node_id, ip, transport.kind, len(self._transports),
        )
        return transport

    async def unregister(self, node_id: str, transport: Transport | None = None) -> None:
        """注销连接。只在当前登记的正是这条 transport 时才移除，防止误删新连接。"""
        async with self._lock:
            current = self._transports.get(node_id)
            if current is None:
                return
            if transport is not None and current is not transport:
                return
            self._transports.pop(node_id, None)
            await current.close()
        logger.info("连接已断开 node=%s 剩余连接数=%d", node_id, len(self._transports))

    # ------------------------------------------------------------------ 查询

    def get(self, node_id: str) -> Transport | None:
        t = self._transports.get(node_id)
        if t is not None and not t.is_alive():
            return None
        return t

    def is_connected(self, node_id: str) -> bool:
        return self.get(node_id) is not None

    def connected_nodes(self) -> list[str]:
        return [nid for nid in list(self._transports) if self.is_connected(nid)]

    def count(self) -> int:
        return len(self.connected_nodes())

    # ------------------------------------------------------------------ 下发

    async def send(self, node_id: str, payload: dict[str, Any]) -> bool:
        """向单个节点发消息。节点不在线返回 False（由调用方决定如何处理）。"""
        transport = self.get(node_id)
        if transport is None:
            logger.warning("指令下发失败：节点未连接 node=%s type=%s",
                           node_id, payload.get("type"))
            return False
        return await transport.send(payload)

    async def send_command(
        self,
        node_id: str,
        cmd_type: str,
        payload: dict[str, Any] | None = None,
        wait: float = 0.0,
    ) -> tuple[bool, str]:
        """下发一条带 cmd_id 的指令。

        返回 (是否送达, cmd_id)。`wait > 0` 时等待 Agent 的 ack/结果，超时不算失败
        （Agent 可能仍在处理，结果后续通过 state 上报回来）。
        """
        cmd_id = uuid.uuid4().hex[:12]
        message = {
            "type": cmd_type,
            "cmd_id": cmd_id,
            "ts": time.time(),
            "payload": payload or {},
        }
        ok = await self.send(node_id, message)
        if not ok:
            return False, cmd_id

        logger.info("指令已下发 node=%s type=%s cmd_id=%s", node_id, cmd_type, cmd_id)
        if wait <= 0:
            return True, cmd_id

        future: asyncio.Future = asyncio.get_running_loop().create_future()
        self._pending[cmd_id] = future
        try:
            await asyncio.wait_for(future, timeout=wait)
        except asyncio.TimeoutError:
            logger.debug("等待指令回执超时 cmd_id=%s（不影响后续状态上报）", cmd_id)
        except Exception as exc:
            logger.debug("等待指令回执异常 cmd_id=%s err=%s", cmd_id, exc)
        finally:
            self._pending.pop(cmd_id, None)
        return True, cmd_id

    def resolve_pending(self, cmd_id: str, result: Any = None) -> None:
        """Agent 回执到达时兑现对应的等待。"""
        future = self._pending.get(cmd_id)
        if future is not None and not future.done():
            future.set_result(result)

    async def close_all(self) -> None:
        """关闭全部连接（进程退出时调用）。"""
        for node_id in list(self._transports):
            await self.unregister(node_id)


# 全局单例
manager = ConnectionManager()
