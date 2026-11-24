//! 部署任务接口（里程碑 3）。与 Python api/routes_deployments.py 对齐。
//!
//! 创建即下发（deploy_queue 异步并发执行，上限 2），前端轮询任务详情看进度。

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path as AxPath, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::auth::SharedAuth;
use crate::api::helpers;
use crate::enums::{self, deploy_node_status, deploy_task_status};
use crate::error::{ApiError, ApiResult};
use crate::services::compat::check_compatibility;
use crate::state::AppState;
use crate::time;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/deployments", get(list_deployments).post(create_deployment))
        .route("/deployments/queue/stats", get(queue_stats))
        .route("/deployments/precheck", post(precheck_deployment))
        .route("/deployments/{task_id}", get(get_deployment))
        .route("/deployments/{task_id}/cancel", post(cancel_deployment))
        .route("/deployments/{task_id}/delete", post(delete_deployment))
        .route("/deployments/{task_id}/retry", post(retry_deployment))
        .route("/deployments/{task_id}/start", post(start_deployment))
}

#[derive(Deserialize)]
struct CreateDeployment {
    model_id: String,
    node_ids: Vec<String>,
    #[serde(default)]
    note: String,
}

async fn create_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    ci: ConnectInfo<SocketAddr>,
    Json(body): Json<CreateDeployment>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;

    let model = helpers::read_model(&conn, &body.model_id)
        .ok_or_else(|| ApiError::not_found("model not found"))?;
    if body.node_ids.is_empty() {
        return Err(ApiError::bad_request("node_ids cannot be empty"));
    }

    let task_id = crate::util::new_uuid_hex();
    let now = time::now_db();
    let target_count = body.node_ids.len() as i64;

    conn.execute(
        "INSERT INTO deploy_tasks (task_id, model_id, model_name, target_count, status, note, created_by, created_at) \
         VALUES (?1,?2,?3,?4,'pending',?5,'platform',?6)",
        rusqlite::params![task_id, model.model_id, model.name, target_count, body.note, now],
    )
    .map_err(|e| ApiError::internal(format!("创建部署任务失败：{e}")))?;

    let mut seen: HashSet<String> = HashSet::new();
    for node_id in &body.node_ids {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        let node_name = state
            .registry
            .get(node_id)
            .and_then(|n| n["name"].as_str().map(String::from))
            .unwrap_or_else(|| node_id.clone());
        let bytes_total = if model.file_size > 0 { model.file_size } else { 0 };
        conn.execute(
            "INSERT INTO deploy_task_nodes (task_id, node_id, node_name, status, progress, error, \
             retry_count, bytes_downloaded, bytes_total, rollback_note, updated_at) \
             VALUES (?1,?2,?3,'pending',0,'',0,0,?4,'',?5)",
            rusqlite::params![task_id, node_id, node_name, bytes_total, now],
        )
        .map_err(|e| ApiError::internal(format!("创建部署节点失败：{e}")))?;
    }

    // 入队（不在这里直接发指令）：并发控制 + 兼容性/磁盘预检都在队列里做。
    for node_id in &seen {
        state.deploy_queue.submit(&task_id, node_id);
    }

    let mut sorted_nodes: Vec<String> = seen.into_iter().collect();
    sorted_nodes.sort();
    crate::audit::record(
        &state.pool,
        "deployment.create",
        "shared-token",
        "deployment",
        &task_id,
        "",
        true,
        Some(&json!({"model_id": model.model_id, "model_name": model.name, "nodes": sorted_nodes})),
        "创建部署任务",
        &helpers::client_ip(&ci),
    );

    Ok(Json(
        helpers::task_dict(&conn, &task_id)
            .ok_or_else(|| ApiError::internal("部署任务创建后读取失败"))?,
    ))
}

async fn list_deployments(_auth: SharedAuth, State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    // 与 list_models / list_nodes / list_transfers 对齐：包成 {items,total}。
    // Python 原版这里漏包了、直接返回数组，前端 DeploymentListOut 期望 {items,total}，
    // 结果 data.items 永远是 undefined，部署列表一直空着（"创建下发后看不到状态"）。
    let items = helpers::list_tasks(&conn);
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

async fn queue_stats(_auth: SharedAuth, State(state): State<SharedState>) -> Json<Value> {
    Json(json!({
        "queued": state.deploy_queue.qsize(),
        "concurrency": state.deploy_queue.concurrency,
    }))
}

async fn get_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(task_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    helpers::task_dict(&conn, &task_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("deployment not found"))
}

async fn cancel_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(task_id): AxPath<String>,
    ci: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;

    let task_exists: bool = conn
        .query_row(
            "SELECT 1 FROM deploy_tasks WHERE task_id = ?1",
            [&task_id],
            |_| Ok(()),
        )
        .is_ok();
    if !task_exists {
        return Err(ApiError::not_found("deployment not found"));
    }

    // 找出还没到终态的节点。注意：终态判定看的是 status，不是 node_id。
    let terminal: Vec<&str> = enums::deploy_terminal_node_states().to_vec();
    let mut pending: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT node_id, status FROM deploy_task_nodes WHERE task_id = ?1")
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        let rows = stmt
            .query_map([&task_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        for row in rows.flatten() {
            if !terminal.contains(&row.1.as_str()) {
                pending.push(row.0);
            }
        }
    }
    if pending.is_empty() {
        return Err(ApiError::conflict("任务已结束，没有可取消的节点"));
    }

    let now = time::now_db();
    for node_id in &pending {
        conn.execute(
            "UPDATE deploy_task_nodes SET status = 'cancelled', error = CASE WHEN error = '' THEN '用户取消' ELSE error END, finished_at = ?2 WHERE task_id = ?1 AND node_id = ?3",
            rusqlite::params![task_id, now, node_id],
        )
        .map_err(|e| ApiError::internal(format!("取消部署节点失败：{e}")))?;
    }
    conn.execute(
        "UPDATE deploy_tasks SET status = 'cancelled', finished_at = ?2 WHERE task_id = ?1",
        rusqlite::params![task_id, now],
    )
    .map_err(|e| ApiError::internal(format!("取消部署任务失败：{e}")))?;

    let mut notified: serde_json::Map<String, Value> = serde_json::Map::new();
    for node_id in &pending {
        let ok = state.deploy_queue.cancel_node(&task_id, node_id).await;
        notified.insert(node_id.clone(), json!(ok));
    }

    crate::audit::record(
        &state.pool,
        "deployment.cancel",
        "shared-token",
        "deployment",
        &task_id,
        "",
        true,
        Some(&json!({"notified": notified})),
        "取消部署任务",
        &helpers::client_ip(&ci),
    );

    let mut dict = helpers::task_dict(&conn, &task_id)
        .ok_or_else(|| ApiError::internal("取消后读取任务失败"))?;
    dict.as_object_mut().unwrap().insert("notified".to_string(), json!(notified));
    Ok(Json(dict))
}

async fn delete_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(task_id): AxPath<String>,
    ci: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    if !deploy_task_exists(&conn, &task_id) {
        return Err(ApiError::not_found("deployment not found"));
    }

    // 还有节点在跑：先取消再删，避免手机仍在下载却把记录抹掉。
    // 这里不再像旧版那样对进行中的任务直接回 409 —— 用户点「删除」的意图就是
    // 把这条记录清掉，平台替他把未完成的节点取消掉更符合预期，少一次往返。
    let terminal: Vec<&str> = enums::deploy_terminal_node_states().to_vec();
    let mut pending: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT node_id, status FROM deploy_task_nodes WHERE task_id = ?1")
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        let rows = stmt
            .query_map([&task_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        for row in rows.flatten() {
            if !terminal.contains(&row.1.as_str()) {
                pending.push(row.0);
            }
        }
    }

    let mut cancelled_nodes = 0i64;
    if !pending.is_empty() {
        let now = time::now_db();
        for node_id in &pending {
            conn.execute(
                "UPDATE deploy_task_nodes SET status = 'cancelled', \
                 error = CASE WHEN error = '' THEN '删除任务时取消' ELSE error END, finished_at = ?2 \
                 WHERE task_id = ?1 AND node_id = ?3",
                rusqlite::params![task_id, now, node_id],
            )
            .map_err(|e| ApiError::internal(format!("取消部署节点失败：{e}")))?;
            cancelled_nodes += 1;
        }
        conn.execute(
            "UPDATE deploy_tasks SET status = 'cancelled', finished_at = ?2 WHERE task_id = ?1",
            rusqlite::params![task_id, now],
        )
        .map_err(|e| ApiError::internal(format!("取消部署任务失败：{e}")))?;

        let mut notified: serde_json::Map<String, Value> = serde_json::Map::new();
        for node_id in &pending {
            let ok = state.deploy_queue.cancel_node(&task_id, node_id).await;
            notified.insert(node_id.clone(), json!(ok));
        }
        crate::audit::record(
            &state.pool,
            "deployment.cancel",
            "shared-token",
            "deployment",
            &task_id,
            "",
            true,
            Some(&json!({"notified": notified, "reason": "delete"})),
            "删除任务时取消未完成节点",
            &helpers::client_ip(&ci),
        );
    }

    conn.execute("DELETE FROM deploy_task_nodes WHERE task_id = ?1", [&task_id])
        .map_err(|e| ApiError::internal(format!("删除部署节点失败：{e}")))?;
    conn.execute("DELETE FROM deploy_tasks WHERE task_id = ?1", [&task_id])
        .map_err(|e| ApiError::internal(format!("删除部署任务失败：{e}")))?;

    crate::audit::record(
        &state.pool,
        "deployment.delete",
        "shared-token",
        "deployment",
        &task_id,
        "",
        true,
        Some(&json!({"task_id": task_id, "cancelled_nodes": cancelled_nodes})),
        "删除部署任务",
        &helpers::client_ip(&ci),
    );

    Ok(Json(json!({ "deleted": true, "task_id": task_id })))
}

async fn precheck_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Json(body): Json<CreateDeployment>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let model = helpers::read_model(&conn, &body.model_id)
        .ok_or_else(|| ApiError::not_found("model not found"))?;
    let compat_model = model.compat();

    let mut results = Vec::new();
    for node_id in &body.node_ids {
        match helpers::read_compat_node(&conn, node_id) {
            None => results.push(json!({
                "node_id": node_id,
                "node_name": node_id,
                "ok": false,
                "reason": "节点不存在",
                "checks": [],
            })),
            Some(node) => {
                let node_name = state
                    .registry
                    .get(node_id)
                    .and_then(|n| n["name"].as_str().map(String::from))
                    .unwrap_or_else(|| node_id.clone());
                let mut r = check_compatibility(&compat_model, &node);
                if let Some(obj) = r.as_object_mut() {
                    obj.insert("node_id".to_string(), json!(node_id));
                    obj.insert("node_name".to_string(), json!(node_name));
                }
                results.push(r);
            }
        }
    }
    Ok(Json(json!({ "model_id": body.model_id, "results": results })))
}

async fn retry_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(task_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    if !deploy_task_exists(&conn, &task_id) {
        return Err(ApiError::not_found("deployment not found"));
    }

    conn.execute(
        "UPDATE deploy_task_nodes SET status = 'pending', progress = 0, error = '', rollback_note = '', retry_count = retry_count + 1 \
         WHERE task_id = ?1 AND status = 'failed'",
        [&task_id],
    )
    .map_err(|e| ApiError::internal(format!("重试部署失败：{e}")))?;
    conn.execute(
        "UPDATE deploy_tasks SET status = 'pending' WHERE task_id = ?1",
        [&task_id],
    )
    .map_err(|e| ApiError::internal(format!("重试部署失败：{e}")))?;

    let mut node_ids: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT node_id FROM deploy_task_nodes WHERE task_id = ?1 AND status = 'pending'")
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        let rows = stmt
            .query_map([&task_id], |r| r.get::<_, String>(0))
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        for row in rows.flatten() {
            node_ids.push(row);
        }
    }
    for node_id in &node_ids {
        state.deploy_queue.submit(&task_id, node_id);
    }

    helpers::task_dict(&conn, &task_id)
        .map(Json)
        .ok_or_else(|| ApiError::internal("重试后读取任务失败"))
}

async fn start_deployment(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(task_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    if !deploy_task_exists(&conn, &task_id) {
        return Err(ApiError::not_found("deployment not found"));
    }
    let model_id: String = conn
        .query_row("SELECT model_id FROM deploy_tasks WHERE task_id = ?1", [&task_id], |r| r.get(0))
        .map_err(|e| ApiError::internal(format!("查询部署任务失败：{e}")))?;
    if helpers::read_model(&conn, &model_id).is_none() {
        return Err(ApiError::not_found("model not found"));
    }

    conn.execute(
        "UPDATE deploy_tasks SET status = 'pending' WHERE task_id = ?1",
        [&task_id],
    )
    .map_err(|e| ApiError::internal(format!("启动部署失败：{e}")))?;
    conn.execute(
        "UPDATE deploy_task_nodes SET status = 'pending', error = '', progress = 0 WHERE task_id = ?1",
        [&task_id],
    )
    .map_err(|e| ApiError::internal(format!("启动部署失败：{e}")))?;

    let mut node_ids: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT node_id FROM deploy_task_nodes WHERE task_id = ?1")
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        let rows = stmt
            .query_map([&task_id], |r| r.get::<_, String>(0))
            .map_err(|e| ApiError::internal(format!("查询部署节点失败：{e}")))?;
        for row in rows.flatten() {
            node_ids.push(row);
        }
    }
    for node_id in &node_ids {
        state.deploy_queue.submit(&task_id, node_id);
    }

    helpers::task_dict(&conn, &task_id)
        .map(Json)
        .ok_or_else(|| ApiError::internal("启动后读取任务失败"))
}

fn deploy_task_exists(conn: &rusqlite::Connection, task_id: &str) -> bool {
    conn.query_row("SELECT 1 FROM deploy_tasks WHERE task_id = ?1", [task_id], |_| Ok(()))
        .is_ok()
}

// 保证 const 被引用（状态常量在 SQL 里用字符串字面量，这里显式引用以保持契约清晰）。
#[allow(dead_code)]
fn _state_consts() -> (&'static str, &'static str, &'static str, &'static str) {
    (
        deploy_node_status::PENDING,
        deploy_node_status::CANCELLED,
        deploy_task_status::PENDING,
        deploy_task_status::CANCELLED,
    )
}
