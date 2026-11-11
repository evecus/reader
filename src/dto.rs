use serde_json::{json, Map, Value};

use crate::{
    db::{self, Db, Row},
    error::{not_found, ApiResult},
    state::AppState,
};

const OVERRIDABLE: [&str; 10] = [
    "title", "author", "publisher", "language", "isbn", "description", "series",
    "seriesIndex", "pubdate", "tags",
];

/// Read the manual override layer. These rows are never rewritten by a rescan.
pub fn overrides(db: &Db, book_id: &str) -> ApiResult<Map<String, Value>> {
    let rows = db.query(
        "SELECT field, value FROM metadata_overrides WHERE book_id = ?1",
        [book_id],
    )?;
    let mut out = Map::new();
    for row in rows {
        out.insert(db::s(&row, "field"), Value::String(db::s(&row, "value")));
    }
    Ok(out)
}

fn effective(field: &str, row: &Row, ov: &Map<String, Value>) -> Value {
    if let Some(v) = ov.get(field) {
        return v.clone();
    }
    let column = match field {
        "seriesIndex" => "series_index",
        _ => field,
    };
    if field == "seriesIndex" {
        return row
            .get(column)
            .cloned()
            .unwrap_or(Value::Null);
    }
    if field == "tags" {
        let raw = db::s(row, "tags");
        return serde_json::from_str(&raw).unwrap_or_else(|_| json!([]));
    }
    Value::String(db::s(row, column))
}

/// Assemble the BookDto the H5 client consumes. Field names are the contract;
/// changing one is a client-visible break.
pub fn book_dto(db: &Db, row: &Row) -> ApiResult<Value> {
    let id = db::s(row, "id");
    let ov = overrides(db, &id)?;
    let manual_fields: Vec<Value> = OVERRIDABLE
        .iter()
        .filter(|f| ov.contains_key(**f))
        .map(|f| Value::String(f.to_string()))
        .collect();

    let cover = {
        let path = db::s(row, "cover_path");
        if path.is_empty() {
            Value::Null
        } else {
            Value::String(format!("/api/v1/books/{id}/cover"))
        }
    };

    let mut out = Map::new();
    out.insert("id".into(), Value::String(id));
    for field in [
        "title", "author", "publisher", "language", "isbn", "description", "series",
        "seriesIndex", "pubdate", "tags",
    ] {
        out.insert(field.into(), effective(field, row, &ov));
    }
    out.insert("format".into(), Value::String(db::s(row, "format")));
    out.insert("coverUrl".into(), cover);
    out.insert("fileSize".into(), Value::from(db::i(row, "file_size")));
    out.insert(
        "pageCount".into(),
        row.get("page_count").cloned().unwrap_or(Value::Null),
    );
    out.insert("source".into(), Value::String(db::s(row, "source")));
    out.insert("manualFields".into(), Value::Array(manual_fields));
    out.insert("updatedAt".into(), Value::from(db::i(row, "updated_at")));
    let added = row
        .get("added_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| db::i(row, "updated_at"));
    out.insert("addedAt".into(), Value::from(added));
    if let Some(v) = row.get("shelf_hidden") {
        out.insert(
            "shelfState".into(),
            Value::String(if db::i(row, "shelf_hidden") == 0 { "on" } else { "off" }.into()),
        );
        let _ = v;
    }
    if let Some(v) = row.get("reading_state") {
        out.insert("readingState".into(), v.clone());
    }
    Ok(Value::Object(out))
}

const READING_STATE: &str = "CASE
  WHEN ub.reading_status IN ('reading','finished') THEN ub.reading_status
  WHEN rp.percentage >= 0.99 THEN 'finished'
  WHEN rp.percentage > 0 THEN 'reading'
  WHEN ub.reading_status = 'want' THEN 'want'
  ELSE NULL END";

const BOOK_COLUMNS: &str = "b.id, b.identifier, b.content_hash, b.format, b.title, b.author, b.publisher, b.language, b.isbn, b.description, b.series, b.series_index, b.tags, b.pubdate, b.cover_path, b.file_size, b.page_count, b.source, b.updated_at";

/// Unqualified variant for single-row selects (no `b.` prefix / alias).
pub const BOOK_COLUMNS_PUB: &str = "id, identifier, content_hash, format, title, author, publisher, language, isbn, description, series, series_index, tags, pubdate, cover_path, file_size, page_count, source, updated_at";

pub struct ListQuery {
    pub search: Option<String>,
    pub author: Option<String>,
    pub series: Option<String>,
    pub tag: Option<String>,
    pub format: Option<String>,
    pub path: Option<String>,
    pub status: Option<String>,
    pub sort: String,
    pub order: String,
    pub scope: String,
    pub page: i64,
    pub page_size: i64,
    pub file_formats: Option<Vec<String>>,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            search: None,
            author: None,
            series: None,
            tag: None,
            format: None,
            path: None,
            status: None,
            sort: "added".into(),
            order: "desc".into(),
            scope: "shelf".into(),
            page: 1,
            page_size: 50,
            file_formats: None,
        }
    }
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Two scopes: `shelf` (what this account can read) and `library` (what is in
/// a folder, with `shelfState`). Kept as one SQL builder so paging is coherent.
pub fn list_books(db: &Db, user_id: &str, q: &ListQuery) -> ApiResult<Value> {
    let page = q.page.max(1);
    let page_size = q.page_size.clamp(1, 200);
    let library = q.scope == "library";

    let mut wheres: Vec<String> = Vec::new();
    // Params are bound in two groups: join params first, then where params.
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if library {
        params.push(Box::new(user_id.to_string())); // LEFT JOIN user
        wheres.push(
            "EXISTS (SELECT 1 FROM book_files f WHERE f.book_id = b.id AND f.missing = 0)".into(),
        );
    } else {
        params.push(Box::new(user_id.to_string())); // ub.user_id = ?
        wheres.push("ub.user_id = ?".into());
        wheres.push("ub.hidden = 0".into());
        wheres.push(
            "(EXISTS (SELECT 1 FROM book_files f WHERE f.book_id = b.id AND f.missing = 0)
              OR EXISTS (SELECT 1 FROM acquired_files a WHERE a.book_id = b.id)
              OR EXISTS (SELECT 1 FROM chapter_publications c WHERE c.book_id = b.id AND c.user_id = ub.user_id))"
                .into(),
        );
    }

    if let Some(formats) = &q.file_formats {
        let placeholders = (0..formats.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        wheres.push(format!("b.format IN ({placeholders})"));
        for f in formats {
            params.push(Box::new(f.clone()));
        }
    }
    if let Some(search) = &q.search {
        wheres.push("(b.title LIKE ? OR b.author LIKE ? OR b.series LIKE ? OR b.isbn = ?)".into());
        let like = format!("%{search}%");
        for _ in 0..3 {
            params.push(Box::new(like.clone()));
        }
        params.push(Box::new(search.clone()));
    }
    if let Some(author) = &q.author {
        wheres.push("b.author = ?".into());
        params.push(Box::new(author.clone()));
    }
    if let Some(series) = &q.series {
        wheres.push("b.series = ?".into());
        params.push(Box::new(series.clone()));
    }
    if let Some(format) = &q.format {
        wheres.push("b.format = ?".into());
        params.push(Box::new(format.clone()));
    }
    if let Some(tag) = &q.tag {
        wheres.push("b.tags LIKE ?".into());
        params.push(Box::new(format!("%\"{tag}\"%")));
    }
    if let Some(path) = &q.path {
        let prefix = path.trim_end_matches('/');
        wheres.push(
            "EXISTS (SELECT 1 FROM book_files f WHERE f.book_id = b.id AND f.missing = 0 AND f.rel_path LIKE ? ESCAPE '\\')"
                .into(),
        );
        params.push(Box::new(format!("{}/%", escape_like(prefix))));
    }
    if !library {
        if let Some(status) = &q.status {
            wheres.push(format!("({READING_STATE}) = ?"));
            params.push(Box::new(status.clone()));
        }
    }

    let sort_column = match q.sort.as_str() {
        "title" => "b.title",
        "author" => "b.author",
        "updated" => "b.updated_at",
        _ => "ub.added_at",
    };
    let order = if q.order == "asc" { "ASC" } else { "DESC" };
    let join = if library {
        "LEFT JOIN user_books ub ON ub.book_id = b.id AND ub.user_id = ?"
    } else {
        "JOIN user_books ub ON ub.book_id = b.id LEFT JOIN reading_progress rp ON rp.user_id = ub.user_id AND rp.book_id = b.id"
    };
    let shelf_column = if library {
        // A missing `user_books` row and `hidden = 1` are both "off the shelf".
        // Without COALESCE, LEFT JOIN's NULL reads as 0 and an off-shelf book
        // is reported as `on`, which hides the library page's 加入书架 button.
        ", COALESCE(ub.hidden, 1) AS shelf_hidden"
    } else {
        ""
    };
    let where_sql = wheres.join(" AND ");
    let total_sql = format!("SELECT COUNT(*) AS n FROM books b {join} WHERE {where_sql}");
    let total = db
        .query(&total_sql, rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())))?
        .first()
        .map(|r| db::i(r, "n"))
        .unwrap_or(0);

    let mut row_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    // Rebuild params for the paged query (same order as count).
    row_params.push(Box::new(user_id.to_string()));
    if let Some(formats) = &q.file_formats {
        for f in formats {
            row_params.push(Box::new(f.clone()));
        }
    }
    if let Some(search) = &q.search {
        let like = format!("%{search}%");
        for _ in 0..3 {
            row_params.push(Box::new(like.clone()));
        }
        row_params.push(Box::new(search.clone()));
    }
    if let Some(author) = &q.author {
        row_params.push(Box::new(author.clone()));
    }
    if let Some(series) = &q.series {
        row_params.push(Box::new(series.clone()));
    }
    if let Some(format) = &q.format {
        row_params.push(Box::new(format.clone()));
    }
    if let Some(tag) = &q.tag {
        row_params.push(Box::new(format!("%\"{tag}\"%")));
    }
    if let Some(path) = &q.path {
        let prefix = path.trim_end_matches('/');
        row_params.push(Box::new(format!("{}/%", escape_like(prefix))));
    }
    if !library {
        if let Some(status) = &q.status {
            row_params.push(Box::new(status.clone()));
        }
    }
    row_params.push(Box::new(page_size));
    row_params.push(Box::new((page - 1) * page_size));

    let projection = if library {
        format!("{BOOK_COLUMNS}, ub.added_at{shelf_column}")
    } else {
        format!("{BOOK_COLUMNS}, ub.added_at, ({READING_STATE}) AS reading_state")
    };
    let sql = format!(
        "SELECT {projection} FROM books b {join} WHERE {where_sql} ORDER BY {sort_column} {order}, b.title ASC LIMIT ? OFFSET ?"
    );
    let rows = db.query(&sql, rusqlite::params_from_iter(row_params.iter().map(|p| p.as_ref())))?;
    let items: ApiResult<Vec<Value>> = rows.iter().map(|r| book_dto(db, r)).collect();

    Ok(json!({
        "items": items?,
        "total": total,
        "page": page,
        "pageSize": page_size,
    }))
}

pub fn get_book(db: &Db, user_id: &str, book_id: &str) -> ApiResult<Value> {
    let row = db
        .one(
            &format!("SELECT {BOOK_COLUMNS} FROM books WHERE id = ?1"),
            [book_id],
        )?
        .ok_or_else(|| not_found("book not found", "NOT_FOUND"))?;
    let visible = db
        .one(
            "SELECT COUNT(*) AS n FROM user_books WHERE user_id = ?1 AND book_id = ?2 AND hidden = 0",
            rusqlite::params![user_id, book_id],
        )?
        .map(|r| db::i(&r, "n"))
        .unwrap_or(0);
    if visible == 0 {
        return Err(not_found("book not found", "NOT_FOUND"));
    }
    book_dto(db, &row)
}

pub fn facets(db: &Db, user_id: &str) -> ApiResult<Value> {
    let rows = db.query(
        "SELECT b.author, b.series, b.tags, b.format FROM books b JOIN user_books ub ON ub.book_id = b.id WHERE ub.user_id = ?1 AND ub.hidden = 0",
        [user_id],
    )?;
    let mut authors = std::collections::BTreeSet::new();
    let mut series = std::collections::BTreeSet::new();
    let mut tags = std::collections::BTreeSet::new();
    let mut formats = std::collections::BTreeSet::new();
    for r in rows {
        let a = db::s(&r, "author");
        if !a.is_empty() { authors.insert(a); }
        let s = db::s(&r, "series");
        if !s.is_empty() { series.insert(s); }
        let f = db::s(&r, "format");
        if !f.is_empty() { formats.insert(f); }
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&db::s(&r, "tags")) {
            for t in arr {
                if let Some(s) = t.as_str() { tags.insert(s.to_string()); }
            }
        }
    }
    Ok(json!({
        "authors": authors.into_iter().collect::<Vec<_>>(),
        "series": series.into_iter().collect::<Vec<_>>(),
        "tags": tags.into_iter().collect::<Vec<_>>(),
        "formats": formats.into_iter().collect::<Vec<_>>(),
    }))
}

pub fn set_overrides(state: &AppState, book_id: &str, patch: &Value, user_id: &str) -> ApiResult<()> {
    let now = crate::state::now_ms();
    let Some(obj) = patch.as_object() else { return Ok(()); };
    state.db.transaction(|conn| {
        for (field, value) in obj {
            let text = match value {
                Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(","),
                Value::Null => continue,
                _ => match value {
                    Value::String(s) => s.clone(),
                    _ => value.to_string(),
                },
            };
            conn.execute(
                "INSERT INTO metadata_overrides (book_id, field, value, updated_by, updated_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(book_id, field) DO UPDATE SET value = excluded.value,
                   updated_by = excluded.updated_by, updated_at = excluded.updated_at",
                rusqlite::params![book_id, field, text, user_id, now],
            )?;
        }
        conn.execute("UPDATE books SET updated_at = ?1 WHERE id = ?2", rusqlite::params![now, book_id])?;
        Ok(())
    })
}

pub fn clear_override(state: &AppState, book_id: &str, field: &str) -> ApiResult<()> {
    let now = crate::state::now_ms();
    state.db.transaction(|conn| {
        conn.execute(
            "DELETE FROM metadata_overrides WHERE book_id = ?1 AND field = ?2",
            rusqlite::params![book_id, field],
        )?;
        conn.execute("UPDATE books SET updated_at = ?1 WHERE id = ?2", rusqlite::params![now, book_id])?;
        Ok(())
    })
}
