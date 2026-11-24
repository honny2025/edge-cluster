//! 时间工具：统一 UTC 时间，DB 与 API 两种字符串形态。
//!
//! 与 Python 版对齐的三种形态：
//! - **内存**：`chrono::NaiveDateTime`（naive UTC，对应 `utcnow()`）。
//! - **DB 存储**：`"YYYY-MM-DD HH:MM:SS.ffffff"`（空格分隔、恒 6 位微秒）——
//!   与 SQLAlchemy SQLite 的 DATETIME 落库格式一致，保证字符串字典序 = 时间序，
//!   且能与旧库互读。
//! - **API 输出**：`"YYYY-MM-DDTHH:MM:SS"` 或 `"YYYY-MM-DDTHH:MM:SS.ffffff"`（T 分隔、
//!   微秒为 0 时省略）—— 对应 Python `datetime.isoformat()`。

use chrono::{NaiveDateTime, TimeZone, Utc};

/// 当前 naive UTC 时间（对应 Python `utcnow()`）。
pub fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// DB 落库格式（空格分隔，恒 6 位微秒）。
pub fn format_db(dt: &NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

pub fn now_db() -> String {
    format_db(&now())
}

/// API 输出格式（T 分隔，微秒为 0 时省略，与 Python isoformat 一致）。
pub fn format_iso(dt: &NaiveDateTime) -> String {
    let micros = dt.and_utc().timestamp_subsec_micros();
    if micros == 0 {
        dt.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        dt.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}

/// 解析 DB 格式字符串。失败返回 None（库里脏数据不应拖垮整个请求）。
pub fn parse_db(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.6f").ok()
}

/// DB 格式字符串 → API isoformat 字符串。
pub fn db_to_iso(s: &str) -> String {
    parse_db(s).map(|dt| format_iso(&dt)).unwrap_or_else(|| s.to_string())
}

/// DB 格式字符串 → epoch 秒（f64，含微秒小数）。对应 Python `datetime.timestamp()`。
pub fn db_to_epoch(s: &str) -> Option<f64> {
    parse_db(s).map(|dt| {
        dt.and_utc().timestamp() as f64 + dt.and_utc().timestamp_subsec_micros() as f64 / 1e6
    })
}

/// epoch 秒 → DB 格式字符串。
#[allow(dead_code)] // 与 Python 版对齐
pub fn epoch_to_db(epoch: f64) -> String {
    let secs = epoch.floor() as i64;
    let micros = ((epoch - epoch.floor()) * 1e6).round() as u32;
    let dt = Utc
        .timestamp_opt(secs, micros * 1000)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap());
    format_db(&dt.naive_utc())
}

/// 两个 DB 格式时间相差的秒数（now 在前，old 在后）。
pub fn db_elapsed_secs(now_str: &str, old_str: &str) -> Option<f64> {
    let a = parse_db(now_str)?;
    let b = parse_db(old_str)?;
    Some((a - b).num_milliseconds() as f64 / 1000.0)
}
