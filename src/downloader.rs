//! 整本书后台下载：目录抓取 → 逐章下载正文 → 写入离线缓存，可查询进度 / 取消。

use crate::book::BookService;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::model::{Book, BookChapter, CachedChapter};
use futures::stream::{self, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 下载任务状态
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTask {
    pub id: String,
    pub book_url: String,
    pub origin: String,
    pub book_name: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    /// pending / running / done / cancelled / error
    pub status: String,
    pub message: String,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    #[serde(skip)]
    pub cancel: Arc<AtomicBool>,
}

/// 启动下载的参数
#[derive(Clone)]
pub struct DownloadParams {
    pub origin: String,
    pub book_url: String,
    pub toc_url: String,
    pub name: String,
    pub author: String,
}

pub struct DownloadManager {
    db: Arc<Db>,
    books: Arc<BookService>,
    tasks: Arc<Mutex<HashMap<String, DownloadTask>>>,
    /// 同时下载的章节数
    concurrency: usize,
}

impl DownloadManager {
    pub fn new(db: Arc<Db>, books: Arc<BookService>) -> Self {
        Self {
            db,
            books,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            concurrency: 4,
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, DownloadTask>> {
        self.tasks.lock().unwrap()
    }

    /// 启动一个后台下载任务，立即返回任务快照
    pub fn start(&self, params: DownloadParams) -> AppResult<DownloadTask> {
        if params.origin.is_empty() || params.book_url.is_empty() {
            return Err(AppError::bad_request("缺少书源或书籍地址"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let task = DownloadTask {
            id: id.clone(),
            book_url: params.book_url.clone(),
            origin: params.origin.clone(),
            book_name: params.name.clone(),
            total: 0,
            done: 0,
            failed: 0,
            status: "pending".into(),
            message: "准备中".into(),
            created_at: chrono::Utc::now().timestamp_millis(),
            finished_at: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.map().insert(id.clone(), task.clone());

        // 克隆依赖供后台任务使用
        let db = self.db.clone();
        let books = self.books.clone();
        let tasks = self.tasks.clone();
        let concurrency = self.concurrency;
        let cancel = task.cancel.clone();
        let task_id = id.clone();

        tokio::spawn(async move {
            run_download(task_id, params, db, books, tasks, concurrency, cancel).await;
        });

        Ok(task)
    }

    /// 列出所有任务（新任务在前）
    pub fn list(&self) -> Vec<DownloadTask> {
        let mut all: Vec<DownloadTask> = self.map().values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all
    }

    pub fn get(&self, id: &str) -> Option<DownloadTask> {
        self.map().get(id).cloned()
    }

    /// 取消任务：设置取消标志；若尚未完成，状态立即置为 cancelled
    pub fn cancel(&self, id: &str) -> AppResult<()> {
        let mut map = self.map();
        let task = map.get_mut(id).ok_or_else(|| AppError::not_found("任务不存在"))?;
        task.cancel.store(true, Ordering::Relaxed);
        if task.status == "pending" || task.status == "running" {
            task.status = "cancelled".into();
            task.message = "已取消".into();
            task.finished_at = Some(chrono::Utc::now().timestamp_millis());
        }
        Ok(())
    }

    /// 清除已完成/取消/失败的任务记录
    pub fn clear_finished(&self) -> usize {
        let mut map = self.map();
        let before = map.len();
        map.retain(|_, t| t.status == "pending" || t.status == "running");
        before - map.len()
    }
}

/// 更新指定任务的字段
fn patch<F: FnOnce(&mut DownloadTask)>(tasks: &Arc<Mutex<HashMap<String, DownloadTask>>>, id: &str, f: F) {
    if let Some(t) = tasks.lock().unwrap().get_mut(id) {
        f(t);
    }
}

/// 后台下载主流程
async fn run_download(
    id: String,
    params: DownloadParams,
    db: Arc<Db>,
    books: Arc<BookService>,
    tasks: Arc<Mutex<HashMap<String, DownloadTask>>>,
    concurrency: usize,
    cancel: Arc<AtomicBool>,
) {
    patch(&tasks, &id, |t| {
        t.status = "running".into();
        t.message = "获取目录".into();
    });

    // 1. 找书源
    let sources = match db.list_sources(false) {
        Ok(s) => s,
        Err(e) => {
            patch(&tasks, &id, |t| {
                t.status = "error".into();
                t.message = format!("读取书源失败: {}", e.message);
                t.finished_at = Some(chrono::Utc::now().timestamp_millis());
            });
            return;
        }
    };
    let source = match sources.into_iter().find(|s| s.book_source_url == params.origin) {
        Some(s) => s,
        None => {
            patch(&tasks, &id, |t| {
                t.status = "error".into();
                t.message = "书源不存在".into();
                t.finished_at = Some(chrono::Utc::now().timestamp_millis());
            });
            return;
        }
    };

    // 2. 抓目录
    let book = Book {
        book_url: params.book_url.clone(),
        origin: params.origin.clone(),
        origin_name: source.book_source_name.clone(),
        name: params.name.clone(),
        author: params.author.clone(),
        toc_url: if params.toc_url.is_empty() { params.book_url.clone() } else { params.toc_url.clone() },
        ..Default::default()
    };
    let chapters: Vec<BookChapter> = match books.chapters(&source, &book).await {
        Ok(c) => c,
        Err(e) => {
            patch(&tasks, &id, |t| {
                t.status = "error".into();
                t.message = format!("获取目录失败: {}", e.message);
                t.finished_at = Some(chrono::Utc::now().timestamp_millis());
            });
            return;
        }
    };

    let total = chapters.len();
    patch(&tasks, &id, |t| {
        t.total = total;
        t.message = format!("共 {} 章，开始下载", total);
    });

    if cancel.load(Ordering::Relaxed) {
        return;
    }

    // 3. 并发逐章下载（buffer_unordered 限制并发）
    let next_urls: Vec<Option<String>> = (0..chapters.len())
        .map(|i| chapters.get(i + 1).map(|c| c.url.clone()))
        .collect();
    let results = stream::iter(chapters.into_iter().enumerate().map(|(idx, ch)| {
        let next_url = next_urls[idx].clone();
        let db = db.clone();
        let books = books.clone();
        let source = source.clone();
        let book = book.clone();
        let cancel = cancel.clone();
        let book_url = book.book_url.clone();
        async move {
            if cancel.load(Ordering::Relaxed) {
                return (false, true); // (success, skipped)
            }
            // 已缓存则跳过
            if let Ok(Some(_)) = db.get_cached(&book_url, &ch.url) {
                return (false, true);
            }
            match books.content_with_next(&source, &book, &ch, next_url.as_deref()).await {
                Ok(content) => {
                    let cached = CachedChapter {
                        book_url: book_url.clone(),
                        chapter_url: ch.url.clone(),
                        title: ch.title.clone(),
                        content,
                        cached_at: chrono::Utc::now().timestamp_millis(),
                    };
                    match db.cache_chapter(&cached) {
                        Ok(_) => (true, false),
                        Err(_) => (false, false),
                    }
                }
                Err(_) => (false, false),
            }
        }
    }))
    .buffer_unordered(concurrency);

    let mut stream = results;
    while let Some((success, skipped)) = stream.next().await {
        if cancel.load(Ordering::Relaxed) { break; }
        patch(&tasks, &id, |t| {
            if skipped {
                t.done += 1;
            } else if success {
                t.done += 1;
            } else {
                t.failed += 1;
            }
            t.message = format!("已下载 {}/{}", t.done, t.total);
        });
    }

    // 4. 收尾
    patch(&tasks, &id, |t| {
        t.finished_at = Some(chrono::Utc::now().timestamp_millis());
        if cancel.load(Ordering::Relaxed) {
            t.status = "cancelled".into();
            t.message = "已取消".into();
        } else {
            t.status = "done".into();
            t.message = format!("完成：成功 {} 章，失败 {} 章", t.done, t.failed);
        }
    });
}
