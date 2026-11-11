//! Library file manager. Every path goes through `resolve_inside`, which
//! refuses traversal and never follows symlinks (matching lib/paths.ts).

use std::collections::{HashMap, HashSet};
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

/// Directories the scanner never descends into; `BrowseEntry.hiddenByRule`
/// reports them so a book that failed to appear on the shelf can be explained.
const SKIPPED_DIRS: [&str; 6] = [".git", "@eaDir", "#recycle", ".DS_Store", "lost+found", "__MACOSX"];
/// Never offered for listing: reachable by name, never by discovery.
const VCS_DIRS: [&str; 3] = [".git", ".svn", ".hg"];
/// `server/src/services/browse.ts`: BROWSE_PAGE_SIZE / BROWSE_PAGE_SIZE_MAX.
pub const BROWSE_PAGE_SIZE: i64 = 200;
const BROWSE_PAGE_SIZE_MAX: i64 = 1000;

fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[dot + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// A *page* inside a book rather than a book: a loose `.jpg` is a book on its
/// own, but the same extension inside a comic folder is one of its pages.
fn is_page_extension(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "jpe" | "png" | "gif" | "webp" | "bmp" | "avif" | "tif" | "tiff" | "jxl")
}

fn is_book_file(name: &str) -> bool {
    let ext = extension_of(name);
    !ext.is_empty() && indexer::supported_extension(name) && !is_page_extension(&ext)
}

fn crumbs_for(rel: &str) -> Vec<Value> {
    let mut crumbs = vec![json!({ "name": "书库", "path": "" })];
    if rel.is_empty() { return crumbs; }
    let mut acc = String::new();
    for part in rel.split('/') {
        if acc.is_empty() { acc.push_str(part); } else { acc.push('/'); acc.push_str(part); }
        crumbs.push(json!({ "name": part, "path": acc }));
    }
    crumbs
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::PermissionsExt;
    (meta.permissions().mode() & 0o777) as u64
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u64 { 0 }

/// The library as a *tree*, for the file-manager screen. The response shape is
/// `BrowseListing` from the Node server — `crumbs` and `entries`, not a flat
/// `items` array — because the page renders breadcrumbs, per-row scanner flags
/// and the caller's shelf state from those fields.
pub fn browse(state: &AppState, path: &str, page: i64, page_size: i64, user_id: &str) -> ApiResult<Value> {
    let rel = path.trim_end_matches('/');
    let dir = resolve_inside(&state.config.books_dir, rel)?;
    let info = std::fs::metadata(&dir)
        .map_err(|_| api_err(404, "DIR_NOT_FOUND", "no such directory"))?;
    if !info.is_dir() { return Err(bad_request("not a directory", "NOT_A_DIRECTORY")); }

    let mut rows: Vec<Value> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if VCS_DIRS.contains(&name.as_str()) { continue; }
        let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        let child_abs = resolve_inside(&state.config.books_dir, &child_rel)?;
        // symlink_metadata: a link is shown as `other`, never followed. Walking
        // a link out of the mount would describe a tree the index does not have.
        let meta = match std::fs::symlink_metadata(&child_abs) { Ok(m) => m, Err(_) => continue };
        let ft = meta.file_type();
        let is_dir = ft.is_dir();
        let is_file = ft.is_file();
        let kind = if is_dir { "dir" } else if is_file { "file" } else { "other" };
        let size = if is_dir { 0u64 } else { meta.len() };
        let mtime = meta.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64).unwrap_or(0);
        let hidden = name.starts_with('.');
        let hidden_by_rule = if is_dir {
            SKIPPED_DIRS.contains(&name.as_str()) || hidden
        } else {
            SKIPPED_DIRS.contains(&name.as_str())
        };
        let ext = extension_of(&name);
        let scanned = is_file && is_book_file(&name);
        let indexed = state.db
            .one("SELECT 1 FROM book_files WHERE rel_path = ?1", [&child_rel])?
            .is_some();
        rows.push(json!({
            "name": name,
            "path": child_rel,
            "type": kind,
            "size": size,
            "mtime": mtime,
            "mode": mode_of(&meta),
            "hidden": hidden,
            "hiddenByRule": hidden_by_rule,
            "scanned": scanned,
            "ext": ext,
            "indexed": indexed,
            "shelfState": Value::Null,
        }));
    }
    rows.sort_by(|a, b| {
        a.get("name").and_then(Value::as_str).unwrap_or("")
            .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
    });

    // Resolve shelf state for the whole page in two queries, not one per row.
    // A missing `user_books` row and `hidden = 1` are both "off the shelf".
    let indexed_paths: Vec<String> = rows.iter()
        .filter(|r| r.get("indexed").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|r| r.get("path").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut book_by_path: HashMap<String, String> = HashMap::new();
    if !indexed_paths.is_empty() {
        let placeholders = (0..indexed_paths.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT rel_path, book_id FROM book_files WHERE rel_path IN ({placeholders})");
        let params: Vec<Box<dyn rusqlite::ToSql>> = indexed_paths.iter()
            .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::ToSql>).collect();
        for r in state.db.query(&sql, rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())))? {
            book_by_path.insert(db::s(&r, "rel_path"), db::s(&r, "book_id"));
        }
    }
    let book_ids: Vec<String> = book_by_path.values().cloned().collect::<HashSet<_>>().into_iter().collect();
    let mut on_shelf: HashSet<String> = HashSet::new();
    if !book_ids.is_empty() {
        let placeholders = (0..book_ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT book_id FROM user_books WHERE user_id = ? AND hidden = 0 AND book_id IN ({placeholders})"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(user_id.to_string())];
        params.extend(book_ids.iter().map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>));
        for r in state.db.query(&sql, rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())))? {
            on_shelf.insert(db::s(&r, "book_id"));
        }
    }
    for row in rows.iter_mut() {
        let is_dir = row.get("type").and_then(Value::as_str) == Some("dir");
        let shelf = if is_dir { Value::Null } else {
            match row.get("path").and_then(Value::as_str).and_then(|p| book_by_path.get(p)) {
                Some(id) => json!(if on_shelf.contains(id) { "on" } else { "off" }),
                None => Value::Null,
            }
        };
        if let Some(obj) = row.as_object_mut() { obj.insert("shelfState".into(), shelf); }
    }

    // Counts describe the whole directory; `entries` is one page of it.
    let total = rows.len();
    let dirs = rows.iter().filter(|r| r.get("type").and_then(Value::as_str) == Some("dir")).count();
    let files = rows.iter().filter(|r| r.get("type").and_then(Value::as_str) == Some("file")).count();
    let total_size: u64 = rows.iter().filter_map(|r| r.get("size").and_then(Value::as_u64)).sum();
    let per_page = page_size.clamp(1, BROWSE_PAGE_SIZE_MAX) as usize;
    let offset = (page.max(1) - 1) as usize * per_page;
    let entries: Vec<Value> = rows.iter().skip(offset).take(per_page).cloned().collect();
    let parent = if rel.is_empty() { Value::Null } else {
        match rel.rsplit_once('/') {
            Some((p, _)) => json!(p),
            None => json!(""),
        }
    };
    Ok(json!({
        "path": rel,
        "crumbs": crumbs_for(rel),
        "parent": parent,
        "entries": entries,
        "total": total,
        "dirs": dirs,
        "files": files,
        "size": total_size,
        "writable": writable_at(&dir),
        "name": if rel.is_empty() { "" } else { rel.rsplit('/').next().unwrap_or("") },
    }))
}

/// Probe by actually creating a directory: mode bits lie on a read-only bind
/// mount, and a cached answer would go stale the moment the mount is remounted.
fn writable_at(dir: &Path) -> bool {
    let probe = dir.join(format!(".reader-write-probe-{}", uuid::Uuid::new_v4()));
    match std::fs::create_dir(&probe) {
        Ok(()) => { let _ = std::fs::remove_dir(&probe); true }
        Err(_) => false,
    }
}

pub fn is_writable(books_dir: &Path) -> bool { writable_at(books_dir) }

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
