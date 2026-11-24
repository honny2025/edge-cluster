//! 首页大屏聚合（里程碑 2）。
//!
//! 核心纪律：汇总只统计 online 且采样新鲜的节点；离线节点的旧值绝不参与汇总。
//! 与 Python services/overview.py 对齐。

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDateTime;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::DbPool;
use crate::enums::node_status;
use crate::time;

use super::alerting;
use super::metrics_query::{self, MetricRow};
use super::node_registry::NodeRegistry;

/// 趋势图最多返回多少个数据点（超出则自动加大分桶粒度）。
const MAX_SERIES_POINTS: usize = 180;

fn avg(vals: &[f64]) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    let s: f64 = vals.iter().sum();
    Some((s / vals.len() as f64 * 100.0).round() / 100.0)
}

fn percent_float(used: f64, total: f64) -> Option<f64> {
    metrics_query::percent(Some(used), Some(total))
}

/// 当前时刻的集群快照。
pub fn build_overview(registry: &NodeRegistry) -> Value {
    let now = time::now();
    let nodes = registry.list(None);
    let node_ids: Vec<String> = nodes
        .iter()
        .map(|n| n["node_id"].as_str().unwrap_or("").to_string())
        .collect();

    let conn = registry.pool.get();
    let conn = conn.ok();

    let latest: HashMap<String, MetricRow> = conn
        .as_ref()
        .map(|c| metrics_query::latest_samples(c, Some(&node_ids)))
        .unwrap_or_default();

    let (services_running, services_total) = match conn.as_ref() {
        Some(c) => (
            count_where(c, "SELECT COUNT(*) FROM services WHERE status = 'running'"),
            count_where(c, "SELECT COUNT(*) FROM services"),
        ),
        None => (0, 0),
    };

    // ---- 节点状态统计 ----
    let mut total = 0i64;
    let mut online = 0i64;
    let mut offline = 0i64;
    let mut never_seen = 0i64;
    for n in &nodes {
        total += 1;
        match n["status"].as_str().unwrap_or("") {
            node_status::ONLINE => online += 1,
            node_status::OFFLINE => offline += 1,
            node_status::NEVER_SEEN => never_seen += 1,
            _ => {}
        }
    }

    // ---- 集群资源汇总：只算 online + 采样新鲜的节点 ----
    let mut cpu_vals: Vec<f64> = Vec::new();
    let mut gpu_vals: Vec<f64> = Vec::new();
    let mut npu_vals: Vec<f64> = Vec::new();
    let mut temp_vals: Vec<f64> = Vec::new();
    let mut mem_used_sum: i64 = 0;
    let mut mem_total_sum: i64 = 0;
    let mut sto_used_sum: i64 = 0;
    let mut sto_total_sum: i64 = 0;
    let mut counted_ids: Vec<String> = Vec::new();
    let mut stale_ids: Vec<String> = Vec::new();

    let online_nodes: Vec<&Value> = nodes
        .iter()
        .filter(|n| n["status"] == node_status::ONLINE)
        .collect();
    for n in online_nodes {
        let nid = n["node_id"].as_str().unwrap_or("").to_string();
        let sample = latest.get(&nid);
        if sample.is_none() {
            stale_ids.push(nid);
            continue;
        }
        let sample = sample.unwrap();
        if !metrics_query::is_fresh(Some(sample), now) {
            stale_ids.push(nid);
            continue;
        }
        counted_ids.push(nid);
        if let Some(v) = sample.cpu_usage {
            cpu_vals.push(v);
        }
        if let Some(v) = sample.gpu_usage {
            gpu_vals.push(v);
        }
        if let Some(v) = sample.npu_usage {
            npu_vals.push(v);
        }
        if let Some(v) = sample.temperature {
            temp_vals.push(v);
        }
        if let Some(v) = sample.mem_used {
            mem_used_sum += v;
        }
        if let Some(v) = sample.mem_total {
            mem_total_sum += v;
        }
        if let Some(v) = sample.storage_used {
            sto_used_sum += v;
        }
        if let Some(v) = sample.storage_total {
            sto_total_sum += v;
        }
    }

    let resources = json!({
        "cpu_avg": avg(&cpu_vals),
        "gpu_avg": avg(&gpu_vals),
        "npu_avg": avg(&npu_vals),
        "temperature_avg": avg(&temp_vals),
        "temperature_max": temp_vals.iter().cloned().fold(None, |m, v| Some(m.map_or(v, |x: f64| x.max(v)))),
        "mem_used": if mem_total_sum > 0 { Some(mem_used_sum) } else { None },
        "mem_total": if mem_total_sum > 0 { Some(mem_total_sum) } else { None },
        "mem_percent": metrics_query::percent(Some(mem_used_sum as f64), Some(mem_total_sum as f64)),
        "storage_used": if sto_total_sum > 0 { Some(sto_used_sum) } else { None },
        "storage_total": if sto_total_sum > 0 { Some(sto_total_sum) } else { None },
        "storage_percent": metrics_query::percent(Some(sto_used_sum as f64), Some(sto_total_sum as f64)),
        "counted_nodes": counted_ids.len(),
        "online_nodes": online,
        "stale_nodes": stale_ids,
    });

    let alerts = alerting::evaluate(&nodes, &latest);

    // 各节点最新指标：离线或采样陈旧的节点一律给 None。
    let mut nodes_metrics = serde_json::Map::new();
    for n in &nodes {
        let nid = n["node_id"].as_str().unwrap_or("").to_string();
        let sample = latest.get(&nid);
        let usable = n["status"] == node_status::ONLINE && metrics_query::is_fresh(sample.map(|s| s), now);
        let val = if usable {
            sample.map(|s| s.public_json()).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        nodes_metrics.insert(nid, val);
    }

    let abnormal: Vec<Value> = nodes
        .iter()
        .filter(|n| n["status"] != node_status::ONLINE)
        .map(|n| {
            json!({
                "node_id": n["node_id"],
                "name": n["name"],
                "status": n["status"],
                "last_heartbeat": n["last_heartbeat"],
                "ip": n["ip"],
                "is_simulated": n["is_simulated"],
            })
        })
        .collect();

    let critical = alerts.iter().filter(|a| a["level"] == "critical").count();
    let warning = alerts.iter().filter(|a| a["level"] == "warning").count();

    json!({
        "generated_at": time::format_iso(&now),
        "node_stats": {
            "total": total,
            "online": online,
            "offline": offline,
            "never_seen": never_seen,
        },
        "resources": resources,
        "services": {"running": services_running, "total": services_total},
        "alerts": alerts,
        "alert_summary": {
            "critical": critical,
            "warning": warning,
            "total": alerts.len(),
        },
        "abnormal_nodes": abnormal,
        "nodes_metrics": nodes_metrics,
        "recent_deployments": [],
        "stale_nodes": stale_ids,
    })
}

fn count_where(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
}

struct PerNode {
    mu: Vec<f64>,
    mt: Vec<f64>,
    su: Vec<f64>,
    st: Vec<f64>,
}

impl Default for PerNode {
    fn default() -> Self {
        Self { mu: Vec::new(), mt: Vec::new(), su: Vec::new(), st: Vec::new() }
    }
}

struct Bucket {
    ts: NaiveDateTime,
    cpu: Vec<f64>,
    gpu: Vec<f64>,
    npu: Vec<f64>,
    temp: Vec<f64>,
    per_node: HashMap<String, PerNode>,
    nodes: HashSet<String>,
    tiers: HashSet<String>,
}

impl Bucket {
    fn new() -> Self {
        Self {
            ts: NaiveDateTime::MIN,
            cpu: Vec::new(),
            gpu: Vec::new(),
            npu: Vec::new(),
            temp: Vec::new(),
            per_node: HashMap::new(),
            nodes: HashSet::new(),
            tiers: HashSet::new(),
        }
    }
}

fn empty_series(minutes: i64, online_nodes: usize, scope: &str) -> Value {
    json!({
        "minutes": minutes,
        "bucket_seconds": 0,
        "count": 0,
        "scope": scope,
        "online_nodes": online_nodes,
        "items": [],
    })
}

/// 集群资源时间序列：每个时间桶内，对所有节点做平均 / 汇总。
pub fn build_cluster_series(registry: &NodeRegistry, minutes: i64) -> Value {
    let now = time::now();
    let since = now - chrono::Duration::minutes(minutes);

    let online_ids: Vec<String> = registry
        .list(None)
        .into_iter()
        .filter(|n| n["status"] == node_status::ONLINE)
        .map(|n| n["node_id"].as_str().unwrap_or("").to_string())
        .collect();

    let conn = match registry.pool.get() {
        Ok(c) => c,
        Err(_) => return empty_series(minutes, online_ids.len(), "online"),
    };

    let window_ids = metrics_query::window_node_ids(&conn, since, now);
    let within_raw = since >= metrics_query::raw_floor(now);
    let scope = if within_raw { "online" } else { "all" };
    let ids: Vec<String> = if within_raw {
        online_ids.iter().filter(|i| window_ids.contains(i)).cloned().collect()
    } else {
        window_ids.clone()
    };

    if ids.is_empty() {
        return empty_series(minutes, online_ids.len(), scope);
    }

    let (rows, plan) = metrics_query::fetch_history(&conn, &ids, since, now);
    drop(conn);

    if rows.is_empty() {
        return empty_series(minutes, online_ids.len(), scope);
    }

    let span = ((now - since).num_milliseconds() as f64 / 1000.0).max(1.0);
    let bucket = ((span / MAX_SERIES_POINTS as f64).floor() as i64).max(5);

    let base = rows[0].0.ts;
    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();
    for (r, tier) in &rows {
        let idx = ((r.ts - base).num_milliseconds() as f64 / 1000.0 / bucket as f64).floor() as i64;
        let b = buckets.entry(idx).or_insert_with(Bucket::new);
        b.ts = r.ts;
        if let Some(v) = r.cpu_usage {
            b.cpu.push(v);
        }
        if let Some(v) = r.gpu_usage {
            b.gpu.push(v);
        }
        if let Some(v) = r.npu_usage {
            b.npu.push(v);
        }
        if let Some(v) = r.temperature {
            b.temp.push(v);
        }
        let pn = b.per_node.entry(r.node_id.clone()).or_default();
        if let Some(v) = r.mem_used {
            pn.mu.push(v as f64);
        }
        if let Some(v) = r.mem_total {
            pn.mt.push(v as f64);
        }
        if let Some(v) = r.storage_used {
            pn.su.push(v as f64);
        }
        if let Some(v) = r.storage_total {
            pn.st.push(v as f64);
        }
        b.nodes.insert(r.node_id.clone());
        b.tiers.insert(tier.clone());
    }

    let mut items: Vec<Value> = Vec::new();
    for b in buckets.values() {
        let mut mu = 0.0f64;
        let mut mt = 0.0f64;
        let mut su = 0.0f64;
        let mut st = 0.0f64;
        for pn in b.per_node.values() {
            if !pn.mu.is_empty() && !pn.mt.is_empty() {
                mu += avg(&pn.mu).unwrap_or(0.0);
                mt += avg(&pn.mt).unwrap_or(0.0);
            }
            if !pn.su.is_empty() && !pn.st.is_empty() {
                su += avg(&pn.su).unwrap_or(0.0);
                st += avg(&pn.st).unwrap_or(0.0);
            }
        }
        let tier_label = if b.tiers.len() == 1 {
            b.tiers.iter().next().cloned().unwrap_or_else(|| "mixed".to_string())
        } else {
            "mixed".to_string()
        };
        items.push(json!({
            "ts": time::format_iso(&b.ts),
            "cpu_avg": avg(&b.cpu),
            "gpu_avg": avg(&b.gpu),
            "npu_avg": avg(&b.npu),
            "temperature_avg": avg(&b.temp),
            "mem_used": if mt > 0.0 { Some(mu as i64) } else { None },
            "mem_total": if mt > 0.0 { Some(mt as i64) } else { None },
            "mem_percent": percent_float(mu, mt),
            "storage_used": if st > 0.0 { Some(su as i64) } else { None },
            "storage_total": if st > 0.0 { Some(st as i64) } else { None },
            "storage_percent": percent_float(su, st),
            "node_count": b.nodes.len(),
            "tier": tier_label,
        }));
    }

    let tiers: Vec<Value> = plan
        .iter()
        .map(|(t, a, b)| json!({"tier": t, "from": time::format_iso(a), "to": time::format_iso(b)}))
        .collect();

    json!({
        "minutes": minutes,
        "bucket_seconds": bucket,
        "online_nodes": online_ids.len(),
        "scope": scope,
        "tiers": tiers,
        "count": items.len(),
        "items": items,
    })
}

/// 给前端用：该节点当前指标是否可展示。
#[allow(dead_code)] // 与 Python 版对齐
pub fn node_metric_status(pool: &DbPool, node: &Value) -> Value {
    let nid = node["node_id"].as_str().unwrap_or("").to_string();
    let latest: Option<MetricRow> = pool
        .get()
        .ok()
        .and_then(|c| metrics_query::latest_samples(&c, Some(&[nid.clone()])).remove(&nid));

    let now = time::now();
    let online = node["status"] == node_status::ONLINE;
    let fresh = metrics_query::is_fresh(latest.as_ref(), now);
    json!({
        "displayable": online && fresh,
        "online": online,
        "fresh": fresh,
        "age_seconds": metrics_query::sample_age_seconds(latest.as_ref(), now),
    })
}
