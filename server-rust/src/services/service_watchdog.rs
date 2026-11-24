//! 平台侧服务看门狗（需求 §7）。与 Python services/service_watchdog.py 对齐。
//!
//! 三件事，全部"发现并如实记录"，不替用户做决定：
//! 1. 探活：周期 GET 运行中服务的 health_url，收敛到七态状态机；
//! 2. 崩溃兜底：Agent 报了 stopped 但库里仍 active 的服务收敛掉；
//! 3. 重连恢复：节点离线→在线后，把 auto_start=1 但没在跑的服务重新拉起。
//!
//! 红线：节点离线时绝不改状态；拿不到证据不下结论；不主动杀/重启；
//! 单次 tick 全程兜底，看门狗自己不能死。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::config;
use crate::connection::ConnectionManager;
use crate::db::DbPool;
use crate::enums;
use crate::time;

const _RECOVERY_GRACE: f64 = 15.0;
const _PROBE_TIMEOUT: f64 = 3.0;

/// 服务"应该有个进程在跑/正在变动"的状态（探活与崩溃兜底的目标集合）。
const ACTIVE_STATES_SQL: &str = "'starting','running','restarting'";

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn is_active_service_state(s: &str) -> bool {
    enums::service_active_states().contains(&s)
}

struct ProbeJob {
    service_id: String,
    node_id: String,
    port: i64,
    health_url: String,
    ip: String,
}

fn read_probe_jobs(conn: &Connection) -> Vec<ProbeJob> {
    let mut out = Vec::new();
    let sql = format!(
        "SELECT s.service_id, s.node_id, s.port, s.health_url, n.ip \
         FROM services s LEFT JOIN nodes n ON n.node_id = s.node_id \
         WHERE s.status IN ({ACTIVE_STATES_SQL})"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let iter = match stmt.query_map([], |r| {
        Ok(ProbeJob {
            service_id: r.get(0)?,
            node_id: r.get(1)?,
            port: r.get(2)?,
            health_url: r.get(3)?,
            ip: r.get(4)?,
        })
    }) {
        Ok(it) => it,
        Err(_) => return out,
    };
    for j in iter.flatten() {
        out.push(j);
    }
    out
}

fn probe_url(job: &ProbeJob) -> String {
    let ip = job.ip.trim();
    if ip.is_empty() || job.port <= 0 {
        return String::new();
    }
    let path = job.health_url.trim_start_matches('/');
    if path.is_empty() {
        format!("http://{ip}:{}/", job.port)
    } else {
        format!("http://{ip}:{}/{}", job.port, path)
    }
}

/// 读一条服务当前状态。返回 None 表示不存在。
fn read_service_status(conn: &Connection, service_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT status FROM services WHERE service_id = ?1",
        [service_id],
        |r| r.get(0),
    )
    .ok()
}

fn read_service_node_status(conn: &Connection, service_id: &str) -> Option<(String, String)> {
    conn.query_row(
        "SELECT node_id, status FROM services WHERE service_id = ?1",
        [service_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

fn read_active_services(conn: &Connection) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let sql = format!("SELECT service_id, node_id FROM services WHERE status IN ({ACTIVE_STATES_SQL})");
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return out,
    };
    if let Ok(iter) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
        for row in iter.flatten() {
            out.push(row);
        }
    }
    out
}

fn read_known_service_ids(conn: &Connection, node_id: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stmt = match conn.prepare("SELECT service_id FROM services WHERE node_id = ?1") {
        Ok(s) => s,
        Err(_) => return out,
    };
    if let Ok(iter) = stmt.query_map([node_id], |r| r.get::<_, String>(0)) {
        for row in iter.flatten() {
            out.insert(row);
        }
    }
    out
}

struct RecoveryItem {
    service_id: String,
    command: String,
    args: Value,
    auto_restart: bool,
}

fn read_auto_start_services(conn: &Connection, node_id: &str) -> Vec<RecoveryItem> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(
        "SELECT service_id, command, args, auto_restart FROM services \
         WHERE node_id = ?1 AND auto_start = 1",
    ) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let iter = match stmt.query_map([node_id], |r| {
        let args: Value = r
            .get::<_, Option<String>>(2)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| json!([]));
        Ok(RecoveryItem {
            service_id: r.get(0)?,
            command: r.get(1)?,
            args,
            auto_restart: r.get::<_, i64>(3)? != 0,
        })
    }) {
        Ok(it) => it,
        Err(_) => return out,
    };
    for item in iter.flatten() {
        if !item.command.trim().is_empty() {
            out.push(item);
        }
    }
    out
}

pub struct ServiceWatchdog {
    manager: Arc<ConnectionManager>,
    pool: DbPool,
    fail_streak: Mutex<HashMap<String, usize>>,
    online: Mutex<HashSet<String>>,
    recovery_due: Mutex<HashMap<String, f64>>,
    http: reqwest::Client,
}

impl ServiceWatchdog {
    pub fn new(manager: Arc<ConnectionManager>, pool: DbPool) -> Arc<Self> {
        Arc::new(Self {
            manager,
            pool,
            fail_streak: Mutex::new(HashMap::new()),
            online: Mutex::new(HashSet::new()),
            recovery_due: Mutex::new(HashMap::new()),
            http: reqwest::Client::new(),
        })
    }

    /// 启动看门狗循环。配置关闭时不动。
    pub fn start(self: &Arc<Self>) {
        if !config::get().server.service_watchdog_enabled {
            return;
        }
        let wd = self.clone();
        tokio::spawn(async move { wd.loop_forever().await });
    }

    async fn loop_forever(&self) {
        let interval = config::get().server.service_probe_interval.max(5) as u64;
        loop {
            if let Err(e) = self.tick().await {
                eprintln!("[watchdog] tick 异常 err={e}");
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }

    async fn tick(&self) -> Result<(), String> {
        let online: Vec<String> = self.manager.online_node_ids().await;
        let online_set: HashSet<String> = online.iter().cloned().collect();
        self.track_reconnects(&online_set);
        self.probe_services(&online_set).await;
        self.collect_crashes(&online).await;
        self.run_pending_recovery().await;
        Ok(())
    }

    fn track_reconnects(&self, online: &HashSet<String>) {
        let now = now_epoch();
        let mut prev = self.online.lock().unwrap();
        for nid in online.difference(&prev) {
            self.recovery_due
                .lock()
                .unwrap()
                .insert(nid.clone(), now + _RECOVERY_GRACE);
        }
        for nid in prev.difference(online) {
            self.recovery_due.lock().unwrap().remove(nid);
            let mut fs = self.fail_streak.lock().unwrap();
            let prefix = format!("{nid}:");
            fs.retain(|k, _| !k.starts_with(&prefix));
        }
        *prev = online.clone();
    }

    // -------------------------------------------------------------- 探活
    async fn probe_services(&self, online: &HashSet<String>) {
        let jobs: Vec<ProbeJob> = {
            let conn = match self.pool.get() {
                Ok(c) => c,
                Err(_) => return,
            };
            read_probe_jobs(&conn)
        };
        for job in jobs {
            if !online.contains(&job.node_id) {
                continue;
            }
            let url = probe_url(&job);
            if url.is_empty() {
                continue;
            }
            let (ok, detail) = self.http_probe(&url).await;
            let sid = job.service_id.clone();
            let streak = {
                let mut fs = self.fail_streak.lock().unwrap();
                if ok {
                    fs.remove(&sid);
                    0
                } else {
                    let n = fs.entry(sid.clone()).or_insert(0);
                    *n += 1;
                    *n
                }
            };
            self.apply_probe(&sid, ok, &detail, streak);
        }
    }

    async fn http_probe(&self, url: &str) -> (bool, String) {
        match self
            .http
            .get(url)
            .timeout(Duration::from_secs_f64(_PROBE_TIMEOUT))
            .send()
            .await
        {
            Ok(resp) => {
                let code = resp.status().as_u16();
                (code < 400, format!("HTTP {code}"))
            }
            Err(e) => {
                let s = e.to_string();
                let short: String = s.chars().take(200).collect();
                (false, short)
            }
        }
    }

    fn apply_probe(&self, sid: &str, ok: bool, detail: &str, streak: usize) {
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(_) => return,
        };
        let status = match read_service_status(&conn, sid) {
            Some(s) => s,
            None => return,
        };
        let now = time::now_db();
        if ok {
            if status == enums::service_status::STARTING || status == enums::service_status::ERROR {
                let _ = conn.execute(
                    "UPDATE services SET probe_ok = 1, last_probe_at = ?2, status = 'running', \
                     state_changed_at = ?2, last_error = '' WHERE service_id = ?1",
                    params![sid, now],
                );
            } else {
                let _ = conn.execute(
                    "UPDATE services SET probe_ok = 1, last_probe_at = ?2 WHERE service_id = ?1",
                    params![sid, now],
                );
            }
        } else {
            let threshold = config::get().alerts.probe_timeout_warn.max(1) as usize;
            if streak >= threshold {
                let msg = format!("健康检查连续 {streak} 次失败：{detail}");
                if status != enums::service_status::ERROR {
                    let _ = conn.execute(
                        "UPDATE services SET probe_ok = 0, last_probe_at = ?2, status = 'error', \
                         state_changed_at = ?2, last_error = ?3 WHERE service_id = ?1",
                        params![sid, now, msg],
                    );
                } else {
                    let _ = conn.execute(
                        "UPDATE services SET probe_ok = 0, last_probe_at = ?2, last_error = ?3 \
                         WHERE service_id = ?1",
                        params![sid, now, msg],
                    );
                }
            } else {
                let _ = conn.execute(
                    "UPDATE services SET probe_ok = 0, last_probe_at = ?2 WHERE service_id = ?1",
                    params![sid, now],
                );
            }
        }
    }

    // -------------------------------------------------------------- 崩溃兜底
    async fn collect_crashes(&self, online: &[String]) {
        if online.is_empty() {
            return;
        }
        let online_set: HashSet<String> = online.iter().cloned().collect();
        let active: Vec<(String, String)> = {
            let conn = match self.pool.get() {
                Ok(c) => c,
                Err(_) => return,
            };
            read_active_services(&conn)
        };
        let active_ids: HashSet<String> = active
            .iter()
            .filter(|(_, nid)| online_set.contains(nid))
            .map(|(sid, _)| sid.clone())
            .collect();

        let mut nodes: Vec<String> = online.to_vec();
        nodes.sort();
        for node_id in nodes {
            let state = match self.manager.fetch_service_states(&node_id).await {
                Some(s) => s,
                None => continue,
            };
            self.prune_orphans(&node_id, &state).await;
            if active_ids.is_empty() {
                continue;
            }
            for (sid, info) in &state {
                if !active_ids.contains(sid) {
                    continue;
                }
                if info.get("status").and_then(|v| v.as_str()) == Some("running") {
                    continue;
                }
                self.reconcile_one(&node_id, sid, info);
            }
        }
    }

    fn reconcile_one(&self, node_id: &str, sid: &str, info: &Value) {
        let conn = match self.pool.get() {
            Ok(c) => c,
            Err(_) => return,
        };
        let (svc_node, status) = match read_service_node_status(&conn, sid) {
            Some(v) => v,
            None => return,
        };
        if svc_node != node_id {
            return;
        }
        if !is_active_service_state(&status) {
            return;
        }
        let code = info.get("exit_code").and_then(|v| v.as_i64());
        let crashed = matches!(code, Some(c) if c != 0);
        let new_status = if crashed {
            enums::service_status::CRASHED
        } else {
            enums::service_status::STOPPED
        };
        let now = time::now_db();
        let _ = conn.execute(
            "UPDATE services SET status = ?2, state_changed_at = ?3, pid = NULL WHERE service_id = ?1",
            params![sid, new_status, now],
        );
        if let Some(c) = code {
            if c != 0 {
                let msg = format!("进程已退出（code={c}）");
                let _ = conn.execute(
                    "UPDATE services SET last_error = ?2 WHERE service_id = ?1",
                    params![sid, msg],
                );
            }
        }
    }

    async fn prune_orphans(&self, node_id: &str, state: &HashMap<String, Value>) {
        if state.is_empty() {
            return;
        }
        let known: HashSet<String> = {
            let conn = match self.pool.get() {
                Ok(c) => c,
                Err(_) => return,
            };
            read_known_service_ids(&conn, node_id)
        };
        for sid in state.keys() {
            if !known.contains(sid) {
                let _ = self
                    .manager
                    .send_command(node_id, "remove_service", json!({"service_id": sid}), 0.0)
                    .await;
            }
        }
    }

    // -------------------------------------------------------------- 重连恢复
    async fn run_pending_recovery(&self) {
        let now = now_epoch();
        let due: Vec<String> = {
            let mut rd = self.recovery_due.lock().unwrap();
            let due: Vec<String> = rd
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(k, _)| k.clone())
                .collect();
            for k in &due {
                rd.remove(k);
            }
            due
        };
        if due.is_empty() {
            return;
        }
        let online_set = self.online.lock().unwrap().clone();
        for node_id in due {
            if !online_set.contains(&node_id) {
                continue;
            }
            let state = self.manager.fetch_service_states(&node_id).await;
            let planned: Vec<RecoveryItem> = {
                let conn = match self.pool.get() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                read_auto_start_services(&conn, &node_id)
            };
            for item in planned {
                let sid = item.service_id.clone();
                let Some(st) = state.as_ref() else {
                    // 拿不到状态就别动手：宁可漏恢复一次，也不在"其实在跑"的服务上再来一遍 start。
                    break;
                };
                if st.get(&sid).and_then(|v| v.get("status").and_then(|s| s.as_str()))
                    == Some("running")
                {
                    continue;
                }
                let (delivered, _cmd) = self
                    .manager
                    .send_command(
                        &node_id,
                        "start_service",
                        json!({
                            "service_id": sid,
                            "command": item.command,
                            "args": item.args,
                            "auto_restart": item.auto_restart,
                            "auto_start": true,
                        }),
                        0.0,
                    )
                    .await;
                if !delivered {
                    continue;
                }
                let conn = match self.pool.get() {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Some(status) = read_service_status(&conn, &sid) {
                    if !is_active_service_state(&status) {
                        let _ = conn.execute(
                            "UPDATE services SET status = 'starting' WHERE service_id = ?1",
                            [&sid],
                        );
                    }
                }
            }
        }
    }
}
