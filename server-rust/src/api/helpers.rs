//! 行 → JSON 映射（对应 Python 各 route 里的 `_out` / `out` / `_task_dict` / `_node`）。
//!
//! 这些映射是 wire 协议的组成部分：字段名、缺省值（空串 / 空列表 / null）必须与
//! Python 版逐字一致，否则前端会看到"有值但没送出来"或"类型对不上"。
//! 新增字段只改这里，不要在各路由里手写字段列表（历史教训：漏一个字段极难排查）。

use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::services::compat::{CompatModel, CompatNode};
use crate::time;

// ------------------------------------------------------------------ 通用

pub fn client_ip(ci: &ConnectInfo<SocketAddr>) -> String {
    ci.0.ip().to_string()
}

fn parse_json_array(s: Option<&str>) -> Vec<Value> {
    s.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn parse_json_value(s: Option<&str>) -> Option<Value> {
    s.and_then(|s| serde_json::from_str::<Value>(s).ok())
}

// ------------------------------------------------------------------ 模型

pub struct ModelRow {
    pub model_id: String,
    pub name: String,
    pub version: String,
    pub file_name: String,
    pub file_size: i64,
    pub framework: String,
    pub format: String,
    pub sha256: String,
    pub quantization: String,
    pub target_soc: String,
    pub qnn_version: String,
    pub backends: Option<String>,
    pub deps: Option<String>,
    pub run_params: Option<String>,
    pub manifest: Option<String>,
    pub self_test_command: String,
    pub self_test_timeout: i64,
    pub description: String,
    pub uploaded_at: String,
    pub storage_path: String,
    pub download_count: i64,
}

const MODEL_COLS: &str = "model_id, name, version, file_name, file_size, framework, format, \
    sha256, quantization, target_soc, qnn_version, backends, deps, run_params, manifest, \
    self_test_command, self_test_timeout, description, uploaded_at, storage_path, download_count";

fn row_to_model(r: &rusqlite::Row) -> rusqlite::Result<ModelRow> {
    Ok(ModelRow {
        model_id: r.get(0)?,
        name: r.get(1)?,
        version: r.get(2)?,
        file_name: r.get(3)?,
        file_size: r.get(4)?,
        framework: r.get(5)?,
        format: r.get(6)?,
        sha256: r.get(7)?,
        quantization: r.get(8)?,
        target_soc: r.get(9)?,
        qnn_version: r.get(10)?,
        backends: r.get(11)?,
        deps: r.get(12)?,
        run_params: r.get(13)?,
        manifest: r.get(14)?,
        self_test_command: r.get(15)?,
        self_test_timeout: r.get(16)?,
        description: r.get(17)?,
        uploaded_at: r.get(18)?,
        storage_path: r.get(19)?,
        download_count: r.get(20)?,
    })
}

impl ModelRow {
    /// 对应 Python routes_models._out —— Model Registry 元数据的唯一映射点。
    pub fn out(&self) -> Value {
        let backends = parse_json_array(self.backends.as_deref());
        let deps = parse_json_array(self.deps.as_deref());
        let run_params: Value = self
            .run_params
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| json!({}));
        // manifest 逐条去掉内部 storage 字段（Python：只暴露给用户看的清单）。
        let manifest: Vec<Value> = parse_json_array(self.manifest.as_deref())
            .into_iter()
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
        json!({
            "model_id": self.model_id,
            "name": self.name,
            "version": self.version,
            "file_name": self.file_name,
            "file_size": self.file_size,
            "framework": self.framework,
            "format": self.format,
            "sha256": self.sha256,
            "quantization": self.quantization,
            "target_soc": self.target_soc,
            "qnn_version": self.qnn_version,
            "backends": backends,
            "deps": deps,
            "run_params": run_params,
            "manifest": manifest,
            "self_test_command": self.self_test_command,
            "self_test_timeout": self.self_test_timeout,
            "description": self.description,
            "uploaded_at": Some(time::db_to_iso(&self.uploaded_at)),
            "download_count": self.download_count,
        })
    }

    pub fn compat(&self) -> CompatModel {
        CompatModel {
            target_soc: self.target_soc.clone(),
            qnn_version: self.qnn_version.clone(),
            framework: self.framework.clone(),
            backends: self
                .backends
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| {
                    v.as_array().map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                }),
        }
    }
}

pub fn read_model(conn: &Connection, model_id: &str) -> Option<ModelRow> {
    conn.query_row(
        &format!("SELECT {MODEL_COLS} FROM models WHERE model_id = ?1"),
        [model_id],
        row_to_model,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn list_models(conn: &Connection) -> Vec<Value> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(&format!(
        "SELECT {MODEL_COLS} FROM models ORDER BY uploaded_at DESC"
    )) {
        Ok(s) => s,
        Err(_) => return out,
    };
    if let Ok(iter) = stmt.query_map([], row_to_model) {
        for r in iter.flatten() {
            out.push(r.out());
        }
    }
    out
}

/// 节点兼容性判定所需字段（soc / qnn_version / capabilities）。
pub fn read_compat_node(conn: &Connection, node_id: &str) -> Option<CompatNode> {
    conn.query_row(
        "SELECT soc, qnn_version, capabilities FROM nodes WHERE node_id = ?1",
        [node_id],
        |r| {
            Ok(CompatNode {
                soc: r.get::<_, String>(0)?,
                qnn_version: r.get::<_, String>(1)?,
                capabilities: r
                    .get::<_, Option<String>>(2)?
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Object(Default::default())),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

// ------------------------------------------------------------------ 服务

pub struct ServiceRow {
    pub service_id: String,
    pub node_id: String,
    pub model_id: Option<String>,
    pub name: String,
    pub status: String,
    pub port: i64,
    pub endpoint: String,
    pub api_type: String,
    pub pid: Option<i64>,
    pub started_at: Option<String>,
    pub last_error: String,
    pub last_probe_at: Option<String>,
    pub probe_ok: i64,
    pub command: String,
    pub args: Option<String>,
    #[allow(dead_code)] // 与 Python 版服务模型对齐
    pub model_path: String,
    pub health_url: String,
    pub log_path: String,
    pub auto_restart: i64,
    pub auto_start: i64,
    pub restart_count: i64,
    pub last_exit_code: Option<i64>,
    pub state_changed_at: Option<String>,
    pub mem_required_mb: i64,
    pub mutex_group: String,
    pub created_at: String,
}

const SERVICE_COLS: &str = "service_id, node_id, model_id, name, status, port, endpoint, \
    api_type, pid, started_at, last_error, last_probe_at, probe_ok, command, args, model_path, \
    health_url, log_path, auto_restart, auto_start, restart_count, last_exit_code, \
    state_changed_at, mem_required_mb, mutex_group, created_at";

fn row_to_service(r: &rusqlite::Row) -> rusqlite::Result<ServiceRow> {
    Ok(ServiceRow {
        service_id: r.get(0)?,
        node_id: r.get(1)?,
        model_id: r.get(2)?,
        name: r.get(3)?,
        status: r.get(4)?,
        port: r.get(5)?,
        endpoint: r.get(6)?,
        api_type: r.get(7)?,
        pid: r.get(8)?,
        started_at: r.get(9)?,
        last_error: r.get(10)?,
        last_probe_at: r.get(11)?,
        probe_ok: r.get(12)?,
        command: r.get(13)?,
        args: r.get(14)?,
        model_path: r.get(15)?,
        health_url: r.get(16)?,
        log_path: r.get(17)?,
        auto_restart: r.get(18)?,
        auto_start: r.get(19)?,
        restart_count: r.get(20)?,
        last_exit_code: r.get(21)?,
        state_changed_at: r.get(22)?,
        mem_required_mb: r.get(23)?,
        mutex_group: r.get(24)?,
        created_at: r.get(25)?,
    })
}

impl ServiceRow {
    /// 对应 Python routes_services.out —— 完整服务定义。
    pub fn out(&self) -> Value {
        json!({
            "service_id": self.service_id,
            "node_id": self.node_id,
            "model_id": self.model_id,
            "name": self.name,
            "status": self.status,
            "port": self.port,
            "endpoint": self.endpoint,
            "api_type": self.api_type,
            "pid": self.pid,
            "last_error": self.last_error,
            "command": self.command,
            "args": parse_json_value(self.args.as_deref()),
            "health_url": self.health_url,
            "auto_restart": self.auto_restart != 0,
            "auto_start": self.auto_start != 0,
            "restart_count": self.restart_count,
            "last_exit_code": self.last_exit_code,
            "state_changed_at": self.state_changed_at.as_deref().map(time::db_to_iso),
            "mem_required_mb": self.mem_required_mb,
            "mutex_group": self.mutex_group,
            "last_probe_at": self.last_probe_at.as_deref().map(time::db_to_iso),
            "probe_ok": self.probe_ok,
            "log_path": self.log_path,
            "created_at": Some(time::db_to_iso(&self.created_at)),
        })
    }

    /// 对应 Python routes_nodes.node_services 的精简条目。
    pub fn summary(&self) -> Value {
        json!({
            "service_id": self.service_id,
            "node_id": self.node_id,
            "model_id": self.model_id,
            "name": self.name,
            "status": self.status,
            "port": self.port,
            "endpoint": self.endpoint,
            "api_type": self.api_type,
            "pid": self.pid,
            "started_at": self.started_at.as_deref().map(time::db_to_iso),
            "last_error": self.last_error,
        })
    }
}

pub fn read_service(conn: &Connection, service_id: &str) -> Option<ServiceRow> {
    conn.query_row(
        &format!("SELECT {SERVICE_COLS} FROM services WHERE service_id = ?1"),
        [service_id],
        row_to_service,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn list_services_out(conn: &Connection, node_id: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();
    let sql = match node_id {
        Some(_) => format!("SELECT {SERVICE_COLS} FROM services WHERE node_id = ?1 ORDER BY created_at DESC"),
        None => format!("SELECT {SERVICE_COLS} FROM services ORDER BY created_at DESC"),
    };
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let iter = match node_id {
        Some(id) => stmt.query_map([id], row_to_service),
        None => stmt.query_map([], row_to_service),
    };
    if let Ok(iter) = iter {
        for r in iter.flatten() {
            out.push(r.out());
        }
    }
    out
}

pub fn list_service_summaries(conn: &Connection, node_id: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(&format!(
        "SELECT {SERVICE_COLS} FROM services WHERE node_id = ?1"
    )) {
        Ok(s) => s,
        Err(_) => return out,
    };
    if let Ok(iter) = stmt.query_map([node_id], row_to_service) {
        for r in iter.flatten() {
            out.push(r.summary());
        }
    }
    out
}

// ------------------------------------------------------------------ 部署任务

struct DeployNodeRow {
    node_id: String,
    node_name: String,
    status: String,
    progress: i64,
    error: String,
    retry_count: i64,
    bytes_downloaded: i64,
    bytes_total: i64,
    rollback_note: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    updated_at: String,
}

const DEPLOY_NODE_COLS: &str = "node_id, node_name, status, progress, error, retry_count, \
    bytes_downloaded, bytes_total, rollback_note, started_at, finished_at, updated_at";

fn row_to_deploy_node(r: &rusqlite::Row) -> rusqlite::Result<DeployNodeRow> {
    Ok(DeployNodeRow {
        node_id: r.get(0)?,
        node_name: r.get(1)?,
        status: r.get(2)?,
        progress: r.get(3)?,
        error: r.get(4)?,
        retry_count: r.get(5)?,
        bytes_downloaded: r.get(6)?,
        bytes_total: r.get(7)?,
        rollback_note: r.get(8)?,
        started_at: r.get(9)?,
        finished_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

impl DeployNodeRow {
    fn dict(&self) -> Value {
        json!({
            "node_id": self.node_id,
            "node_name": self.node_name,
            "status": self.status,
            "progress": self.progress,
            "error": self.error,
            "retry_count": self.retry_count,
            "bytes_downloaded": self.bytes_downloaded,
            "bytes_total": self.bytes_total,
            "rollback_note": self.rollback_note,
            "started_at": self.started_at.as_deref().map(time::db_to_iso),
            "finished_at": self.finished_at.as_deref().map(time::db_to_iso),
            "updated_at": Some(time::db_to_iso(&self.updated_at)),
        })
    }
}

const TASK_COLS: &str =
    "task_id, model_id, model_name, target_count, status, note, created_by, created_at, finished_at";

fn read_task_nodes(conn: &Connection, task_id: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(&format!(
        "SELECT {DEPLOY_NODE_COLS} FROM deploy_task_nodes WHERE task_id = ?1 ORDER BY id ASC"
    )) {
        Ok(s) => s,
        Err(_) => return out,
    };
    if let Ok(iter) = stmt.query_map([task_id], row_to_deploy_node) {
        for r in iter.flatten() {
            out.push(r.dict());
        }
    }
    out
}

/// 对应 Python routes_deployments._task_dict。
pub fn task_dict(conn: &Connection, task_id: &str) -> Option<Value> {
    let row: (String, String, String, i64, String, String, String, Option<String>) = conn
        .query_row(
            &format!("SELECT {TASK_COLS} FROM deploy_tasks WHERE task_id = ?1"),
            [task_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .ok()?;
    let nodes = read_task_nodes(conn, task_id);
    Some(json!({
        "task_id": row.0,
        "model_id": row.1,
        "model_name": row.2,
        "status": row.4,
        "target_count": row.3,
        "note": row.5,
        "created_at": time::db_to_iso(&row.6),
        "finished_at": row.7.as_deref().map(time::db_to_iso),
        "nodes": nodes,
    }))
}

pub fn list_tasks(conn: &Connection) -> Vec<Value> {
    let mut ids: Vec<String> = Vec::new();
    {
        let mut stmt = match conn.prepare("SELECT task_id FROM deploy_tasks ORDER BY created_at DESC") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        if let Ok(iter) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for r in iter.flatten() {
                ids.push(r);
            }
        };
    }
    ids.iter().filter_map(|id| task_dict(conn, id)).collect()
}
