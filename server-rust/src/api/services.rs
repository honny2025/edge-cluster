//! 服务管理接口（§4 服务 Service）。与 Python api/routes_services.py 对齐。
//!
//! 生命周期（启动/停止/重启）先过资源准入，再经 WS 下发指令到节点 Agent；
//! 探活（check）由平台主动 GET health_url，把 RUNNING↔ERROR 收敛写回。

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::auth::SharedAuth;
use crate::api::helpers;
use crate::enums::{self, service_status};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::time;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/services", get(list_services).post(create_service))
        .route(
            "/services/{service_id}",
            get(get_service).delete(delete_service),
        )
        .route("/services/{service_id}/logs", get(service_logs))
        .route("/services/{service_id}/check", post(check_service))
        .route("/services/{service_id}/{action}", post(service_action))
}

#[derive(Deserialize)]
struct ServiceCreate {
    node_id: String,
    model_id: Option<String>,
    name: String,
    #[serde(default)]
    port: i64,
    #[serde(default = "default_api_type")]
    api_type: String,
    #[serde(default)]
    command: String,
    args: Option<Value>,
    #[serde(default)]
    health_url: String,
    #[serde(default = "default_true")]
    auto_restart: bool,
    #[serde(default)]
    auto_start: bool,
    #[serde(default)]
    mem_required_mb: i64,
    #[serde(default)]
    mutex_group: String,
}

fn default_api_type() -> String {
    "other".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct ListQuery {
    node_id: Option<String>,
}

async fn list_services(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    Ok(Json(Value::Array(helpers::list_services_out(
        &conn,
        q.node_id.as_deref(),
    ))))
}

async fn create_service(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Json(body): Json<ServiceCreate>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;

    if state.registry.get(&body.node_id).is_none() {
        return Err(ApiError::not_found("node not found"));
    }
    if let Some(mid) = &body.model_id {
        if helpers::read_model(&conn, mid).is_none() {
            return Err(ApiError::not_found("model not found"));
        }
    }

    let sid = crate::util::new_uuid_hex();
    let now = time::now_db();
    let args_json = body.args.as_ref().map(|v| v.to_string());
    let mem_required = if body.mem_required_mb > 0 { body.mem_required_mb } else { 0 };
    let mutex = body.mutex_group.trim().to_string();

    conn.execute(
        "INSERT INTO services (service_id, node_id, model_id, name, status, port, api_type, \
         command, args, health_url, auto_restart, auto_start, mem_required_mb, mutex_group, \
         created_at, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)",
        rusqlite::params![
            sid,
            body.node_id,
            body.model_id,
            body.name,
            service_status::STOPPED,
            body.port,
            body.api_type,
            body.command,
            args_json,
            body.health_url,
            if body.auto_restart { 1i64 } else { 0 },
            if body.auto_start { 1i64 } else { 0 },
            mem_required,
            mutex,
            now,
        ],
    )
    .map_err(|e| ApiError::internal(format!("创建服务失败：{e}")))?;

    let row = helpers::read_service(&conn, &sid)
        .ok_or_else(|| ApiError::internal("服务创建后读取失败"))?;
    Ok(Json(row.out()))
}

async fn get_service(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(service_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    helpers::read_service(&conn, &service_id)
        .map(|r| Json(r.out()))
        .ok_or_else(|| ApiError::not_found("service not found"))
}

async fn delete_service(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(service_id): Path<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let row = helpers::read_service(&conn, &service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    if enums::service_active_states().contains(&row.status.as_str()) {
        return Err(ApiError::conflict(format!(
            "服务仍在 {status} 状态，请先停止再删除（避免设备上留下看不见的孤儿进程）",
            status = row.status
        )));
    }
    let node_id = row.node_id.clone();
    let name = row.name.clone();

    conn.execute("DELETE FROM services WHERE service_id = ?1", [&service_id])
        .map_err(|e| ApiError::internal(format!("删除服务失败：{e}")))?;
    drop(conn);

    let (delivered, _cmd) = state
        .manager
        .send_command(
            &node_id,
            "remove_service",
            json!({"service_id": service_id}),
            0.0,
        )
        .await;

    crate::audit::record(
        &state.pool,
        "service.delete",
        "shared-token",
        "service",
        &service_id,
        &node_id,
        true,
        Some(&json!({"name": name, "agent_notified": delivered})),
        "删除服务",
        &helpers::client_ip(&ci),
    );

    Ok(Json(json!({"deleted": service_id, "agent_notified": delivered})))
}

#[derive(Deserialize)]
struct LogsQuery {
    #[serde(default = "default_lines")]
    lines: i64,
}
fn default_lines() -> i64 {
    200
}

async fn service_logs(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(service_id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let row = helpers::read_service(&conn, &service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;
    let log_path = row.log_path.clone();

    let lines = q.lines.clamp(1, 5000);
    let (delivered, _cmd_id, result) = state
        .manager
        .send_command_result(
            &row.node_id,
            "service_logs",
            json!({"service_id": row.service_id, "lines": lines}),
            5.0,
        )
        .await;
    if !delivered {
        return Err(ApiError::service_unavailable("node is offline, 无法取回日志"));
    }
    match result {
        None => Ok(Json(json!({
            "service_id": service_id,
            "log_path": log_path,
            "lines": [],
            "note": "节点未返回日志（可能正在处理或已超时）",
        }))),
        Some(r) => Ok(Json(json!({
            "service_id": service_id,
            "log_path": log_path,
            "lines": r.get("lines").cloned().unwrap_or_else(|| json!([])),
            "total_bytes": r.get("total_bytes"),
            "status": r.get("status"),
        }))),
    }
}

async fn check_service(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path(service_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let row = helpers::read_service(&conn, &service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let node_ip = node_ip(&state, &row.node_id);
    if node_ip.is_none() || row.port == 0 {
        return Err(ApiError::bad_request(
            "服务缺少可达地址（节点 IP 或端口为空），无法探活",
        ));
    }
    let path = row.health_url.trim_start_matches('/');
    let url = format!("http://{}:{}/{}", node_ip.unwrap(), row.port, path);

    let (ok, detail) = match probe_get(&url).await {
        Ok(status) => (status < 400, format!("HTTP {status}")),
        Err(e) => (false, e),
    };

    let now = time::now_db();
    let new_status = if ok && row.status == service_status::ERROR {
        Some(service_status::RUNNING)
    } else if !ok && row.status == service_status::RUNNING {
        Some(service_status::ERROR)
    } else {
        None
    };

    conn.execute(
        "UPDATE services SET last_probe_at = ?2, probe_ok = ?3 WHERE service_id = ?1",
        rusqlite::params![service_id, now, if ok { 1i64 } else { 0 }],
    )
    .map_err(|e| ApiError::internal(format!("更新探活状态失败：{e}")))?;
    if let Some(st) = new_status {
        conn.execute(
            "UPDATE services SET status = ?2 WHERE service_id = ?1",
            rusqlite::params![service_id, st],
        )
        .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;
    }

    let fresh = helpers::read_service(&conn, &service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;
    let mut out = fresh.out();
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "probe".to_string(),
            json!({"url": url, "ok": ok, "detail": detail}),
        );
    }
    Ok(Json(out))
}

async fn probe_get(url: &str) -> Result<u16, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    match client.get(url).send().await {
        Ok(resp) => Ok(resp.status().as_u16()),
        Err(e) => Err(e.to_string()),
    }
}

fn node_ip(state: &SharedState, node_id: &str) -> Option<String> {
    state
        .registry
        .get(node_id)
        .and_then(|v| v.get("ip").and_then(|i| i.as_str()).map(String::from))
        .filter(|s| !s.is_empty())
}

async fn service_action(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Path((service_id, action)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return Err(ApiError::bad_request("action must be start, stop or restart"));
    }
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let row = helpers::read_service(&conn, &service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;
    if state.registry.get(&row.node_id).is_none() {
        return Err(ApiError::not_found("node not found (service 指向的节点已不存在)"));
    }

    // ---- 停止段 ----
    if action == "stop" || action == "restart" {
        conn.execute(
            "UPDATE services SET status = ?2, pid = NULL, last_error = '' WHERE service_id = ?1",
            rusqlite::params![service_id, service_status::STOPPED],
        )
        .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;

        let (delivered, cmd_id) = state
            .manager
            .send_command(&row.node_id, "stop_service", json!({"service_id": service_id}), 0.0)
            .await;
        if !delivered {
            conn.execute(
                "UPDATE services SET status = 'error', last_error = 'node is offline or command delivery failed' WHERE service_id = ?1",
                [&service_id],
            )
            .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;
            return Ok(Json(action_out(&state, &service_id, &cmd_id, delivered)?));
        }
        if action == "stop" {
            return Ok(Json(action_out(&state, &service_id, &cmd_id, delivered)?));
        }
    }

    // ---- 启动段：内存准入 ----
    if row.mem_required_mb > 0 {
        if let Some(free) = latest_mem_free_mb(&conn, &row.node_id) {
            if free < row.mem_required_mb {
                conn.execute(
                    "UPDATE services SET status = 'error', last_error = ?2 WHERE service_id = ?1",
                    rusqlite::params![
                        service_id,
                        format!(
                            "资源准入失败：节点可用内存约 {free}MB，低于服务要求的 {}MB",
                            row.mem_required_mb
                        )
                    ],
                )
                .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;
                return Err(ApiError::conflict(format!(
                    "资源准入失败：节点可用内存约 {free}MB < 需求 {}MB",
                    row.mem_required_mb
                )));
            }
        }
    }

    // ---- 互斥组 ----
    if !row.mutex_group.is_empty() {
        if let Some(rival) = find_mutex_rival(&conn, &row) {
            conn.execute(
                "UPDATE services SET status = 'error', last_error = ?2 WHERE service_id = ?1",
                rusqlite::params![
                    service_id,
                    format!("互斥组 '{}' 已有运行中的服务 {}", row.mutex_group, rival)
                ],
            )
            .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;
            return Err(ApiError::conflict(format!(
                "互斥组 '{}' 已有运行中的服务（{}），不可同时启动",
                row.mutex_group, rival
            )));
        }
    }

    conn.execute(
        "UPDATE services SET status = ?2, last_error = '' WHERE service_id = ?1",
        rusqlite::params![service_id, service_status::STARTING],
    )
    .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;

    let args: Value = row
        .args
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!([]));
    let (delivered, cmd_id) = state
        .manager
        .send_command(
            &row.node_id,
            "start_service",
            json!({
                "service_id": service_id,
                "command": row.command,
                "args": args,
                "auto_restart": row.auto_restart != 0,
                "auto_start": row.auto_start != 0,
            }),
            0.0,
        )
        .await;
    if !delivered {
        conn.execute(
            "UPDATE services SET status = 'error', last_error = 'node is offline or command delivery failed' WHERE service_id = ?1",
            [&service_id],
        )
        .map_err(|e| ApiError::internal(format!("更新服务状态失败：{e}")))?;
    }
    Ok(Json(action_out(&state, &service_id, &cmd_id, delivered)?))
}

fn action_out(state: &SharedState, service_id: &str, cmd_id: &str, delivered: bool) -> ApiResult<Value> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let row = helpers::read_service(&conn, service_id)
        .ok_or_else(|| ApiError::not_found("service not found"))?;
    let mut out = row.out();
    if let Some(obj) = out.as_object_mut() {
        obj.insert("command_id".to_string(), json!(cmd_id));
        obj.insert("delivered".to_string(), json!(delivered));
    }
    Ok(out)
}

fn latest_mem_free_mb(conn: &rusqlite::Connection, node_id: &str) -> Option<i64> {
    let row: Option<(Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT mem_total, mem_used FROM metric_samples WHERE node_id = ?1 ORDER BY ts DESC LIMIT 1",
            [node_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let (total, used) = match row {
        Some((Some(t), Some(u))) => (t, u),
        _ => return None,
    };
    Some((total - used).max(0) / (1024 * 1024))
}

fn find_mutex_rival(conn: &rusqlite::Connection, row: &helpers::ServiceRow) -> Option<String> {
    let states = enums::service_occupying_states();
    let placeholders = vec!["?"; states.len()].join(",");
    let sql = format!(
        "SELECT name FROM services WHERE node_id = ?1 AND mutex_group = ?2 \
         AND service_id != ?3 AND status IN ({placeholders}) LIMIT 1"
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(row.node_id.clone()),
        Box::new(row.mutex_group.clone()),
        Box::new(row.service_id.clone()),
    ];
    for s in states {
        params.push(Box::new(s.to_string()));
    }
    conn.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get::<_, String>(0))
        .ok()
}
