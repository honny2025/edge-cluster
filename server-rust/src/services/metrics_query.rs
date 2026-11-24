//! 指标聚合查询（里程碑 2）。
//!
//! 职责很窄：
//! 1. 取每个节点的最新一条指标采样（一次 SQL 拿全，不做 N+1）。
//! 2. 用「新鲜度」过滤陈旧采样，避免拿旧值当当前值。
//! 3. 按 `tier_plan` 把查询窗口切到 raw/minute/hour 三层，取历史曲线。
//!
//! 与 Python 版 services/metrics_query.py 对齐。纪律：
//! - 集群汇总只统计 online 节点；采样比心跳周期落后太多（STALE_FACTOR 倍）视为陈旧。
//! - 指标为 NULL 一律不参与、不造假。

use std::collections::HashMap;

use chrono::NaiveDateTime;
use rusqlite::{Connection, Row};

use crate::config;
use crate::time;

/// 采样新鲜度容忍倍数：采样落后超过 heartbeat_interval * 该倍数即视为陈旧。
pub const STALE_FACTOR: f64 = 3.0;

/// 原始采样层名。另外两层用 KIND_MINUTE / KIND_HOUR。
pub const TIER_RAW: &str = "raw";
pub const KIND_MINUTE: &str = "minute";
pub const KIND_HOUR: &str = "hour";

/// 一张指标行的统一形态（metric_samples 与 metric_rollups 同名列对齐）。
#[derive(Debug, Clone)]
pub struct MetricRow {
    pub node_id: String,
    pub ts: NaiveDateTime,
    pub cpu_usage: Option<f64>,
    pub gpu_usage: Option<f64>,
    pub npu_usage: Option<f64>,
    pub mem_used: Option<i64>,
    pub mem_total: Option<i64>,
    pub storage_used: Option<i64>,
    pub storage_total: Option<i64>,
    pub temperature: Option<f64>,
    pub cpu_temp: Option<f64>,
    pub gpu_temp: Option<f64>,
    pub cpu_freq_mhz: Option<f64>,
    pub gpu_freq_mhz: Option<f64>,
    pub battery_percent: Option<f64>,
    pub battery_temp: Option<f64>,
    pub battery_status: Option<String>,
    pub power_watts: Option<f64>,
    pub net_rx_bytes: Option<i64>,
    pub net_tx_bytes: Option<i64>,
    pub load_avg: Option<f64>,
    pub extra: Option<String>,
}

impl MetricRow {
    /// 对应 Python `_row_to_dict`（供 overview / alerting 用）。
    /// 注意：这里把 `ts` 留在 struct 里，`public_json` 只输出 `ts_iso`（丢弃 datetime）。
    pub fn ts_iso(&self) -> String {
        time::format_iso(&self.ts)
    }

    /// 指标 → 可 JSON 化的 Value（对应 `_metric_public`：丢弃内部 datetime，只留 ts_iso）。
    pub fn public_json(&self) -> serde_json::Value {
        let extra: serde_json::Value = self
            .extra
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "ts_iso": self.ts_iso(),
            "cpu_usage": self.cpu_usage,
            "gpu_usage": self.gpu_usage,
            "npu_usage": self.npu_usage,
            "mem_used": self.mem_used,
            "mem_total": self.mem_total,
            "storage_used": self.storage_used,
            "storage_total": self.storage_total,
            "temperature": self.temperature,
            "power_watts": self.power_watts,
            "net_rx_bytes": self.net_rx_bytes,
            "net_tx_bytes": self.net_tx_bytes,
            "load_avg": self.load_avg,
            "extra": extra,
        })
    }

    /// 对应 Python `MetricOut.from_row`：输出 `ts`（isoformat）+ 全部指标字段
    /// （含 cpu_temp / gpu_temp / cpu_freq_mhz / gpu_freq_mhz / 电池 等扩展字段）。
    /// 与 `public_json` 的区别：`public_json` 给大屏 overview 用（只留 ts_iso + 12 字段），
    /// 这里是节点指标历史接口的形状（`ts` + 全部 21 个字段）。
    pub fn api_json(&self) -> serde_json::Value {
        let extra: serde_json::Value = self
            .extra
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "ts": self.ts_iso(),
            "cpu_usage": self.cpu_usage,
            "gpu_usage": self.gpu_usage,
            "npu_usage": self.npu_usage,
            "mem_used": self.mem_used,
            "mem_total": self.mem_total,
            "storage_used": self.storage_used,
            "storage_total": self.storage_total,
            "temperature": self.temperature,
            "power_watts": self.power_watts,
            "net_rx_bytes": self.net_rx_bytes,
            "net_tx_bytes": self.net_tx_bytes,
            "load_avg": self.load_avg,
            "cpu_temp": self.cpu_temp,
            "gpu_temp": self.gpu_temp,
            "cpu_freq_mhz": self.cpu_freq_mhz,
            "gpu_freq_mhz": self.gpu_freq_mhz,
            "battery_percent": self.battery_percent,
            "battery_temp": self.battery_temp,
            "battery_status": self.battery_status,
            "extra": extra,
        })
    }
}

/// 两张表同名的指标列（顺序固定，供 SELECT 与行映射共用）。
const METRIC_COLS: &str = "node_id, ts, cpu_usage, gpu_usage, npu_usage, mem_used, mem_total, \
    storage_used, storage_total, temperature, cpu_temp, gpu_temp, cpu_freq_mhz, gpu_freq_mhz, \
    battery_percent, battery_temp, battery_status, power_watts, net_rx_bytes, net_tx_bytes, \
    load_avg, extra";

fn row_to_metric(r: &Row) -> rusqlite::Result<MetricRow> {
    let ts_raw: String = r.get(1)?;
    let ts = time::parse_db(&ts_raw).unwrap_or(NaiveDateTime::MIN);
    Ok(MetricRow {
        node_id: r.get(0)?,
        ts,
        cpu_usage: r.get(2)?,
        gpu_usage: r.get(3)?,
        npu_usage: r.get(4)?,
        mem_used: r.get(5)?,
        mem_total: r.get(6)?,
        storage_used: r.get(7)?,
        storage_total: r.get(8)?,
        temperature: r.get(9)?,
        cpu_temp: r.get(10)?,
        gpu_temp: r.get(11)?,
        cpu_freq_mhz: r.get(12)?,
        gpu_freq_mhz: r.get(13)?,
        battery_percent: r.get(14)?,
        battery_temp: r.get(15)?,
        battery_status: r.get(16)?,
        power_watts: r.get(17)?,
        net_rx_bytes: r.get(18)?,
        net_tx_bytes: r.get(19)?,
        load_avg: r.get(20)?,
        extra: r.get(21)?,
    })
}

/// 某一层能覆盖的最早时刻。`days <= 0` 表示不清理，返回 `datetime.min`。
fn tier_floor(days: i64, now: NaiveDateTime) -> NaiveDateTime {
    if days <= 0 {
        return NaiveDateTime::MIN;
    }
    now - chrono::Duration::days(days)
}

/// 原始采样层能覆盖的最早时刻。
pub fn raw_floor(now: NaiveDateTime) -> NaiveDateTime {
    tier_floor(config::get().server.metric_raw_retention_days, now)
}

fn minute_floor(now: NaiveDateTime) -> NaiveDateTime {
    tier_floor(config::get().server.metric_minute_retention_days, now)
}

fn hour_floor(now: NaiveDateTime) -> NaiveDateTime {
    tier_floor(config::get().server.metric_hour_retention_days, now)
}

/// 把 [since, until) 切成互不重叠的段，每段交给当时最细的可用层。
/// 返回按时间升序排列的 [(tier, from, to), ...]。
pub fn tier_plan(
    since: NaiveDateTime,
    until: NaiveDateTime,
    now: NaiveDateTime,
) -> Vec<(String, NaiveDateTime, NaiveDateTime)> {
    let raw_floor = raw_floor(now);
    let minute_floor = minute_floor(now);
    let hour_floor = hour_floor(now);

    let mut segments: Vec<(String, NaiveDateTime, NaiveDateTime)> = Vec::new();

    let raw_a = since.max(raw_floor);
    let raw_b = until;
    if raw_b > raw_a {
        segments.push((TIER_RAW.to_string(), raw_a, raw_b));
    }

    let minute_b = until.min(raw_floor);
    let minute_a = since.max(minute_floor);
    if minute_b > minute_a {
        segments.push((KIND_MINUTE.to_string(), minute_a, minute_b));
    }

    let hour_b = until.min(minute_floor).min(raw_floor);
    let hour_a = since.max(hour_floor);
    if hour_b > hour_a {
        segments.push((KIND_HOUR.to_string(), hour_a, hour_b));
    }

    segments.sort_by(|a, b| a.1.cmp(&b.1));
    segments
}

/// 窗口内确实有数据的节点集合（三层都算）。返回去重排序后的 node_id 列表。
pub fn window_node_ids(
    conn: &Connection,
    since: NaiveDateTime,
    until: NaiveDateTime,
) -> Vec<String> {
    let now = time::now();
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (tier, a, b) in tier_plan(since, until, now) {
        if b <= a {
            continue;
        }
        let a_db = time::format_db(&a);
        let b_db = time::format_db(&b);
        let sql = if tier == TIER_RAW {
            "SELECT DISTINCT node_id FROM metric_samples WHERE ts >= ?1 AND ts < ?2".to_string()
        } else {
            format!(
                "SELECT DISTINCT node_id FROM metric_rollups WHERE kind = '{}' AND ts >= ?1 AND ts < ?2",
                tier
            )
        };
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(iter) = stmt.query_map([&a_db, &b_db], |r| r.get::<_, String>(0)) {
                for row in iter.flatten() {
                    ids.insert(row);
                }
            }
        }
    }
    let mut out: Vec<String> = ids.into_iter().collect();
    out.sort();
    out
}

/// 按 `tier_plan` 取历史指标。返回 [(行, 层名), ...]（按时间升序）和实际用到的段。
pub fn fetch_history(
    conn: &Connection,
    node_ids: &[String],
    since: NaiveDateTime,
    until: NaiveDateTime,
) -> (Vec<(MetricRow, String)>, Vec<(String, NaiveDateTime, NaiveDateTime)>) {
    let now = time::now();
    let plan = tier_plan(since, until, now);
    if node_ids.is_empty() || plan.is_empty() {
        return (Vec::new(), plan);
    }

    let mut rows: Vec<(MetricRow, String)> = Vec::new();
    for (tier, a, b) in &plan {
        let a_db = time::format_db(a);
        let b_db = time::format_db(b);
        let sql = if tier == TIER_RAW {
            format!(
                "SELECT {METRIC_COLS} FROM metric_samples \
                 WHERE node_id IN ({}) AND ts >= ? AND ts < ?",
                placeholders(node_ids.len())
            )
        } else {
            format!(
                "SELECT {METRIC_COLS} FROM metric_rollups \
                 WHERE kind = '{}' AND node_id IN ({}) AND ts >= ? AND ts < ?",
                tier,
                placeholders(node_ids.len())
            )
        };
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = node_ids.iter().map(|s| Box::new(s.clone()) as Box<dyn rusqlite::ToSql>).collect();
        params.push(Box::new(a_db.clone()));
        params.push(Box::new(b_db.clone()));
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();

        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(iter) = stmt.query_map(param_refs.as_slice(), row_to_metric) {
                for r in iter.flatten() {
                    rows.push((r, tier.clone()));
                }
            }
        }
    }

    rows.sort_by(|a, b| a.0.ts.cmp(&b.0.ts));
    (rows, plan)
}

fn placeholders(n: usize) -> String {
    (0..n).map(|_| "?").collect::<Vec<_>>().join(",")
}

/// 取每个节点的最新一条采样（窗口函数一次查询解决，避免 N+1）。
/// 返回 node_id → MetricRow 映射。
pub fn latest_samples(
    conn: &Connection,
    node_ids: Option<&[String]>,
) -> HashMap<String, MetricRow> {
    let mut out = HashMap::new();

    let sql = match node_ids {
        Some(ids) if !ids.is_empty() => format!(
            "SELECT {METRIC_COLS} FROM (\
                SELECT {METRIC_COLS}, ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY ts DESC) AS rn \
                FROM metric_samples WHERE node_id IN ({}) \
             ) WHERE rn = 1",
            placeholders(ids.len())
        ),
        _ => format!(
            "SELECT {METRIC_COLS} FROM (\
                SELECT {METRIC_COLS}, ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY ts DESC) AS rn \
                FROM metric_samples \
             ) WHERE rn = 1"
        ),
    };

    let ids = node_ids.map(|s| s.to_vec());
    let params: Vec<Box<dyn rusqlite::ToSql>> = ids
        .unwrap_or_default()
        .iter()
        .map(|s| Box::new(s.clone()) as Box<dyn rusqlite::ToSql>)
        .collect();
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();

    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(iter) = stmt.query_map(param_refs.as_slice(), row_to_metric) {
            for r in iter.flatten() {
                out.insert(r.node_id.clone(), r);
            }
        }
    }
    out
}

/// 采样是否足够新鲜。拿不到时间戳一律视为不新鲜。
pub fn is_fresh(sample: Option<&MetricRow>, now: NaiveDateTime) -> bool {
    let s = match sample {
        Some(s) => s,
        None => return false,
    };
    let age = (now - s.ts).num_milliseconds() as f64 / 1000.0;
    age <= config::get().server.heartbeat_interval as f64 * STALE_FACTOR
}

/// 计算使用率百分比。任一为空或分母为 0 时返回 None（不编造 0%）。
pub fn percent(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    let used = used?;
    let total = total?;
    if total == 0.0 {
        return None;
    }
    Some((100.0 * used / total * 100.0).round() / 100.0)
}

/// 采样距今多少秒（保留 1 位小数）。用于前端"数据已陈旧 42s"提示。
#[allow(dead_code)] // 与 Python 版对齐，供前端“数据已陈旧”提示
pub fn sample_age_seconds(sample: Option<&MetricRow>, now: NaiveDateTime) -> Option<f64> {
    let s = sample?;
    let age = (now - s.ts).num_milliseconds() as f64 / 1000.0;
    Some((age * 10.0).round() / 10.0)
}
