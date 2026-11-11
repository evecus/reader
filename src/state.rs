use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde_json::{json, Value};

use crate::{
    auth::{auth as do_auth, AuthUser},
    config::Config,
    db::Db,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Arc<Db>,
    pub http: reqwest::Client,
    pub scan: Arc<tokio::sync::Mutex<ScanState>>,
    pub sources: Arc<crate::sources::SourceHost>,
}

/// Scan progress is process-local: the client polls /api/v1/library/scan.
#[derive(Default)]
pub struct ScanState {
    pub running: bool,
    pub started_at: i64,
    pub finished_at: i64,
    pub last_error: Option<String>,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub scanned: usize,
}

/// Convenience extractor: `Auth` in a handler signature replaces the repeated
/// `let user = auth(&headers, &state)?` line and keeps the error shape uniform.
pub struct Auth(pub AuthUser);

impl FromRequestParts<AppState> for Auth {
    type Rejection = crate::error::AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let user = do_auth(&parts.headers, state)?;
        Ok(Auth(user))
    }
}

pub fn ok() -> Value {
    json!({})
}

/// Current wall-clock time in milliseconds, the unit used by every DB timestamp.
pub fn now_ms() -> i64 {
    Config::now_ms()
}




// Compile-time proof the state is a valid axum state type.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync::<AppState>();
    fn assert_extractor<S: Send + Sync + 'static, E: axum::extract::FromRequestParts<S>>() {}
    assert_extractor::<AppState, Auth>();
};
