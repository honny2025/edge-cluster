//! 文件传输会话管理（需求 §4）。与 Python services/file_transfers.py 对齐。
//!
//! 三段式：平台建会话（一次性令牌）→ Agent 主动 HTTP 搬运 → 平台收尾落库。
//! 两个方向都由 Agent 发起 HTTP（平台永不主动连手机）：
//! - pull（设备 → 平台）：Agent 读文件后 POST 上来，浏览器再从平台下载；
//! - push（平台 → 设备）：浏览器先上传到平台暂存，Agent 再 GET 拉走。
//!
//! 一次性令牌只对**一个 transfer_id** 有效、只在 TTL 内有效。

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::config;
use crate::db::{DbPool, PooledConn};
use crate::time;

pub const PENDING: &str = "pending";
pub const TRANSFERRING: &str = "transferring";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
pub const EXPIRED: &str = "expired";
pub const CANCELLED: &str = "cancelled";

/// 还在进行中的状态：过期清理只碰这些，已结束的记录保留（要有据可查）。
fn is_active(s: &str) -> bool {
    matches!(s, PENDING | TRANSFERRING)
}

fn is_terminal(s: &str) -> bool {
    matches!(s, DONE | FAILED | EXPIRED | CANCELLED)
}

const TRANSFER_COLUMNS: &str = "transfer_id, node_id, direction, path, file_name, \
    staging_path, size, sha256, status, error, token, created_at, finished_at, bytes_done";

struct TransferRow {
    transfer_id: String,
    node_id: String,
    direction: String,
    path: String,
    file_name: String,
    staging_path: String,
    size: i64,
    sha256: String,
    status: String,
    error: String,
    token: String,
    created_at: String,
    finished_at: Option<String>,
    bytes_done: i64,
}

fn row_to_transfer(r: &rusqlite::Row) -> rusqlite::Result<TransferRow> {
    Ok(TransferRow {
        transfer_id: r.get(0)?,
        node_id: r.get(1)?,
        direction: r.get(2)?,
        path: r.get(3)?,
        file_name: r.get(4)?,
        staging_path: r.get(5)?,
        size: r.get(6)?,
        sha256: r.get(7)?,
        status: r.get(8)?,
        error: r.get(9)?,
        token: r.get(10)?,
        created_at: r.get(11)?,
        finished_at: r.get(12)?,
        bytes_done: r.get(13)?,
    })
}

fn read_transfer(conn: &Connection, transfer_id: &str) -> Option<TransferRow> {
    conn.query_row(
        &format!("SELECT {TRANSFER_COLUMNS} FROM file_transfers WHERE transfer_id = ?1"),
        [transfer_id],
        row_to_transfer,
    )
    .optional()
    .ok()
    .flatten()
}

impl TransferRow {
    fn snapshot(&self, include_token: bool) -> Value {
        let mut out = json!({
            "transfer_id": self.transfer_id,
            "node_id": self.node_id,
            "direction": self.direction,
            "path": self.path,
            "file_name": self.file_name,
            "staging_path": self.staging_path,
            "size": self.size,
            "sha256": self.sha256,
            "status": self.status,
            "error": self.error,
            "bytes_done": self.bytes_done,
            "created_at": time::db_to_iso(&self.created_at),
            "finished_at": self.finished_at.as_deref().map(time::db_to_iso),
        });
        if include_token {
            out["token"] = json!(self.token);
        }
        out
    }
}

pub struct TransferService {
    pool: DbPool,
}

impl TransferService {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    fn conn(&self) -> Option<PooledConn> {
        self.pool.get().ok()
    }

    fn staging_root(&self) -> PathBuf {
        config::get().staging_path()
    }

    /// 一个传输会话独占一个目录。平铺会导致清理 A 时误删 B 正在传的文件。
    pub fn staging_dir(&self, transfer_id: &str) -> PathBuf {
        let d = self.staging_root().join(transfer_id);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// 找到这个会话暂存的文件（按库里记的名字）。
    pub fn local_file(&self, transfer_id: &str) -> Option<PathBuf> {
        let conn = self.conn()?;
        let row = read_transfer(&conn, transfer_id)?;
        if row.staging_path.is_empty() {
            return None;
        }
        let p = PathBuf::from(&row.staging_path);
        p.is_file().then_some(p)
    }

    /// 先生成 id，再建目录。push 方向要先落盘才知道大小和哈希。
    pub fn new_id() -> String {
        crate::util::new_uuid_hex()[..16].to_string()
    }

    pub fn planned_path(&self, transfer_id: &str, file_name: &str) -> PathBuf {
        let name = if file_name.is_empty() {
            "file.bin".to_string()
        } else {
            file_name.to_string()
        };
        self.staging_dir(transfer_id).join(name)
    }

    pub fn create(
        &self,
        node_id: &str,
        direction: &str,
        path: &str,
        transfer_id: &str,
        file_name: &str,
        size: i64,
        sha256: &str,
    ) -> Option<Value> {
        if direction != "pull" && direction != "push" {
            return None;
        }
        let tid = if transfer_id.is_empty() {
            Self::new_id()
        } else {
            transfer_id.to_string()
        };
        let staging = self.planned_path(&tid, file_name);
        let staging_str = staging.to_string_lossy().to_string();
        let file_name = if file_name.is_empty() {
            staging
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file.bin".to_string())
        } else {
            file_name.to_string()
        };
        let token = crate::util::new_urlsafe_token();
        let now = time::now_db();

        let conn = self.conn()?;
        let res = conn.execute(
            "INSERT INTO file_transfers (transfer_id, node_id, direction, path, file_name, \
             staging_path, size, sha256, status, error, token, created_at, finished_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                tid, node_id, direction, path, file_name, staging_str,
                size, sha256, PENDING, "", token, now, Option::<String>::None
            ],
        );
        if res.is_err() {
            return None;
        }
        read_transfer(&conn, &tid).map(|r| r.snapshot(true))
    }

    pub fn get(&self, transfer_id: &str) -> Option<Value> {
        let conn = self.conn()?;
        read_transfer(&conn, transfer_id).map(|r| r.snapshot(false))
    }

    /// 校验一次性令牌。过期 / 非进行中的会话一律拒绝。
    pub fn authorize(&self, transfer_id: &str, token: &str) -> Option<Value> {
        if transfer_id.is_empty() || token.is_empty() {
            return None;
        }
        let conn = self.conn()?;
        let row = read_transfer(&conn, transfer_id)?;
        if row.token.is_empty() {
            return None;
        }
        if !crate::security::constant_time_eq(token, &row.token) {
            return None;
        }
        if !is_active(&row.status) {
            return None;
        }
        if self.is_expired(&row) {
            let _ = conn.execute(
                "UPDATE file_transfers SET status = ?2, finished_at = ?3 WHERE transfer_id = ?1",
                params![transfer_id, EXPIRED, time::now_db()],
            );
            return None;
        }
        Some(row.snapshot(false))
    }

    pub fn list(&self, node_id: &str, limit: i64) -> Vec<Value> {
        let conn = match self.conn() {
            Some(c) => c,
            None => return vec![],
        };
        let lim = limit.clamp(1, 500);
        let mut rows: Vec<TransferRow> = vec![];
        let result: Result<(), rusqlite::Error> = if node_id.is_empty() {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TRANSFER_COLUMNS} FROM file_transfers ORDER BY created_at DESC LIMIT ?1"
                ))
                .ok();
            if let Some(stmt) = stmt.as_mut() {
                if let Ok(iter) = stmt.query_map([lim], row_to_transfer) {
                    for r in iter.flatten() {
                        rows.push(r);
                    }
                }
            }
            Ok(())
        } else {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {TRANSFER_COLUMNS} FROM file_transfers WHERE node_id = ?1 \
                     ORDER BY created_at DESC LIMIT ?2"
                ))
                .ok();
            if let Some(stmt) = stmt.as_mut() {
                if let Ok(iter) = stmt.query_map(params![node_id, lim], row_to_transfer) {
                    for r in iter.flatten() {
                        rows.push(r);
                    }
                }
            }
            Ok(())
        };
        let _ = result;
        rows.iter().map(|r| r.snapshot(false)).collect()
    }

    /// 推进状态。终态一旦写入就不再被覆盖（除非再次失败/取消）。
    pub fn mark(
        &self,
        transfer_id: &str,
        status: &str,
        error: &str,
        size: Option<i64>,
        sha256: &str,
    ) {
        let conn = match self.conn() {
            Some(c) => c,
            None => return,
        };
        let row = match read_transfer(&conn, transfer_id) {
            Some(r) => r,
            None => return,
        };
        // 已经成功的绝不回头。
        if row.status == DONE && status != DONE {
            return;
        }
        if is_terminal(&row.status) && !is_terminal(status) {
            return;
        }
        // 未显式给出的字段保留旧值（与 Python 只更新给定字段语义一致）。
        let new_error: String = if error.is_empty() {
            row.error.clone()
        } else {
            error.chars().take(2000).collect()
        };
        let new_size: i64 = size.unwrap_or(row.size);
        let new_sha256: String = if sha256.is_empty() {
            row.sha256.clone()
        } else {
            sha256.to_string()
        };
        let finished: Option<String> = if is_terminal(status) {
            Some(time::now_db())
        } else {
            row.finished_at.clone()
        };
        let _ = conn.execute(
            "UPDATE file_transfers SET status = ?2, error = ?3, size = ?4, \
             sha256 = ?5, finished_at = ?6 WHERE transfer_id = ?1",
            params![transfer_id, status, new_error, new_size, new_sha256, finished],
        );
    }

    /// 记录实时进度（已传字节数）。只在传输进行中更新，终态后不再写。
    pub fn update_progress(&self, transfer_id: &str, bytes_done: i64) {
        let conn = match self.conn() {
            Some(c) => c,
            None => return,
        };
        let _ = conn.execute(
            "UPDATE file_transfers SET bytes_done = ?2 \
             WHERE transfer_id = ?1 AND status IN ('pending','transferring')",
            params![transfer_id, bytes_done],
        );
    }

    /// 把超时未完成的会话标为 expired，并删掉暂存文件。返回清理数量。
    pub fn expire_stale(&self) -> i64 {
        let ttl = (config::get().files.transfer_ttl_minutes.max(1)) * 60;
        let cutoff = time::format_db(&(time::now() - chrono::Duration::seconds(ttl)));
        let conn = match self.conn() {
            Some(c) => c,
            None => return 0,
        };
        let mut ids: Vec<String> = vec![];
        {
            let mut stmt = match conn.prepare(
                "SELECT transfer_id FROM file_transfers \
                 WHERE status IN ('pending','transferring') AND created_at < ?1",
            ) {
                Ok(s) => s,
                Err(_) => return 0,
            };
            let iter = match stmt.query_map([&cutoff], |r| r.get::<_, String>(0)) {
                Ok(it) => it,
                Err(_) => return 0,
            };
            for r in iter.flatten() {
                ids.push(r);
            }
        }
        let now = time::now_db();
        for tid in &ids {
            let _ = conn.execute(
                "UPDATE file_transfers SET status = 'expired', \
                 error = COALESCE(NULLIF(error,''), '传输超时，会话已失效'), \
                 finished_at = ?2 WHERE transfer_id = ?1",
                params![tid, now],
            );
        }
        for tid in &ids {
            self.discard_staging(tid);
        }
        ids.len() as i64
    }

    /// 真正删除一条传输记录（终态记录的清理入口，区别于「取消进行中的传输」）。
    /// 前端对 done/failed/cancelled/expired 记录点的「删除」走这里：记录从库里消失，
    /// 暂存文件一并清掉。返回是否真的删到了行。
    pub fn delete_record(&self, transfer_id: &str) -> bool {
        let conn = match self.conn() {
            Some(c) => c,
            None => return false,
        };
        let affected = conn
            .execute("DELETE FROM file_transfers WHERE transfer_id = ?1", params![transfer_id])
            .unwrap_or(0);
        if affected > 0 {
            self.discard_staging(transfer_id);
        }
        affected > 0
    }

    /// 删掉会话的暂存目录。失败只记日志。
    pub fn discard_staging(&self, transfer_id: &str) {
        let d = self.staging_root().join(transfer_id);
        if d.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&d) {
                eprintln!("清理暂存目录失败 transfer={transfer_id} err={e}");
            }
        }
    }

    fn is_expired(&self, row: &TransferRow) -> bool {
        let ttl = config::get().files.transfer_ttl_minutes.max(1);
        match time::db_elapsed_secs(&time::now_db(), &row.created_at) {
            Some(elapsed) => elapsed > ttl as f64 * 60.0,
            None => false,
        }
    }
}
