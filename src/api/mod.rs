//! REST API 路由与处理函数。

use crate::error::{AppError, AppResult};
use crate::model::{Book, CachedChapter, ExploreEntry, ShelfItem};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/sources", axum::routing::get(list_sources).post(import_sources))
        .route("/api/sources/{url}", axum::routing::delete(delete_source))
        .route("/api/sources/{url}/toggle", axum::routing::patch(toggle_source))
        .route("/api/sources/test", axum::routing::post(test_source))
        .route("/api/search", axum::routing::get(search))
        .route("/api/explore", axum::routing::get(explore))
        .route("/api/book/info", axum::routing::get(book_info))
        .route("/api/book/chapters", axum::routing::get(chapters))
        .route("/api/book/content", axum::routing::get(content))
        .route("/api/shelf", axum::routing::get(list_shelf).post(add_shelf))
        .route("/api/shelf/{book_url}", axum::routing::delete(remove_shelf))
        .route("/api/shelf/progress", axum::routing::patch(update_progress))
        .route("/api/cache", axum::routing::get(list_cache).delete(clear_cache))
        .route("/api/cache/chapter", axum::routing::post(cache_chapter).delete(delete_cache))
        .route("/api/download", axum::routing::get(list_downloads).post(start_download).delete(clear_finished_downloads))
        .route("/api/download/{id}", axum::routing::get(get_download).delete(cancel_download))
        .route("/api/settings", axum::routing::get(get_settings).post(set_setting))
        .with_state(state)
}

// ---- 书源 ----
async fn list_sources(State(s): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    let sources = s.db.list_sources(false)?;
    Ok(Json(json!(sources)))
}

#[derive(Debug, Deserialize)]
struct ImportBody { content: String }

async fn import_sources(State(s): State<Arc<AppState>>, Json(body): Json<ImportBody>) -> AppResult<Json<Value>> {
    let content = body.content.trim();
    let sources: Vec<crate::model::BookSource> = if content.starts_with('[') {
        serde_json::from_str(content)?
    } else if content.starts_with('{') {
        vec![serde_json::from_str(content)?]
    } else if content.starts_with("http") {
        // 网络 URL 导入
        let (text, _) = s.fetcher.get(content, None).await?;
        if text.trim().starts_with('[') {
            serde_json::from_str(&text)?
        } else {
            vec![serde_json::from_str(&text)?]
        }
    } else {
        return Err(AppError::bad_request("不支持的导入内容（需 JSON 或 URL）"));
    };
    let mut count = 0;
    for src in &sources {
        if src.book_source_url.is_empty() { continue; }
        let raw = serde_json::to_string(src)?;
        s.db.upsert_source(src, &raw)?;
        count += 1;
    }
    Ok(Json(json!({ "imported": count })))
}

async fn delete_source(State(s): State<Arc<AppState>>, Path(url): Path<String>) -> AppResult<StatusCode> {
    s.db.delete_source(&url)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn toggle_source(State(s): State<Arc<AppState>>, Path(url): Path<String>, Json(body): Json<Value>) -> AppResult<StatusCode> {
    let enabled = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    s.db.set_source_enabled(&url, enabled)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct TestBody { url: String }

async fn test_source(State(s): State<Arc<AppState>>, Json(body): Json<TestBody>) -> AppResult<Json<Value>> {
    // 尝试搜索 "测试"，看能否解析
    let sources = s.db.list_sources(false)?;
    let src = sources.iter().find(|x| x.book_source_url == body.url)
        .ok_or_else(|| AppError::not_found("书源不存在"))?;
    match s.books.search(src, "test", 1).await {
        Ok(books) => Ok(Json(json!({ "ok": true, "count": books.len() }))),
        Err(e) => Ok(Json(json!({ "ok": false, "error": e.message }))),
    }
}

// ---- 搜索 / 发现 ----
#[derive(Debug, Deserialize)]
struct SearchParams { key: String, #[serde(default = "default_page")] page: i32, origin: Option<String> }
fn default_page() -> i32 { 1 }

async fn search(State(s): State<Arc<AppState>>, Query(p): Query<SearchParams>) -> AppResult<Json<Value>> {
    let sources = if let Some(origin) = &p.origin {
        s.db.list_sources(false)?.into_iter().filter(|x| &x.book_source_url == origin).collect()
    } else {
        s.db.list_sources(true)?
    };
    let results = s.books.search_multi(sources, &p.key, p.page).await;

    let mut all_books: Vec<Book> = Vec::new();
    let mut errors = Vec::new();
    for (name, result) in results {
        match result {
            Ok(mut books) => {
                for b in books.iter_mut() {
                    // 合并已存在书籍的来源
                    if let Some(existing) = all_books.iter_mut().find(|x| x.name == b.name && x.author == b.author) {
                        for o in &b.origins {
                            if !existing.origins.contains(o) { existing.origins.push(o.clone()); }
                        }
                    } else {
                        all_books.push(b.clone());
                    }
                }
            }
            Err(e) => errors.push(json!({ "source": name, "error": e.message })),
        }
    }
    Ok(Json(json!({ "books": all_books, "errors": errors })))
}

#[derive(Debug, Deserialize)]
struct ExploreParams { origin: String, url: String, #[serde(default = "default_page")] page: i32 }

async fn explore(State(s): State<Arc<AppState>>, Query(p): Query<ExploreParams>) -> AppResult<Json<Value>> {
    let sources = s.db.list_sources(false)?;
    let src = sources.iter().find(|x| x.book_source_url == p.origin)
        .ok_or_else(|| AppError::not_found("书源不存在"))?;
    // 解析 exploreUrl JSON 找到对应 title 的 url
    let target = if p.url.starts_with("http") { p.url.clone() } else {
        let entries: Vec<ExploreEntry> = serde_json::from_str(src.explore_url.as_deref().unwrap_or("[]")).unwrap_or_default();
        entries.iter().find(|e| e.title == p.url).map(|e| e.url.clone()).unwrap_or(p.url.clone())
    };
    let books = s.books.explore(src, &target, p.page).await?;
    Ok(Json(json!({ "books": books })))
}

// ---- 书籍 ----
#[derive(Debug, Deserialize)]
struct BookInfoParams { origin: String, url: String }

async fn book_info(State(s): State<Arc<AppState>>, Query(p): Query<BookInfoParams>) -> AppResult<Json<Value>> {
    let sources = s.db.list_sources(false)?;
    let src = sources.iter().find(|x| x.book_source_url == p.origin)
        .ok_or_else(|| AppError::not_found("书源不存在"))?;
    let book = s.books.book_info(src, &p.url).await?;
    Ok(Json(json!(book)))
}

#[derive(Debug, Deserialize)]
struct ChaptersParams { origin: String, url: String, toc_url: Option<String>, name: Option<String>, author: Option<String> }

async fn chapters(State(s): State<Arc<AppState>>, Query(p): Query<ChaptersParams>) -> AppResult<Json<Value>> {
    let sources = s.db.list_sources(false)?;
    let src = sources.iter().find(|x| x.book_source_url == p.origin)
        .ok_or_else(|| AppError::not_found("书源不存在"))?;
    let book = Book {
        book_url: p.url.clone(), origin: p.origin.clone(),
        origin_name: src.book_source_name.clone(),
        name: p.name.unwrap_or_default(), author: p.author.unwrap_or_default(),
        toc_url: p.toc_url.unwrap_or(p.url), ..Default::default()
    };
    let chapters = s.books.chapters(src, &book).await?;
    Ok(Json(json!({ "chapters": chapters })))
}

#[derive(Debug, Deserialize)]
struct ContentParams { origin: String, url: String, book_url: String, title: Option<String>, next_url: Option<String> }

async fn content(State(s): State<Arc<AppState>>, Query(p): Query<ContentParams>) -> AppResult<Json<Value>> {
    let sources = s.db.list_sources(false)?;
    let src = sources.iter().find(|x| x.book_source_url == p.origin)
        .ok_or_else(|| AppError::not_found("书源不存在"))?;
    let book = Book { book_url: p.book_url.clone(), origin: p.origin.clone(), ..Default::default() };
    let chapter = crate::model::BookChapter {
        index: 0, title: p.title.unwrap_or_default(), url: p.url.clone(),
        is_volume: false, is_vip: false,
    };
    let text = s.books.content_with_next(src, &book, &chapter, p.next_url.as_deref()).await?;
    Ok(Json(json!({ "content": text })))
}

// ---- 书架 ----
async fn list_shelf(State(s): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    Ok(Json(json!(s.db.list_shelf()?)))
}

async fn add_shelf(State(s): State<Arc<AppState>>, Json(item): Json<ShelfItem>) -> AppResult<Json<Value>> {
    s.db.upsert_shelf(&item)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct RemoveShelfParams { origin: Option<String> }
async fn remove_shelf(State(s): State<Arc<AppState>>, Path(book_url): Path<String>, Query(q): Query<RemoveShelfParams>) -> AppResult<StatusCode> {
    s.db.remove_shelf(&book_url, q.origin.as_deref().unwrap_or(""))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ProgressBody { book_url: String, origin: String, chapter_index: usize, chapter_title: String }
async fn update_progress(State(s): State<Arc<AppState>>, Json(body): Json<ProgressBody>) -> AppResult<Json<Value>> {
    if let Some(mut item) = s.db.get_shelf(&body.book_url, &body.origin)? {
        item.dur_chapter_index = body.chapter_index;
        item.dur_chapter_title = body.chapter_title;
        item.updated_at = chrono::Utc::now().timestamp_millis();
        s.db.upsert_shelf(&item)?;
    }
    Ok(Json(json!({ "ok": true })))
}

// ---- 缓存 ----
async fn list_cache(State(s): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    let items = s.db.list_cached()?;
    let out: Vec<Value> = items.iter().map(|c| json!({
        "bookUrl": c.book_url, "chapterUrl": c.chapter_url, "title": c.title,
        "cachedAt": c.cached_at, "size": c.content.len()
    })).collect();
    Ok(Json(json!(out)))
}

#[derive(Debug, Deserialize)]
struct CacheBody { book_url: String, chapter_url: String, title: String, content: String }
async fn cache_chapter(State(s): State<Arc<AppState>>, Json(body): Json<CacheBody>) -> AppResult<Json<Value>> {
    s.db.cache_chapter(&CachedChapter {
        book_url: body.book_url, chapter_url: body.chapter_url, title: body.title,
        content: body.content, cached_at: chrono::Utc::now().timestamp_millis(),
    })?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct DeleteCacheParams { book_url: String, chapter_url: String }
async fn delete_cache(State(s): State<Arc<AppState>>, Query(p): Query<DeleteCacheParams>) -> AppResult<StatusCode> {
    s.db.delete_cached(&p.book_url, &p.chapter_url)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn clear_cache(State(s): State<Arc<AppState>>) -> AppResult<StatusCode> {
    s.db.clear_cached()?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- 后台下载 ----
#[derive(Debug, Deserialize)]
struct DownloadBody {
    origin: String,
    book_url: String,
    #[serde(default)]
    toc_url: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    author: String,
}

async fn start_download(State(s): State<Arc<AppState>>, Json(body): Json<DownloadBody>) -> AppResult<Json<Value>> {
    use crate::downloader::DownloadParams;
    let task = s.downloader.start(DownloadParams {
        origin: body.origin,
        book_url: body.book_url,
        toc_url: body.toc_url,
        name: body.name,
        author: body.author,
    })?;
    Ok(Json(json!(task)))
}

async fn list_downloads(State(s): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    Ok(Json(json!(s.downloader.list())))
}

async fn get_download(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> AppResult<Json<Value>> {
    let task = s.downloader.get(&id).ok_or_else(|| AppError::not_found("任务不存在"))?;
    Ok(Json(json!(task)))
}

async fn cancel_download(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> AppResult<StatusCode> {
    s.downloader.cancel(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_finished_downloads(State(s): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    let n = s.downloader.clear_finished();
    Ok(Json(json!({ "cleared": n })))
}

// ---- 设置 ----
async fn get_settings(State(s): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    Ok(Json(Value::Object(s.db.all_settings()?)))
}
#[derive(Debug, Deserialize)]
struct SettingBody { key: String, value: String }
async fn set_setting(State(s): State<Arc<AppState>>, Json(body): Json<SettingBody>) -> AppResult<Json<Value>> {
    s.db.set_setting(&body.key, &body.value)?;
    Ok(Json(json!({ "ok": true })))
}
