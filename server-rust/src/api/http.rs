//! 文件下发响应：支持 HTTP Range / ETag / Last-Modified 的流式文件响应。
//!
//! 与 Python 版 Starlette FileResponse 的行为对齐：
//! - 带 `Range` → 206 + 分片 + `Content-Range`；
//! - 不带 → 200 全量；
//! - 恒带 `Accept-Ranges: bytes`、`ETag`、`Last-Modified`，供 Agent 断点续传判断。
//! 模型动辄几个 GB，绝不能整块读进内存 —— 一律按 1MB 分片流式吐。

use std::path::Path;
use std::time::SystemTime;

use axum::body::Body;
use axum::http::header::{self, HeaderValue};
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use futures::Stream;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::ApiError;

const CHUNK: u64 = 1024 * 1024;

/// 解析 `Range: bytes=...` 单段区间。返回 (start, end_inclusive)，越界时修正。
/// 不支持多段（与 Starlette 一致，多段时按全量 200 处理）。
fn parse_range(header: Option<&str>, size: u64) -> Option<(u64, u64)> {
    let header = header?;
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // 多段 → 忽略，回落到全量
    }
    if size == 0 {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    if start_s.is_empty() {
        // suffix 形式：bytes=-N 取最后 N 字节
        let n: u64 = end_s.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = size.saturating_sub(n);
        return Some((start, size - 1));
    }
    let start: u64 = start_s.trim().parse().ok()?;
    if start >= size {
        return None;
    }
    let end = if end_s.trim().is_empty() {
        size - 1
    } else {
        let e: u64 = end_s.trim().parse().ok()?;
        e.min(size - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

/// Range 头「语法有效但起点越界」（start >= size）时为 true —— 该回 416 而非 200。
/// 没有 Range / 多段 / 后缀 / 无法解析时一律 false，走原有 200 全量逻辑。
fn range_out_of_bounds(header: Option<&str>, size: u64) -> bool {
    let spec = match header.and_then(|h| h.trim().strip_prefix("bytes=")) {
        Some(s) => s,
        None => return false,
    };
    if spec.contains(',') {
        return false;
    }
    let start_s = match spec.split_once('-') {
        Some((s, _)) => s,
        None => return false,
    };
    if start_s.is_empty() {
        return false; // 后缀形式 bytes=-N
    }
    let start: u64 = match start_s.trim().parse() {
        Ok(s) => s,
        Err(_) => return false,
    };
    start >= size
}

fn http_date(st: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = st.into();
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn sanitize_filename(name: &str) -> String {
    name.replace('"', "_").replace('\r', "_").replace('\n', "_")
}

fn stream_file(
    file: tokio::fs::File,
    remaining: u64,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    futures::stream::unfold((file, remaining), |(mut f, mut rem)| async move {
        if rem == 0 {
            return None;
        }
        let cap = rem.min(CHUNK) as usize;
        let mut buf = vec![0u8; cap];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                rem -= n as u64;
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), (f, rem)))
            }
            Err(e) => Some((Err(e), (f, rem))),
        }
    })
}

/// `If-Range` 命中时是否该用分片。与 Starlette FileResponse 对齐：
/// `If-Range` 值等于当前 `ETag` 或 `Last-Modified` 才沿用 Range，否则回落全量 200。
/// 这样能避免「文件已被替换，Agent 却拼出前半旧版 + 后半新版」的坏文件。
fn should_use_range(if_range: Option<&str>, etag: &str, last_modified: &str) -> bool {
    match if_range {
        Some(ir) if !ir.trim().is_empty() => ir == etag || ir == last_modified,
        _ => true, // 未带 If-Range：照常沿用 Range
    }
}

/// 流式下发一个本地文件。`extra` 为附加响应头（如 X-Model-SHA256 / X-File-SHA256）。
/// 文件不存在返回 404（与 Python「记录在、文件不在」的报错语义一致）。
pub async fn file_response(
    path: &Path,
    filename: &str,
    content_type: &str,
    extra: &[(&str, String)],
    range: Option<&str>,
    if_range: Option<&str>,
) -> Result<Response, ApiError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ApiError::not_found("模型文件缺失（记录在，文件不在）"))?;
    let meta = file
        .metadata()
        .await
        .map_err(|_| ApiError::not_found("模型文件缺失（记录在，文件不在）"))?;
    let size = meta.len();
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let etag = format!("\"{}:{}\"", mtime.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), size);
    let last_modified = http_date(mtime);

    // If-Range 与当前版本不符时，忽略 Range，整份全量重下。
    let effective_range = if should_use_range(if_range, &etag, &last_modified) {
        range
    } else {
        None
    };
    // Range 语法有效但起点越界（start >= size）：按 RFC 7233 回 416，而不是回退 200 全量。
    // 否则 Agent 断点续传时若 offset 已到 / 越过文件末尾，拿到 200 全量再 append，
    // 会把整份文件再写一遍（文件翻倍、SHA 永远对不上、永远落不了位）。
    if range_out_of_bounds(effective_range, size) {
        let mut b = Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{size}"));
        for (name, value) in extra {
            if let Ok(v) = HeaderValue::from_str(value) {
                b = b.header(*name, v);
            }
        }
        return Ok(b
            .body(Body::empty())
            .map_err(|e| ApiError::internal(format!("构造响应失败：{e}")))?);
    }
    let range_part = parse_range(effective_range, size);
    let (status, start, length) = match range_part {
        Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        None => (StatusCode::OK, 0, size),
    };

    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", sanitize_filename(filename)),
        )
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::LAST_MODIFIED, &last_modified)
        .header(header::CONTENT_LENGTH, length);
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", start, start + length - 1, size),
        );
    }
    for (name, value) in extra {
        if let Ok(v) = HeaderValue::from_str(value) {
            builder = builder.header(*name, v);
        }
    }

    // seek 到起点再流式吐。仅分片请求需要 seek（全量从头开始）。
    if start > 0 {
        if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
            return Err(ApiError::internal(format!("读取文件失败：{e}")));
        }
    }

    let body = Body::from_stream(stream_file(file, length));
    Ok(builder.body(body).map_err(|e| ApiError::internal(format!("构造响应失败：{e}")))?)
}
