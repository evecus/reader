//! 应用全局状态。

use crate::book::BookService;
use crate::db::Db;
use crate::downloader::DownloadManager;
use crate::error::AppResult;
use crate::fetcher::Fetcher;
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppState {
    pub db: Arc<Db>,
    pub books: Arc<BookService>,
    pub fetcher: Arc<Fetcher>,
    pub downloader: Arc<DownloadManager>,
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> AppResult<Self> {
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| crate::error::AppError::internal(format!("create data dir: {e}")))?;
        let db = Arc::new(Db::open(&data_dir.join("reader.db"))?);
        // Fetcher 与业务链共享同一 Cookie jar
        let fetcher = Arc::new(Fetcher::new());
        let books = Arc::new(BookService::new(fetcher.clone(), db.clone()));
        let downloader = Arc::new(DownloadManager::new(db.clone(), books.clone()));
        Ok(Self { db, books, fetcher, downloader, data_dir })
    }
}
