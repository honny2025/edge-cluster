//! 审计查询接口（需求 §8「任务审计」）。与 Python api/routes_audit.py 对齐。
//!
//! 只读：写入统一走 `crate::audit::record`，这里没有任何写接口。

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::auth::SharedAuth;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::time;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/audit", get(list_audit))
        .route("/audit/actions", get(list_actions))
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    action: String,
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    actor: String,
    ok: Option<i64>,
}
fn default_limit() -> i64 {
    100
}

fn out_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    // 列序：id, ts, actor, client_ip, action, target_type, target_id, node_id, ok, detail, message
    let detail: Option<String> = r.get(9)?;
    let detail_val = detail
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "ts": time::db_to_iso(&r.get::<_, String>(1)?),
        "actor": r.get::<_, String>(2)?,
        "client_ip": r.get::<_, String>(3)?,
        "action": r.get::<_, String>(4)?,
        "target_type": r.get::<_, String>(5)?,
        "target_id": r.get::<_, String>(6)?,
        "node_id": r.get::<_, String>(7)?,
        "ok": r.get::<_, i64>(8)?,
        "detail": detail_val,
        "message": r.get::<_, String>(10)?,
    }))
}

async fn list_audit(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;

    let limit = q.limit.clamp(1, 1000);
    let offset = q.offset.max(0);

    // 动态拼接 WHERE。action 前缀匹配（与 Python 一致，不转义 LIKE 通配符）。
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if !q.action.is_empty() {
        clauses.push("action LIKE ?".to_string());
        params.push(rusqlite::types::Value::Text(format!("{}%", q.action)));
    }
    if !q.node_id.is_empty() {
        clauses.push("node_id = ?".to_string());
        params.push(rusqlite::types::Value::Text(q.node_id.clone()));
    }
    if !q.actor.is_empty() {
        clauses.push("actor = ?".to_string());
        params.push(rusqlite::types::Value::Text(q.actor.clone()));
    }
    if let Some(ok) = q.ok {
        clauses.push("ok = ?".to_string());
        params.push(rusqlite::types::Value::Integer(ok));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    let col = "id, ts, actor, client_ip, action, target_type, target_id, node_id, ok, detail, message";
    let list_sql = format!(
        "SELECT {col} FROM audit_logs{where_sql} ORDER BY id DESC LIMIT ? OFFSET ?"
    );
    let total_sql = format!("SELECT COUNT(*) FROM audit_logs{where_sql}");

    let mut items = Vec::new();
    {
        let mut stmt = conn
            .prepare(&list_sql)
            .map_err(|e| ApiError::internal(format!("查询审计失败：{e}")))?;
        let mut list_params = params.clone();
        list_params.push(rusqlite::types::Value::Integer(limit));
        list_params.push(rusqlite::types::Value::Integer(offset));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(list_params), out_row)
            .map_err(|e| ApiError::internal(format!("查询审计失败：{e}")))?;
        for r in rows.flatten() {
            items.push(r);
        }
    }

    let total: i64 = conn
        .query_row(
            &total_sql,
            rusqlite::params_from_iter(params),
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(Json(json!({
        "items": items,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

async fn list_actions(
    _auth: SharedAuth,
    State(state): State<SharedState>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT action FROM audit_logs WHERE action != '' ORDER BY action")
        .map_err(|e| ApiError::internal(format!("查询审计动作失败：{e}")))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| ApiError::internal(format!("查询审计动作失败：{e}")))?;
    let items: Vec<String> = rows.flatten().collect();
    Ok(Json(json!({"items": items})))
}
