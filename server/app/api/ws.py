"""WebSocket 端点。

两个通道：
- `/ws/agent?token=xxx`    Agent ↔ 平台（注册、心跳、指令下发、结果上报）
- `/ws/dashboard?token=xxx` 浏览器 ↔ 平台（实时事件推送）

设计纪律：
- 单节点消息解析失败只记日志，不影响其他连接（长跑要求）；
- Agent 侧消息一律走 `_dispatch`，未知类型回 error 而不是断开；
- 浏览器连接只读，不接受写操作。
"""

from __future__ import annotations

import asyncio
import json
import time
from typing import Any

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from ..core.connection import manager
from ..core.logging_conf import get_logger
from ..core.security import require_token_ws
from ..services.event_bus import bus
from ..services.node_registry import registry

router = APIRouter(tags=["websocket"])
logger = get_logger("api.ws")

# Dashboard 心跳保活间隔（秒）
_DASHBOARD_PING_INTERVAL = 25


# ====================================================================== Agent


@router.websocket("/ws/agent")
async def ws_agent(websocket: WebSocket) -> None:
    if not await require_token_ws(websocket):
        # 1008 = policy violation
        await websocket.close(code=1008, reason="invalid token")
        return

    await websocket.accept()
    peer_ip = websocket.client.host if websocket.client else ""
    node_id: str | None = None
    logger.info("Agent 连接已接受 ip=%s", peer_ip)

    try:
        while True:
            raw = await websocket.receive_text()
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                logger.warning("收到非法 JSON，已忽略 ip=%s", peer_ip)
                continue
            if not isinstance(msg, dict):
                continue

            msg_type = str(msg.get("type") or "")
            # 首帧必须是 register，否则无法把连接绑定到节点
            if node_id is None and msg_type != "register":
                logger.warning("首帧不是 register（type=%s），忽略并等待注册 ip=%s",
                               msg_type, peer_ip)
                continue

            try:
                node_id = await _dispatch(websocket, node_id, msg, peer_ip)
            except Exception as exc:
                # 单条消息处理失败不能断连
                logger.error("Agent 消息处理异常 type=%s node=%s err=%s",
                             msg_type, node_id, exc, exc_info=True)
                await _safe_send(websocket, {
                    "type": "error",
                    "cmd_id": msg.get("cmd_id"),
                    "message": f"平台处理消息失败: {exc}",
                })

    except WebSocketDisconnect:
        logger.info("Agent 连接断开（客户端主动）node=%s ip=%s", node_id, peer_ip)
    except Exception as exc:
        logger.warning("Agent 连接异常关闭 node=%s ip=%s err=%s", node_id, peer_ip, exc)
    finally:
        if node_id:
            await manager.unregister(node_id)
        else:
            try:
                await websocket.close()
            except Exception:
                pass


async def _dispatch(
    websocket: WebSocket, node_id: str | None, msg: dict[str, Any], peer_ip: str
) -> str | None:
    """处理一条 Agent 消息，返回（可能更新的）node_id。"""
    msg_type = str(msg.get("type") or "")
    payload = msg.get("payload") if isinstance(msg.get("payload"), dict) else {}

    # ------------------------------------------------------------ register
    if msg_type == "register":
        info = {**payload, **{k: v for k, v in msg.items() if k not in ("type", "payload")}}
        reported_id = str(info.get("node_id") or "").strip()
        if not reported_id:
            logger.warning("register 缺少 node_id，拒绝 ip=%s", peer_ip)
            await _safe_send(websocket, {"type": "error", "message": "register 缺少 node_id"})
            return None

        # 注册连接（若已有同名连接会被替换）
        await manager.register(reported_id, websocket, ip=peer_ip)
        snapshot = registry.register(
            info, ip=peer_ip, is_simulated=bool(info.get("simulate"))
        )
        await _safe_send(websocket, {
            "type": "register_ack",
            "node_id": reported_id,
            "ts": time.time(),
            "server_version": "0.1.0-m1",
            "heartbeat_interval": payload.get("heartbeat_interval") or 5,
            "status": snapshot["status"],
        })
        return reported_id

    # ------------------------------------------------------------ heartbeat
    if msg_type == "heartbeat":
        assert node_id is not None
        snapshot = registry.heartbeat(node_id, payload)
        if "ip" in payload and payload["ip"]:
            pass  # 已在 registry 内更新
        await _safe_send(websocket, {
            "type": "heartbeat_ack",
            "ts": time.time(),
            "server_status": snapshot["status"] if snapshot else "unknown",
        })
        return node_id

    if msg_type == "pong":
        return node_id

    # ------------------------------------------------------------ 其他（M3/M4 填充）
    if msg_type == "ack":
        cmd_id = str(msg.get("cmd_id") or "")
        manager.resolve_pending(cmd_id, payload)
        # 将部署回执写回逐节点状态，避免任务长期停留在 downloading。
        if payload.get("status") in {"ready", "failed"} and payload.get("task_id"):
            from ..database import SessionLocal
            from ..models.deployment import DeployTask, DeployTaskNode
            from ..models.enums import DeployNodeStatus, aggregate_deploy_status
            with SessionLocal() as db:
                row = db.query(DeployTaskNode).filter(DeployTaskNode.task_id == payload["task_id"], DeployTaskNode.node_id == node_id).first()
                task = db.get(DeployTask, payload["task_id"])
                if row and task:
                    row.status = DeployNodeStatus.READY.value if payload["status"] == "ready" else DeployNodeStatus.FAILED.value
                    row.progress = 100 if row.status == DeployNodeStatus.READY.value else row.progress
                    row.bytes_downloaded = int(payload.get("bytes_downloaded") or row.bytes_downloaded)
                    row.error = str(payload.get("error") or "")
                    task.status = aggregate_deploy_status([n.status for n in task.nodes])
                    db.commit()
        return node_id

    logger.debug("暂未处理的 Agent 消息 type=%s node=%s", msg_type, node_id)
    return node_id


async def _safe_send(websocket: WebSocket, payload: dict[str, Any]) -> None:
    """发送失败静默（连接可能已断）。"""
    try:
        await websocket.send_json(payload)
    except Exception:
        pass


# ================================================================= Dashboard


@router.websocket("/ws/dashboard")
async def ws_dashboard(websocket: WebSocket) -> None:
    if not await require_token_ws(websocket):
        await websocket.close(code=1008, reason="invalid token")
        return

    await websocket.accept()
    queue = await bus.subscribe()
    logger.info("Dashboard 已连接 ip=%s", websocket.client.host if websocket.client else "?")

    async def _pump_events() -> None:
        while True:
            message = await queue.get()
            await websocket.send_json(message)

    async def _pump_ping() -> None:
        while True:
            await asyncio.sleep(_DASHBOARD_PING_INTERVAL)
            await websocket.send_json({"type": "ping", "ts": time.time()})

    pump = asyncio.create_task(_pump_events())
    pinger = asyncio.create_task(_pump_ping())
    try:
        while True:
            # 浏览器侧只读；收到任何消息仅用于保活判断
            await websocket.receive_text()
    except WebSocketDisconnect:
        pass
    except Exception as exc:
        logger.debug("Dashboard 连接异常 ip=%s err=%s", websocket.client, exc)
    finally:
        for task in (pump, pinger):
            task.cancel()
        await bus.unsubscribe(queue)
        logger.info("Dashboard 已断开 剩余订阅者统计中")
