//! 部署下发队列（M3）。与 Python services/deploy_queue.py 对齐。
//!
//! 有界并发（默认 2）+ 下发前兼容性/磁盘预检，避免"10 台手机同时下 4GB"打满带宽。
//! 平台地址取 WS 握手时的 Host 头（真实连平台地址），不取 127.0.0.1。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::config;
use crate::db::DbPool;
use crate::enums;
use crate::services::compat::{check_compatibility, CompatModel, CompatNode};
use crate::services::model_tokens::DownloadTokens;
use crate::time;

const DISK_HEADROOM: f64 = 0.10;

/// 模型行（下发所需字段）。
struct ModelRec {
    model_id: String,
    #[allow(dead_code)] // 与 Python 版下发记录对齐
    name: String,
    file_name: String,
    file_size: i64,
    sha256: String,
    framework: String,
    target_soc: String,
    qnn_version: String,
    backends: Option<String>,
    manifest: Option<String>,
    self_test_command: String,
    self_test_timeout: i64,
}

fn read_model(conn: &Connection, model_id: &str) -> Option<ModelRec> {
    conn.query_row(
        "SELECT model_id, name, file_name, file_size, sha256, framework, target_soc, \
         qnn_version, backends, manifest, self_test_command, self_test_timeout \
         FROM models WHERE model_id = ?1",
        [model_id],
        |r| {
            Ok(ModelRec {
                model_id: r.get(0)?,
                name: r.get(1)?,
                file_name: r.get(2)?,
                file_size: r.get(3)?,
                sha256: r.get(4)?,
                framework: r.get(5)?,
                target_soc: r.get(6)?,
                qnn_version: r.get(7)?,
                backends: r.get(8)?,
                manifest: r.get(9)?,
                self_test_command: r.get(10)?,
                self_test_timeout: r.get(11)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

fn read_task_status(conn: &Connection, task_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT status FROM deploy_tasks WHERE task_id = ?1",
        [task_id],
        |r| r.get(0),
    )
    .ok()
}

fn read_task_model_id(conn: &Connection, task_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT model_id FROM deploy_tasks WHERE task_id = ?1",
        [task_id],
        |r| r.get(0),
    )
    .ok()
}

fn read_deploy_node_status(conn: &Connection, task_id: &str, node_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT status FROM deploy_task_nodes WHERE task_id = ?1 AND node_id = ?2",
        params![task_id, node_id],
        |r| r.get(0),
    )
    .ok()
}

fn read_node_compat(conn: &Connection, node_id: &str) -> Option<CompatNode> {
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

/// 读取节点最近一次采样里的 (used, total) 存储字节。拿不到返回 None。
fn read_latest_storage(conn: &Connection, node_id: &str) -> Option<(i64, i64)> {
    conn.query_row(
        "SELECT storage_used, storage_total FROM metric_samples \
         WHERE node_id = ?1 ORDER BY ts DESC LIMIT 1",
        [node_id],
        |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
    )
    .ok()
    .and_then(|(u, t)| match (u, t) {
        (Some(u), Some(t)) => Some((u, t)),
        _ => None,
    })
}

fn parse_string_list(s: &Option<String>) -> Option<Vec<String>> {
    let s = s.as_deref()?;
    let v: Value = serde_json::from_str(s).ok()?;
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
}

fn _mb(n: i64) -> i64 {
    n / 1024 / 1024
}

pub struct DeployQueue {
    pub concurrency: usize,
    tx: mpsc::UnboundedSender<(String, String)>,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String)>>>,
    node_base: Mutex<HashMap<String, String>>,
    manager: Arc<crate::connection::ConnectionManager>,
    pool: DbPool,
    tokens: Arc<DownloadTokens>,
    queued: AtomicUsize,
}

impl DeployQueue {
    pub fn new(
        manager: Arc<crate::connection::ConnectionManager>,
        pool: DbPool,
        tokens: Arc<DownloadTokens>,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            concurrency: config::get().server.deploy_concurrency.max(1) as usize,
            tx,
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            node_base: Mutex::new(HashMap::new()),
            manager,
            pool,
            tokens,
            queued: AtomicUsize::new(0),
        })
    }

    /// 启动 worker。进程生命周期内一次。
    pub fn start(self: &Arc<Self>) {
        for i in 0..self.concurrency {
            let q = self.clone();
            tokio::spawn(async move { q.worker(i).await });
        }
    }

    pub fn qsize(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn submit(&self, task_id: &str, node_id: &str) {
        self.queued.fetch_add(1, Ordering::SeqCst);
        let _ = self
            .tx
            .send((task_id.to_string(), node_id.to_string()));
    }

    pub fn set_node_base(&self, node_id: &str, host: &str) {
        if !node_id.is_empty() && !host.is_empty() {
            self.node_base
                .lock()
                .unwrap()
                .insert(node_id.to_string(), host.to_string());
        }
    }

    pub fn get_node_base(&self, node_id: &str) -> String {
        let host = self
            .node_base
            .lock()
            .unwrap()
            .get(node_id)
            .cloned()
            .unwrap_or_default();
        if host.is_empty() {
            return String::new();
        }
        if host.contains("://") {
            host
        } else {
            format!("http://{host}")
        }
    }

    /// 节点（重）连上后，把该节点被打断的下发任务重新入队。
    /// 手机端 Agent 会从 `.part` 断点续传，所以这里是"续传"，不是"从头再来"。
    pub fn redrive_for_node(&self, node_id: &str) {
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(_) => return,
        };
        // 只重投"传输/校验中被打断"的阶段；pending 还在队列里、终态不用管。
        const INTERRUPTIBLE: &[&str] = &[
            enums::deploy_node_status::DOWNLOADING,
            enums::deploy_node_status::VERIFYING,
            enums::deploy_node_status::INSTALLING,
            enums::deploy_node_status::TESTING,
        ];
        let mut tasks: Vec<String> = Vec::new();
        let mut stmt = match conn.prepare(
            "SELECT task_id, status, error FROM deploy_task_nodes WHERE node_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let rows = stmt.query_map(rusqlite::params![node_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (task_id, status, error) = row;
                let offline_killed = status == enums::deploy_node_status::FAILED
                    && error.contains("节点离线");
                // 「节点离线」导致的 failed 也在重投之列：手机进程被杀后平台会把它
                // 判失败，若不在重连时续上，用户只能手动重试。Agent 端会从 .part
                // 断点续传，重投不是从头再来。其他原因（SHA 不符等）的 failed 不碰。
                if INTERRUPTIBLE.contains(&status.as_str()) || offline_killed {
                    tasks.push(task_id);
                }
            }
        }
        for task_id in tasks {
            self.submit(&task_id, node_id);
        }
    }

    async fn worker(&self, _idx: usize) {
        loop {
            let (task_id, node_id) = {
                let mut rx = self.rx.lock().await;
                match rx.recv().await {
                    Some(item) => {
                        self.queued.fetch_sub(1, Ordering::SeqCst);
                        item
                    }
                    None => return,
                }
            };
            if let Err(e) = self.dispatch(&task_id, &node_id).await {
                self.mark_dispatch_failed(&task_id, &node_id, &e);
            }
        }
    }

    /// 下发过程本身抛异常 → 收敛成节点失败。
    fn mark_dispatch_failed(&self, task_id: &str, node_id: &str, exc: &str) {
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(_) => return,
        };
        let status: Option<String> = read_deploy_node_status(&conn, task_id, node_id);
        match status {
            Some(s) if enums::is_terminal_node_state(&s) => return,
            None => return,
            _ => {}
        }
        let error: String = format!("平台下发异常：{exc}").chars().take(2000).collect();
        let _ = conn.execute(
            "UPDATE deploy_task_nodes SET status = 'failed', error = ?3, finished_at = ?4 \
             WHERE task_id = ?1 AND node_id = ?2",
            params![task_id, node_id, error, time::now_db()],
        );
        self.settle_task(&conn, task_id);
    }

    async fn dispatch(&self, task_id: &str, node_id: &str) -> Result<(), String> {
        // ---- 出队时再判一次取消（排队期间用户可能已取消）----
        let (task_status, model) = {
            let conn = self.pool.get().map_err(|e| e.to_string())?;
            let node_status = match read_deploy_node_status(&conn, task_id, node_id) {
                Some(s) => s,
                None => return Ok(()),
            };
            if node_status == enums::deploy_node_status::CANCELLED {
                self.settle_task(&conn, task_id);
                return Ok(());
            }
            let task_model_id = match read_task_model_id(&conn, task_id) {
                Some(m) => m,
                None => return Ok(()),
            };
            let model = match read_model(&conn, &task_model_id) {
                Some(m) => m,
                None => return Ok(()),
            };
            let task_status = read_task_status(&conn, task_id).unwrap_or_default();
            (task_status, model)
        };

        if task_status == enums::deploy_task_status::CANCELLED {
            let conn = self.pool.get().map_err(|e| e.to_string())?;
            let _ = conn.execute(
                "UPDATE deploy_task_nodes SET status = 'cancelled' \
                 WHERE task_id = ?1 AND node_id = ?2",
                params![task_id, node_id],
            );
            self.settle_task(&conn, task_id);
            return Ok(());
        }

        // ---- 兼容性预检（需求 §5）----
        let node_compat = {
            let conn = self.pool.get().map_err(|e| e.to_string())?;
            read_node_compat(&conn, node_id)
        };
        if let Some(node) = &node_compat {
            let compat_model = CompatModel {
                target_soc: model.target_soc.clone(),
                qnn_version: model.qnn_version.clone(),
                framework: model.framework.clone(),
                backends: parse_string_list(&model.backends),
            };
            let compat = check_compatibility(&compat_model, node);
            if compat.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                let reason = compat
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("兼容性检查未通过")
                    .to_string();
                let conn = self.pool.get().map_err(|e| e.to_string())?;
                let error: String = format!("兼容性检查未通过：{reason}")
                    .chars()
                    .take(2000)
                    .collect();
                let _ = conn.execute(
                    "UPDATE deploy_task_nodes SET status = 'failed', error = ?3 \
                     WHERE task_id = ?1 AND node_id = ?2",
                    params![task_id, node_id, error],
                );
                self.settle_task(&conn, task_id);
                return Ok(());
            }
        }

        // ---- 磁盘预检 ----
        let disk = {
            let conn = self.pool.get().map_err(|e| e.to_string())?;
            read_latest_storage(&conn, node_id)
        };
        if !self.check_disk(disk, model.file_size) {
            let (free, need) = match disk {
                Some((u, t)) => (t - u, (model.file_size as f64 * (1.0 + DISK_HEADROOM)) as i64),
                None => (0, (model.file_size as f64 * (1.0 + DISK_HEADROOM)) as i64),
            };
            let reason = format!(
                "磁盘空间不足：剩余 {} MB，需要 {} MB（含 {}% 安全余量）",
                _mb(free),
                _mb(need),
                (DISK_HEADROOM * 100.0) as i64
            );
            let conn = self.pool.get().map_err(|e| e.to_string())?;
            let _ = conn.execute(
                "UPDATE deploy_task_nodes SET status = 'failed', error = ?3, finished_at = NULL \
                 WHERE task_id = ?1 AND node_id = ?2",
                params![task_id, node_id, reason],
            );
            self.settle_task(&conn, task_id);
            return Ok(());
        }

        // ---- 下发 ----
        let base = self.get_node_base(node_id);
        let url = format!("{base}/api/v1/models/{}/download", model.model_id);
        let payload = self.build_payload(task_id, node_id, &model, &url, &base);
        let (delivered, _cmd) = self
            .manager
            .send_command(node_id, "deploy_model", payload, 0.0)
            .await;

        let conn = self.pool.get().map_err(|e| e.to_string())?;
        let new_status = if delivered {
            enums::deploy_node_status::DOWNLOADING
        } else {
            enums::deploy_node_status::FAILED
        };
        let new_error = if delivered {
            ""
        } else {
            "节点离线或指令下发失败"
        };
        let _ = conn.execute(
            "UPDATE deploy_task_nodes SET status = ?3, error = ?4 \
             WHERE task_id = ?1 AND node_id = ?2",
            params![task_id, node_id, new_status, new_error],
        );
        if delivered {
            // started_at 只补一次（保留最早启动时间）。
            let _ = conn.execute(
                "UPDATE deploy_task_nodes SET started_at = COALESCE(started_at, ?3) \
                 WHERE task_id = ?1 AND node_id = ?2",
                params![task_id, node_id, time::now_db()],
            );
        }
        self.settle_task(&conn, task_id);
        Ok(())
    }

    fn build_payload(
        &self,
        task_id: &str,
        node_id: &str,
        model: &ModelRec,
        url: &str,
        base: &str,
    ) -> Value {
        let token = self.tokens.issue(&model.model_id, node_id);

        let mut manifest: Vec<Value> = Vec::new();
        let raw: Vec<Value> = model
            .manifest
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| Vec::new());
        for item in raw {
            let obj = match item.as_object() {
                Some(o) => o,
                None => continue,
            };
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                continue;
            }
            manifest.push(json!({
                "name": name,
                "kind": obj.get("kind").and_then(|v| v.as_str()).unwrap_or("other"),
                "size": obj.get("size").and_then(|v| v.as_i64()).unwrap_or(0),
                "sha256": obj.get("sha256").and_then(|v| v.as_str()).unwrap_or(""),
                "url": format!("{base}/api/v1/models/{}/manifest/{}", model.model_id, name),
            }));
        }

        let cmd = model.self_test_command.trim();
        let self_test = if cmd.is_empty() {
            Value::Null
        } else {
            json!({
                "command": cmd,
                "timeout": model.self_test_timeout.max(1),
            })
        };

        json!({
            "url": url,
            "sha256": model.sha256,
            "target": model.file_name,
            "size": model.file_size,
            "task_id": task_id,
            "model_id": model.model_id,
            "download_token": token,
            "manifest": manifest,
            "self_test": self_test,
        })
    }

    /// 通知某节点中止这次部署（尽力而为）。状态变更由调用方（路由）先行落库。
    pub async fn cancel_node(&self, task_id: &str, node_id: &str) -> bool {
        let (delivered, _cmd) = self
            .manager
            .send_command(node_id, "cancel_deploy", json!({"task_id": task_id}), 0.0)
            .await;
        delivered
    }

    fn check_disk(&self, disk: Option<(i64, i64)>, file_size: i64) -> bool {
        if file_size <= 0 {
            return true;
        }
        let (used, total) = match disk {
            Some(v) => v,
            None => return true,
        };
        if total <= 0 {
            return true;
        }
        let free = total - used;
        let need = (file_size as f64 * (1.0 + DISK_HEADROOM)) as i64;
        free >= need
    }

    fn settle_task(&self, conn: &Connection, task_id: &str) {
        let task_status = match read_task_status(conn, task_id) {
            Some(s) => s,
            None => return,
        };
        let mut statuses: Vec<String> = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT status FROM deploy_task_nodes WHERE task_id = ?1") {
            if let Ok(iter) = stmt.query_map([task_id], |r| r.get::<_, String>(0)) {
                for s in iter.flatten() {
                    statuses.push(s);
                }
            }
        }
        if statuses.is_empty() {
            return;
        }
        let all_terminal = statuses.iter().all(|s| enums::is_terminal_node_state(s));
        if all_terminal {
            let agg = enums::aggregate_deploy_status(&statuses);
            let _ = conn.execute(
                "UPDATE deploy_tasks SET status = ?2, finished_at = ?3 WHERE task_id = ?1",
                params![task_id, agg, time::now_db()],
            );
        } else if task_status == enums::deploy_task_status::PENDING {
            let _ = conn.execute(
                "UPDATE deploy_tasks SET status = 'running' WHERE task_id = ?1",
                [task_id],
            );
        }
    }
}
