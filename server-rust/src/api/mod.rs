//! HTTP 接口装配。与 Python app/main.py 的路由挂载顺序逐项对齐。
//!
//! 挂载顺序（有重叠前缀时必须先更具体的，与 Python 一致）：
//!   system / nodes / models / deployments / services / gateway /
//!   files(共享 token) / files.agent_router(传输令牌) / audit
//! 全部挂在 `/api/v1` 下；WS 与静态托管挂在根路径。

pub mod audit;
pub mod auth;
pub mod deployments;
pub mod files;
pub mod gateway;
pub mod helpers;
pub mod http;
pub mod models;
pub mod nodes;
pub mod services;
pub mod system;
pub mod ws;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderValue, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower::Layer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::state::AppState;

const VERSION: &str = "0.5.0-m5";

type SharedState = Arc<AppState>;

/// 装配完整路由：API + WS + 静态托管 + CORS。返回无状态的路由器。
pub fn build_router(state: SharedState) -> Router {
    // ---- REST API，统一挂在 /api/v1 ----
    let api = Router::new()
        .merge(system::router())
        .merge(nodes::router())
        .merge(models::router())
        .merge(deployments::router())
        .merge(services::router())
        .merge(gateway::router())
        .merge(files::router())
        // Agent 侧字节搬运口走一次性传输令牌，信任域不同，单独一条。
        .merge(files::agent_router())
        .merge(audit::router());

    let mut app = Router::new()
        .nest("/api/v1", api)
        .route("/api/v1", get(api_root))
        // WS 不挂 /api/v1 下，保持路径简短。
        .merge(ws::router());

    // ---- Web 静态托管（单端口交付）----
    let dist = web_dist(&state);
    if state.config.web.serve_static && dist.join("index.html").exists() {
        let serve = SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, no-store, must-revalidate"),
        )
        // 前端产物（尤其 index.html 引用的 hash JS 名）禁止缓存：
        // 桌面壳 WebView2 对无 Cache-Control 的响应做启发式缓存，改完前端不刷新，
        // 界面就一直吃旧 JS（现象是「部署列表能更新、但传输记录的下载/删除按钮还是旧的」）。
        .layer(
            ServeDir::new(&dist).fallback(ServeFile::new(dist.join("index.html"))),
        );
        app = app.fallback_service(serve);
    } else {
        app = app.route("/", get(index_placeholder));
    }

    // MVP 局域网场景直接放开跨域（与 Python CORSMiddleware 一致，不 allow_credentials）。
    // 请求体上限：模型/部署包上传动辄数百 MB 到数 GB（如 Qwen3-1.7B 约 1.1GB），
    // axum 默认 2MB 会直接把 multipart 截断成「解析失败」。这里放宽到 max_transfer_mb，
    // 与 Python 版（uvicorn 不限 body，仅靠 files.max_transfer_mb 约束传输）对齐。
    let body_limit = state.config.files.max_transfer_mb.max(1) as usize * 1024 * 1024;
    app.layer(
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any),
    )
    .layer(DefaultBodyLimit::max(body_limit))
    .with_state(state)
}

/// `web/dist` 目录（与 Python `BASE_DIR.parent / "web" / "dist"` 对齐）。
fn web_dist(state: &SharedState) -> std::path::PathBuf {
    let base = state.config.base_dir();
    base.parent()
        .unwrap_or_else(|| base.as_path())
        .join("web")
        .join("dist")
}

async fn api_root() -> Json<serde_json::Value> {
    Json(json!({
        "name": "edge-cluster-server",
        "version": VERSION,
        "docs": "/docs",
    }))
}

/// 前端未构建时的占位首页（与 Python 的 else 分支一致）。
async fn index_placeholder() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "name": "edge-cluster-server",
            "version": VERSION,
            "hint": "前端未构建。开发态请运行 cd web && npm run dev（默认 5173）；或先 npm run build 生成 web/dist 再刷新本页。",
            "docs": "/docs",
        })),
    )
}
