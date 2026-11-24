//! 网关转发路由（§7 最简网关）。与 Python api/routes_gateway.py 对齐。
//!
//! `/gateway/{service_id}/{path}` → `http://{node.ip}:{service.port}/{path}`，
//! 透传方法/头/体/查询参数；每转一次记耗时与计数（见 services::gateway）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::{get, MethodRouter};
use axum::{Json, Router};
use serde_json::Value;

use crate::api::auth::SharedAuth;
use crate::api::helpers;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/gateway/{service_id}/stats", get(gateway_stats))
        .route("/gateway/{service_id}", forward_router(forward_root))
        .route("/gateway/{service_id}/{*path}", forward_router(forward_path))
}

/// 只透传 Python 版支持的方法（GET/POST/PUT/DELETE/PATCH/OPTIONS/HEAD）。
fn forward_router<H, T>(handler: H) -> MethodRouter<SharedState>
where
    H: axum::handler::Handler<T, SharedState> + Clone,
    T: Send + Sync + 'static,
{
    get(handler.clone())
        .post(handler.clone())
        .put(handler.clone())
        .delete(handler.clone())
        .patch(handler.clone())
        .options(handler.clone())
        .head(handler)
}

async fn gateway_stats(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(service_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    if helpers::read_service(&conn, &service_id).is_none() {
        return Err(ApiError::not_found("service not found"));
    }
    Ok(Json(state.gateway.get(&service_id)))
}

async fn forward_root(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(service_id): AxPath<String>,
    method: Method,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> ApiResult<Response> {
    do_forward(&state, &service_id, "", method, headers, body, query).await
}

async fn forward_path(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath((service_id, path)): AxPath<(String, String)>,
    method: Method,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> ApiResult<Response> {
    do_forward(&state, &service_id, &path, method, headers, body, query).await
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "host" | "content-length" | "connection" | "transfer-encoding"
    )
}

async fn do_forward(
    state: &SharedState,
    service_id: &str,
    path: &str,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    query: Option<String>,
) -> ApiResult<Response> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let svc = helpers::read_service(&conn, service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let node_ip = state
        .registry
        .get(&svc.node_id)
        .and_then(|v| v.get("ip").and_then(|i| i.as_str()).map(String::from))
        .filter(|s| !s.is_empty());
    if node_ip.is_none() || svc.port == 0 {
        return Err(ApiError::bad_request(
            "服务缺少可达地址（节点 IP 或端口为空），无法转发",
        ));
    }

    let mut url = format!("http://{}:{}/{}", node_ip.unwrap(), svc.port, path.trim_start_matches('/'));
    if let Some(q) = query {
        if !q.is_empty() {
            url = format!("{url}?{q}");
        }
    }

    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|e| ApiError::bad_request(format!("不支持的方法：{e}")))?;

    let mut rh = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if is_hop_by_hop(&lower) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            rh.insert(n, v);
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| ApiError::internal(format!("构造网关客户端失败：{e}")))?;

    let t0 = Instant::now();
    match client.request(req_method, url.as_str()).headers(rh).body(body).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let resp_headers = resp.headers().clone();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| ApiError::internal(format!("读取转发响应失败：{e}")))?;
            let ms = t0.elapsed().as_secs_f64() * 1000.0;

            let mut builder = Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY));
            for (name, value) in resp_headers.iter() {
                let lower = name.as_str().to_ascii_lowercase();
                if is_hop_by_hop(&lower) {
                    continue;
                }
                builder = builder.header(name.clone(), value.clone());
            }
            let response = builder
                .body(Body::from(bytes))
                .map_err(|e| ApiError::internal(format!("构造转发响应失败：{e}")))?;

            state.gateway.record(service_id, true, ms, Some(status as i64));
            Ok(response)
        }
        Err(e) => {
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            state.gateway.record(service_id, false, ms, None);
            Err(ApiError::bad_gateway(format!("网关转发失败：{e}")))
        }
    }
}
