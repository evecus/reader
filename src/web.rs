use std::path::Path;

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use serde_json::json;

use crate::state::AppState;

/// The built H5 client is compiled into the binary. `debug-embed` keeps that
/// true in debug builds too, so a `cargo build` artifact is just as self
/// contained as the release image. No WEB_DIR / disk lookup at runtime.
#[derive(RustEmbed)]
#[folder = "web/dist"]
pub struct Assets;

fn content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream"),
    }
}

fn immutable(path: &str) -> bool {
    // Vite emits content-hashed chunks under assets/; the fixed entry names
    // (client.js, index.css) must stay no-cache so a redeploy is picked up.
    path.starts_with("assets/") && {
        let stem = path.rsplit('/').next().unwrap_or("");
        let core = stem.split('.').next().unwrap_or("");
        core.rsplit_once('-')
            .map(|(_, tail)| tail.len() >= 6 && tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(false)
    }
}

fn send_embedded(path: &str, cache: &'static str) -> Response {
    let Some(file) = Assets::get(path) else {
        return (StatusCode::NOT_FOUND, json_error("NOT_FOUND", "not found")).into_response();
    };
    let mut resp = Response::new(Body::from(file.data.into_owned()));
    if let Ok(v) = HeaderValue::from_str(content_type(path)) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache),
    );
    resp
}

fn json_error(code: &str, message: &str) -> axum::Json<serde_json::Value> {
    axum::Json(json!({ "error": { "code": code, "message": message } }))
}

/// The SPA fallback must not shadow an API route: a client parsing
/// `error.code` would otherwise report "server broken" instead of 404.
pub async fn web_fallback(request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/').to_string();
    let raw = percent_decode(&path);
    if raw == "api" || raw.starts_with("api/") {
        return (
            StatusCode::NOT_FOUND,
            json_error("NOT_FOUND", "not found"),
        )
            .into_response();
    }
    // Reject path traversal explicitly even though the embedded lookup cannot
    // escape the binary: it keeps the intent obvious and blocks `..%2f..%2f`.
    if raw.split('/').any(|seg| seg == "..") {
        return (
            StatusCode::NOT_FOUND,
            json_error("NOT_FOUND", "not found"),
        )
            .into_response();
    }
    if !raw.is_empty() && Assets::get(&raw).is_some() {
        let cache = if immutable(&raw) {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return send_embedded(&raw, cache);
    }
    if Assets::get("index.html").is_some() {
        return send_embedded("index.html", "no-cache");
    }
    (
        StatusCode::NOT_FOUND,
        json_error("NO_WEB_CLIENT", "embedded web client is missing"),
    )
        .into_response()
}


pub fn url_decode(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .into_owned()
}

fn percent_decode(input: &str) -> String {
    url_decode(input)
}

/// Reflect the request Origin by default (LAN self-hosting, no cookies).
/// When CORS_ORIGINS is set, an unknown origin is rejected before routing.
pub async fn cors_guard(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let allowed = state.config.cors_origins.is_empty()
        || state.config.cors_origins.iter().any(|o| o == "*" || o.as_str() == origin.as_str());
    if !origin.is_empty() && !allowed {
        return (
            StatusCode::FORBIDDEN,
            json_error("FORBIDDEN", "origin not allowed"),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let reflected = if state.config.cors_origins.is_empty() || state.config.cors_origins.iter().any(|o| o == "*") {
        if origin.is_empty() { "*".to_string() } else { origin.clone() }
    } else {
        origin.clone()
    };
    if let Ok(v) = HeaderValue::from_str(&reflected) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,PUT,PATCH,DELETE,OPTIONS"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("origin"));
    response
}


