//! 系统接口：健康检查、运行信息、首页大屏聚合、指标分层、备份恢复、平台日志。
//! 与 Python api/routes_system.py 对齐。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Multipart, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::auth::SharedAuth;
use crate::api::http::file_response;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

const VERSION: &str = "0.5.0-m5";

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/system/health", get(health))
        .route("/system/info", get(info))
        .route("/system/overview", get(overview))
        .route("/system/cluster-metrics", get(cluster_metrics))
        .route("/system/metric-tiers", get(metric_tiers))
        .route("/system/metric-tiers/run", post(metric_tiers_run))
        .route("/system/backup/status", get(backup_status))
        .route("/system/backup", get(download_backup))
        .route("/system/backup/inspect", post(inspect_backup))
        .route("/system/backup/restore", post(restore_backup))
        .route("/system/server-logs", get(server_logs))
}

/// 存活探针（无需鉴权）。
async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "server_time": crate::time::format_iso(&crate::time::now()),
    }))
}

async fn info(_auth: SharedAuth, State(state): State<SharedState>) -> Json<Value> {
    let stats = state.registry.stats();
    let now_db = crate::time::now_db();
    let uptime = crate::time::db_elapsed_secs(&now_db, &state.started_at)
        .map(|s| s as i64)
        .unwrap_or(0);
    Json(json!({
        "version": VERSION,
        "server_time": crate::time::db_to_iso(&now_db),
        "started_at": crate::time::db_to_iso(&state.started_at),
        "uptime_seconds": uptime,
        "connected_nodes": state.manager.count().await,
        "node_stats": {
            "total": stats.get("total").copied().unwrap_or(0),
            "online": stats.get("online").copied().unwrap_or(0),
            "offline": stats.get("offline").copied().unwrap_or(0),
            "never_seen": stats.get("never_seen").copied().unwrap_or(0),
        },
        "heartbeat_interval": state.config.server.heartbeat_interval,
        "offline_threshold": state.config.server.offline_threshold,
        "discovery_port": state.config.server.discovery_port,
    }))
}

async fn overview(_auth: SharedAuth, State(state): State<SharedState>) -> Json<Value> {
    Json(crate::services::overview::build_overview(state.registry.as_ref()))
}

#[derive(Deserialize)]
struct ClusterMetricsQuery {
    #[serde(default = "default_minutes")]
    minutes: i64,
}
fn default_minutes() -> i64 {
    30
}

async fn cluster_metrics(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<ClusterMetricsQuery>,
) -> ApiResult<Json<Value>> {
    let minutes = q.minutes.clamp(1, 60 * 24 * 365);
    Ok(Json(crate::services::overview::build_cluster_series(
        state.registry.as_ref(),
        minutes,
    )))
}

async fn metric_tiers(_auth: SharedAuth, State(state): State<SharedState>) -> Json<Value> {
    Json(crate::services::metric_rollup::tier_status(&state.pool))
}

#[derive(Deserialize)]
struct FullQuery {
    #[serde(default)]
    full: bool,
}

async fn metric_tiers_run(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<FullQuery>,
) -> Json<Value> {
    Json(crate::services::metric_rollup::run_once(&state.pool, Some(q.full)))
}

async fn backup_status(_auth: SharedAuth, State(_state): State<SharedState>) -> Json<Value> {
    Json(crate::services::backup::backup_status())
}

#[derive(Deserialize)]
struct IncludeModelsQuery {
    #[serde(default)]
    include_models: bool,
}

async fn download_backup(
    _auth: SharedAuth,
    State(_state): State<SharedState>,
    Query(q): Query<IncludeModelsQuery>,
) -> ApiResult<impl IntoResponse> {
    let path = crate::services::backup::create_backup(q.include_models, None)
        .map_err(|e| ApiError::internal(format!("备份失败：{e}")))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "backup.zip".to_string());
    let resp = file_response(
        &path,
        &name,
        "application/zip",
        &[("X-Backup-File", name.clone())],
        None,
        None,
    )
    .await?;
    Ok(resp)
}

/// 把 multipart 里的 `file` 字段流式落盘到临时文件，返回 (路径, 原始文件名)。
async fn save_upload(mut mp: Multipart) -> ApiResult<(PathBuf, String)> {
    let dir = std::env::temp_dir().join(format!("edge-upload-{}", crate::util::new_uuid_hex()));
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(format!("创建临时目录失败：{e}")))?;
    let mut saved: Option<(PathBuf, String)> = None;
    while let Ok(Some(field)) = mp.next_field().await {
        if field.name() != Some("file") {
            continue;
        }
        let fname = field.file_name().unwrap_or("backup.zip").to_string();
        let safe = PathBuf::from(&fname)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "backup.zip".to_string());
        let dest = dir.join(safe);
        let mut out = tokio::fs::File::create(&dest)
            .await
            .map_err(|e| ApiError::internal(format!("创建临时文件失败：{e}")))?;
        let mut stream = field;
        use axum::body::Bytes;
        use tokio::io::AsyncWriteExt;
        while let Ok(Some(chunk)) = stream.chunk().await {
            out.write_all(&chunk)
                .await
                .map_err(|e| ApiError::internal(format!("写入临时文件失败：{e}")))?;
        }
        out.flush().await.ok();
        let _ = Bytes::default(); // 保留 bytes 依赖引用，避免未使用警告
        saved = Some((dest, fname));
        break;
    }
    match saved {
        Some(s) => Ok(s),
        None => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(ApiError::bad_request("缺少上传文件（file 字段）"))
        }
    }
}

async fn inspect_backup(
    _auth: SharedAuth,
    State(_state): State<SharedState>,
    mp: Multipart,
) -> ApiResult<Json<Value>> {
    let (path, _fname) = save_upload(mp).await?;
    let result = crate::services::backup::inspect_backup(&path);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
    Ok(Json(result))
}

#[derive(Deserialize)]
struct ConfirmQuery {
    #[serde(default)]
    confirm: bool,
}

async fn restore_backup(
    _auth: SharedAuth,
    State(_state): State<SharedState>,
    Query(q): Query<ConfirmQuery>,
    mp: Multipart,
) -> ApiResult<Json<Value>> {
    let (path, _fname) = save_upload(mp).await?;
    let result = crate::services::backup::restore_backup(&path, q.confirm);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
    // Python 版：非 dry_run 且失败时返回 400
    if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
        && !result.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false)
    {
        let msg = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("恢复失败")
            .to_string();
        return Err(ApiError::bad_request(msg));
    }
    Ok(Json(result))
}

#[derive(Deserialize)]
struct LogsQuery {
    #[serde(default = "default_lines")]
    lines: usize,
}
fn default_lines() -> usize {
    200
}

async fn server_logs(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    Query(q): Query<LogsQuery>,
) -> Json<Value> {
    let lines = q.lines.clamp(1, 5000);
    let log_file = state.config.logs_path().join("server.log");
    let path_str = log_file.to_string_lossy().to_string();
    if !log_file.exists() {
        return Json(json!({"lines": [], "total_lines": 0, "log_file": path_str}));
    }
    match tail(&log_file, lines) {
        Ok(content) => Json(json!({
            "lines": content,
            "total_lines": content.len(),
            "log_file": path_str,
        })),
        Err(e) => Json(json!({
            "lines": [format!("读取日志失败: {e}")],
            "total_lines": 0,
            "log_file": path_str,
        })),
    }
}

fn tail(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let size = f.seek(SeekFrom::End(0))?;
    let mut pos = size;
    let mut data: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 8192];
    while pos > 0 && data.iter().filter(|&&b| b == b'\n').count() <= n {
        let step = 8192u64.min(pos);
        pos -= step;
        f.seek(SeekFrom::Start(pos))?;
        let mut read = 0;
        while read < step as usize {
            let take = (step as usize - read).min(buf.len());
            let r = f.read(&mut buf[..take])?;
            if r == 0 {
                break;
            }
            read += r;
            let mut chunk = buf[..r].to_vec();
            chunk.append(&mut data);
            data = chunk;
        }
    }
    let text = String::from_utf8_lossy(&data).to_string();
    let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].to_vec())
}
