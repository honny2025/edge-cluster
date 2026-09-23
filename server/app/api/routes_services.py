from __future__ import annotations
import uuid
from fastapi import APIRouter, Depends, HTTPException
from ..core.connection import manager
from pydantic import BaseModel
from sqlalchemy.orm import Session
from ..database import get_session
from ..models.node import Node
from ..models.model import Model
from ..models.service import Service
from ..models.enums import ServiceStatus

router=APIRouter(prefix="/services",tags=["services"])
class ServiceCreate(BaseModel):
    node_id:str; model_id:str|None=None; name:str; port:int=0; api_type:str="other"; command:str=""; args:list|None=None; health_url:str=""; auto_restart:bool=True

def out(s:Service): return {"service_id":s.service_id,"node_id":s.node_id,"model_id":s.model_id,"name":s.name,"status":s.status,"port":s.port,"endpoint":s.endpoint,"api_type":s.api_type,"pid":s.pid,"last_error":s.last_error,"command":s.command,"args":s.args,"health_url":s.health_url,"auto_restart":bool(s.auto_restart)}
@router.get("")
def list_services(node_id:str|None=None,db:Session=Depends(get_session)):
    q=db.query(Service)
    if node_id:q=q.filter(Service.node_id==node_id)
    return [out(s) for s in q.order_by(Service.created_at.desc()).all()]
@router.post("")
def create_service(body:ServiceCreate,db:Session=Depends(get_session)):
    if not db.get(Node,body.node_id): raise HTTPException(404,"node not found")
    if body.model_id and not db.get(Model,body.model_id): raise HTTPException(404,"model not found")
    sid=uuid.uuid4().hex; s=Service(service_id=sid,node_id=body.node_id,model_id=body.model_id,name=body.name,port=body.port,api_type=body.api_type,command=body.command,args=body.args,health_url=body.health_url,auto_restart=int(body.auto_restart),status=ServiceStatus.STOPPED.value)
    db.add(s);db.commit();db.refresh(s);return out(s)
@router.post("/{service_id}/{action}")
async def service_action(service_id:str,action:str,db:Session=Depends(get_session)):
    s=db.get(Service,service_id)
    if not s: raise HTTPException(404,"service not found")
    if action not in {"start","stop","restart"}: raise HTTPException(400,"action must be start, stop or restart")
    s.status=ServiceStatus.STARTING.value if action in {"start","restart"} else ServiceStatus.STOPPED.value
    if action=="stop": s.pid=None
    s.last_error=""
    db.commit();db.refresh(s)
    delivered, cmd_id = await manager.send_command(s.node_id, f"{action}_service", {"service_id": s.service_id, "command": s.command, "args": s.args or []})
    if not delivered:
        s.status=ServiceStatus.ERROR.value; s.last_error="node is offline or command delivery failed"; db.commit(); db.refresh(s)
    return {**out(s), "command_id": cmd_id, "delivered": delivered}
@router.get("/{service_id}/logs")
def service_logs(service_id:str,db:Session=Depends(get_session)):
    s=db.get(Service,service_id)
    if not s: raise HTTPException(404,"service not found")
    return {"service_id":service_id,"log_path":s.log_path,"lines":[]}
