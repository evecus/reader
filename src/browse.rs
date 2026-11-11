//! Library file manager. Every path goes through `resolve_inside`, which
//! refuses traversal and never follows symlinks (matching lib/paths.ts).

use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::{
    db,
    error::{api_err, bad_request, ApiResult},
    indexer,
    state::AppState,
};

pub fn resolve_inside(books_dir: &Path, rel: &str) -> ApiResult<PathBuf> {
    let normalized = rel.replace('\\', "/");
    let mut out = books_dir.to_path_buf();
    for part in normalized.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(bad_request("path escapes BOOKS_DIR", "PATH_TRAVERSAL")),
            s => {
                if s.contains('\0') { return Err(bad_request("invalid path", "PATH_TRAVERSAL")); }
                out.push(s);
            }
        }
    }
    // Re-check by components after building, to catch any encoded trickery.
    for c in out.components() {
        if let Component::ParentDir = c {
            return Err(bad_request("path escapes BOOKS_DIR", "PATH_TRAVERSAL"));
        }
    }
    Ok(out)
}

fn rel_of(books_dir: &Path, abs: &Path) -> String {
    abs.strip_prefix(books_dir).unwrap_or(abs).to_string_lossy().replace('\\', "/")
}

pub fn browse(state: &AppState, path: &str, page: i64) -> ApiResult<Value> {
    let dir = resolve_inside(&state.config.books_dir, path)?;
    if !dir.exists() { return Err(api_err(404, "NOT_FOUND", "directory not found")); }
    if !dir.is_dir() { return Err(bad_request("not a directory", "NOT_A_DIRECTORY")); }
    let page_size = 100i64;
    let mut entries: Vec<(String, bool, u64, i64)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = ft.is_dir();
        let meta = entry.metadata()?;
        let size = if is_dir { 0 } else { meta.len() };
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64).unwrap_or(0);
        entries.push((name, is_dir, size, mtime));
    }
    entries.sort_by(|a, b| {
        if a.1 != b.1 { return b.1.cmp(&a.1); }
        indexer::natural_compare(&a.0, &b.0)
    });
    let total = entries.len() as i64;
    let start = ((page.max(1) - 1) * page_size) as usize;
    let slice = entries.iter().skip(start).take(page_size as usize);
    let items: Vec<Value> = slice.map(|(name, is_dir, size, mtime)| {
        let rel = if path.is_empty() { name.clone() } else { format!("{}/{}", path.trim_end_matches('/'), name) };
        json!({
            "name": name,
            "path": rel,
            "isDirectory": is_dir,
            "size": size,
            "mtime": mtime,
        })
    }).collect();
    Ok(json!({
        "path": path,
        "items": items,
        "total": total,
        "page": page.max(1),
        "pageSize": page_size,
        "writable": is_writable(&state.config.books_dir),
    }))
}

pub fn is_writable(books_dir: &Path) -> bool {
    let probe = books_dir.join(".reader-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => { let _ = std::fs::remove_file(probe); true }
        Err(_) => false,
    }
}

fn require_writable(state: &AppState) -> ApiResult<()> {
    if !is_writable(&state.config.books_dir) {
        return Err(api_err(403, "READ_ONLY", "BOOKS_DIR is read-only"));
    }
    Ok(())
}

pub fn move_paths(state: &AppState, paths: &[String], target: &str) -> ApiResult<Value> {
    require_writable(state)?;
    let dest_dir = resolve_inside(&state.config.books_dir, target)?;
    std::fs::create_dir_all(&dest_dir)?;
    let mut moved = 0;
    for p in paths {
        let src = resolve_inside(&state.config.books_dir, p)?;
        if !src.exists() { continue; }
        let name = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let dest = dest_dir.join(&name);
        if dest.exists() { continue; }
        std::fs::rename(&src, &dest)?;
        moved += 1;
    }
    Ok(json!({ "moved": moved, "target": target }))
}

pub fn rename(state: &AppState, path: &str, name: &str) -> ApiResult<Value> {
    require_writable(state)?;
    if name.contains('/') || name.contains('\\') || name == ".." {
        return Err(bad_request("invalid name", "BAD_REQUEST"));
    }
    let src = resolve_inside(&state.config.books_dir, path)?;
    let dest = src.parent().unwrap_or(Path::new(".")).join(name);
    if dest.exists() { return Err(api_err(409, "ALREADY_EXISTS", "target already exists")); }
    std::fs::rename(&src, &dest)?;
    Ok(json!({ "path": rel_of(&state.config.books_dir, &dest) }))
}

pub fn mkdir(state: &AppState, path: &str, name: &str) -> ApiResult<Value> {
    require_writable(state)?;
    if name.contains('/') || name.contains('\\') || name == ".." {
        return Err(bad_request("invalid name", "BAD_REQUEST"));
    }
    let parent = resolve_inside(&state.config.books_dir, path)?;
    let dest = parent.join(name);
    std::fs::create_dir_all(&dest)?;
    Ok(json!({ "path": rel_of(&state.config.books_dir, &dest) }))
}

pub fn delete_paths(state: &AppState, paths: &[String]) -> ApiResult<Value> {
    require_writable(state)?;
    let mut removed = 0;
    for p in paths {
        let target = resolve_inside(&state.config.books_dir, p)?;
        if !target.exists() { continue; }
        if target.is_dir() { let _ = std::fs::remove_dir_all(&target); } else { let _ = std::fs::remove_file(&target); }
        // Flag rows missing immediately; the next scan will reconcile.
        state.db.exec(
            "UPDATE book_files SET missing = 1 WHERE rel_path = ?1 OR rel_path LIKE ?2",
            rusqlite::params![p, format!("{}/%", p.trim_end_matches('/'))],
        )?;
        removed += 1;
    }
    Ok(json!({ "removed": removed }))
}

pub fn batch_metadata(state: &AppState, paths: &[String], fields: &Value) -> ApiResult<Value> {
    let mut changed = 0;
    for p in paths {
        let rows = state.db.query(
            "SELECT DISTINCT f.book_id FROM book_files f WHERE f.rel_path = ?1 OR f.rel_path LIKE ?2",
            rusqlite::params![p, format!("{}/%", p.trim_end_matches('/'))],
        )?;
        for r in rows {
            let id = db::s(&r, "book_id");
            crate::dto::set_overrides(state, &id, fields, "admin")?;
            changed += 1;
        }
    }
    Ok(json!({ "updated": changed, "failed": 0 }))
}

pub fn batch_shelf(state: &AppState, body: &Value, user_id: &str) -> ApiResult<Value> {
    let action = body.get("action").and_then(Value::as_str).unwrap_or("add");
    let mut ids: Vec<String> = Vec::new();
    if let Some(arr) = body.get("bookIds").and_then(Value::as_array) {
        ids.extend(arr.iter().filter_map(|v| v.as_str().map(str::to_string)));
    }
    if let Some(paths) = body.get("paths").and_then(Value::as_array) {
        for p in paths.iter().filter_map(|v| v.as_str()) {
            let rows = state.db.query(
                "SELECT DISTINCT f.book_id FROM book_files f WHERE f.rel_path = ?1 OR f.rel_path LIKE ?2",
                rusqlite::params![p, format!("{}/%", p.trim_end_matches('/'))],
            )?;
            ids.extend(rows.iter().map(|r| db::s(r, "book_id")));
        }
    }
    let now = crate::state::now_ms();
    let mut changed = 0;
    for id in ids {
        match action {
            "remove" | "hide" => {
                state.db.exec("UPDATE user_books SET hidden = 1 WHERE user_id = ?1 AND book_id = ?2",
                    rusqlite::params![user_id, id])?;
            }
            _ => {
                state.db.exec(
                    "INSERT INTO user_books (user_id, book_id, added_at, hidden) VALUES (?1,?2,?3,0)
                     ON CONFLICT(user_id, book_id) DO UPDATE SET hidden = 0",
                    rusqlite::params![user_id, id, now],
                )?;
            }
        }
        changed += 1;
    }
    Ok(json!({ "updated": changed, "failed": 0 }))
}

pub fn stats(state: &AppState) -> ApiResult<Value> {
    let books = state.db.count("SELECT COUNT(*) FROM books")?;
    let files = state.db.count("SELECT COUNT(*) FROM book_files WHERE missing = 0")?;
    let missing = state.db.count("SELECT COUNT(*) FROM book_files WHERE missing = 1")?;
    Ok(json!({ "books": books, "files": files, "missing": missing }))
}

pub fn formats() -> Value {
    json!({ "formats": [
        { "id": "epub", "label": "EPUB", "extensions": ["epub"] },
        { "id": "txt", "label": "TXT", "extensions": ["txt", "text", "md"] },
        { "id": "cbz", "label": "漫画压缩包", "extensions": ["cbz", "zip"] },
        { "id": "pdf", "label": "PDF", "extensions": ["pdf"] },
        { "id": "image", "label": "图片", "extensions": ["jpg", "jpeg", "png", "webp", "gif", "bmp", "avif"] }
    ] })
}

/// Persist one uploaded file. Multipart is streamed by the route, so `bytes`
/// here is already the complete part; a 4GB comic is never buffered by axum.
pub fn save_upload(state: &AppState, rel_path: &str, target_dir_rel: &str, bytes: &[u8], on_conflict: &str) -> ApiResult<Value> {
    require_writable(state)?;
    let safe = rel_path.replace('\\', "/");
    // Strip any directory components the client supplied in the filename.
    let name = safe.rsplit('/').next().unwrap_or(&safe);
    if name.is_empty() || name == ".." { return Err(bad_request("invalid filename", "BAD_REQUEST")); }
    let target_dir = resolve_inside(&state.config.books_dir, target_dir_rel)?;
    std::fs::create_dir_all(&target_dir)?;
    let mut target = target_dir.join(name);
    if target.exists() {
        match on_conflict {
            "skip" => return Ok(json!({ "status": "skipped", "path": rel_of(&state.config.books_dir, &target) })),
            "overwrite" => {}
            _ => {
                let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
                let ext = Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("");
                let mut n = 1;
                loop {
                    let candidate = if ext.is_empty() { format!("{stem} ({n})") } else { format!("{stem} ({n}).{ext}") };
                    target = target_dir.join(&candidate);
                    if !target.exists() { break; }
                    n += 1;
                }
            }
        }
    }
    std::fs::write(&target, bytes)?;
    Ok(json!({ "status": "written", "path": rel_of(&state.config.books_dir, &target) }))
}
