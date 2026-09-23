"""节点相关 REST 接口。"""

from __future__ import annotations

from datetime import datetime, timedelta

from fastapi import APIRouter, Depends, HTTPException, Query
from sqlalchemy import select

from ..core.logging_conf import get_logger
from ..core.security import require_token
from ..database import SessionLocal
from ..models import MetricSample, Node, Service
from ..schemas import MetricOut, MetricSeriesOut, NodeListOut, NodeOut, NodeUpdateIn
from ..services.node_registry import registry

router = APIRouter(prefix="/nodes", tags=["nodes"], dependencies=[Depends(require_token)])
logger = get_logger("api.nodes")


@router.get("", response_model=NodeListOut, summary="节点列表")
def list_nodes(status: str | None = Query(default=None, description="按状态过滤")):
    items = registry.list(status=status)
    return NodeListOut(items=[NodeOut(**i) for i in items], total=len(items))


@router.get("/{node_id}", response_model=NodeOut, summary="节点详情")
def get_node(node_id: str):
    item = registry.get(node_id)
    if item is None:
        raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
    return NodeOut(**item)


@router.patch("/{node_id}", response_model=NodeOut, summary="修改节点（改名/备注）")
def update_node(node_id: str, body: NodeUpdateIn):
    with SessionLocal() as db:
        node = db.get(Node, node_id)
        if node is None:
            raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
        node.name = body.name
        if body.note is not None:
            node.note = body.note
        db.commit()
    logger.info("节点信息已更新 node=%s name=%s", node_id, body.name)
    item = registry.get(node_id)
    if item is None:
        raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
    return NodeOut(**item)


@router.delete("/{node_id}", summary="删除节点记录")
def delete_node(node_id: str):
    if not registry.delete(node_id):
        raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
    return {"ok": True, "node_id": node_id}


@router.get("/{node_id}/metrics", response_model=MetricSeriesOut, summary="节点指标历史")
def node_metrics(
    node_id: str,
    minutes: int = Query(default=30, ge=1, le=60 * 24 * 7, description="回看多少分钟"),
    limit: int = Query(default=500, ge=1, le=5000, description="最多返回多少点"),
):
    since = datetime.utcnow() - timedelta(minutes=minutes)
    with SessionLocal() as db:
        if db.get(Node, node_id) is None:
            raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
        rows = list(
            db.scalars(
                select(MetricSample)
                .where(MetricSample.node_id == node_id, MetricSample.ts >= since)
                .order_by(MetricSample.ts.desc())
                .limit(limit)
            )
        )
    # 倒序取最近 N 条后翻正，保证曲线时间递增
    rows.reverse()
    items = [
        MetricOut(
            ts=r.ts.isoformat(),
            cpu_usage=r.cpu_usage,
            gpu_usage=r.gpu_usage,
            npu_usage=r.npu_usage,
            mem_used=r.mem_used,
            mem_total=r.mem_total,
            storage_used=r.storage_used,
            storage_total=r.storage_total,
            temperature=r.temperature,
            power_watts=r.power_watts,
            net_rx_bytes=r.net_rx_bytes,
            net_tx_bytes=r.net_tx_bytes,
            load_avg=r.load_avg,
            extra=r.extra,
        )
        for r in rows
    ]
    return MetricSeriesOut(node_id=node_id, count=len(items), items=items)


@router.get("/{node_id}/metrics/latest", response_model=MetricOut | None, summary="节点最新指标")
def node_metrics_latest(node_id: str):
    with SessionLocal() as db:
        if db.get(Node, node_id) is None:
            raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
        row = db.scalars(
            select(MetricSample)
            .where(MetricSample.node_id == node_id)
            .order_by(MetricSample.ts.desc())
            .limit(1)
        ).first()
    if row is None:
        return None
    return MetricOut(
        ts=row.ts.isoformat(),
        cpu_usage=row.cpu_usage,
        gpu_usage=row.gpu_usage,
        npu_usage=row.npu_usage,
        mem_used=row.mem_used,
        mem_total=row.mem_total,
        storage_used=row.storage_used,
        storage_total=row.storage_total,
        temperature=row.temperature,
        power_watts=row.power_watts,
        net_rx_bytes=row.net_rx_bytes,
        net_tx_bytes=row.net_tx_bytes,
        load_avg=row.load_avg,
        extra=row.extra,
    )


@router.get("/{node_id}/services", summary="节点上的服务列表")
def node_services(node_id: str):
    with SessionLocal() as db:
        if db.get(Node, node_id) is None:
            raise HTTPException(status_code=404, detail=f"节点不存在: {node_id}")
        rows = list(db.scalars(select(Service).where(Service.node_id == node_id)))
    return {
        "items": [
            {
                "service_id": s.service_id,
                "node_id": s.node_id,
                "model_id": s.model_id,
                "name": s.name,
                "status": s.status,
                "port": s.port,
                "endpoint": s.endpoint,
                "api_type": s.api_type,
                "pid": s.pid,
                "started_at": s.started_at.isoformat() if s.started_at else None,
                "last_error": s.last_error,
            }
            for s in rows
        ],
        "total": len(rows),
    }
