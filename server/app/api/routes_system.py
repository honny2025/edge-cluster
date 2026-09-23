"""系统接口：健康检查、运行信息、首页大屏聚合、平台日志。"""

from __future__ import annotations

from pathlib import Path

from fastapi import APIRouter, Depends, Query

from ..config import settings
from ..core.connection import manager
from ..core.logging_conf import get_logger
from ..core.security import require_token
from ..models import utcnow
from ..schemas import (
    ClusterSeriesOut,
    HealthOut,
    NodeStats,
    OverviewOut,
    ServerLogOut,
    SystemInfoOut,
)
from ..services.node_registry import registry
from ..services.overview import overview

router = APIRouter(prefix="/system", tags=["system"])
logger = get_logger("api.system")

VERSION = "0.2.0-m2"
_STARTED_AT = utcnow()


@router.get("/health", response_model=HealthOut, summary="存活探针（无需鉴权）")
def health():
    return HealthOut(version=VERSION, server_time=utcnow().isoformat())


@router.get("/info", response_model=SystemInfoOut, summary="平台运行信息", dependencies=[Depends(require_token)])
def info():
    stats = registry.stats()
    return SystemInfoOut(
        version=VERSION,
        server_time=utcnow().isoformat(),
        started_at=_STARTED_AT.isoformat(),
        uptime_seconds=int((utcnow() - _STARTED_AT).total_seconds()),
        connected_nodes=manager.count(),
        node_stats=NodeStats(
            total=stats.get("total", 0),
            online=stats.get("online", 0),
            offline=stats.get("offline", 0),
            never_seen=stats.get("never_seen", 0),
        ),
        heartbeat_interval=settings.server.heartbeat_interval,
        offline_threshold=settings.server.offline_threshold,
        discovery_port=settings.server.discovery_port,
    )


@router.get("/overview", response_model=OverviewOut, summary="首页大屏聚合数据",
            dependencies=[Depends(require_token)])
def system_overview():
    """节点统计 + 集群资源汇总 + 告警 + 异常节点，一次返回。

    汇总**只统计在线且采样新鲜的节点**，离线节点的旧数据不参与，
    避免"用旧数据冒充在线"。
    """
    return overview.build_overview()


@router.get("/cluster-metrics", response_model=ClusterSeriesOut, summary="集群资源时间序列",
            dependencies=[Depends(require_token)])
def cluster_metrics(minutes: int = Query(default=30, ge=1, le=60 * 24 * 7, description="回看多少分钟")):
    """大屏趋势图数据源：每个时间桶内对所有在线节点的采样做平均/累加。"""
    return overview.build_cluster_series(minutes=minutes)


@router.get("/server-logs", response_model=ServerLogOut, summary="平台自身日志（最近 N 行）",
            dependencies=[Depends(require_token)])
def server_logs(lines: int = Query(default=200, ge=1, le=5000)):
    log_file = settings.logs_path / "server.log"
    if not log_file.exists():
        return ServerLogOut(lines=[], total_lines=0, log_file=str(log_file))
    try:
        content = _tail(log_file, lines)
    except Exception as exc:
        logger.warning("读取平台日志失败 err=%s", exc)
        return ServerLogOut(lines=[f"读取日志失败: {exc}"], total_lines=0, log_file=str(log_file))
    return ServerLogOut(lines=content, total_lines=len(content), log_file=str(log_file))


def _tail(path: Path, n: int) -> list[str]:
    """从文件尾部读 n 行。大文件也能秒回，不把整个日志读进内存。"""
    block = 8192
    data = b""
    with open(path, "rb") as f:
        f.seek(0, 2)
        size = f.tell()
        pos = size
        while pos > 0 and data.count(b"\n") <= n:
            step = min(block, pos)
            pos -= step
            f.seek(pos)
            data = f.read(step) + data
    text = data.decode("utf-8", errors="replace")
    return text.splitlines()[-n:]
