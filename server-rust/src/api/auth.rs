//! 共享 token 鉴权提取器，以及从请求里取令牌的小工具。
//!
//! 与 Python `core/security.py` 的 `require_token` 对齐：
//! - REST 共享 token 走 `X-Auth-Token` 头或 `?token=` 查询参数；
//! - 下载令牌 / 传输令牌是另外的信任域，由各路由自行读取（见 models / files）。

use axum::extract::FromRequestParts;
use axum::http::header::HeaderMap;
use axum::http::request::Parts;

use crate::error::ApiError;
use crate::security;

/// 按名字读请求头（大小写不敏感，axum 已统一小写存储）。
pub fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 从查询串里取 `token`（一次解析，URL 解码）。
pub fn query_token(query: Option<&str>) -> Option<String> {
    let q = query?;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == "token" {
                let decoded = urlencoding::decode(v).unwrap_or_else(|_| v.into());
                let s = decoded.trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// 从 REST 请求里取候选共享 token（头优先，其次查询串）。
pub fn candidate_token(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    header_value(headers, "x-auth-token").or_else(|| query_token(query))
}

/// 共享 token 鉴权提取器。作为 handler 参数即等于 FastAPI 的 `Depends(require_token)`。
/// 校验失败统一 401（与 Python 的报错文案一致）。
pub struct SharedAuth;

impl<S> FromRequestParts<S> for SharedAuth
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let token = candidate_token(&parts.headers, parts.uri.query());
        if security::verify_token(token.as_deref()) {
            Ok(SharedAuth)
        } else {
            Err(ApiError::unauthorized("无效或缺失的 token（请检查 X-Auth-Token）"))
        }
    }
}
