//! 统一错误类型与 HTTP 响应映射。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::fmt;

#[derive(Debug, Clone)]
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl AppError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { status, code: code.into(), message: message.into() }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", msg)
    }
    pub fn timeout(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::GATEWAY_TIMEOUT, "TIMEOUT", msg)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self { Self::internal(format!("DB: {e}")) }
}
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self { Self::bad_request(format!("JSON: {e}")) }
}
impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self { Self::internal(format!("HTTP: {e}")) }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": { "code": self.code, "message": self.message } }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
