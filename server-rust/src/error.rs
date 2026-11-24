//! API 错误类型。统一映射为 FastAPI 风格的 `{"detail": "..."}` 响应体，
//! 状态码与 Python 版 HTTPException 逐处对齐，前端无需感知实现语言差异。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub struct ApiError {
    pub status: StatusCode,
    pub detail: String,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    pub fn bad_request(d: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, d)
    }
    pub fn unauthorized(d: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, d)
    }
    pub fn not_found(d: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, d)
    }
    pub fn conflict(d: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, d)
    }
    pub fn payload_too_large(d: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, d)
    }
    pub fn internal(d: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, d)
    }
    pub fn bad_gateway(d: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, d)
    }
    pub fn service_unavailable(d: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, d)
    }
    pub fn gateway_timeout(d: impl Into<String>) -> Self {
        Self::new(StatusCode::GATEWAY_TIMEOUT, d)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "detail": self.detail }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
