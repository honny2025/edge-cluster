"""心跳巡检：判定节点离线（状态机裁决者）。

为什么不用"WS 断开 = 离线"：
- 网络抖动、Wi-Fi 漫游、手机锁屏省电都会造成短暂断连，直接判离线会疯狂闪烁；
- 真实离线与短暂抖动在平台侧无法从单次断连区分，只能靠时间窗 + 连续次数防抖。

判定时序（重要，用于解释实际耗时）：
    第 0s   最后一次心跳到达
    第 15s  超过 offline_threshold → 第 1 次判定超时（计数 1）
    第 20s  第 2 次巡查 → 计数 2
    第 25s  第 3 次巡查 → 计数 3 ≥ offline_confirm_times → 落库为 offline
    即：**实际标记耗时 ≈ offline_threshold + (offline_confirm_times - 1) × heartbeat_interval**
    （默认 15 + 2×5 = 25s）

若业务上要求严格"15s 内标记离线"，把 `offline_confirm_times` 设为 1 即可，
代价是网络抖动时状态可能闪烁。这是「快速判离线」与「防抖」之间的固有取舍，
由配置决定，不写死在代码里。

额外职责：
- 在线节点的 uptime_seconds 定期刷新；
- 指标采样定期清理（保留 metric_retention_days 天）。
"""

from __future__ import annotations

import asyncio
from datetime import timedelta

from sqlalchemy import delete, select

from ..config import settings
from ..core.logging_conf import get_logger
from ..database import SessionLocal
from ..models import MetricSample, Node, NodeStatus, utcnow
from .event_bus import bus

logger = get_logger("services.heartbeat_monitor")


class HeartbeatMonitor:
    def __init__(self) -> None:
        self._task: asyncio.Task | None = None
        self._stop = asyncio.Event()
        self._last_cleanup_day: str = ""

    # ------------------------------------------------------------------ 生命周期

    async def start(self) -> None:
        if self._task and not self._task.done():
            return
        self._stop.clear()
        self._task = asyncio.create_task(self._run(), name="heartbeat-monitor")
        logger.info(
            "心跳巡检已启动：周期=%ss 超时阈值=%ss 防抖次数=%d",
            settings.server.heartbeat_interval,
            settings.server.offline_threshold,
            settings.server.offline_confirm_times,
        )

    async def stop(self) -> None:
        self._stop.set()
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except (asyncio.CancelledError, Exception):
                pass
            self._task = None

    # ------------------------------------------------------------------ 主循环

    async def _run(self) -> None:
        interval = max(1, int(settings.server.heartbeat_interval))
        while not self._stop.is_set():
            try:
                # 巡检是同步的 SQLite 操作，放到线程里跑，避免阻塞事件循环
                await asyncio.to_thread(self._check_once)
                self._maybe_cleanup()
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                # 巡检出错必须自愈，绝不能让巡检任务死掉（长期无人值守要求）
                logger.error("心跳巡检异常（将继续运行）err=%s", exc, exc_info=True)
            try:
                await asyncio.wait_for(self._stop.wait(), timeout=interval)
            except asyncio.TimeoutError:
                pass

    def _check_once(self) -> None:
        threshold = int(settings.server.offline_threshold)
        confirm = max(1, int(settings.server.offline_confirm_times))
        now = utcnow()
        cutoff = now - timedelta(seconds=threshold)

        changes: list[dict] = []
        with SessionLocal() as db:
            nodes = list(db.scalars(select(Node).where(Node.status == NodeStatus.ONLINE.value)))
            for node in nodes:
                hb = node.last_heartbeat
                if hb is None:
                    node.missed_heartbeats += 1
                elif hb >= cutoff:
                    # 心跳新鲜：清空防抖计数，顺手刷新在线时长
                    node.missed_heartbeats = 0
                    node.refresh_uptime()
                    continue
                else:
                    node.missed_heartbeats += 1

                if node.missed_heartbeats >= confirm:
                    gap = (now - hb).total_seconds() if hb else -1
                    node.status = NodeStatus.OFFLINE.value
                    node.uptime_seconds = 0
                    changes.append(
                        {
                            "node_id": node.node_id,
                            "name": node.name,
                            "status": node.status,
                            "last_heartbeat": hb.isoformat() if hb else None,
                            "missed_heartbeats": node.missed_heartbeats,
                        }
                    )
                    logger.warning(
                        "节点离线 node=%s name=%s 距上次心跳 %.0fs"
                        "（阈值 %ss + 防抖 %d 次×%ss，理论 %.0fs）",
                        node.node_id, node.name, gap, threshold,
                        node.missed_heartbeats, settings.server.heartbeat_interval,
                        threshold + (node.missed_heartbeats - 1) * settings.server.heartbeat_interval,
                    )
                else:
                    logger.debug(
                        "心跳超时（防抖中 %d/%d）node=%s",
                        node.missed_heartbeats, confirm, node.node_id,
                    )
            db.commit()

        for change in changes:
            bus.publish_threadsafe("node_offline", change)

    # ------------------------------------------------------------------ 指标清理

    def _maybe_cleanup(self) -> None:
        """每天清理一次过期指标采样。"""
        today = utcnow().strftime("%Y-%m-%d")
        if today == self._last_cleanup_day:
            return
        self._last_cleanup_day = today
        days = int(settings.server.metric_retention_days)
        if days <= 0:
            return
        cutoff = utcnow() - timedelta(days=days)
        try:
            with SessionLocal() as db:
                result = db.execute(delete(MetricSample).where(MetricSample.ts < cutoff))
                db.commit()
                if result.rowcount:
                    logger.info("清理过期指标采样 %d 条（保留 %d 天）", result.rowcount, days)
        except Exception as exc:
            logger.warning("指标清理失败（忽略）err=%s", exc)


monitor = HeartbeatMonitor()
