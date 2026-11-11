use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub type ApiResult<T> = Result<T, AppError>;

/// The single error serialiser. Every endpoint keeps the same shape so old
/// clients keep working: { "error": { "code": ..., "message": ... } }.
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl AppError {
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn internal(message: impl std::fmt::Display) -> Self {
        Self::new(500, "INTERNAL", message.to_string())
    }
}

pub fn api_err(status: u16, code: impl Into<String>, message: impl Into<String>) -> AppError {
    AppError::new(status, code, message)
}

pub fn bad_request(message: impl Into<String>, code: impl Into<String>) -> AppError {
    AppError::new(400, code, message)
}

pub fn unauthorized(message: impl Into<String>, code: impl Into<String>) -> AppError {
    AppError::new(401, code, message)
}

pub fn forbidden(message: impl Into<String>, code: impl Into<String>) -> AppError {
    AppError::new(403, code, message)
}

pub fn not_found(message: impl Into<String>, code: impl Into<String>) -> AppError {
    AppError::new(404, code, message)
}

pub fn conflict(message: impl Into<String>, code: impl Into<String>) -> AppError {
    AppError::new(409, code, message)
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

macro_rules! impl_from {
    ($($ty:ty),* $(,)?) => {
        $(
            impl From<$ty> for AppError {
                fn from(e: $ty) -> Self { AppError::internal(e) }
            }
        )*
    };
}

impl_from!(
    rusqlite::Error,
    std::io::Error,
    serde_json::Error,
    zip::result::ZipError,
    reqwest::Error,
    quick_xml::Error,
    std::string::FromUtf8Error,
    base64::DecodeError,
    scrypt::errors::InvalidParams,
    scrypt::errors::InvalidOutputLen,
);

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        AppError::new(400, "BAD_REQUEST", e.to_string())
    }
}
