//! 指标降采样与分层清理。
//!
//! 顺序：先聚合 → 再删原始。聚合抛异常时整轮跳过，一条不删（宁可磁盘多占，不可历史断档）。
//! 按桶精确 upsert（INSERT ... ON CONFLICT DO UPDATE），没扫到原始数据的桶一律不碰。
//!
//! 与 Python 版 services/metric_rollup.py 对齐。
//! - 均值 = 算术平均；`samples` 如实记条数。
//! - net_rx_bytes / net_tx_bytes 是累计计数器，存桶内最后一个值。
//! - battery_status 是枚举字符串，存桶内最后一个非空值。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{NaiveDateTime, TimeZone, Utc};
use rusqlite::{params, Connection};

use crate::config;
use crate::db::DbPool;
use crate::time;

use super::metrics_query::{KIND_HOUR, KIND_MINUTE, TIER_RAW};

// 桶宽（秒）。
pub const BUCKET_MINUTE: i64 = 60;
pub const BUCKET_HOUR: i64 = 3600;

// 求平均的字段（数值型）。
const AVG_FIELDS: &[&str] = &[
    "cpu_usage", "gpu_usage", "npu_usage", "mem_used", "mem_total", "storage_used",
    "storage_total", "temperature", "cpu_temp", "gpu_temp", "cpu_freq_mhz", "gpu_freq_mhz",
    "battery_percent", "battery_temp", "power_watts", "load_avg",
];

// 取桶内最后一个非空值的字段。
#[allow(dead_code)] // 与 Python 版 rollup 字段清单对齐
const LAST_FIELDS: &[&str] = &["battery_status", "net_rx_bytes", "net_tx_bytes"];

// 额外记 min/max 的字段。
const STAT_FIELDS: &[&str] = &[
    "cpu_usage", "gpu_usage", "npu_usage", "temperature", "mem_used", "storage_used",
    "battery_percent",
];

// 需要转回 int 的字段（原列是 BigInteger）。
const INT_FIELDS: &[&str] = &[
    "mem_used", "mem_total", "storage_used", "storage_total", "net_rx_bytes", "net_tx_bytes",
];

// 一次处理多长的时间窗。
const CHUNK_SECONDS: i64 = 6 * 3600;

// 进程内状态：第一次 run_once 走全量回溯，之后走增量。
static BOOTSTRAPPED: AtomicBool = AtomicBool::new(false);

fn bucket_seconds(kind: &str) -> Option<i64> {
    match kind {
        KIND_MINUTE => Some(BUCKET_MINUTE),
        KIND_HOUR => Some(BUCKET_HOUR),
        _ => None,
    }
}

pub fn tier_retention_days() -> HashMap<&'static str, i64> {
    let s = &config::get().server;
    let mut m = HashMap::new();
    m.insert(TIER_RAW, s.metric_raw_retention_days);
    m.insert(KIND_MINUTE, s.metric_minute_retention_days);
    m.insert(KIND_HOUR, s.metric_hour_retention_days);
    m
}

fn chunk_span_seconds(kind: &str) -> i64 {
    let b = bucket_seconds(kind).unwrap_or(CHUNK_SECONDS);
    CHUNK_SECONDS.max(b * 2)
}

/// 时间戳向下对齐到桶边界（epoch 秒整除，结果只取决于绝对时刻）。
fn align(dt: NaiveDateTime, bucket: i64) -> NaiveDateTime {
    let epoch = dt.and_utc().timestamp();
    let aligned = epoch - (epoch % bucket);
    Utc.timestamp_opt(aligned, 0)
        .single()
        .map(|t| t.naive_utc())
        .unwrap_or(dt)
}

// ------------------------------------------------------------------ 累加器

struct Acc {
    n: i64,
    first_ts: NaiveDateTime,
    last_ts: NaiveDateTime,
    sums: HashMap<&'static str, f64>,
    counts: HashMap<&'static str, i64>,
    mins: HashMap<&'static str, f64>,
    maxs: HashMap<&'static str, f64>,
    last_status: Option<String>,
    last_rx: Option<i64>,
    last_tx: Option<i64>,
}

impl Acc {
    fn new(first_ts: NaiveDateTime) -> Self {
        Self {
            n: 0,
            first_ts,
            last_ts: first_ts,
            sums: HashMap::new(),
            counts: HashMap::new(),
            mins: HashMap::new(),
            maxs: HashMap::new(),
            last_status: None,
            last_rx: None,
            last_tx: None,
        }
    }
}

fn acc_float(acc: &mut Acc, key: &'static str, v: Option<f64>) {
    if let Some(v) = v {
        *acc.sums.entry(key).or_insert(0.0) += v;
        *acc.counts.entry(key).or_insert(0) += 1;
    }
}

fn acc_stat(acc: &mut Acc, key: &'static str, v: Option<f64>) {
    if let Some(v) = v {
        let min = acc.mins.entry(key).or_insert(v);
        if v < *min {
            *min = v;
        }
        let max = acc.maxs.entry(key).or_insert(v);
        if v > *max {
            *max = v;
        }
    }
}

// 聚合读取用到的源列（与 AVG/STAT/LAST 去重后一致）。
struct SrcRow {
    node_id: String,
    ts: NaiveDateTime,
    cpu_usage: Option<f64>,
    gpu_usage: Option<f64>,
    npu_usage: Option<f64>,
    mem_used: Option<i64>,
    mem_total: Option<i64>,
    storage_used: Option<i64>,
    storage_total: Option<i64>,
    temperature: Option<f64>,
    cpu_temp: Option<f64>,
    gpu_temp: Option<f64>,
    cpu_freq_mhz: Option<f64>,
    gpu_freq_mhz: Option<f64>,
    battery_percent: Option<f64>,
    battery_temp: Option<f64>,
    power_watts: Option<f64>,
    load_avg: Option<f64>,
    battery_status: Option<String>,
    net_rx_bytes: Option<i64>,
    net_tx_bytes: Option<i64>,
}

const SRC_COLS: &str = "node_id, ts, cpu_usage, gpu_usage, npu_usage, mem_used, mem_total, \
    storage_used, storage_total, temperature, cpu_temp, gpu_temp, cpu_freq_mhz, gpu_freq_mhz, \
    battery_percent, battery_temp, power_watts, load_avg, battery_status, net_rx_bytes, \
    net_tx_bytes";

fn row_to_src(r: &rusqlite::Row) -> rusqlite::Result<SrcRow> {
    let ts_raw: String = r.get(1)?;
    Ok(SrcRow {
        node_id: r.get(0)?,
        ts: time::parse_db(&ts_raw).unwrap_or(NaiveDateTime::MIN),
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
        power_watts: r.get(16)?,
        load_avg: r.get(17)?,
        battery_status: r.get(18)?,
        net_rx_bytes: r.get(19)?,
        net_tx_bytes: r.get(20)?,
    })
}

fn accumulate(acc: &mut Acc, row: &SrcRow) {
    acc.n += 1;
    acc.last_ts = row.ts;

    acc_float(acc, "cpu_usage", row.cpu_usage);
    acc_float(acc, "gpu_usage", row.gpu_usage);
    acc_float(acc, "npu_usage", row.npu_usage);
    acc_float(acc, "mem_used", row.mem_used.map(|v| v as f64));
    acc_float(acc, "mem_total", row.mem_total.map(|v| v as f64));
    acc_float(acc, "storage_used", row.storage_used.map(|v| v as f64));
    acc_float(acc, "storage_total", row.storage_total.map(|v| v as f64));
    acc_float(acc, "temperature", row.temperature);
    acc_float(acc, "cpu_temp", row.cpu_temp);
    acc_float(acc, "gpu_temp", row.gpu_temp);
    acc_float(acc, "cpu_freq_mhz", row.cpu_freq_mhz);
    acc_float(acc, "gpu_freq_mhz", row.gpu_freq_mhz);
    acc_float(acc, "battery_percent", row.battery_percent);
    acc_float(acc, "battery_temp", row.battery_temp);
    acc_float(acc, "power_watts", row.power_watts);
    acc_float(acc, "load_avg", row.load_avg);

    acc_stat(acc, "cpu_usage", row.cpu_usage);
    acc_stat(acc, "gpu_usage", row.gpu_usage);
    acc_stat(acc, "npu_usage", row.npu_usage);
    acc_stat(acc, "temperature", row.temperature);
    acc_stat(acc, "mem_used", row.mem_used.map(|v| v as f64));
    acc_stat(acc, "storage_used", row.storage_used.map(|v| v as f64));
    acc_stat(acc, "battery_percent", row.battery_percent);

    if let Some(v) = &row.battery_status {
        acc.last_status = Some(v.clone());
    }
    if let Some(v) = row.net_rx_bytes {
        acc.last_rx = Some(v);
    }
    if let Some(v) = row.net_tx_bytes {
        acc.last_tx = Some(v);
    }
}

fn is_int_field(f: &str) -> bool {
    INT_FIELDS.contains(&f)
}

fn finalize_avg(acc: &Acc, f: &str) -> Option<f64> {
    let c = acc.counts.get(f).copied().unwrap_or(0);
    if c == 0 {
        return None;
    }
    let avg = acc.sums.get(f).copied().unwrap_or(0.0) / c as f64;
    if is_int_field(f) {
        Some(avg.round())
    } else {
        Some((avg * 10000.0).round() / 10000.0)
    }
}

fn finalize_avg_int(acc: &Acc, f: &str) -> Option<i64> {
    finalize_avg(acc, f).map(|v| v as i64)
}

struct RollupRec {
    node_id: String,
    kind: String,
    ts: String,
    samples: i64,
    first_ts: String,
    last_ts: String,
    cpu_usage: Option<f64>,
    gpu_usage: Option<f64>,
    npu_usage: Option<f64>,
    mem_used: Option<i64>,
    mem_total: Option<i64>,
    storage_used: Option<i64>,
    storage_total: Option<i64>,
    temperature: Option<f64>,
    cpu_temp: Option<f64>,
    gpu_temp: Option<f64>,
    cpu_freq_mhz: Option<f64>,
    gpu_freq_mhz: Option<f64>,
    battery_percent: Option<f64>,
    battery_temp: Option<f64>,
    power_watts: Option<f64>,
    load_avg: Option<f64>,
    battery_status: Option<String>,
    net_rx_bytes: Option<i64>,
    net_tx_bytes: Option<i64>,
    extra: String,
}

fn finalize(node_id: &str, kind: &str, bucket_start: NaiveDateTime, acc: &Acc) -> RollupRec {
    let bsec = bucket_seconds(kind).unwrap_or(0);

    // extra JSON，与 Python `_finalize` 对齐。
    let mut stats = serde_json::Map::new();
    for f in STAT_FIELDS {
        if let Some(min) = acc.mins.get(f) {
            let max = acc.maxs.get(f).copied().unwrap_or(*min);
            stats.insert(
                f.to_string(),
                serde_json::json!({"min": min, "max": max}),
            );
        }
    }
    let mut field_samples = serde_json::Map::new();
    for f in AVG_FIELDS {
        if let Some(c) = acc.counts.get(f) {
            if *c > 0 {
                field_samples.insert(f.to_string(), serde_json::json!(c));
            }
        }
    }
    let extra = serde_json::json!({
        "stats": stats,
        "rollup": {
            "kind": kind,
            "bucket_seconds": bsec,
            "samples": acc.n,
            "field_samples": field_samples,
        },
    });

    RollupRec {
        node_id: node_id.to_string(),
        kind: kind.to_string(),
        ts: time::format_db(&bucket_start),
        samples: acc.n,
        first_ts: time::format_db(&acc.first_ts),
        last_ts: time::format_db(&acc.last_ts),
        cpu_usage: finalize_avg(acc, "cpu_usage"),
        gpu_usage: finalize_avg(acc, "gpu_usage"),
        npu_usage: finalize_avg(acc, "npu_usage"),
        mem_used: finalize_avg_int(acc, "mem_used"),
        mem_total: finalize_avg_int(acc, "mem_total"),
        storage_used: finalize_avg_int(acc, "storage_used"),
        storage_total: finalize_avg_int(acc, "storage_total"),
        temperature: finalize_avg(acc, "temperature"),
        cpu_temp: finalize_avg(acc, "cpu_temp"),
        gpu_temp: finalize_avg(acc, "gpu_temp"),
        cpu_freq_mhz: finalize_avg(acc, "cpu_freq_mhz"),
        gpu_freq_mhz: finalize_avg(acc, "gpu_freq_mhz"),
        battery_percent: finalize_avg(acc, "battery_percent"),
        battery_temp: finalize_avg(acc, "battery_temp"),
        power_watts: finalize_avg(acc, "power_watts"),
        load_avg: finalize_avg(acc, "load_avg"),
        battery_status: acc.last_status.clone(),
        net_rx_bytes: acc.last_rx,
        net_tx_bytes: acc.last_tx,
        extra: extra.to_string(),
    }
}

const UPSERT_SQL: &str = "INSERT INTO metric_rollups (\
    node_id, kind, ts, samples, first_ts, last_ts, \
    cpu_usage, gpu_usage, npu_usage, mem_used, mem_total, storage_used, storage_total, \
    temperature, cpu_temp, gpu_temp, cpu_freq_mhz, gpu_freq_mhz, battery_percent, battery_temp, \
    power_watts, load_avg, battery_status, net_rx_bytes, net_tx_bytes, extra) \
    VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26) \
    ON CONFLICT(node_id, kind, ts) DO UPDATE SET \
    samples=excluded.samples, first_ts=excluded.first_ts, last_ts=excluded.last_ts, \
    cpu_usage=excluded.cpu_usage, gpu_usage=excluded.gpu_usage, npu_usage=excluded.npu_usage, \
    mem_used=excluded.mem_used, mem_total=excluded.mem_total, storage_used=excluded.storage_used, \
    storage_total=excluded.storage_total, temperature=excluded.temperature, \
    cpu_temp=excluded.cpu_temp, gpu_temp=excluded.gpu_temp, cpu_freq_mhz=excluded.cpu_freq_mhz, \
    gpu_freq_mhz=excluded.gpu_freq_mhz, battery_percent=excluded.battery_percent, \
    battery_temp=excluded.battery_temp, power_watts=excluded.power_watts, \
    load_avg=excluded.load_avg, battery_status=excluded.battery_status, \
    net_rx_bytes=excluded.net_rx_bytes, net_tx_bytes=excluded.net_tx_bytes, extra=excluded.extra";

fn upsert_batch(conn: &Connection, batch: &[RollupRec]) -> rusqlite::Result<()> {
    for rec in batch {
        conn.execute(
            UPSERT_SQL,
            params![
                rec.node_id,
                rec.kind,
                rec.ts,
                rec.samples,
                rec.first_ts,
                rec.last_ts,
                rec.cpu_usage,
                rec.gpu_usage,
                rec.npu_usage,
                rec.mem_used,
                rec.mem_total,
                rec.storage_used,
                rec.storage_total,
                rec.temperature,
                rec.cpu_temp,
                rec.gpu_temp,
                rec.cpu_freq_mhz,
                rec.gpu_freq_mhz,
                rec.battery_percent,
                rec.battery_temp,
                rec.power_watts,
                rec.load_avg,
                rec.battery_status,
                rec.net_rx_bytes,
                rec.net_tx_bytes,
                rec.extra,
            ],
        )?;
    }
    Ok(())
}

/// 把 [since, until) 的原始采样聚合成 kind 粒度。返回写入的桶数。幂等。
pub fn rollup_range(
    conn: &Connection,
    kind: &str,
    since: NaiveDateTime,
    until: NaiveDateTime,
) -> i64 {
    let bucket = match bucket_seconds(kind) {
        Some(b) => b,
        None => return 0,
    };
    if until <= since {
        return 0;
    }

    let span = chrono::Duration::seconds(chunk_span_seconds(kind));
    let mut cursor = since;
    let mut written: i64 = 0;

    while cursor < until {
        let win_end = (cursor + span).min(until);
        let since_db = time::format_db(&cursor);
        let until_db = time::format_db(&win_end);

        let sql = format!(
            "SELECT {SRC_COLS} FROM metric_samples WHERE ts >= ?1 AND ts < ?2 \
             ORDER BY node_id ASC, ts ASC"
        );
        let mut rows: Vec<SrcRow> = Vec::new();
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(iter) = stmt.query_map([&since_db, &until_db], row_to_src) {
                for r in iter.flatten() {
                    rows.push(r);
                }
            }
        }

        if !rows.is_empty() {
            let mut accs: HashMap<(String, NaiveDateTime), Acc> = HashMap::new();
            for r in &rows {
                let key = (r.node_id.clone(), align(r.ts, bucket));
                let acc = accs.entry(key).or_insert_with(|| Acc::new(r.ts));
                accumulate(acc, r);
            }

            let mut payload: Vec<RollupRec> = Vec::new();
            for ((node_id, bstart), acc) in &accs {
                payload.push(finalize(node_id, kind, *bstart, acc));
            }
            for chunk in payload.chunks(500) {
                // 批次写失败会打断这一轮聚合，交由 run_once 捕获并整体回滚。
                if let Err(e) = upsert_batch(conn, chunk) {
                    eprintln!("metric_rollup 批次写入失败 kind={kind} err={e}");
                }
            }
            written += payload.len() as i64;
        }

        cursor = win_end;
    }

    written
}

// ------------------------------------------------------------------ 清理

fn prune_raw(conn: &Connection, days: i64) -> i64 {
    if days <= 0 {
        return 0;
    }
    let cutoff = time::now() - chrono::Duration::days(days);
    let cutoff_db = time::format_db(&cutoff);
    conn.execute(
        "DELETE FROM metric_samples WHERE ts < ?1",
        [&cutoff_db],
    )
    .map(|n| n as i64)
    .unwrap_or(0)
}

fn prune_rollup(conn: &Connection, kind: &str, days: i64) -> i64 {
    if days <= 0 {
        return 0;
    }
    let cutoff = time::now() - chrono::Duration::days(days);
    let cutoff_db = time::format_db(&cutoff);
    conn.execute(
        "DELETE FROM metric_rollups WHERE kind = ?1 AND ts < ?2",
        params![kind, cutoff_db],
    )
    .map(|n| n as i64)
    .unwrap_or(0)
}

// ------------------------------------------------------------------ 对外入口

/// 聚合 + 分层清理，一整轮。返回统计信息（也用于 API 汇报）。
/// `full=None`：进程内第一次调用做全量回溯，之后只增量处理最近 `rollup_interval*3` 分钟。
pub fn run_once(pool: &DbPool, full: Option<bool>) -> serde_json::Value {
    let started = time::now();
    let full = full.unwrap_or_else(|| !BOOTSTRAPPED.load(Ordering::SeqCst));

    let mut rollup = serde_json::Map::new();
    let mut pruned = serde_json::Map::new();
    let mut ok = false;
    let mut error: Option<String> = None;

    let retention = tier_retention_days();

    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "started_at": time::format_iso(&started),
                "full": full,
                "rollup": {},
                "pruned": {},
                "ok": false,
                "error": e.to_string(),
                "finished_at": time::format_iso(&time::now()),
                "duration_ms": 0,
            });
        }
    };

    let tx_result = conn.transaction();
    let tx = match tx_result {
        Ok(t) => t,
        Err(e) => {
            return serde_json::json!({
                "started_at": time::format_iso(&started),
                "full": full,
                "rollup": {},
                "pruned": {},
                "ok": false,
                "error": e.to_string(),
                "finished_at": time::format_iso(&time::now()),
                "duration_ms": 0,
            });
        }
    };

    let outcome = (|| -> rusqlite::Result<()> {
        let (min_ts, max_ts): (Option<String>, Option<String>) = tx.query_row(
            "SELECT MIN(ts), MAX(ts) FROM metric_samples",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        match (min_ts.as_deref().and_then(time::parse_db), max_ts.as_deref().and_then(time::parse_db)) {
            (None, _) | (_, None) => {
                rollup.insert(KIND_MINUTE.to_string(), serde_json::json!(0));
                rollup.insert(KIND_HOUR.to_string(), serde_json::json!(0));
            }
            (Some(min_dt), Some(max_dt)) => {
                let since = if full {
                    min_dt
                } else {
                    let interval = config::get().server.metric_rollup_interval_minutes.max(1);
                    let lookback = chrono::Duration::minutes(interval * 3);
                    min_dt.max(started - lookback)
                };
                // 上界给 now，避免 max_ts 之后又来了新采样却没被覆盖到。
                let upper = max_dt.max(started) + chrono::Duration::seconds(1);
                for kind in [KIND_MINUTE, KIND_HOUR] {
                    let n = rollup_range(&tx, kind, since, upper);
                    rollup.insert(kind.to_string(), serde_json::json!(n));
                }
            }
        }

        // —— 走到这里说明聚合全部成功，才允许删 ——
        let raw_days = retention.get(TIER_RAW).copied().unwrap_or(0);
        pruned.insert(TIER_RAW.to_string(), serde_json::json!(prune_raw(&tx, raw_days)));
        for kind in [KIND_MINUTE, KIND_HOUR] {
            let days = retention.get(kind).copied().unwrap_or(0);
            pruned.insert(kind.to_string(), serde_json::json!(prune_rollup(&tx, kind, days)));
        }
        Ok(())
    })();

    match outcome {
        Ok(()) => {
            let _ = tx.commit();
            ok = true;
            if full {
                BOOTSTRAPPED.store(true, Ordering::SeqCst);
            }
        }
        Err(e) => {
            let _ = tx.rollback();
            error = Some(e.to_string());
            eprintln!("指标分层维护失败（本轮不删任何数据）err={}", e);
        }
    }

    let finished = time::now();
    let duration_ms = (finished - started).num_milliseconds();
    serde_json::json!({
        "started_at": time::format_iso(&started),
        "full": full,
        "rollup": rollup,
        "pruned": pruned,
        "ok": ok,
        "error": error,
        "finished_at": time::format_iso(&finished),
        "duration_ms": duration_ms,
    })
}

/// 各层现状：保留期、行数、时间跨度。
pub fn tier_status(pool: &DbPool) -> serde_json::Value {
    let retention = tier_retention_days();
    let now = time::now();
    let mut tiers = serde_json::Map::new();

    if let Ok(conn) = pool.get() {
        let raw: Option<(i64, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT COUNT(*), MIN(ts), MAX(ts) FROM metric_samples",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        let (rows, lo, hi) = raw.unwrap_or((0, None, None));
        tiers.insert(
            TIER_RAW.to_string(),
            serde_json::json!({
                "kind": TIER_RAW,
                "bucket_seconds": config::get().server.heartbeat_interval,
                "retention_days": retention.get(TIER_RAW).copied().unwrap_or(0),
                "rows": rows,
                "oldest": lo.as_deref().map(time::db_to_iso),
                "newest": hi.as_deref().map(time::db_to_iso),
            }),
        );

        for kind in [KIND_MINUTE, KIND_HOUR] {
            let row: Option<(i64, Option<String>, Option<String>)> = conn
                .query_row(
                    "SELECT COUNT(*), MIN(ts), MAX(ts) FROM metric_rollups WHERE kind = ?1",
                    [kind],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let (rows, lo, hi) = row.unwrap_or((0, None, None));
            tiers.insert(
                kind.to_string(),
                serde_json::json!({
                    "kind": kind,
                    "bucket_seconds": bucket_seconds(kind).unwrap_or(0),
                    "retention_days": retention.get(kind).copied().unwrap_or(0),
                    "rows": rows,
                    "oldest": lo.as_deref().map(time::db_to_iso),
                    "newest": hi.as_deref().map(time::db_to_iso),
                }),
            );
        }
    }

    serde_json::json!({
        "retention_days": retention,
        "now": time::format_iso(&now),
        "tiers": tiers,
    })
}

#[cfg(test)]
mod tests {
    use super::super::metrics_query::{tier_plan, KIND_HOUR, KIND_MINUTE, TIER_RAW};
    use super::{rollup_range, run_once, tier_status};
    use crate::config::{self, init_global, Config, DataConfig};
    use crate::db::DbPool;
    use crate::time;
    use chrono::{Duration, NaiveDateTime, TimeZone, Utc};
    use rusqlite::Connection;
    use std::path::PathBuf;

    struct Results {
        failures: Vec<String>,
    }
    impl Results {
        fn new() -> Self {
            Self { failures: vec![] }
        }
        fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
            if !ok {
                self.failures.push(format!("{name} — {}", detail.as_ref()));
            }
        }
        fn finish(self) {
            assert!(
                self.failures.is_empty(),
                "\n指标分层验收失败 {} 项：\n{}",
                self.failures.len(),
                self.failures.join("\n")
            );
        }
    }

    /// 建一个指向临时库的全局配置，返回连接池与临时根目录。
    /// 保留期用默认值（raw=2 / minute=30 / hour=365），与 Python 脚本一致。
    fn setup() -> (DbPool, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("edge-metric-tiers-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let db = data.join("edge.db");
        init_global(Config {
            data: DataConfig {
                db_path: db.to_string_lossy().to_string(),
                ..DataConfig::default()
            },
            ..Config::default()
        });
        let pool = crate::db::open_pool(&db).unwrap();
        {
            let conn = pool.get().unwrap();
            crate::db::init_db(&conn).unwrap();
        }
        (pool, root)
    }

    fn seed_node(conn: &Connection) {
        let now = time::now_db();
        conn.execute(
            "INSERT INTO nodes (node_id, name, status, created_at, updated_at) \
             VALUES ('tiers-01','tiers-01','online',?1,?1)",
            [now],
        )
        .unwrap();
    }

    /// 只写 Python `_add` 关心的列，其余列（gpu/npu 等）保持 NULL —— 供 C 段验证“不造假”。
    fn add_sample(
        conn: &Connection,
        ts: NaiveDateTime,
        cpu: f64,
        mem_used: i64,
        mem_total: i64,
        net_rx: i64,
        battery_status: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO metric_samples (node_id, ts, cpu_usage, mem_used, mem_total, \
             net_rx_bytes, battery_status) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                "tiers-01",
                time::format_db(&ts),
                cpu,
                mem_used,
                mem_total,
                net_rx,
                battery_status
            ],
        )
        .unwrap();
    }

    struct Row {
        ts: NaiveDateTime,
        cpu_usage: Option<f64>,
        gpu_usage: Option<f64>,
        npu_usage: Option<f64>,
        mem_used: Option<i64>,
        samples: i64,
        battery_status: Option<String>,
        net_rx_bytes: Option<i64>,
        extra: serde_json::Value,
    }

    fn query_minute(conn: &Connection) -> Vec<Row> {
        let mut stmt = conn
            .prepare(
                "SELECT ts, cpu_usage, gpu_usage, npu_usage, mem_used, samples, \
                 battery_status, net_rx_bytes, extra FROM metric_rollups \
                 WHERE kind='minute' ORDER BY ts ASC",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                let ts_raw: String = r.get(0)?;
                let extra_raw: String = r.get(8)?;
                Ok(Row {
                    ts: time::parse_db(&ts_raw).unwrap_or(NaiveDateTime::MIN),
                    cpu_usage: r.get(1)?,
                    gpu_usage: r.get(2)?,
                    npu_usage: r.get(3)?,
                    mem_used: r.get(4)?,
                    samples: r.get(5)?,
                    battery_status: r.get(6)?,
                    net_rx_bytes: r.get(7)?,
                    extra: serde_json::from_str(&extra_raw).unwrap_or(serde_json::Value::Null),
                })
            })
            .unwrap();
        rows.flatten().collect()
    }

    fn fingerprint(rows: &[Row]) -> Vec<(String, Option<f64>, i64)> {
        rows.iter()
            .map(|row| (time::format_db(&row.ts), row.cpu_usage, row.samples))
            .collect()
    }

    /// 时间戳向下对齐到整分钟（epoch 秒整除），等价于 rollup 里的桶对齐。
    fn minute_aligned(dt: NaiveDateTime) -> NaiveDateTime {
        let epoch = dt.and_utc().timestamp();
        Utc.timestamp_opt(epoch - (epoch % 60), 0)
            .single()
            .unwrap()
            .naive_utc()
    }

    fn segs_of(
        now2: NaiveDateTime,
        minutes: i64,
    ) -> Vec<(String, NaiveDateTime, NaiveDateTime)> {
        tier_plan(now2 - Duration::minutes(minutes), now2, now2)
    }

    fn tiers_of(segs: &[(String, NaiveDateTime, NaiveDateTime)]) -> Vec<&str> {
        segs.iter().map(|(t, _, _)| t.as_str()).collect()
    }

    /// 按时间排序后，任意两段不得相交；相邻两段必须首尾相接（不留缝）。
    fn no_overlap(segs: &[(String, NaiveDateTime, NaiveDateTime)]) -> bool {
        let mut ss = segs.to_vec();
        ss.sort_by(|a, b| a.1.cmp(&b.1));
        for i in 0..ss.len().saturating_sub(1) {
            if ss[i].2 > ss[i + 1].1 {
                return false; // 重叠
            }
            if ss[i].2 < ss[i + 1].1 {
                return false; // 中间有缝
            }
        }
        true
    }

    fn ordered(segs: &[(String, NaiveDateTime, NaiveDateTime)]) -> bool {
        segs.iter().all(|(_, a, b)| a < b)
    }

    #[test]
    fn metric_tiers() {
        let _guard = crate::config::CONFIG_TEST_LOCK.lock().unwrap();
        let mut r = Results::new();
        let (pool, root) = setup();

        // 基时刻对齐到整分钟，方便手算期望值
        let base = minute_aligned(time::now()) - Duration::hours(1);

        {
            let conn = pool.get().unwrap();
            seed_node(&conn);
            // 第 1 分钟：3 条采样
            add_sample(&conn, base + Duration::seconds(5), 10.0, 1000, 10000, 100, Some("charging"));
            add_sample(&conn, base + Duration::seconds(20), 20.0, 2000, 10000, 200, None);
            add_sample(&conn, base + Duration::seconds(55), 30.0, 3000, 10000, 300, Some("discharging"));
            // 第 2 分钟：1 条采样
            add_sample(
                &conn,
                base + Duration::minutes(1) + Duration::seconds(10),
                40.0,
                4000,
                10000,
                400,
                Some("discharging"),
            );
        }

        // ---- A 桶对齐 ----
        let (n_min, rows) = {
            let conn = pool.get().unwrap();
            let n = rollup_range(
                &conn,
                KIND_MINUTE,
                base - Duration::minutes(1),
                base + Duration::minutes(5),
            );
            (n, query_minute(&conn))
        };
        r.check(
            "分钟级写出了 2 个桶",
            n_min == 2 && rows.len() == 2,
            format!("written={n_min} rows={}", rows.len()),
        );
        r.check(
            "桶起点对齐到整分钟（秒/微秒为 0）",
            rows.iter().all(|row| row.ts.and_utc().timestamp() % 60 == 0),
            format!(
                "{:?}",
                rows.iter().map(|row| row.ts).collect::<Vec<_>>()
            ),
        );
        if !rows.is_empty() {
            r.check(
                "第 1 个桶的起点 == 期望值",
                rows[0].ts == base,
                format!("{} vs {}", rows[0].ts, base),
            );
        }

        // ---- B 聚合正确性 ----
        if rows.len() == 2 {
            let b0 = &rows[0];
            let b1 = &rows[1];
            r.check(
                "cpu 均值 (10+20+30)/3 = 20",
                b0.cpu_usage == Some(20.0),
                format!("{:?}", b0.cpu_usage),
            );
            r.check(
                "cpu max = 30",
                b0.extra["stats"]["cpu_usage"]["max"].as_f64() == Some(30.0),
                format!("{:?}", b0.extra["stats"]["cpu_usage"]["max"]),
            );
            r.check(
                "cpu min = 10",
                b0.extra["stats"]["cpu_usage"]["min"].as_f64() == Some(10.0),
                format!("{:?}", b0.extra["stats"]["cpu_usage"]["min"]),
            );
            r.check("samples 计数 = 3", b0.samples == 3, format!("{}", b0.samples));
            r.check(
                "mem_used 均值取整 = 2000（BIGINT 列不回退成浮点）",
                b0.mem_used == Some(2000),
                format!("{:?}", b0.mem_used),
            );
            r.check(
                "未跨桶污染：第 2 桶 cpu = 40",
                b1.cpu_usage == Some(40.0),
                format!("{:?}", b1.cpu_usage),
            );
            r.check("第 2 桶 samples = 1", b1.samples == 1, format!("{}", b1.samples));
            r.check(
                "field_samples 如实记录参与平均的条数",
                b0.extra["rollup"]["field_samples"]["cpu_usage"].as_i64() == Some(3),
                format!(
                    "{:?}",
                    b0.extra["rollup"]["field_samples"]["cpu_usage"]
                ),
            );
        }

        // ---- C 不造假：拿不到就是 None ----
        if rows.len() == 2 {
            let b0 = &rows[0];
            r.check(
                "整桶都没采到的 gpu_usage 必须是 None（不是 0.0）",
                b0.gpu_usage.is_none(),
                format!("{:?}", b0.gpu_usage),
            );
            r.check(
                "gpu_usage 不进入 stats（没有 min/max 可言）",
                b0.extra["stats"].get("gpu_usage").is_none(),
                format!(
                    "{:?}",
                    b0.extra["stats"]
                        .as_object()
                        .map(|o| o.keys().collect::<Vec<_>>())
                ),
            );
            r.check(
                "整桶没采到的 npu_usage 同样是 None",
                b0.npu_usage.is_none(),
                format!("{:?}", b0.npu_usage),
            );
        }

        // ---- D 特殊字段语义 ----
        if rows.len() == 2 {
            let b0 = &rows[0];
            r.check(
                "net_rx_bytes 取桶内最后一个值 300（不是均值 200）",
                b0.net_rx_bytes == Some(300),
                format!("{:?}", b0.net_rx_bytes),
            );
            r.check(
                "battery_status 取最后一个非空值 discharging（跳过中间的 None）",
                b0.battery_status.as_deref() == Some("discharging"),
                format!("{:?}", b0.battery_status),
            );
        }

        // ---- E 幂等与顺序安全 ----
        let before = {
            let conn = pool.get().unwrap();
            query_minute(&conn)
        };
        {
            let conn = pool.get().unwrap();
            rollup_range(
                &conn,
                KIND_MINUTE,
                base - Duration::minutes(1),
                base + Duration::minutes(5),
            );
        }
        let after = {
            let conn = pool.get().unwrap();
            query_minute(&conn)
        };
        r.check(
            "重跑聚合：不产生重复桶，值也不变",
            fingerprint(&before) == fingerprint(&after),
            format!("{} → {}", fingerprint(&before).len(), fingerprint(&after).len()),
        );

        // 关键：模拟“原始数据已被清理”，再重跑聚合。已有的分钟级必须原样保留。
        {
            let conn = pool.get().unwrap();
            conn.execute("DELETE FROM metric_samples", []).unwrap();
        }
        let wrote = {
            let conn = pool.get().unwrap();
            rollup_range(
                &conn,
                KIND_MINUTE,
                base - Duration::minutes(1),
                base + Duration::minutes(5),
            )
        };
        let survived = {
            let conn = pool.get().unwrap();
            query_minute(&conn)
        };
        r.check(
            "原始数据删光后重跑聚合：写入 0 个桶",
            wrote == 0,
            format!("written={wrote}"),
        );
        r.check(
            "且已有的分钟级数据一条都不能少（先删后插写法会在这里全灭）",
            survived.len() == 2,
            format!("剩 {} 桶", survived.len()),
        );

        // ---- F 分层清理 ----
        let now = time::now();
        {
            let conn = pool.get().unwrap();
            // 3 天前的原始（超过 raw 保留 2 天）；30 天前的分钟级（未超 minute 保留 30 天）；
            // 400 天前的小时级（超过 hour 保留 365 天）
            add_sample(&conn, now - Duration::days(3), 1.0, 0, 0, 0, None);
            conn.execute(
                "INSERT INTO metric_rollups (node_id, kind, ts, samples, cpu_usage) \
                 VALUES ('tiers-01','minute',?1,12,5.0)",
                [time::format_db(&(now - Duration::days(30) + Duration::hours(1)))],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO metric_rollups (node_id, kind, ts, samples, cpu_usage) \
                 VALUES ('tiers-01','hour',?1,720,6.0)",
                [time::format_db(&(now - Duration::days(400)))],
            )
            .unwrap();
        }

        let stats = run_once(&pool, None);
        r.check(
            "run_once 返回 ok=True",
            stats["ok"].as_bool() == Some(true),
            format!("{:?}", stats.get("error")),
        );
        if stats["ok"].as_bool() == Some(true) {
            r.check(
                "原始层清理到了过期数据（raw=2 天）",
                stats["pruned"][TIER_RAW].as_i64().unwrap_or(0) >= 1,
                format!("{:?}", stats["pruned"][TIER_RAW]),
            );
            r.check(
                "分钟级层清理未误删 30 天内的数据（minute=30 天）",
                stats["pruned"][KIND_MINUTE].as_i64().unwrap_or(-1) == 0,
                format!("{:?}", stats["pruned"][KIND_MINUTE]),
            );
            r.check(
                "小时级层清掉了 400 天前的数据（hour=365 天）",
                stats["pruned"][KIND_HOUR].as_i64().unwrap_or(0) >= 1,
                format!("{:?}", stats["pruned"][KIND_HOUR]),
            );
        }

        let left_raw_old: i64 = {
            let conn = pool.get().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM metric_samples WHERE ts < ?1",
                [time::format_db(&(now - Duration::days(2)))],
                |r| r.get(0),
            )
            .unwrap()
        };
        r.check(
            "清理后库里确实没有超过 2 天的原始采样",
            left_raw_old == 0,
            format!("{left_raw_old}"),
        );

        // 顺序：原始被删之前，聚合必须已经产出（否则 3 天前那条就永久丢了）
        let exists: i64 = {
            let conn = pool.get().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM metric_rollups WHERE kind='minute' AND ts <= ?1 AND ts > ?2",
                rusqlite::params![
                    time::format_db(&(now - Duration::days(3))),
                    time::format_db(&(now - Duration::days(3) - Duration::minutes(5)))
                ],
                |r| r.get(0),
            )
            .unwrap()
        };
        r.check(
            "被清理的那条原始采样，其分钟级聚合已先落库（证明顺序是先聚合后删除）",
            exists >= 1,
            format!("命中 {exists} 桶"),
        );

        // ---- G 保留期为 0 = 不清理 ----
        {
            let mut cfg = config::get();
            cfg.server.metric_hour_retention_days = 0;
            init_global(cfg);
        }
        let before_hour: i64 = {
            let conn = pool.get().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM metric_rollups WHERE kind='hour'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO metric_rollups (node_id, kind, ts, samples, cpu_usage) \
                 VALUES ('tiers-01','hour',?1,1,9.0)",
                [time::format_db(&(now - Duration::days(900)))],
            )
            .unwrap();
        }
        let st2 = run_once(&pool, None);
        let after_hour: i64 = {
            let conn = pool.get().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM metric_rollups WHERE kind='hour'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        r.check(
            "hour 保留期设为 0 时不清任何小时级数据",
            st2["ok"].as_bool() == Some(true) && after_hour == before_hour + 1,
            format!(
                "{before_hour} → {after_hour}, pruned={:?}",
                st2["pruned"].get(KIND_HOUR)
            ),
        );
        {
            let mut cfg = config::get();
            cfg.server.metric_hour_retention_days = 365;
            init_global(cfg);
        }

        // ---- H 现状汇报接口 ----
        let st = tier_status(&pool);
        r.check(
            "tier_status 覆盖三层",
            st["tiers"]
                .as_object()
                .map(|o| {
                    let mut keys: Vec<&str> = o.keys().map(|s| s.as_str()).collect();
                    keys.sort_unstable();
                    keys == vec![KIND_HOUR, KIND_MINUTE, TIER_RAW]
                })
                .unwrap_or(false),
            format!(
                "{:?}",
                st["tiers"]
                    .as_object()
                    .map(|o| o.keys().collect::<Vec<_>>())
            ),
        );
        r.check(
            "每层都带保留期与行数",
            st["tiers"].as_object().map(|o| {
                o.values().all(|v| {
                    ["retention_days", "rows", "oldest", "newest"]
                        .iter()
                        .all(|k| v.get(k).is_some())
                })
            }).unwrap_or(false),
            format!(
                "{:?}",
                st["tiers"]
                    .as_object()
                    .map(|o| o
                        .iter()
                        .map(|(k, v)| (k.clone(), v["rows"].clone()))
                        .collect::<Vec<_>>())
            ),
        );

        // ---- I 查询分层：不重叠、不留缝、不重复计数 ----
        let now2 = time::now();

        let seg30 = segs_of(now2, 30);
        r.check(
            "30 分钟窗口只读原始层",
            tiers_of(&seg30) == ["raw"],
            format!("{:?}", tiers_of(&seg30)),
        );

        let seg7d = segs_of(now2, 60 * 24 * 7);
        {
            let mut t = tiers_of(&seg7d);
            t.sort_unstable();
            r.check(
                "7 天窗口 = minute + raw 两层",
                t == ["minute", "raw"],
                format!("{t:?}"),
            );
        }
        r.check(
            "7 天窗口两层首尾相接、不重叠、不留缝",
            no_overlap(&seg7d),
            format!(
                "{:?}",
                seg7d.iter().map(|(t, a, b)| (t, *a, *b)).collect::<Vec<_>>()
            ),
        );
        r.check("所有段的起点都早于终点", ordered(&seg7d), "有段起点不早于终点");

        let seg1y = segs_of(now2, 60 * 24 * 365);
        {
            let mut t = tiers_of(&seg1y);
            t.sort_unstable();
            r.check(
                "365 天窗口三层齐全：hour + minute + raw",
                t == ["hour", "minute", "raw"],
                format!("{t:?}"),
            );
        }
        r.check(
            "365 天窗口三层首尾相接、不重叠、不留缝",
            no_overlap(&seg1y),
            format!(
                "{:?}",
                seg1y.iter().map(|(t, a, b)| (t, *a, *b)).collect::<Vec<_>>()
            ),
        );

        // 最容易写错的一条：原始层不清理（保留期 0）时，聚合层不得再参与
        {
            let mut cfg = config::get();
            cfg.server.metric_raw_retention_days = 0;
            init_global(cfg);
        }
        let seg_zero = segs_of(now2, 60 * 24 * 365);
        r.check(
            "原始层保留期设为 0 时只剩 raw 一段，聚合层不得再参与",
            tiers_of(&seg_zero) == ["raw"],
            format!("{:?}", tiers_of(&seg_zero)),
        );
        {
            let mut cfg = config::get();
            cfg.server.metric_raw_retention_days = 2;
            init_global(cfg);
        }

        // 反向：分钟级不清理时，hour 段必须为空（否则与 minute 重复）
        {
            let mut cfg = config::get();
            cfg.server.metric_minute_retention_days = 0;
            init_global(cfg);
        }
        let seg_m0 = segs_of(now2, 60 * 24 * 365);
        r.check(
            "分钟级保留期设为 0 时，hour 段必须为空（不得与 minute 重复）",
            !tiers_of(&seg_m0).contains(&"hour"),
            format!("{:?}", tiers_of(&seg_m0)),
        );
        r.check("此时剩下两层仍不重叠", no_overlap(&seg_m0), "");
        {
            let mut cfg = config::get();
            cfg.server.metric_minute_retention_days = 30;
            init_global(cfg);
        }

        let _ = std::fs::remove_dir_all(&root);
        r.finish();
    }
}
