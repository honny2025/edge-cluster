//! 文件管理接口（需求 §4）。与 Python api/routes_files.py 对齐。
//!
//! 两个 router，两种信任域：控制台走共享 token，Agent 走一次性传输令牌。
//! 平台不主动连手机 —— 上传/下载都由 Agent 发起 HTTP。

use std::path::Path as FsPath;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, Multipart, Path as AxPath, Query, RawQuery, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::api::auth::{header_value, query_token, SharedAuth};
use crate::api::helpers;
use crate::api::http::file_response;
use crate::config;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type SharedState = Arc<AppState>;

const _OP_WAIT: f64 = 20.0;
const _BIG_WAIT: f64 = 120.0;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/nodes/{node_id}/files/roots", get(list_roots))
        .route("/nodes/{node_id}/files", get(list_files))
        .route("/nodes/{node_id}/files/stat", get(stat_file))
        .route("/nodes/{node_id}/files/du", get(du_file))
        .route("/nodes/{node_id}/files/mkdir", post(mkdir))
        .route("/nodes/{node_id}/files/delete", post(delete_file))
        .route("/nodes/{node_id}/files/copy", post(copy_file))
        .route("/nodes/{node_id}/files/move", post(move_file))
        .route("/nodes/{node_id}/files/pull", post(pull_file))
        .route("/nodes/{node_id}/files/push", post(push_file))
        .route("/files/transfers", get(list_transfers))
        .route("/files/transfers/{transfer_id}", get(get_transfer))
        .route(
            "/files/transfers/{transfer_id}/download",
            get(download_transfer),
        )
        .route("/files/transfers/{transfer_id}", delete(cancel_transfer))
}

/// Agent 专用路由：一次性传输令牌，不走 SharedAuth。
pub fn agent_router() -> Router<SharedState> {
    Router::new()
        .route(
            "/files/transfers/{transfer_id}/data",
            get(send_to_agent).post(receive_from_agent),
        )
}

// ------------------------------------------------------------------ 工具

fn require_node(state: &SharedState, node_id: &str) -> ApiResult<()> {
    if state.registry.get(node_id).is_none() {
        return Err(ApiError::not_found(format!("节点不存在：{node_id}")));
    }
    Ok(())
}

async fn node_op(
    state: &SharedState,
    node_id: &str,
    cmd: &str,
    payload: Value,
    wait: f64,
) -> ApiResult<Value> {
    let (delivered, _cmd_id, result) = state
        .manager
        .send_command_result(node_id, cmd, payload, wait)
        .await;
    if !delivered {
        return Err(ApiError::service_unavailable("节点不在线，无法执行文件操作"));
    }
    let result = result.ok_or_else(|| {
        ApiError::gateway_timeout(
            "设备无响应（指令已送达但未在超时内回执，可能路径过大或设备繁忙）",
        )
    })?;
    if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("操作失败")
            .to_string();
        return Err(ApiError::bad_request(err));
    }
    match result.get("result") {
        Some(v) if v.is_object() => Ok(v.clone()),
        Some(v) => Ok(json!({"value": v})),
        None => Ok(json!({"value": Value::Null})),
    }
}

fn agent_base(state: &SharedState, node_id: &str) -> ApiResult<String> {
    let base = state.deploy_queue.get_node_base(node_id);
    if base.is_empty() {
        return Err(ApiError::service_unavailable(
            "还没有记录到该节点连平台时使用的地址，无法下发传输指令（请等下一次心跳后重试）",
        ));
    }
    Ok(base)
}

async fn send_fs_command(state: &SharedState, node_id: &str, cmd: &str, payload: Value) -> ApiResult<Value> {
    let (delivered, cmd_id) = state.manager.send_command(node_id, cmd, payload, 0.0).await;
    if !delivered {
        return Err(ApiError::service_unavailable("节点不在线，无法下发传输指令"));
    }
    Ok(json!({"dispatched": true, "cmd_id": cmd_id}))
}

// ------------------------------------------------------------------ 节点文件操作

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    500
}

async fn list_roots(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    Ok(Json(node_op(&state, &node_id, "fs_roots", json!({}), _OP_WAIT).await?))
}

async fn list_files(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    Ok(Json(
        node_op(
            &state,
            &node_id,
            "fs_list",
            json!({"path": q.path, "offset": q.offset, "limit": q.limit}),
            _OP_WAIT,
        )
        .await?,
    ))
}

#[derive(Deserialize)]
struct StatQuery {
    path: String,
}

async fn stat_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    Query(q): Query<StatQuery>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    match node_op(&state, &node_id, "fs_stat", json!({"path": q.path}), _OP_WAIT).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            if e.status == axum::http::StatusCode::BAD_REQUEST && e.detail.contains("不存在") {
                Err(ApiError::not_found(e.detail))
            } else {
                Err(e)
            }
        }
    }
}

async fn du_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    Query(q): Query<StatQuery>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    Ok(Json(node_op(&state, &node_id, "fs_du", json!({"path": q.path}), _BIG_WAIT).await?))
}

#[derive(Deserialize)]
struct MkdirIn {
    path: String,
    #[serde(default = "default_true")]
    parents: bool,
}
fn default_true() -> bool {
    true
}

async fn mkdir(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<MkdirIn>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    let out = node_op(
        &state,
        &node_id,
        "fs_mkdir",
        json!({"path": body.path, "parents": body.parents}),
        _OP_WAIT,
    )
    .await?;
    crate::audit::record(
        &state.pool,
        "file.mkdir",
        "shared-token",
        "file",
        &body.path,
        &node_id,
        true,
        Some(&json!({"path": body.path})),
        "新建目录",
        &helpers::client_ip(&ci),
    );
    Ok(Json(out))
}

#[derive(Deserialize)]
struct DeleteIn {
    path: String,
    #[serde(default)]
    recursive: bool,
}

async fn delete_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<DeleteIn>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    match node_op(
        &state,
        &node_id,
        "fs_delete",
        json!({"path": body.path, "recursive": body.recursive}),
        _OP_WAIT,
    )
    .await
    {
        Ok(out) => {
            crate::audit::record(
                &state.pool,
                "file.delete",
                "shared-token",
                "file",
                &body.path,
                &node_id,
                true,
                Some(&json!({"path": body.path, "recursive": body.recursive,
                    "removed_entries": out.get("removed_entries")})),
                "删除文件/目录",
                &helpers::client_ip(&ci),
            );
            Ok(Json(out))
        }
        Err(e) => {
            crate::audit::record(
                &state.pool,
                "file.delete",
                "shared-token",
                "file",
                &body.path,
                &node_id,
                false,
                Some(&json!({"path": body.path, "recursive": body.recursive})),
                &e.detail,
                &helpers::client_ip(&ci),
            );
            Err(e)
        }
    }
}

#[derive(Deserialize)]
struct TransferIn {
    src: String,
    dst: String,
    #[serde(default)]
    overwrite: bool,
}

async fn copy_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<TransferIn>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    let out = node_op(
        &state,
        &node_id,
        "fs_copy",
        json!({"src": body.src, "dst": body.dst, "overwrite": body.overwrite}),
        _OP_WAIT,
    )
    .await?;
    crate::audit::record(
        &state.pool,
        "file.copy",
        "shared-token",
        "file",
        &body.dst,
        &node_id,
        true,
        Some(&json!({"src": body.src, "dst": body.dst})),
        "复制",
        &helpers::client_ip(&ci),
    );
    Ok(Json(out))
}

async fn move_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<TransferIn>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    let out = node_op(
        &state,
        &node_id,
        "fs_move",
        json!({"src": body.src, "dst": body.dst, "overwrite": body.overwrite}),
        _OP_WAIT,
    )
    .await?;
    crate::audit::record(
        &state.pool,
        "file.move",
        "shared-token",
        "file",
        &body.dst,
        &node_id,
        true,
        Some(&json!({"src": body.src, "dst": body.dst})),
        "移动/重命名",
        &helpers::client_ip(&ci),
    );
    Ok(Json(out))
}

#[derive(Deserialize)]
struct PathIn {
    path: String,
}

async fn pull_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<PathIn>,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    let info = node_op(&state, &node_id, "fs_stat", json!({"path": body.path}), _OP_WAIT).await?;
    if info.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(ApiError::bad_request(
            "不能直接下载目录：请先选中具体文件，或用「大小统计」了解目录占用",
        ));
    }
    let name = info
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            FsPath::new(&body.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.bin")
                .to_string()
        });
    let size = info.get("size").and_then(|v| v.as_i64()).unwrap_or(0);

    let mut tr = state
        .transfers
        .create(&node_id, "pull", &body.path, "", &name, size, "")
        .ok_or_else(|| ApiError::internal("创建传输会话失败"))?;

    let tid = tr["transfer_id"].as_str().unwrap_or("").to_string();
    let token = tr["token"].as_str().unwrap_or("").to_string();
    let size_val = tr["size"].as_i64().unwrap_or(0);
    let base = agent_base(&state, &node_id)?;
    let url = format!("{base}/api/v1/files/transfers/{tid}/data");
    send_fs_command(
        &state,
        &node_id,
        "fs_fetch",
        json!({"transfer_id": tid, "path": body.path, "url": url, "token": token, "size": size_val}),
    )
    .await?;

    crate::audit::record(
        &state.pool,
        "file.pull",
        "shared-token",
        "file",
        &body.path,
        &node_id,
        true,
        Some(&json!({"path": body.path, "transfer_id": tid})),
        "从设备下载文件",
        &helpers::client_ip(&ci),
    );
    if let Some(obj) = tr.as_object_mut() {
        obj.remove("token");
    }
    Ok(Json(tr))
}

async fn push_file(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(node_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    require_node(&state, &node_id)?;
    let limit = (config::get().files.max_transfer_mb.max(1)) * 1024 * 1024;
    let tid = crate::services::file_transfers::TransferService::new_id();
    let staging = state.transfers.staging_dir(&tid);
    let tmp = staging.join("__upload.tmp");

    let mut path_field = String::new();
    let mut overwrite = false;
    let mut hasher = Sha256::new();
    let mut total: i64 = 0;
    let mut has_file = false;

    let mut out = tokio::fs::File::create(&tmp)
        .await
        .map_err(|e| ApiError::internal(format!("创建暂存文件失败：{e}")))?;
    use tokio::io::AsyncWriteExt;

    while let Some(field) = mp.next_field().await.map_err(|e| ApiError::bad_request(e.to_string()))? {
        let fname = field.name().unwrap_or("").to_string();
        match fname.as_str() {
            "path" => {
                path_field = field.text().await.unwrap_or_default();
            }
            "overwrite" => {
                overwrite = field.text().await.unwrap_or_default() == "true";
            }
            "file" => {
                has_file = true;
                let mut field = field;
                while let Some(chunk) = field.chunk().await.map_err(|e| ApiError::bad_request(e.to_string()))? {
                    if chunk.is_empty() {
                        continue;
                    }
                    total += chunk.len() as i64;
                    if total > limit {
                        let _ = tokio::fs::remove_file(&tmp).await;
                        state.transfers.discard_staging(&tid);
                        return Err(ApiError::payload_too_large(format!(
                            "文件超过单次传输上限（{} MB）",
                            config::get().files.max_transfer_mb
                        )));
                    }
                    hasher.update(&chunk);
                    out.write_all(&chunk)
                        .await
                        .map_err(|e| ApiError::internal(format!("写入暂存文件失败：{e}")))?;
                }
            }
            _ => {
                let _ = field.text().await;
            }
        }
    }
    out.flush().await.map_err(|e| ApiError::internal(format!("写入暂存文件失败：{e}")))?;
    drop(out);

    if !has_file {
        let _ = tokio::fs::remove_file(&tmp).await;
        state.transfers.discard_staging(&tid);
        return Err(ApiError::bad_request("缺少上传文件（file 字段）"));
    }
    if path_field.trim().is_empty() {
        let _ = tokio::fs::remove_file(&tmp).await;
        state.transfers.discard_staging(&tid);
        return Err(ApiError::bad_request("缺少目标路径（path 字段）"));
    }

    let file_name = FsPath::new(&path_field)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("file.bin")
        .to_string();
    let final_path = staging.join(&file_name);
    tokio::fs::rename(&tmp, &final_path)
        .await
        .map_err(|e| ApiError::internal(format!("暂存文件落位失败：{e}")))?;

    let sha = hex::encode(hasher.finalize());
    let mut tr = state
        .transfers
        .create(&node_id, "push", &path_field, &tid, &file_name, total, &sha)
        .ok_or_else(|| ApiError::internal("创建传输会话失败"))?;

    let tid = tr["transfer_id"].as_str().unwrap_or("").to_string();
    let token = tr["token"].as_str().unwrap_or("").to_string();
    let size_val = tr["size"].as_i64().unwrap_or(0);
    let base = agent_base(&state, &node_id)?;
    let url = format!("{base}/api/v1/files/transfers/{tid}/data");
    send_fs_command(
        &state,
        &node_id,
        "fs_push",
        json!({
            "transfer_id": tid, "path": path_field, "url": url, "token": token,
            "size": size_val, "sha256": sha, "overwrite": overwrite,
        }),
    )
    .await?;

    crate::audit::record(
        &state.pool,
        "file.push",
        "shared-token",
        "file",
        &path_field,
        &node_id,
        true,
        Some(&json!({"path": path_field, "bytes": total, "sha256": sha, "transfer_id": tid})),
        "向设备上传文件",
        &helpers::client_ip(&ci),
    );
    if let Some(obj) = tr.as_object_mut() {
        obj.remove("token");
    }
    Ok(Json(tr))
}

// ------------------------------------------------------------------ 会话查询

#[derive(Deserialize)]
struct TransfersQuery {
    #[serde(default)]
    node_id: String,
    #[serde(default = "default_transfer_limit")]
    limit: i64,
}
fn default_transfer_limit() -> i64 {
    50
}

async fn list_transfers(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<TransfersQuery>,
) -> Json<Value> {
    state.transfers.expire_stale();
    let items = state.transfers.list(&q.node_id, q.limit);
    let total = items.len();
    Json(json!({"items": items, "total": total}))
}

async fn get_transfer(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(transfer_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    state
        .transfers
        .get(&transfer_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("传输记录不存在"))
}

async fn download_transfer(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(transfer_id): AxPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let item = state
        .transfers
        .get(&transfer_id)
        .ok_or_else(|| ApiError::not_found("传输记录不存在"))?;
    if item["direction"].as_str() != Some("pull") {
        return Err(ApiError::bad_request("这是上传方向的传输，没有可下载的文件"));
    }
    let status = item["status"].as_str().unwrap_or("");
    if status != "done" {
        return Err(ApiError::conflict(format!(
            "文件尚未就绪（当前状态：{status}）"
        )));
    }
    let path = state
        .transfers
        .local_file(&transfer_id)
        .ok_or_else(|| ApiError::not_found("暂存文件已不存在（可能已过期清理）"))?;
    let filename = item["file_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.bin")
                .to_string()
        });
    let sha = item["sha256"].as_str().unwrap_or("").to_string();
    let range = headers.get("range").and_then(|v| v.to_str().ok());
    let if_range = headers.get("if-range").and_then(|v| v.to_str().ok());
    file_response(
        &path,
        &filename,
        "application/octet-stream",
        &[("x-file-sha256", sha)],
        range,
        if_range,
    )
    .await
}

async fn cancel_transfer(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(transfer_id): AxPath<String>,
    ci: ConnectInfo<std::net::SocketAddr>,
) -> ApiResult<Json<Value>> {
    let item = state
        .transfers
        .get(&transfer_id)
        .ok_or_else(|| ApiError::not_found("传输记录不存在"))?;
    let status = item["status"].as_str().unwrap_or("");
    let node_id = item["node_id"].as_str().unwrap_or("").to_string();
    let direction = item["direction"].as_str().unwrap_or("").to_string();
    let path = item["path"].as_str().unwrap_or("").to_string();

    // 终态记录：前端「删除」是清库（含暂存文件），不是「取消」；进行中的才是取消。
    let final_status = if matches!(status, "done" | "failed" | "cancelled" | "expired") {
        if state.transfers.delete_record(&transfer_id) {
            "deleted"
        } else {
            // 并发下记录已不在，仍按成功响应，前端刷新后自然消失。
            "deleted"
        }
    } else {
        state.transfers.mark(&transfer_id, "cancelled", "用户取消", None, "");
        state.transfers.discard_staging(&transfer_id);
        "cancelled"
    };

    crate::audit::record(
        &state.pool,
        "file.cancel",
        "shared-token",
        "transfer",
        &transfer_id,
        &node_id,
        true,
        Some(&json!({"direction": direction, "path": path})),
        "删除/取消文件传输",
        &helpers::client_ip(&ci),
    );
    Ok(Json(json!({"transfer_id": transfer_id, "status": final_status})))
}

// ------------------------------------------------------------------ Agent 专用（传输令牌）

fn authorize_transfer(
    state: &SharedState,
    transfer_id: &str,
    headers: &HeaderMap,
    query: Option<&str>,
) -> ApiResult<Value> {
    let token = header_value(headers, "x-transfer-token").or_else(|| query_token(query));
    let token = token.unwrap_or_default();
    state
        .transfers
        .authorize(transfer_id, &token)
        .ok_or_else(|| ApiError::unauthorized("传输令牌无效、已过期或会话已结束"))
}

async fn receive_from_agent(
    State(state): State<SharedState>,
    AxPath(transfer_id): AxPath<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> ApiResult<Json<Value>> {
    let info = authorize_transfer(&state, &transfer_id, &headers, query.as_deref())?;
    if info["direction"].as_str() != Some("pull") {
        return Err(ApiError::bad_request("该会话是上传方向，不接受 Agent 推送"));
    }
    let staging_path = info["staging_path"].as_str().unwrap_or("").to_string();
    let dest = FsPath::new(&staging_path);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let limit = (config::get().files.max_transfer_mb.max(1)) * 1024 * 1024;

    // 续传偏移：agent 在 POST 前先用 GET 探测平台已收字节数，这里据它继续追加。
    let offset: u64 = headers
        .get("x-upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    state.transfers.mark(&transfer_id, "transferring", "", None, "");
    match stream_body_to_file(body, dest, limit, offset).await {
        Ok((total, sha)) => {
            if total <= 0 {
                let _ = std::fs::remove_file(dest);
                state.transfers.mark(&transfer_id, "failed", "收到的文件是空的", None, "");
                return Err(ApiError::bad_request("收到的文件是空的"));
            }
            state
                .transfers
                .mark(&transfer_id, "done", "", Some(total), &sha);
            Ok(Json(json!({"ok": true, "bytes": total, "sha256": sha})))
        }
        Err(e) => {
            // 关键：失败时**保留残片**，并回退到 pending 让 agent 能探测续传，
            // 而不是删掉文件、判死 —— 那正是"传一半断网就前功尽弃"的根因。
            state.transfers.mark(&transfer_id, "pending", &e.detail, None, "");
            Err(e)
        }
    }
}

async fn stream_body_to_file(
    body: Body,
    dest: &FsPath,
    limit: i64,
    offset: u64,
) -> ApiResult<(i64, String)> {
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut hasher = Sha256::new();
    let mut total: i64 = offset as i64;

    // 续传：残片大小正好等于偏移时，先把残片哈希进去，再从偏移处追加。
    // 对不上（上一轮平台多收/少收）就丢弃残片、从零重传。
    let existing = tokio::fs::metadata(dest).await.map(|m| m.len()).unwrap_or(0);
    if offset > 0 && existing == offset {
        let mut f = tokio::fs::File::open(dest)
            .await
            .map_err(|e| ApiError::internal(format!("打开残片失败：{e}")))?;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = f.read(&mut buf)
                .await
                .map_err(|e| ApiError::internal(format!("读取残片失败：{e}")))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    } else if offset > 0 {
        total = 0;
        let _ = tokio::fs::remove_file(dest).await;
    }

    let mut out = if offset > 0 && existing == offset {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(dest)
            .await
            .map_err(|e| ApiError::internal(format!("打开文件追加失败：{e}")))?
    } else {
        tokio::fs::File::create(dest)
            .await
            .map_err(|e| ApiError::internal(format!("创建文件失败：{e}")))?
    };

    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::internal(format!("读取上传数据失败：{e}")))?;
        if chunk.is_empty() {
            continue;
        }
        total += chunk.len() as i64;
        if total > limit {
            return Err(ApiError::payload_too_large(format!(
                "文件超过单次传输上限（{} MB）",
                config::get().files.max_transfer_mb
            )));
        }
        hasher.update(&chunk);
        out.write_all(&chunk)
            .await
            .map_err(|e| ApiError::internal(format!("写入文件失败：{e}")))?;
    }
    out.flush().await.map_err(|e| ApiError::internal(format!("写入文件失败：{e}")))?;
    Ok((total, hex::encode(hasher.finalize())))
}

async fn send_to_agent(
    State(state): State<SharedState>,
    AxPath(transfer_id): AxPath<String>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let info = authorize_transfer(&state, &transfer_id, &headers, query.as_deref())?;
    let direction = info["direction"].as_str().unwrap_or("");
    // pull 方向的 GET 是「续传探测」：告诉 agent 平台已收到多少字节，好从断点继续上传。
    if direction == "pull" {
        let staging_path = info["staging_path"].as_str().unwrap_or("").to_string();
        let offset = std::fs::metadata(&staging_path).map(|m| m.len()).unwrap_or(0);
        return Ok(Json(json!({"offset": offset})).into_response());
    }
    if direction != "push" {
        return Err(ApiError::bad_request("该会话是下载方向，不接受 Agent 拉取"));
    }
    let staging_path = info["staging_path"].as_str().unwrap_or("").to_string();
    let path = FsPath::new(&staging_path);
    if !path.is_file() {
        state.transfers.mark(&transfer_id, "failed", "平台暂存文件缺失", None, "");
        return Err(ApiError::not_found("平台暂存文件已不存在"));
    }
    state.transfers.mark(&transfer_id, "transferring", "", None, "");
    let filename = info["file_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.bin")
                .to_string()
        });
    let sha = info["sha256"].as_str().unwrap_or("").to_string();
    let range = headers.get("range").and_then(|v| v.to_str().ok());
    let if_range = headers.get("if-range").and_then(|v| v.to_str().ok());
    file_response(
        &path,
        &filename,
        "application/octet-stream",
        &[("x-file-sha256", sha)],
        range,
        if_range,
    )
    .await
}
