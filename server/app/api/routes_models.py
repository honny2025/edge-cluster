from __future__ import annotations
import hashlib, uuid
from pathlib import Path
from fastapi import APIRouter, Depends, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse
from sqlalchemy.orm import Session
from ..database import get_session
from ..config import settings
from ..models.model import Model

router = APIRouter(prefix="/models", tags=["models"])

@router.get("")
def list_models(db: Session = Depends(get_session)):
    return [m.__dict__ | {"_sa_instance_state": None} for m in db.query(Model).order_by(Model.uploaded_at.desc()).all()]

@router.post("")
async def upload_model(file: UploadFile = File(...), name: str = Form(...), version: str = Form("v1"), framework: str = Form("other"), format: str = Form(""), description: str = Form(""), db: Session = Depends(get_session)):
    model_id = uuid.uuid4().hex
    target_dir = settings.models_path; target_dir.mkdir(parents=True, exist_ok=True)
    target = target_dir / f"{model_id}-{Path(file.filename or 'model.bin').name}"
    digest = hashlib.sha256(); size = 0
    with target.open("wb") as out:
        while chunk := await file.read(1024 * 1024): out.write(chunk); digest.update(chunk); size += len(chunk)
    item = Model(model_id=model_id, name=name, version=version, file_name=file.filename or target.name, file_size=size, framework=framework, format=format, sha256=digest.hexdigest(), description=description, storage_path=str(target.relative_to(settings.models_path)))
    db.add(item); db.commit(); db.refresh(item)
    return {"model_id": item.model_id, "name": item.name, "version": item.version, "file_size": size, "sha256": item.sha256}

@router.delete("/{model_id}")
def delete_model(model_id: str, db: Session = Depends(get_session)):
    item = db.get(Model, model_id)
    if not item: raise HTTPException(404, "model not found")
    path = settings.models_path / item.storage_path
    if path.exists(): path.unlink()
    db.delete(item); db.commit(); return {"ok": True}

@router.get("/{model_id}/download")
def download_model(model_id: str, db: Session = Depends(get_session)):
    item = db.get(Model, model_id)
    if not item: raise HTTPException(404, "model not found")
    path = settings.models_path / item.storage_path
    if not path.is_file(): raise HTTPException(404, "model file not found")
    item.download_count += 1; db.commit()
    return FileResponse(path, filename=item.file_name, media_type="application/octet-stream")
