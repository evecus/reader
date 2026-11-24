mod api;
mod book;
mod cookie;
mod db;
mod downloader;
mod engine;
mod error;
mod fetcher;
mod model;
mod state;

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rust_embed::RustEmbed;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("reader=info".parse().unwrap()))
        .init();

    let data_dir = std::env::var("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data"));
    let port: u16 = std::env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(5888);

    let state = match state::AppState::new(data_dir) {
        Ok(s) => Arc::new(s),
        Err(e) => { eprintln!("初始化失败: {e}"); std::process::exit(1); }
    };

    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let app = Router::new()
        .route("/api/health", get(health))
        .merge(api::router(state))
        .fallback(static_handler)
        .layer(cors);

    let addr = format!("0.0.0.0:{port}");
    println!("reader 已启动: http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("绑定端口失败");
    axum::serve(listener, app).await.expect("服务异常");
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "name": "reader", "version": "0.1.0" }))
}

async fn static_handler(uri: axum::extract::OriginalUri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let mut resp = Response::new(axum::body::Body::from(file.data.into_owned()));
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        );
        // index.html 不缓存，静态资源长缓存
        let cache = if path.ends_with("index.html") { "no-cache" } else { "public, max-age=86400" };
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        return resp;
    }

    // SPA 兜底：返回 index.html
    if let Some(index) = Assets::get("index.html") {
        let mut resp = Response::new(axum::body::Body::from(index.data.into_owned()));
        resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
        return resp;
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}
