"""鉴权：MVP 用共享 token。

- WS：握手 query 参数 `?token=xxx`（浏览器/Agent 都不便于自定义 header 时更通用）
- REST：`X-Auth-Token` 头，或 query 里的 `token`（便于 curl / 简单脚本）
- UDP 发现：报文里带 token，不一致不回包

不做用户系统、不做 HTTPS/证书体系（任务书 §7 明确排除）。
"""

from __future__ import annotations

import hmac
from typing import Any

from fastapi import Header, HTTPException, Query, Request, status

from ..config import settings
from .logging_conf import get_logger

logger = get_logger("core.security")


def verify_token(token: str | None) -> bool:
    """常数时间比较，避免时序侧信道。"""
    expected = settings.server.token or ""
    if not token:
        return False
    return hmac.compare_digest(str(token), expected)


async def require_token(
    request: Request,
    x_auth_token: str | None = Header(default=None, alias="X-Auth-Token"),
    token: str | None = Query(default=None),
) -> str:
    """FastAPI 依赖：校验 REST 请求的共享 token。"""
    candidate = x_auth_token or token
    if not verify_token(candidate):
        client = request.client.host if request.client else "unknown"
        logger.warning("鉴权失败 REST path=%s client=%s", request.url.path, client)
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="无效或缺失的共享 token（请检查 X-Auth-Token）",
        )
    return candidate  # type: ignore[return-value]


async def require_token_ws(websocket: Any) -> bool:
    """WS 握手校验。失败返回 False，由调用方 close(1008)。"""
    candidate = websocket.query_params.get("token")
    ok = verify_token(candidate)
    if not ok:
        client = websocket.client.host if websocket.client else "unknown"
        logger.warning("鉴权失败 WS path=%s client=%s", websocket.url.path, client)
    return ok
