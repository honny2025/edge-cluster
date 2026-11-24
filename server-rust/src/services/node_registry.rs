//! 节点注册与状态维护（三态状态机 + 指标入库）。
//!
//! 与 Python 版 services/node_registry.py 对齐。关键纪律：
//! WebSocket 断开不直接判离线 —— 由心跳巡检统一裁决。

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::Value;

use crate::db::{DbPool, PooledConn};
use crate::enums::node_status;
use crate::event_bus::EventBus;
use crate::time;

// ------------------------------------------------------------------ 值转换

fn prefer_known(new: Option<&Value>, old: &str) -> String {
    match new {
        Some(Value::String(s)) => {
            let v = s.trim();
            if !v.is_empty() && v != "Unknown" {
                v.to_string()
            } else {
                old.to_string()
            }
        }
        _ => old.to_string(),
    }
}

fn tri_state(v: Option<&Value>, default: i64) -> i64 {
    match v {
        Some(Value::Bool(b)) => {
            if *b {
                1
            } else {
                0
            }
        }
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|i| matches!(i, -1 | 0 | 1))
            .unwrap_or(default),
        Some(Value::String(s)) => s
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|i| matches!(i, -1 | 0 | 1))
            .unwrap_or(default),
        _ => default,
    }
}

fn short_str_or_none(v: Option<&Value>, limit: usize) -> Option<String> {
    let s = match v {
        Some(Value::String(s)) => s,
        _ => return None,
    };
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.chars().take(limit).collect())
}

fn float_or_none(v: Option<&Value>, lo: f64, hi: f64) -> Option<f64> {
    let f = match v {
        Some(Value::Number(n)) => n.as_f64()?,
        Some(Value::String(s)) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if f.is_nan() {
        return None;
    }
    if f >= lo && f <= hi {
        Some(f)
    } else {
        None
    }
}

fn int_or_none(v: Option<&Value>) -> Option<i64> {
    let n: i64 = match v {
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i
            } else {
                n.as_f64()? as i64
            }
        }
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            (s.parse::<f64>().ok()?) as i64
        }
        _ => return None,
    };
    if n >= 0 {
        Some(n)
    } else {
        None
    }
}

// ------------------------------------------------------------------ 行模型

const NODE_COLUMNS: &str = "node_id, name, model, soc, android_version, agent_version, ip, \
    cpu, gpu, npu, qnn_version, is_root, capabilities, device_token, device_token_issued_at, \
    status, last_heartbeat, online_since, uptime_seconds, missed_heartbeats, is_simulated, \
    note, created_at, updated_at";

struct NodeRow {
    node_id: String,
    name: String,
    model: String,
    soc: String,
    android_version: String,
    agent_version: String,
    ip: String,
    cpu: String,
    gpu: String,
    npu: String,
    qnn_version: String,
    is_root: i64,
    capabilities: Option<String>,
    device_token: String,
    device_token_issued_at: Option<String>,
    status: String,
    last_heartbeat: Option<String>,
    online_since: Option<String>,
    uptime_seconds: i64,
    missed_heartbeats: i64,
    is_simulated: i64,
    note: String,
    created_at: String,
    updated_at: String,
}

fn read_node(conn: &Connection, node_id: &str) -> Option<NodeRow> {
    conn.query_row(
        &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE node_id = ?1"),
        [node_id],
        row_to_node,
    )
    .ok()
}

fn row_to_node(r: &rusqlite::Row) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        node_id: r.get(0)?,
        name: r.get(1)?,
        model: r.get(2)?,
        soc: r.get(3)?,
        android_version: r.get(4)?,
        agent_version: r.get(5)?,
        ip: r.get(6)?,
        cpu: r.get(7)?,
        gpu: r.get(8)?,
        npu: r.get(9)?,
        qnn_version: r.get(10)?,
        is_root: r.get(11)?,
        capabilities: r.get(12)?,
        device_token: r.get(13)?,
        device_token_issued_at: r.get(14)?,
        status: r.get(15)?,
        last_heartbeat: r.get(16)?,
        online_since: r.get(17)?,
        uptime_seconds: r.get(18)?,
        missed_heartbeats: r.get(19)?,
        is_simulated: r.get(20)?,
        note: r.get(21)?,
        created_at: r.get(22)?,
        updated_at: r.get(23)?,
    })
}

impl NodeRow {
    fn snapshot(&self) -> Value {
        let capabilities: Value = self
            .capabilities
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Object(Default::default()));
        serde_json::json!({
            "node_id": self.node_id,
            "name": self.name,
            "model": self.model,
            "soc": self.soc,
            "android_version": self.android_version,
            "cpu": self.cpu,
            "gpu": self.gpu,
            "npu": self.npu,
            "qnn_version": self.qnn_version,
            "is_root": self.is_root,
            "capabilities": capabilities,
            "agent_version": self.agent_version,
            "ip": self.ip,
            "status": self.status,
            "last_heartbeat": self.last_heartbeat.as_deref().map(time::db_to_iso),
            "last_heartbeat_ts": self.last_heartbeat.as_deref().and_then(time::db_to_epoch),
            "online_since": self.online_since.as_deref().map(time::db_to_iso),
            "uptime_seconds": self.uptime_seconds,
            "is_simulated": self.is_simulated != 0,
            "note": self.note,
            "created_at": Some(time::db_to_iso(&self.created_at)),
            "updated_at": Some(time::db_to_iso(&self.updated_at)),
        })
    }
}

// ------------------------------------------------------------------ 服务

pub struct NodeRegistry {
    pub pool: DbPool,
    pub bus: EventBus,
}

impl NodeRegistry {
    pub fn new(pool: DbPool, bus: EventBus) -> Self {
        Self { pool, bus }
    }

    fn conn(&self) -> Option<PooledConn> {
        self.pool.get().ok()
    }

    /// Agent 首次注册（或重连后重新注册）。返回节点快照。
    pub fn register(&self, info: &Value, ip: &str, is_simulated: bool) -> Value {
        let node_id = info
            .get("node_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if node_id.is_empty() {
            // 调用方（WS）应已在 register 前拦截空 node_id，这里兜底返回空快照
            return Value::Null;
        }

        let now = time::now_db();
        let conn = match self.conn() {
            Some(c) => c,
            None => return Value::Null,
        };

        let existing = read_node(&conn, &node_id);
        let is_new = existing.is_none();

        let g = |k: &str| info.get(k);
        let gs = |k: &str| g(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

        let name = if is_new {
            gs("name").clone()
        } else {
            let old = existing.as_ref().map(|n| n.name.clone()).unwrap_or_default();
            if old.is_empty() {
                let v = gs("name");
                if v.is_empty() {
                    node_id.clone()
                } else {
                    v
                }
            } else {
                old
            }
        };
        // Python：if is_new or not node.name: node.name = info.name or node_id
        let name = if is_new || name.is_empty() {
            let v = gs("name");
            if v.is_empty() {
                node_id.clone()
            } else {
                v
            }
        } else {
            name
        };

        let old = existing.as_ref();
        let model = if !gs("model").is_empty() { gs("model") } else { old.map(|n| n.model.clone()).unwrap_or_default() };
        let soc = if !gs("soc").is_empty() { gs("soc") } else { old.map(|n| n.soc.clone()).unwrap_or_default() };
        let android_version = if !gs("android_version").is_empty() { gs("android_version") } else { old.map(|n| n.android_version.clone()).unwrap_or_default() };
        let agent_version = if !gs("agent_version").is_empty() { gs("agent_version") } else { old.map(|n| n.agent_version.clone()).unwrap_or_default() };

        let cpu = prefer_known(g("cpu"), old.map(|n| n.cpu.as_str()).unwrap_or(""));
        let gpu = prefer_known(g("gpu"), old.map(|n| n.gpu.as_str()).unwrap_or(""));
        let npu = prefer_known(g("npu"), old.map(|n| n.npu.as_str()).unwrap_or(""));
        let qnn_version = prefer_known(g("qnn_version"), old.map(|n| n.qnn_version.as_str()).unwrap_or(""));

        let is_root = if info.get("is_root").is_some() {
            tri_state(g("is_root"), old.map(|n| n.is_root).unwrap_or(-1))
        } else {
            old.map(|n| n.is_root).unwrap_or(-1)
        };

        let capabilities: Option<String> = match g("capabilities") {
            Some(Value::Object(m)) if !m.is_empty() => Some(serde_json::to_string(m).unwrap_or_default()),
            _ => old.and_then(|n| n.capabilities.clone()),
        };

        let reported_ip = gs("ip");
        let observed_ip = ip.to_string();
        let ip_val = if !observed_ip.is_empty() {
            observed_ip.clone()
        } else if !reported_ip.is_empty() {
            reported_ip.clone()
        } else {
            old.map(|n| n.ip.clone()).unwrap_or_default()
        };

        let is_simulated_flag = if is_simulated {
            1
        } else {
            old.map(|n| n.is_simulated).unwrap_or(0)
        };

        let prev_status = old.as_ref().map(|n| n.status.clone()).unwrap_or(node_status::NEVER_SEEN.to_string());
        let online_since = if prev_status != node_status::ONLINE {
            Some(now.clone())
        } else {
            old.as_ref().and_then(|n| n.online_since.clone()).or(Some(now.clone()))
        };

        let created_at = old.as_ref().map(|n| n.created_at.clone()).unwrap_or_else(|| now.clone());
        let note = old.as_ref().map(|n| n.note.clone()).unwrap_or_default();
        let device_token = old.as_ref().map(|n| n.device_token.clone()).unwrap_or_default();
        let device_token_issued_at = old.as_ref().and_then(|n| n.device_token_issued_at.clone());

        let _ = if is_new {
            conn.execute(
                "INSERT INTO nodes (node_id, name, model, soc, android_version, agent_version, ip, cpu, gpu, npu, qnn_version, is_root, capabilities, device_token, device_token_issued_at, status, last_heartbeat, online_since, uptime_seconds, missed_heartbeats, is_simulated, note, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
                rusqlite::params![
                    node_id, name, model, soc, android_version, agent_version, ip_val,
                    cpu, gpu, npu, qnn_version, is_root, capabilities,
                    device_token, device_token_issued_at,
                    node_status::ONLINE, now, online_since, 0i64, 0i64, is_simulated_flag,
                    note, created_at, now
                ],
            )
        } else {
            conn.execute(
                "UPDATE nodes SET name=?2, model=?3, soc=?4, android_version=?5, agent_version=?6, ip=?7, cpu=?8, gpu=?9, npu=?10, qnn_version=?11, is_root=?12, capabilities=?13, status=?14, last_heartbeat=?15, online_since=?16, uptime_seconds=?17, missed_heartbeats=?18, is_simulated=?19, updated_at=?20 WHERE node_id=?1",
                rusqlite::params![
                    node_id, name, model, soc, android_version, agent_version, ip_val,
                    cpu, gpu, npu, qnn_version, is_root, capabilities,
                    node_status::ONLINE, now, online_since, 0i64, 0i64, is_simulated_flag, now
                ],
            )
        };

        let row = read_node(&conn, &node_id).unwrap_or_else(|| NodeRow {
            node_id: node_id.clone(),
            name: name.clone(),
            model: model.clone(),
            soc: soc.clone(),
            android_version: android_version.clone(),
            agent_version: agent_version.clone(),
            ip: ip_val.clone(),
            cpu: cpu.clone(),
            gpu: gpu.clone(),
            npu: npu.clone(),
            qnn_version: qnn_version.clone(),
            is_root,
            capabilities: capabilities.clone(),
            device_token,
            device_token_issued_at,
            status: node_status::ONLINE.to_string(),
            last_heartbeat: Some(now.clone()),
            online_since: online_since.clone(),
            uptime_seconds: 0,
            missed_heartbeats: 0,
            is_simulated: is_simulated_flag,
            note: note.clone(),
            created_at,
            updated_at: now.clone(),
        });

        let snapshot = row.snapshot();
        self.bus
            .publish_threadsafe(if is_new { "node_online" } else { "node_updated" }, snapshot.clone());
        snapshot
    }

    /// 处理一次心跳：刷新时间、恢复在线态、写入指标采样。返回快照。
    pub fn heartbeat(&self, node_id: &str, payload: &Value, peer_ip: &str) -> Option<Value> {
        let now = time::now_db();
        let conn = self.conn()?;

        let existing = read_node(&conn, node_id);
        let mut node = match existing {
            Some(n) => n,
            None => {
                // 平台重启后库丢失 / 节点未注册就心跳 —— 自动补建
                let _ = conn.execute(
                    "INSERT INTO nodes (node_id, name, status, created_at, updated_at) VALUES (?1,?1,?2,?3,?3)",
                    rusqlite::params![node_id, node_status::NEVER_SEEN, now],
                );
                read_node(&conn, node_id)?
            }
        };

        let recovered = node.status != node_status::ONLINE;
        if recovered {
            node.online_since = Some(now.clone());
            node.uptime_seconds = 0;
        }
        node.status = node_status::ONLINE.to_string();
        node.last_heartbeat = Some(now.clone());
        node.missed_heartbeats = 0;

        let reported_ip = payload.get("ip").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !peer_ip.is_empty() {
            node.ip = peer_ip.to_string();
        } else if !reported_ip.is_empty() && node.ip.is_empty() {
            node.ip = reported_ip.clone();
        }
        if node.name.is_empty() {
            if let Some(n) = payload.get("name").and_then(|v| v.as_str()) {
                if !n.is_empty() {
                    node.name = n.to_string();
                }
            }
        }

        if let Some(metrics) = payload.get("metrics") {
            store_metrics(&conn, node_id, metrics, &now);
        }

        // refresh_uptime
        if node.status == node_status::ONLINE {
            if let Some(since) = &node.online_since {
                if let Some(elapsed) = time::db_elapsed_secs(&now, since) {
                    node.uptime_seconds = if elapsed > 0.0 { elapsed as i64 } else { 0 };
                }
            }
        }

        let _ = conn.execute(
            "UPDATE nodes SET status=?2, last_heartbeat=?3, missed_heartbeats=?4, ip=?5, name=?6, online_since=?7, uptime_seconds=?8, updated_at=?9 WHERE node_id=?1",
            rusqlite::params![
                node_id, node.status, node.last_heartbeat, node.missed_heartbeats,
                node.ip, node.name, node.online_since, node.uptime_seconds, now
            ],
        );

        let snapshot = node.snapshot();
        if recovered {
            self.bus.publish_threadsafe("node_online", snapshot.clone());
        }
        Some(snapshot)
    }

    /// 平台启动时把库里残留的 online 状态归零。
    pub fn reset_online_on_boot(&self) -> i64 {
        let conn = match self.conn() {
            Some(c) => c,
            None => return 0,
        };
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE status = ?1",
                [node_status::ONLINE],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let _ = conn.execute(
            "UPDATE nodes SET status = ?2, uptime_seconds = 0, online_since = NULL WHERE status = ?1",
            rusqlite::params![node_status::ONLINE, node_status::OFFLINE],
        );
        if count > 0 {
            self.bus.publish_threadsafe(
                "node_status_reset",
                serde_json::json!({"count": count}),
            );
        }
        count
    }

    /// 独立指标上报通道（M2）。写失败静默。
    #[allow(dead_code)] // 与 Python 版对齐，当前指标经 heartbeat 通道写入
    pub fn store_metrics_payload(&self, node_id: &str, metrics: &Value) {
        let now = time::now_db();
        if let Some(conn) = self.conn() {
            if read_node(&conn, node_id).is_some() {
                store_metrics(&conn, node_id, metrics, &now);
            }
        }
    }

    pub fn get(&self, node_id: &str) -> Option<Value> {
        let conn = self.conn()?;
        let mut node = read_node(&conn, node_id)?;
        if node.status == node_status::ONLINE {
            if let Some(since) = &node.online_since {
                let now = time::now_db();
                if let Some(elapsed) = time::db_elapsed_secs(&now, since) {
                    node.uptime_seconds = if elapsed > 0.0 { elapsed as i64 } else { 0 };
                }
                let _ = conn.execute(
                    "UPDATE nodes SET uptime_seconds = ?2 WHERE node_id = ?1",
                    rusqlite::params![node_id, node.uptime_seconds],
                );
            }
        }
        Some(node.snapshot())
    }

    pub fn list(&self, status: Option<&str>) -> Vec<Value> {
        let conn = match self.conn() {
            Some(c) => c,
            None => return vec![],
        };
        let mut stmt = match status {
            Some(_) => conn
                .prepare(&format!("SELECT {NODE_COLUMNS} FROM nodes WHERE status = ?1"))
                .ok(),
            None => conn.prepare(&format!("SELECT {NODE_COLUMNS} FROM nodes")).ok(),
        };
        let mut rows: Vec<NodeRow> = vec![];
        if let Some(stmt) = stmt.as_mut() {
            let map = |r: &rusqlite::Row| row_to_node(r);
            let iter = match status {
                Some(s) => stmt.query_map([s], map),
                None => stmt.query_map([], map),
            };
            if let Ok(iter) = iter {
                for r in iter.flatten() {
                    rows.push(r);
                }
            }
        }
        let mut out: Vec<Value> = rows.iter().map(|n| n.snapshot()).collect();
        // 在线优先 + 名称排序
        out.sort_by(|a, b| {
            let a_online = a["status"] == node_status::ONLINE;
            let b_online = b["status"] == node_status::ONLINE;
            match (a_online, b_online) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let an = a["name"].as_str().unwrap_or("");
                    let bn = b["name"].as_str().unwrap_or("");
                    an.cmp(bn)
                }
            }
        });
        out
    }

    pub fn stats(&self) -> HashMap<String, i64> {
        let mut counts: HashMap<String, i64> = HashMap::new();
        counts.insert("total".into(), 0);
        counts.insert(node_status::ONLINE.into(), 0);
        counts.insert(node_status::OFFLINE.into(), 0);
        counts.insert(node_status::NEVER_SEEN.into(), 0);
        if let Some(conn) = self.conn() {
            if let Ok(mut stmt) = conn.prepare("SELECT status, COUNT(*) FROM nodes GROUP BY status") {
                if let Ok(iter) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
                    for row in iter.flatten() {
                        counts.insert(row.0, row.1);
                    }
                }
            }
        }
        let total: i64 = counts.values().sum();
        counts.insert("total".into(), total);
        counts
    }

    #[allow(dead_code)] // 与 Python 版对齐，当前改名走 api 内联 UPDATE
    pub fn rename(&self, node_id: &str, name: &str) -> bool {
        let conn = match self.conn() {
            Some(c) => c,
            None => return false,
        };
        let n = conn
            .execute(
                "UPDATE nodes SET name = ?2, updated_at = ?3 WHERE node_id = ?1",
                rusqlite::params![node_id, name, time::now_db()],
            )
            .unwrap_or(0);
        if n > 0 {
            self.bus.publish_threadsafe(
                "node_updated",
                serde_json::json!({"node_id": node_id, "name": name}),
            );
        }
        n > 0
    }

    pub fn delete(&self, node_id: &str) -> bool {
        let conn = match self.conn() {
            Some(c) => c,
            None => return false,
        };
        let n = conn
            .execute("DELETE FROM nodes WHERE node_id = ?1", [node_id])
            .unwrap_or(0);
        if n > 0 {
            self.bus.publish_threadsafe(
                "node_deleted",
                serde_json::json!({"node_id": node_id}),
            );
        }
        n > 0
    }
}

/// 把指标写一条采样记录。空指标直接跳过。
fn store_metrics(conn: &Connection, node_id: &str, metrics: &Value, now: &str) {
    if !metrics.is_object() {
        return;
    }
    let g = |k: &str| metrics.get(k);

    let extra: Option<String> = match g("extra") {
        Some(Value::Object(_)) => Some(g("extra").unwrap().to_string()),
        _ => None,
    };

    let _ = conn.execute(
        "INSERT INTO metric_samples (node_id, ts, cpu_usage, gpu_usage, npu_usage, mem_used, mem_total, storage_used, storage_total, temperature, power_watts, net_rx_bytes, net_tx_bytes, load_avg, cpu_temp, gpu_temp, cpu_freq_mhz, gpu_freq_mhz, battery_percent, battery_temp, battery_status, extra)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
        rusqlite::params![
            node_id,
            now,
            float_or_none(g("cpu_usage"), 0.0, 100000.0),
            float_or_none(g("gpu_usage"), 0.0, 100000.0),
            float_or_none(g("npu_usage"), 0.0, 100000.0),
            int_or_none(g("mem_used")),
            int_or_none(g("mem_total")),
            int_or_none(g("storage_used")),
            int_or_none(g("storage_total")),
            float_or_none(g("temperature"), -50.0, 200.0),
            float_or_none(g("power_watts"), 0.0, 500.0),
            int_or_none(g("net_rx_bytes")),
            int_or_none(g("net_tx_bytes")),
            float_or_none(g("load_avg"), 0.0, 10000.0),
            float_or_none(g("cpu_temp"), -50.0, 200.0),
            float_or_none(g("gpu_temp"), -50.0, 200.0),
            float_or_none(g("cpu_freq_mhz"), 0.0, 100000.0),
            float_or_none(g("gpu_freq_mhz"), 0.0, 100000.0),
            float_or_none(g("battery_percent"), 0.0, 100.0),
            float_or_none(g("battery_temp"), -50.0, 200.0),
            short_str_or_none(g("battery_status"), 24),
            extra,
        ],
    );
}
