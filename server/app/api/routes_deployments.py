from __future__ import annotations
import uuid
from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel
from sqlalchemy.orm import Session
from ..database import get_session
from ..models.model import Model
from ..models.node import Node
from ..models.deployment import DeployTask, DeployTaskNode
from ..models.enums import DeployTaskStatus, DeployNodeStatus, aggregate_deploy_status
from ..core.connection import manager

router = APIRouter(prefix="/deployments", tags=["deployments"])

class CreateDeployment(BaseModel):
    model_id: str
    node_ids: list[str]
    note: str = ""

def _node(n: DeployTaskNode) -> dict:
    return {"node_id": n.node_id, "node_name": n.node_name, "status": n.status, "progress": n.progress, "error": n.error, "retry_count": n.retry_count, "bytes_downloaded": n.bytes_downloaded, "bytes_total": n.bytes_total}

@router.post("")
def create_deployment(body: CreateDeployment, db: Session = Depends(get_session)):
    model = db.get(Model, body.model_id)
    if not model: raise HTTPException(404, "model not found")
    if not body.node_ids: raise HTTPException(400, "node_ids cannot be empty")
    task = DeployTask(task_id=uuid.uuid4().hex, model_id=model.model_id, model_name=model.name, target_count=len(body.node_ids), note=body.note, status=DeployTaskStatus.PENDING.value)
    seen=set()
    for node_id in body.node_ids:
        if node_id in seen: continue
        seen.add(node_id); node=db.get(Node,node_id)
        task.nodes.append(DeployTaskNode(node_id=node_id, node_name=node.name if node else node_id, bytes_total=model.file_size))
    db.add(task); db.commit(); db.refresh(task)
    return {"task_id": task.task_id, "status": task.status, "model_id": task.model_id, "nodes": [_node(n) for n in task.nodes]}

@router.get("")
def list_deployments(db: Session = Depends(get_session)):
    return [{"task_id": t.task_id, "model_id": t.model_id, "model_name": t.model_name, "status": t.status, "target_count": t.target_count, "nodes": [_node(n) for n in t.nodes]} for t in db.query(DeployTask).order_by(DeployTask.created_at.desc()).all()]

@router.get("/{task_id}")
def get_deployment(task_id: str, db: Session = Depends(get_session)):
    task=db.get(DeployTask,task_id)
    if not task: raise HTTPException(404,"deployment not found")
    return {"task_id": task.task_id, "model_id": task.model_id, "model_name": task.model_name, "status": task.status, "target_count": task.target_count, "nodes": [_node(n) for n in task.nodes]}

@router.post("/{task_id}/retry")
def retry_deployment(task_id: str, db: Session = Depends(get_session)):
    task=db.get(DeployTask,task_id)
    if not task: raise HTTPException(404,"deployment not found")
    for n in task.nodes:
        if n.status == DeployNodeStatus.FAILED.value: n.status=DeployNodeStatus.PENDING.value; n.progress=0; n.error=""; n.retry_count += 1
    task.status=DeployTaskStatus.PENDING.value; db.commit(); return get_deployment(task_id,db)

@router.post("/{task_id}/start")
async def start_deployment(task_id: str, request: Request, db: Session = Depends(get_session)):
    task=db.get(DeployTask,task_id)
    if not task: raise HTTPException(404,"deployment not found")
    model=db.get(Model,task.model_id)
    if not model: raise HTTPException(404,"model not found")
    task.status=DeployTaskStatus.RUNNING.value
    results=[]
    for n in task.nodes:
        base=str(request.base_url).rstrip("/")
        ok,cmd=await manager.send_command(n.node_id,"deploy_model",{"url":f"{base}/api/v1/models/{model.model_id}/download","sha256":model.sha256,"target":model.file_name,"task_id":task_id})
        n.status=DeployNodeStatus.DOWNLOADING.value if ok else DeployNodeStatus.FAILED.value
        n.error="" if ok else "node offline or command delivery failed"
        results.append({"node_id":n.node_id,"delivered":ok,"command_id":cmd})
    db.commit(); return {"task_id":task_id,"status":task.status,"results":results}
