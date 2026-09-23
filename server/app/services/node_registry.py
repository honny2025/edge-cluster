"""节点注册与状态维护。

状态机（§4 重点）：
- `never_seen`：从未收到过心跳 —— 对应"没有上报"
- `online`    ：最近一次心跳在 offline_threshold 内
- `offline`   ：曾上线，但心跳超时 —— 对应"节点离线"

关键纪律：**WebSocket 断开不直接判定离线**。断连只说明这条 TCP 通道没了，
最终状态由心跳巡检（HeartbeatMonitor）统一裁决，避免网络抖动造成状态闪烁。
"""

from __future__ import annotations

import time
from typing import Any

from sqlalchemy import func, select
from sqlalchemy.orm import Session

from ..core.logging_conf import get_logger
from ..database import SessionLocal
from ..models import MetricSample, Node, NodeStatus, utcnow
from .event_bus import bus

logger = get_logger("services.node_registry")


def _str_or_none(v: Any) -> Any:
    return v if v not in ("", None) else None


def _int_or_none(v: Any) -> int | None:
    """转 int，失败或负数一律返回 None（拿不到就留空，不造假）。"""
    if v is None or v == "":
        return None
    try:
        n = int(float(v))
    except (TypeError, ValueError):
        return None
    return n if n >= 0 else None


def _float_or_none(v: Any, lo: float = 0.0, hi: float = 100000.0) -> float | None:
    if v is None or v == "":
        return None
    try:
        f = float(v)
    except (TypeError, ValueError):
        return None
    if f != f:  # NaN
        return None
    return f if lo <= f <= hi else None


class NodeRegistry:
    """节点注册 / 心跳处理 / 指标入库。"""

    # ------------------------------------------------------------------ 注册

    def register(self, info: dict[str, Any], ip: str, is_simulated: bool = False) -> dict[str, Any]:
        """Agent 首次注册（或重连后重新注册）。返回节点快照 dict。"""
        node_id = str(info.get("node_id") or "").strip()
        if not node_id:
            raise ValueError("register 缺少 node_id")

        now = utcnow()
        with SessionLocal() as db:
            node = db.get(Node, node_id)
            is_new = node is None
            if node is None:
                node = Node(node_id=node_id, status=NodeStatus.NEVER_SEEN.value)
                db.add(node)

            # 基础信息以 Agent 上报为准（name 允许平台改名后保留平台值）
            if is_new or not node.name:
                node.name = str(info.get("name") or node_id)
            node.model = str(info.get("model") or node.model or "")
            node.soc = str(info.get("soc") or node.soc or "")
            node.android_version = str(info.get("android_version") or node.android_version or "")
            node.agent_version = str(info.get("agent_version") or node.agent_version or "")
            node.ip = str(info.get("ip") or ip or node.ip or "")
            if is_simulated:
                node.is_simulated = 1

            prev_status = node.status
            node.status = NodeStatus.ONLINE.value
            node.last_heartbeat = now
            node.missed_heartbeats = 0
            if prev_status != NodeStatus.ONLINE.value or node.online_since is None:
                node.online_since = now
                node.uptime_seconds = 0
            db.commit()
            db.refresh(node)

            snapshot = self._snapshot(node)

        level = logger.info
        if is_new:
            level("节点首次注册 node=%s name=%s model=%s ip=%s（模拟=%s）",
                  node_id, snapshot["name"], snapshot["model"], snapshot["ip"], is_simulated)
        else:
            level("节点重新注册（重连恢复）node=%s 上次状态=%s", node_id, prev_status)

        bus.publish_threadsafe("node_online" if is_new else "node_updated", snapshot)
        return snapshot

    # ------------------------------------------------------------------ 心跳

    def heartbeat(self, node_id: str, payload: dict[str, Any]) -> dict[str, Any] | None:
        """处理一次心跳：刷新时间、恢复在线态、写入指标采样。"""
        now = utcnow()
        with SessionLocal() as db:
            node = db.get(Node, node_id)
            if node is None:
                # 平台重启后库丢失 / 节点未注册就心跳 —— 自动补建，避免节点永久失联
                logger.warning("收到未注册节点的心跳，自动补建 node=%s", node_id)
                node = Node(node_id=node_id, name=node_id, status=NodeStatus.NEVER_SEEN.value)
                db.add(node)
                db.flush()

            recovered = node.status != NodeStatus.ONLINE.value
            if recovered:
                node.online_since = now
                node.uptime_seconds = 0
            prev_status, prev_hb = node.status, node.last_heartbeat

            node.status = NodeStatus.ONLINE.value
            node.last_heartbeat = now
            node.missed_heartbeats = 0

            # 网络累计流量等也可来自心跳顶层字段
            if payload.get("ip"):
                node.ip = str(payload["ip"])
            if payload.get("name") and not node.name:
                node.name = str(payload["name"])

            metrics = payload.get("metrics") or {}
            self._store_metrics(db, node, metrics, now)
            node.refresh_uptime()
            db.commit()
            db.refresh(node)
            snapshot = self._snapshot(node)

        if recovered:
            gap = (now - prev_hb).total_seconds() if prev_hb else None
            logger.info("节点恢复在线 node=%s 上次状态=%s 中断约=%s",
                        node_id, prev_status, f"{gap:.0f}s" if gap else "未知")
            bus.publish_threadsafe("node_online", snapshot)
        return snapshot

    def reset_online_on_boot(self) -> int:
        """平台启动时把库里残留的 online 状态归零。

        平台重启意味着此前所有 WebSocket 连接都已不存在，库里若还留着 `online`
        并沿用旧的 `online_since` 继续累加 uptime，就是"用旧数据冒充在线" ——
        正是 §4 明令禁止的行为。这里统一降级为 `offline` 并清空在线时长，
        Agent 重连后会重新注册、恢复 online。

        返回被归零的节点数。
        """
        with SessionLocal() as db:
            rows = list(db.scalars(select(Node).where(Node.status == NodeStatus.ONLINE.value)))
            for n in rows:
                n.status = NodeStatus.OFFLINE.value
                n.uptime_seconds = 0
                n.online_since = None
            db.commit()
            count = len(rows)

        if count:
            logger.info("平台启动：%d 个节点由 online 归零为 offline（等待 Agent 重连）", count)
            bus.publish_threadsafe("node_status_reset", {"count": count})
        return count

    def store_metrics_payload(self, node_id: str, metrics: dict[str, Any]) -> None:
        """独立指标上报通道（M2 用，与心跳解耦）。"""
        now = utcnow()
        try:
            with SessionLocal() as db:
                node = db.get(Node, node_id)
                if node is None:
                    return
                self._store_metrics(db, node, metrics, now)
                db.commit()
        except Exception as exc:
            logger.warning("指标入库失败 node=%s err=%s", node_id, exc)

    def _store_metrics(
        self, db: Session, node: Node, metrics: dict[str, Any], now: Any
    ) -> None:
        """把心跳携带的指标写一条采样记录。空指标直接跳过。"""
        if not metrics:
            return
        try:
            sample = MetricSample(
                node_id=node.node_id,
                ts=now,
                cpu_usage=_float_or_none(metrics.get("cpu_usage")),
                gpu_usage=_float_or_none(metrics.get("gpu_usage")),
                npu_usage=_float_or_none(metrics.get("npu_usage")),
                mem_used=_int_or_none(metrics.get("mem_used")),
                mem_total=_int_or_none(metrics.get("mem_total")),
                storage_used=_int_or_none(metrics.get("storage_used")),
                storage_total=_int_or_none(metrics.get("storage_total")),
                temperature=_float_or_none(metrics.get("temperature"), lo=-50.0, hi=200.0),
                power_watts=_float_or_none(metrics.get("power_watts"), lo=0.0, hi=500.0),
                net_rx_bytes=_int_or_none(metrics.get("net_rx_bytes")),
                net_tx_bytes=_int_or_none(metrics.get("net_tx_bytes")),
                load_avg=_float_or_none(metrics.get("load_avg"), lo=0.0, hi=10000.0),
                extra=metrics.get("extra") if isinstance(metrics.get("extra"), dict) else None,
            )
            db.add(sample)
        except Exception as exc:  # 指标写失败不能影响节点在线判定
            logger.warning("指标写入异常 node=%s err=%s", node.node_id, exc)

    # ------------------------------------------------------------------ 查询

    def get(self, node_id: str) -> dict[str, Any] | None:
        with SessionLocal() as db:
            node = db.get(Node, node_id)
            if node is None:
                return None
            node.refresh_uptime()
            db.commit()
            return self._snapshot(node)

    def list(self, status: str | None = None) -> list[dict[str, Any]]:
        with SessionLocal() as db:
            stmt = select(Node)
            if status:
                stmt = stmt.where(Node.status == status)
            nodes = list(db.scalars(stmt))
            out = []
            for n in nodes:
                n.refresh_uptime()
            db.commit()
            for n in nodes:
                out.append(self._snapshot(n))
        # 在线优先 + 名称排序，前端展示稳定
        out.sort(key=lambda x: (x["status"] != NodeStatus.ONLINE.value, x["name"] or x["node_id"]))
        return out

    def stats(self) -> dict[str, int]:
        with SessionLocal() as db:
            rows = db.execute(
                select(Node.status, func.count(Node.node_id)).group_by(Node.status)
            ).all()
        counts = {s: 0 for s in (NodeStatus.ONLINE.value, NodeStatus.OFFLINE.value,
                                 NodeStatus.NEVER_SEEN.value)}
        for status, cnt in rows:
            counts[status] = int(cnt)
        counts["total"] = sum(counts.values())
        return counts

    def rename(self, node_id: str, name: str) -> bool:
        with SessionLocal() as db:
            node = db.get(Node, node_id)
            if node is None:
                return False
            node.name = name
            db.commit()
        logger.info("节点改名 node=%s -> %s", node_id, name)
        bus.publish_threadsafe("node_updated", {"node_id": node_id, "name": name})
        return True

    def delete(self, node_id: str) -> bool:
        with SessionLocal() as db:
            node = db.get(Node, node_id)
            if node is None:
                return False
            db.delete(node)
            db.commit()
        logger.info("节点记录已删除 node=%s", node_id)
        bus.publish_threadsafe("node_deleted", {"node_id": node_id})
        return True

    # ------------------------------------------------------------------ 内部

    @staticmethod
    def _snapshot(node: Node) -> dict[str, Any]:
        return {
            "node_id": node.node_id,
            "name": node.name,
            "model": node.model,
            "soc": node.soc,
            "android_version": node.android_version,
            "agent_version": node.agent_version,
            "ip": node.ip,
            "status": node.status,
            "last_heartbeat": node.last_heartbeat.isoformat() if node.last_heartbeat else None,
            "last_heartbeat_ts": node.last_heartbeat.timestamp() if node.last_heartbeat else None,
            "online_since": node.online_since.isoformat() if node.online_since else None,
            "uptime_seconds": node.uptime_seconds,
            "is_simulated": bool(node.is_simulated),
            "note": node.note,
            "created_at": node.created_at.isoformat() if node.created_at else None,
            "updated_at": node.updated_at.isoformat() if node.updated_at else None,
        }


registry = NodeRegistry()


def now_ts() -> float:
    return time.time()
