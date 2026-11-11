//! Server entry point. Two modes:
//! - `reader`            — the HTTP server (default)
//! - `reader plugin-run <plugin-dir>` — QuickJS plugin child process (NDJSON JSON-RPC on stdio)

mod assets;
mod auth;
mod browse;
mod config;
mod db;
mod dto;
mod error;
mod indexer;
mod opds;
mod plugin_runner;
mod routes;
mod settings;
mod sources;
mod state;
mod sync;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Config;
use crate::db::Db;
use crate::state::{AppState, ScanState};

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("plugin-run") => {
            let dir = args.next().expect("usage: reader plugin-run <plugin-dir>");
            if let Err(e) = plugin_runner::run(&PathBuf::from(dir)) {
                // The host surfaces this as a failed load; keep stderr terse.
                eprintln!("plugin-run failed: {e}");
                std::process::exit(1);
            }
        }
        Some("healthcheck") => {
            // Used by container HEALTHCHECK: a local DB open proves storage is writable.
            let config = match Config::load() {
                Ok(c) => c,
                Err(_) => std::process::exit(1),
            };
            let db_path = config.data_dir.join("reader.db");
            match Db::open(&db_path) {
                Ok(_) => println!("ok"),
                Err(_) => std::process::exit(1),
            }
        }
        _ => {
            if let Err(e) = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(serve())
            {
                eprintln!("reader failed to start: {e}");
                std::process::exit(1);
            }
        }
    }
}

async fn serve() -> error::ApiResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info")
            }),
        )
        .with_target(false)
        .init();

    let config = Arc::new(Config::load()?);
    std::fs::create_dir_all(&config.data_dir)?;
    std::fs::create_dir_all(config.data_dir.join("covers"))?;
    std::fs::create_dir_all(config.data_dir.join("plugins"))?;
    std::fs::create_dir_all(config.data_dir.join("structures"))?;
    std::fs::create_dir_all(&config.books_dir)?;

    let db = Arc::new(Db::open(&config.data_dir.join("reader.db"))?);
    settings::ensure_defaults(&db)?;
    tracing::info!("database ready at {}", config.data_dir.join("reader.db").display());

    let host = Arc::new(sources::SourceHost::new(db.clone(), config.data_dir.clone()));
    let state = AppState {
        config: config.clone(),
        db: db.clone(),
        http: reqwest::Client::builder()
            .user_agent(concat!("reader-rs/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client"),
        scan: Arc::new(tokio::sync::Mutex::new(ScanState::default())),
        sources: host,
    };

    // Periodic background scan (SCAN_INTERVAL seconds, 0 disables).
    if config.scan_interval > 0 {
        let bg_state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(bg_state.config.scan_interval.max(30)));
            ticker.tick().await; // skip the immediate tick; boot scan is explicit
            loop {
                ticker.tick().await;
                indexer::scan_async(bg_state.clone());
            }
        });
    }

    let app = routes::router(state.clone());
    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await
        .map_err(|e| error::api_err(500, "BIND", format!("cannot bind {addr}: {e}")))?;
    tracing::info!("reader listening on http://{addr}");

    let shutdown_state = state.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down");
        shutdown_state.sources.shutdown().await;
    });
    server.await.map_err(|e| error::api_err(500, "SERVER", format!("server error: {e}")))?;
    Ok(())
}
