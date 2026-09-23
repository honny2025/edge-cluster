"""FastAPI 应用装配。

生命周期：
  启动 → 初始化日志 → 建表 → 启动 UDP 发现 → 启动心跳巡检 → （可选）挂载 Web 静态
  关闭 → 停巡检 → 停发现 → 关所有 WS 连接

静态托管：若 web/dist 存在且 web.serve_static=true，则把构建产物挂在根路径，
实现"单端口交付"（开发态仍可用 Vite 的 5173 + proxy）。
"""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles

from .api import routes_nodes, routes_system, routes_models, routes_deployments, routes_services, ws
from .config import BASE_DIR, settings
from .core.connection import manager
from .core.logging_conf import get_logger, setup_logging
from .database import init_db
from .services.discovery import discovery
from .services.event_bus import bus
from .services.heartbeat_monitor import monitor
from .services.node_registry import registry

VERSION = "0.2.0-m2"
API_PREFIX = "/api/v1"

logger = get_logger("app.main")


@asynccontextmanager
async def lifespan(app: FastAPI):
    setup_logging()
    logger.info("=" * 70)
    logger.info("手机边缘计算集群管理平台 启动中 version=%s", VERSION)
    settings.ensure_dirs()
    init_db()
    logger.info("数据库就绪 path=%s", settings.db_file)

    # 平台刚启动时不可能持有任何 Agent 连接：把库里残留的 online 归零，
    # 否则会出现"平台重启后节点仍显示在线、在线时长继续累加"的假在线状态。
    registry.reset_online_on_boot()

    # 事件总线绑定当前事件循环（UDP 线程需要跨线程投递）
    bus.bind_loop(asyncio.get_running_loop())

    discovery.start()
    await monitor.start()

    logger.info("平台已就绪：HTTP http://%s:%s  发现端口=%s",
                settings.server.host, settings.server.port, settings.server.discovery_port)
    logger.info("=" * 70)
    try:
        yield
    finally:
        logger.info("平台正在关闭...")
        await monitor.stop()
        discovery.stop()
        await manager.close_all()
        logger.info("平台已关闭")


app = FastAPI(
    title="手机边缘计算集群管理平台",
    description="把局域网内的 Android 手机当作边缘计算节点统一纳管（MVP 第一阶段）",
    version=VERSION,
    lifespan=lifespan,
    docs_url="/docs",
    redoc_url="/redoc",
)

# MVP 局域网场景，直接放开跨域，便于 Vite dev server 直连
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=False,
    allow_methods=["*"],
    allow_headers=["*"],
)

# ---- 路由 ----
app.include_router(routes_system.router, prefix=API_PREFIX)
app.include_router(routes_nodes.router, prefix=API_PREFIX)
app.include_router(routes_models.router, prefix=API_PREFIX)
app.include_router(routes_deployments.router, prefix=API_PREFIX)
app.include_router(routes_services.router, prefix=API_PREFIX)
app.include_router(ws.router)  # WS 不挂在 /api/v1 下，保持路径简短


@app.get("/api/v1", include_in_schema=False)
async def api_root():
    return {"name": "edge-cluster-server", "version": VERSION, "docs": "/docs"}


# ---- Web 静态托管（单端口交付）----
_WEB_DIST = BASE_DIR.parent / "web" / "dist"

if settings.web.serve_static and _WEB_DIST.exists():
    app.mount("/assets", StaticFiles(directory=str(_WEB_DIST / "assets")), name="assets")

    @app.get("/", include_in_schema=False)
    async def _index():
        return FileResponse(str(_WEB_DIST / "index.html"))

    @app.get("/{full_path:path}", include_in_schema=False)
    async def _spa_fallback(full_path: str):
        """SPA 路由回退；API 与 WS 路径不拦截。"""
        if full_path.startswith(("api/", "ws/", "docs", "redoc")):
            return JSONResponse({"detail": "Not Found"}, status_code=404)
        candidate: Path = _WEB_DIST / full_path
        if candidate.is_file():
            return FileResponse(str(candidate))
        return FileResponse(str(_WEB_DIST / "index.html"))

else:

    @app.get("/", include_in_schema=False)
    async def _index_placeholder():
        return {
            "name": "edge-cluster-server",
            "version": VERSION,
            "hint": "前端未构建。开发态请运行 cd web && npm run dev（默认 5173）；"
                    "或先 npm run build 生成 web/dist 再刷新本页。",
            "docs": "/docs",
        }


def run() -> None:
    """开发态启动入口。"""
    import uvicorn

    setup_logging()
    uvicorn.run(
        "app.main:app",
        host=settings.server.host,
        port=settings.server.port,
        log_level=str(settings.server.log_level).lower(),
        ws_ping_interval=20,
        ws_ping_timeout=20,
    )
