//! 节点接口。与 Python api/routes_nodes.py 对齐。
//!
//! 快照（NodeOut）统一由 `NodeRegistry` 产出，这里不再手写字段列表 ——
//! 参见 schemas/__init__.py NodeOut 与 node_registry::NodeRow::snapshot。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::auth::SharedAuth;
use crate::api::helpers;
use crate::error::{ApiError, ApiResult};
use crate::services::metrics_query;
use crate::state::AppState;
use crate::time;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/nodes", get(list_nodes))
        .route("/nodes/{node_id}", get(get_node).patch(update_node).delete(delete_node))
        .route("/nodes/{node_id}/metrics", get(node_metrics))
        .route("/nodes/{node_id}/metrics/latest", get(node_metrics_latest))
        .route("/nodes/{node_id}/services", get(node_services))
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
}

async fn list_nodes(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<ListQuery>,
) -> Json<Value> {
    let items = state.registry.list(q.status.as_deref());
    let total = items.len();
    Json(json!({ "items": items, "total": total }))
}

async fn get_node(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> ApiResult<Json<Value>> {
    match state.registry.get(&node_id) {
        Some(item) => Ok(Json(item)),
        None => Err(ApiError::not_found(format!("节点不存在: {node_id}"))),
    }
}

#[derive(Deserialize)]
struct UpdateIn {
    name: String,
    #[serde(default)]
    note: Option<String>,
}

async fn update_node(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
    Json(body): Json<UpdateIn>,
) -> ApiResult<Json<Value>> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(ApiError::bad_request("name 长度需在 1..128 之间"));
    }
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let affected = conn
        .execute(
            "UPDATE nodes SET name = ?2, note = COALESCE(?3, note), updated_at = ?4 WHERE node_id = ?1",
            rusqlite::params![node_id, name, body.note, time::now_db()],
        )
        .map_err(|e| ApiError::internal(format!("更新节点失败：{e}")))?;
    if affected == 0 {
        return Err(ApiError::not_found(format!("节点不存在: {node_id}")));
    }
    drop(conn);
    match state.registry.get(&node_id) {
        Some(item) => Ok(Json(item)),
        None => Err(ApiError::not_found(format!("节点不存在: {node_id}"))),
    }
}

async fn delete_node(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> ApiResult<Json<Value>> {
    if !state.registry.delete(&node_id) {
        return Err(ApiError::not_found(format!("节点不存在: {node_id}")));
    }
    Ok(Json(json!({ "ok": true, "node_id": node_id })))
}

#[derive(Deserialize)]
struct MetricsQuery {
    #[serde(default = "default_minutes")]
    minutes: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_minutes() -> i64 {
    30
}
fn default_limit() -> i64 {
    500
}

async fn node_metrics(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<Value>> {
    let minutes = q.minutes.clamp(1, 60 * 24 * 365);
    let limit = q.limit.clamp(1, 5000) as usize;
    if state.registry.get(&node_id).is_none() {
        return Err(ApiError::not_found(format!("节点不存在: {node_id}")));
    }
    let since = time::now() - chrono::Duration::minutes(minutes);
    let until = time::now();
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let (rows, plan) = metrics_query::fetch_history(&conn, &[node_id.clone()], since, until);
    let picked: Vec<&crate::services::metrics_query::MetricRow> = if rows.len() > limit {
        rows[rows.len() - limit..].iter().map(|(r, _)| r).collect()
    } else {
        rows.iter().map(|(r, _)| r).collect()
    };
    let items: Vec<Value> = picked.iter().map(|r| r.api_json()).collect();
    let tiers: Vec<String> = plan.iter().map(|(t, _, _)| t.clone()).collect();
    Ok(Json(json!({
        "node_id": node_id,
        "count": items.len(),
        "items": items,
        "tiers": tiers,
    })))
}

async fn node_metrics_latest(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> ApiResult<Json<Value>> {
    if state.registry.get(&node_id).is_none() {
        return Err(ApiError::not_found(format!("节点不存在: {node_id}")));
    }
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let samples = metrics_query::latest_samples(&conn, Some(&[node_id.clone()]));
    match samples.get(&node_id) {
        Some(row) => Ok(Json(row.api_json())),
        None => Ok(Json(Value::Null)),
    }
}

async fn node_services(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> ApiResult<Json<Value>> {
    if state.registry.get(&node_id).is_none() {
        return Err(ApiError::not_found(format!("节点不存在: {node_id}")));
    }
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let items = helpers::list_service_summaries(&conn, &node_id);
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}
