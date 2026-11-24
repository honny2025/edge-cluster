//! 模型库接口（里程碑 3）。与 Python api/routes_models.py 对齐。
//!
//! 两条路由的鉴权域不同：
//! - 控制台接口走共享 token（`SharedAuth`）；
//! - 下载口接受「共享 token 或该模型的一次性下载令牌」二者其一（自定义校验）。
//! 与 Python 版把下载口拆成 `download_router` 是同一个原因：Agent 拿的是
//! 一次性下载令牌，不能挂在只认共享 token 的依赖上。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Multipart, Path as AxPath, RawQuery, State};
use axum::http::header::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::api::auth::{candidate_token, header_value, SharedAuth};
use crate::api::helpers::{self, ModelRow};
use crate::api::http::file_response;
use crate::error::{ApiError, ApiResult};
use crate::services::compat::check_compatibility;
use crate::state::AppState;

type SharedState = Arc<AppState>;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/models", get(list_models).post(upload_model))
        .route("/models/{model_id}", get(get_model).delete(delete_model).patch(patch_model))
        .route("/models/{model_id}/compat", post(check_model_compat))
        .route("/models/{model_id}/manifest", post(upload_manifest))
        .route("/models/{model_id}/manifest/{name}", get(download_manifest_asset).delete(delete_manifest_item))
        .route("/models/{model_id}/download", get(download_model))
}

// ------------------------------------------------------------------ JSON 表单解析

fn parse_json_list(raw: &str) -> ApiResult<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(json!([]));
    }
    let val: Value = serde_json::from_str(raw)
        .map_err(|e| ApiError::bad_request(format!("JSON 数组格式错误: {e}")))?;
    if let Some(s) = val.as_str() {
        // 允许直接写 "htp,cpu"
        let items: Vec<Value> = s
            .split(',')
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .map(|x| json!(x))
            .collect();
        return Ok(Value::Array(items));
    }
    if !val.is_array() {
        return Err(ApiError::bad_request("需要一个 JSON 数组，例如 [\"htp\",\"cpu\"]"));
    }
    Ok(val)
}

fn parse_json_dict(raw: &str) -> ApiResult<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(json!({}));
    }
    let val: Value = serde_json::from_str(raw)
        .map_err(|e| ApiError::bad_request(format!("JSON 对象格式错误: {e}")))?;
    if !val.is_object() {
        return Err(ApiError::bad_request("需要一个 JSON 对象，例如 {\"n_ctx\":2048}"));
    }
    Ok(val)
}

// ------------------------------------------------------------------ 行读写

fn find_model(state: &SharedState, model_id: &str) -> ApiResult<ModelRow> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    helpers::read_model(&conn, model_id).ok_or_else(|| ApiError::not_found(format!("模型不存在: {model_id}")))
}

fn assets_dir(state: &SharedState, model_id: &str) -> std::path::PathBuf {
    state.config.models_path().join(format!("{model_id}-assets"))
}

fn safe_asset_name(name: &str) -> ApiResult<String> {
    let safe = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if safe.is_empty() || safe == "." || safe == ".." {
        return Err(ApiError::bad_request("附属物文件名非法"));
    }
    Ok(safe)
}

// ------------------------------------------------------------------ 列表 / 详情

async fn list_models(_auth: SharedAuth, State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let items = helpers::list_models(&conn);
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

async fn get_model(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(model_id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(find_model(&state, &model_id)?.out()))
}

// ------------------------------------------------------------------ 上传

async fn upload_model(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    ci: ConnectInfo<SocketAddr>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    let model_id = crate::util::new_uuid_hex();
    let models_dir = state.config.models_path();
    std::fs::create_dir_all(&models_dir)
        .map_err(|e| ApiError::internal(format!("创建模型目录失败：{e}")))?;

    let mut text: HashMap<String, String> = HashMap::new();
    let mut file_info: Option<(String, u64, String)> = None;

    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("解析表单失败：{e}")))?
    {
        let fname = field.name().unwrap_or("").to_string();
        if let Some(orig) = field.file_name() {
            let safe = Path::new(orig)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "model.bin".to_string());
            let dest = models_dir.join(format!("{model_id}-{safe}"));
            let mut out = tokio::fs::File::create(&dest)
                .await
                .map_err(|e| ApiError::internal(format!("创建模型文件失败：{e}")))?;
            let mut hasher = Sha256::new();
            let mut size = 0u64;
            // 写文件阶段一旦出错（网络抖动/超时把 multipart 掐断、磁盘写失败），
            // 立刻删掉半截文件 —— 否则一次上传失败就在 models 目录里留一个几 GB 的孤儿。
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        if let Err(e) = out.write_all(&chunk).await {
                            let _ = tokio::fs::remove_file(&dest).await;
                            return Err(ApiError::internal(format!("写入模型文件失败：{e}")));
                        }
                        hasher.update(&chunk);
                        size += chunk.len() as u64;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&dest).await;
                        return Err(ApiError::internal(format!("读取上传流失败：{e}")));
                    }
                }
            }
            out.flush().await.ok();
            file_info = Some((safe, size, hex::encode(hasher.finalize())));
        } else {
            let val = field
                .text()
                .await
                .map_err(|e| ApiError::bad_request(format!("读取表单字段失败：{e}")))?;
            text.insert(fname, val);
        }
    }

    let (safe_name, file_size, sha256) =
        file_info.ok_or_else(|| ApiError::bad_request("缺少上传文件（file 字段）"))?;
    // 文件已落到磁盘。后续任何校验失败都要把它删掉，别留孤儿文件。
    let saved_file = models_dir.join(format!("{model_id}-{safe_name}"));

    let name = text.get("name").map(|s| s.trim().to_string()).unwrap_or_default();
    if name.is_empty() {
        let _ = tokio::fs::remove_file(&saved_file).await;
        return Err(ApiError::bad_request("name 不能为空"));
    }
    let version = {
        let v = text.get("version").map(|s| s.trim().to_string()).unwrap_or_default();
        if v.is_empty() { "v1".to_string() } else { v }
    };

    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;

    // 同名同版本冲突先查（唯一约束是最后一道防线）。
    let dup: Option<String> = conn
        .query_row(
            "SELECT model_id FROM models WHERE name = ?1 AND version = ?2",
            rusqlite::params![name, version],
            |r| r.get(0),
        )
        .ok();
    if let Some(dup_id) = dup {
        let _ = tokio::fs::remove_file(&saved_file).await;
        return Err(ApiError::conflict(format!(
            "同名同版本已存在：{name} {version}（model_id={dup_id}）"
        )));
    }

    let framework = text
        .get("framework")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "other".to_string());
    let format = text.get("format").map(|s| s.trim().to_string()).unwrap_or_default();
    let description = text.get("description").map(|s| s.to_string()).unwrap_or_default();
    let quantization = text.get("quantization").map(|s| s.trim().to_string()).unwrap_or_default();
    let target_soc = text.get("target_soc").map(|s| s.trim().to_string()).unwrap_or_default();
    let qnn_version = text.get("qnn_version").map(|s| s.trim().to_string()).unwrap_or_default();
    let backends = parse_json_list(text.get("backends").map(|s| s.as_str()).unwrap_or(""))?;
    let deps = parse_json_list(text.get("deps").map(|s| s.as_str()).unwrap_or(""))?;
    let run_params = parse_json_dict(text.get("run_params").map(|s| s.as_str()).unwrap_or(""))?;
    let self_test_command = text
        .get("self_test_command")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let self_test_timeout = text
        .get("self_test_timeout")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(60)
        .max(1);

    let storage_path = format!("{model_id}-{safe_name}");
    let now = crate::time::now_db();

    let res = conn.execute(
        "INSERT INTO models (model_id, name, version, file_name, file_size, framework, format, \
         sha256, quantization, target_soc, qnn_version, backends, deps, run_params, \
         self_test_command, self_test_timeout, description, uploaded_at, storage_path, download_count) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,0)",
        rusqlite::params![
            model_id,
            name,
            version,
            safe_name,
            file_size as i64,
            framework,
            format,
            sha256,
            quantization,
            target_soc,
            qnn_version,
            serde_json::to_string(&backends).unwrap_or_else(|_| "[]".to_string()),
            serde_json::to_string(&deps).unwrap_or_else(|_| "[]".to_string()),
            serde_json::to_string(&run_params).unwrap_or_else(|_| "{}".to_string()),
            self_test_command,
            self_test_timeout,
            description,
            now,
            storage_path,
        ],
    );
    if let Err(e) = res {
        let _ = std::fs::remove_file(models_dir.join(format!("{model_id}-{safe_name}")));
        if e.to_string().contains("UNIQUE") {
            return Err(ApiError::conflict(format!("同名同版本已存在：同名 {version}")));
        }
        return Err(ApiError::internal(format!("模型入库失败：{e}")));
    }

    let model = helpers::read_model(&conn, &model_id)
        .ok_or_else(|| ApiError::internal("模型入库后读取失败"))?;
    crate::audit::record(
        &state.pool,
        "model.upload",
        "shared-token",
        "model",
        &model_id,
        "",
        true,
        Some(&json!({"name": model.name, "size": model.file_size})),
        "上传模型",
        &helpers::client_ip(&ci),
    );
    Ok(Json(model.out()))
}

// ------------------------------------------------------------------ 删除 / 更新

async fn delete_model(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(model_id): AxPath<String>,
    ci: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let model = find_model(&state, &model_id)?;
    let path = state.config.models_path().join(&model.storage_path);
    let mut removed = false;
    if path.is_file() {
        removed = std::fs::remove_file(&path).is_ok();
    }
    let mut assets_removed = 0usize;
    let adir = assets_dir(&state, &model_id);
    if adir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&adir) {
            for f in entries.flatten() {
                if f.path().is_file() && std::fs::remove_file(f.path()).is_ok() {
                    assets_removed += 1;
                }
            }
        }
        let _ = std::fs::remove_dir(&adir);
    }
    state.download_tokens.revoke_model(&model_id);

    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    conn.execute("DELETE FROM models WHERE model_id = ?1", [&model_id])
        .map_err(|e| ApiError::internal(format!("删除模型失败：{e}")))?;

    crate::audit::record(
        &state.pool,
        "model.delete",
        "shared-token",
        "model",
        &model_id,
        "",
        true,
        Some(&json!({"name": model.name, "file_removed": removed, "assets_removed": assets_removed})),
        "删除模型",
        &helpers::client_ip(&ci),
    );
    Ok(Json(json!({
        "ok": true,
        "model_id": model_id,
        "file_removed": removed,
        "assets_removed": assets_removed,
    })))
}

#[derive(Deserialize)]
struct ModelPatch {
    name: Option<String>,
    version: Option<String>,
    framework: Option<String>,
    format: Option<String>,
    description: Option<String>,
    quantization: Option<String>,
    target_soc: Option<String>,
    qnn_version: Option<String>,
    backends: Option<Vec<Value>>,
    deps: Option<Vec<Value>>,
    run_params: Option<std::collections::HashMap<String, Value>>,
    self_test_command: Option<String>,
    self_test_timeout: Option<i64>,
}

async fn patch_model(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(model_id): AxPath<String>,
    ci: ConnectInfo<SocketAddr>,
    Json(body): Json<ModelPatch>,
) -> ApiResult<Json<Value>> {
    find_model(&state, &model_id)?;
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;

    let mut changed: Vec<String> = Vec::new();
    macro_rules! apply {
        ($col:literal, $val:expr) => {{
            conn.execute(
                concat!("UPDATE models SET ", $col, " = ?1 WHERE model_id = ?2"),
                rusqlite::params![$val, &model_id],
            )
            .map_err(|e| ApiError::internal(format!("更新模型失败：{e}")))?;
            changed.push($col.to_string());
        }};
    }

    if let Some(v) = body.name { apply!("name", v.trim().to_string()); }
    if let Some(v) = body.version { apply!("version", v.trim().to_string()); }
    if let Some(v) = body.framework { apply!("framework", v.trim().to_string()); }
    if let Some(v) = body.format { apply!("format", v.trim().to_string()); }
    if let Some(v) = body.description { apply!("description", v); }
    if let Some(v) = body.quantization { apply!("quantization", v.trim().to_string()); }
    if let Some(v) = body.target_soc { apply!("target_soc", v.trim().to_string()); }
    if let Some(v) = body.qnn_version { apply!("qnn_version", v.trim().to_string()); }
    if let Some(v) = body.backends { apply!("backends", serde_json::to_string(&v).unwrap_or_default()); }
    if let Some(v) = body.deps { apply!("deps", serde_json::to_string(&v).unwrap_or_default()); }
    if let Some(v) = body.run_params { apply!("run_params", serde_json::to_string(&v).unwrap_or_default()); }
    if let Some(v) = body.self_test_command { apply!("self_test_command", v.trim().to_string()); }
    if let Some(v) = body.self_test_timeout { apply!("self_test_timeout", v.max(1)); }

    changed.sort();
    crate::audit::record(
        &state.pool,
        "model.update",
        "shared-token",
        "model",
        &model_id,
        "",
        true,
        Some(&json!({"fields": changed})),
        "更新模型登记信息",
        &helpers::client_ip(&ci),
    );

    let model = find_model(&state, &model_id)?;
    Ok(Json(model.out()))
}

// ------------------------------------------------------------------ 兼容性预检

#[derive(Deserialize)]
struct CompatBody {
    node_ids: Vec<String>,
}

async fn check_model_compat(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(model_id): AxPath<String>,
    Json(body): Json<CompatBody>,
) -> ApiResult<Json<Value>> {
    let model = find_model(&state, &model_id)?;
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let compat_model = model.compat();

    let mut results = Vec::new();
    for node_id in &body.node_ids {
        match helpers::read_compat_node(&conn, node_id) {
            None => results.push(json!({
                "node_id": node_id,
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
    Ok(Json(json!({ "model_id": model_id, "results": results })))
}

// ------------------------------------------------------------------ 附属物 Manifest

async fn upload_manifest(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath(model_id): AxPath<String>,
    ci: ConnectInfo<SocketAddr>,
    mut mp: Multipart,
) -> ApiResult<Json<Value>> {
    let model = find_model(&state, &model_id)?;
    let mut kind = "other".to_string();

    // 先收集 kind 文本字段 + 逐个落盘文件，再统一写 manifest。
    let adir = assets_dir(&state, &model_id);
    std::fs::create_dir_all(&adir).map_err(|e| ApiError::internal(format!("创建附属物目录失败：{e}")))?;

    let mut staged: Vec<(String, String, i64, String)> = Vec::new(); // (safe, kind, size, sha256)
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("解析表单失败：{e}")))?
    {
        let fname = field.name().unwrap_or("").to_string();
        if fname == "kind" && field.file_name().is_none() {
            kind = field.text().await.unwrap_or_default();
            continue;
        }
        if let Some(orig) = field.file_name() {
            let safe = safe_asset_name(orig)?;
            let dest = adir.join(&safe);
            let mut out = tokio::fs::File::create(&dest)
                .await
                .map_err(|e| ApiError::internal(format!("创建附属物文件失败：{e}")))?;
            let mut hasher = Sha256::new();
            let mut size = 0i64;
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| ApiError::internal(format!("读取附属物流失败：{e}")))?
            {
                out.write_all(&chunk).await.map_err(|e| ApiError::internal(format!("写入附属物失败：{e}")))?;
                hasher.update(&chunk);
                size += chunk.len() as i64;
            }
            out.flush().await.ok();
            staged.push((safe, String::new(), size, hex::encode(hasher.finalize())));
        }
    }

    let kind = kind.trim();
    let kind = if kind.is_empty() { "other".to_string() } else { kind.to_string() };
    if !matches!(kind.as_str(), "model" | "context" | "dlc" | "lib" | "config" | "other") {
        return Err(ApiError::bad_request(format!("未知的附属物类型: {kind}")));
    }

    // 读现有 manifest，逐条覆盖同名，保留 storage 内部字段。
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    let mut manifest: Vec<Value> = model
        .manifest
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|it| it.is_object())
        .collect();

    let mut added: Vec<Value> = Vec::new();
    for (safe, _k, size, sha) in &staged {
        let entry = json!({
            "name": safe,
            "kind": kind,
            "size": size,
            "sha256": sha,
            "storage": safe,
        });
        manifest.retain(|it| it.get("name").and_then(|v| v.as_str()) != Some(safe.as_str()));
        manifest.push(entry.clone());
        added.push(json!({
            "name": safe,
            "kind": kind,
            "size": size,
            "sha256": sha,
        }));
    }

    conn.execute(
        "UPDATE models SET manifest = ?1 WHERE model_id = ?2",
        rusqlite::params![serde_json::to_string(&manifest).unwrap_or_else(|_| "[]".to_string()), model_id],
    )
    .map_err(|e| ApiError::internal(format!("更新 manifest 失败：{e}")))?;

    let added_names: Vec<String> = added.iter().filter_map(|a| a["name"].as_str().map(String::from)).collect();
    crate::audit::record(
        &state.pool,
        "model.manifest.upload",
        "shared-token",
        "model",
        &model_id,
        "",
        true,
        Some(&json!({"files": added_names, "kind": kind})),
        "上传附属物",
        &helpers::client_ip(&ci),
    );

    let manifest_out: Vec<Value> = manifest
        .iter()
        .filter_map(|it| {
            it.as_object().map(|o| {
                let mut m = serde_json::Map::new();
                for (k, v) in o {
                    if k != "storage" {
                        m.insert(k.clone(), v.clone());
                    }
                }
                Value::Object(m)
            })
        })
        .collect();

    Ok(Json(json!({ "model_id": model_id, "added": added, "manifest": manifest_out })))
}

async fn delete_manifest_item(
    _auth: SharedAuth,
    State(state): State<SharedState>,
    AxPath((model_id, name)): AxPath<(String, String)>,
    ci: ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    let model = find_model(&state, &model_id)?;
    let safe = safe_asset_name(&name)?;
    let manifest: Vec<Value> = model
        .manifest
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|it| it.is_object())
        .collect();
    let remain: Vec<Value> = manifest
        .iter()
        .filter(|it| it.get("name").and_then(|v| v.as_str()) != Some(safe.as_str()))
        .cloned()
        .collect();
    if remain.len() == manifest.len() {
        return Err(ApiError::not_found(format!("附属物不存在: {safe}")));
    }
    let path = assets_dir(&state, &model_id).join(&safe);
    if path.is_file() {
        let _ = std::fs::remove_file(&path);
    }
    let conn = state
        .pool
        .get()
        .map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
    conn.execute(
        "UPDATE models SET manifest = ?1 WHERE model_id = ?2",
        rusqlite::params![serde_json::to_string(&remain).unwrap_or_else(|_| "[]".to_string()), model_id],
    )
    .map_err(|e| ApiError::internal(format!("更新 manifest 失败：{e}")))?;

    crate::audit::record(
        &state.pool,
        "model.manifest.delete",
        "shared-token",
        "model",
        &model_id,
        "",
        true,
        Some(&json!({"name": safe})),
        "删除附属物",
        &helpers::client_ip(&ci),
    );
    Ok(Json(json!({ "ok": true, "deleted": safe })))
}

// ------------------------------------------------------------------ 下载口（共享 token 或下载令牌）

async fn authorize_download(state: &SharedState, model_id: &str, headers: &HeaderMap, raw: Option<&str>) -> ApiResult<()> {
    let shared = candidate_token(headers, raw);
    if crate::security::verify_token(shared.as_deref()) {
        return Ok(());
    }
    let scoped = header_value(headers, "x-download-token").unwrap_or_default();
    if state.download_tokens.authorize(model_id, &scoped) {
        return Ok(());
    }
    Err(ApiError::unauthorized("缺少有效的令牌（X-Auth-Token 或 X-Download-Token）"))
}

async fn download_model(
    State(state): State<SharedState>,
    AxPath(model_id): AxPath<String>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult<axum::response::Response> {
    authorize_download(&state, &model_id, &headers, raw.as_deref()).await?;
    let model = find_model(&state, &model_id)?;
    let path = state.config.models_path().join(&model.storage_path);
    if !path.is_file() {
        return Err(ApiError::not_found("模型文件缺失（记录在，文件不在）"));
    }
    let is_partial = header_value(&headers, "range").is_some();
    if !is_partial {
        let conn = state.pool.get().map_err(|e| ApiError::internal(format!("数据库连接失败：{e}")))?;
        let _ = conn.execute(
            "UPDATE models SET download_count = download_count + 1 WHERE model_id = ?1",
            [&model_id],
        );
    }
    file_response(
        &path,
        &model.file_name,
        "application/octet-stream",
        &[("X-Model-SHA256", model.sha256.clone())],
        header_value(&headers, "range").as_deref(),
        header_value(&headers, "if-range").as_deref(),
    )
    .await
}

async fn download_manifest_asset(
    State(state): State<SharedState>,
    AxPath((model_id, name)): AxPath<(String, String)>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult<axum::response::Response> {
    authorize_download(&state, &model_id, &headers, raw.as_deref()).await?;
    find_model(&state, &model_id)?;
    let safe = safe_asset_name(&name)?;
    let path = assets_dir(&state, &model_id).join(&safe);
    if !path.is_file() {
        return Err(ApiError::not_found(format!("附属物文件缺失: {safe}")));
    }
    file_response(
        &path,
        &safe,
        "application/octet-stream",
        &[],
        header_value(&headers, "range").as_deref(),
        header_value(&headers, "if-range").as_deref(),
    )
    .await
}
