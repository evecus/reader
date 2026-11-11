use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db::{self, Db},
    dto,
    error::{bad_request, ApiResult},
    state::AppState,
};

fn clamp(v: f64) -> f64 {
    if !v.is_finite() { return 0.0; }
    v.clamp(0.0, 1.0)
}

fn progress_dto(row: &db::Row) -> Value {
    json!({
        "bookId": db::s(row, "book_id"),
        "locator": db::s(row, "locator"),
        "percentage": db::f(row, "percentage"),
        "chapterTitle": db::s(row, "chapter_title"),
        "device": db::s(row, "device"),
        "updatedAt": db::i(row, "updated_at"),
    })
}

fn note_dto(row: &db::Row) -> Value {
    json!({
        "id": db::s(row, "id"),
        "bookId": db::s(row, "book_id"),
        "type": db::s(row, "type"),
        "locator": db::s(row, "locator"),
        "text": db::s(row, "text"),
        "comment": db::s(row, "comment"),
        "color": db::s(row, "color"),
        "updatedAt": db::i(row, "updated_at"),
        "deleted": db::i(row, "deleted") == 1,
    })
}

pub fn progress_for(db: &Db, user_id: &str, book_id: &str) -> ApiResult<Option<Value>> {
    Ok(db.one(
        "SELECT book_id, locator, percentage, chapter_title, device, updated_at FROM reading_progress WHERE user_id = ?1 AND book_id = ?2",
        rusqlite::params![user_id, book_id],
    )?.map(|r| progress_dto(&r)))
}

pub fn push(state: &AppState, user_id: &str, progress: &[Value], notes: &[Value]) -> ApiResult<(i64, i64)> {
    let mut accepted = 0i64;
    let mut rejected = 0i64;
    let now = crate::state::now_ms();
    state.db.transaction(|conn| {
        for item in progress {
            let book_id = item.get("bookId").and_then(Value::as_str).unwrap_or("");
            if book_id.is_empty() || item.get("updatedAt").and_then(Value::as_i64).is_none() {
                rejected += 1;
                continue;
            }
            let updated_at = item.get("updatedAt").and_then(Value::as_i64).unwrap_or(now);
            conn.execute(
                "INSERT INTO reading_progress (user_id, book_id, locator, percentage, chapter_title, device, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(user_id, book_id) DO UPDATE SET
                   locator = CASE WHEN excluded.updated_at >= reading_progress.updated_at THEN excluded.locator ELSE reading_progress.locator END,
                   percentage = CASE WHEN excluded.updated_at >= reading_progress.updated_at THEN excluded.percentage ELSE reading_progress.percentage END,
                   chapter_title = CASE WHEN excluded.updated_at >= reading_progress.updated_at THEN excluded.chapter_title ELSE reading_progress.chapter_title END,
                   device = CASE WHEN excluded.updated_at >= reading_progress.updated_at THEN excluded.device ELSE reading_progress.device END,
                   updated_at = MAX(excluded.updated_at, reading_progress.updated_at)",
                rusqlite::params![user_id, book_id,
                    item.get("locator").and_then(Value::as_str).unwrap_or(""),
                    clamp(item.get("percentage").and_then(Value::as_f64).unwrap_or(0.0)),
                    item.get("chapterTitle").and_then(Value::as_str).unwrap_or(""),
                    item.get("device").and_then(Value::as_str).unwrap_or(""),
                    updated_at],
            )?;
            accepted += 1;
        }
        for item in notes {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let book_id = item.get("bookId").and_then(Value::as_str).unwrap_or("");
            if id.is_empty() || book_id.is_empty() { rejected += 1; continue; }
            let owner = crate::db::one(conn,
                "SELECT user_id, book_id FROM notes WHERE id = ?1", [id])?;
            if let Some(o) = &owner {
                if db::s(o, "user_id") != user_id || db::s(o, "book_id") != book_id {
                    rejected += 1;
                    continue;
                }
            }
            let note_type = item.get("type").and_then(Value::as_str).unwrap_or("note");
            if !["note", "highlight", "bookmark"].contains(&note_type) {
                return Err(bad_request(format!("unsupported note type: {note_type}"), "BAD_NOTE_TYPE"));
            }
            let deleted = item.get("deleted").and_then(Value::as_bool).unwrap_or(false);
            conn.execute(
                "INSERT INTO notes (id, user_id, book_id, type, locator, text, comment, color, updated_at, deleted)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(id) DO UPDATE SET
                   type = CASE WHEN excluded.updated_at >= notes.updated_at THEN excluded.type ELSE notes.type END,
                   locator = CASE WHEN excluded.updated_at >= notes.updated_at THEN excluded.locator ELSE notes.locator END,
                   text = CASE WHEN excluded.updated_at >= notes.updated_at THEN excluded.text ELSE notes.text END,
                   comment = CASE WHEN excluded.updated_at >= notes.updated_at THEN excluded.comment ELSE notes.comment END,
                   color = CASE WHEN excluded.updated_at >= notes.updated_at THEN excluded.color ELSE notes.color END,
                   deleted = CASE WHEN excluded.updated_at >= notes.updated_at THEN excluded.deleted ELSE notes.deleted END,
                   updated_at = MAX(excluded.updated_at, notes.updated_at)
                 WHERE notes.user_id = excluded.user_id AND notes.book_id = excluded.book_id",
                rusqlite::params![id, user_id, book_id, note_type,
                    item.get("locator").and_then(Value::as_str).unwrap_or(""),
                    item.get("text").and_then(Value::as_str).unwrap_or(""),
                    item.get("comment").and_then(Value::as_str).unwrap_or(""),
                    item.get("color").and_then(Value::as_str).unwrap_or(""),
                    item.get("updatedAt").and_then(Value::as_i64).unwrap_or(now),
                    if deleted { 1 } else { 0 }],
            )?;
            accepted += 1;
        }
        Ok(())
    })?;
    Ok((accepted, rejected))
}

pub fn pull(db: &Db, user_id: &str, since: i64, book_id: Option<&str>) -> ApiResult<Value> {
    let (p_sql, n_sql): (String, String) = if book_id.is_some() {
        (
            "SELECT book_id, locator, percentage, chapter_title, device, updated_at FROM reading_progress WHERE user_id = ?1 AND updated_at > ?2 AND book_id = ?3 ORDER BY updated_at ASC LIMIT 5000".into(),
            "SELECT id, book_id, type, locator, text, comment, color, updated_at, deleted FROM notes WHERE user_id = ?1 AND updated_at > ?2 AND book_id = ?3 ORDER BY updated_at ASC LIMIT 5000".into(),
        )
    } else {
        (
            "SELECT book_id, locator, percentage, chapter_title, device, updated_at FROM reading_progress WHERE user_id = ?1 AND updated_at > ?2 ORDER BY updated_at ASC LIMIT 5000".into(),
            "SELECT id, book_id, type, locator, text, comment, color, updated_at, deleted FROM notes WHERE user_id = ?1 AND updated_at > ?2 ORDER BY updated_at ASC LIMIT 5000".into(),
        )
    };
    let progress = match book_id {
        Some(b) => db.query(&p_sql, rusqlite::params![user_id, since, b])?,
        None => db.query(&p_sql, rusqlite::params![user_id, since])?,
    };
    let notes = match book_id {
        Some(b) => db.query(&n_sql, rusqlite::params![user_id, since, b])?,
        None => db.query(&n_sql, rusqlite::params![user_id, since])?,
    };
    Ok(json!({
        "serverTime": crate::state::now_ms(),
        "progress": progress.iter().map(progress_dto).collect::<Vec<_>>(),
        "notes": notes.iter().map(note_dto).collect::<Vec<_>>(),
    }))
}

pub fn list_notes(db: &Db, user_id: &str, book_id: Option<&str>, since: i64) -> ApiResult<Value> {
    let pulled = pull(db, user_id, since, book_id)?;
    let notes: Vec<Value> = pulled.get("notes").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().filter(|n| !n.get("deleted").and_then(Value::as_bool).unwrap_or(false)).collect();
    Ok(json!({ "notes": notes, "serverTime": pulled.get("serverTime").cloned().unwrap_or(json!(0)) }))
}

pub fn create_note(state: &AppState, user_id: &str, body: &Value) -> ApiResult<Value> {
    let book_id = body.get("bookId").and_then(Value::as_str).unwrap_or("");
    if book_id.is_empty() { return Err(bad_request("bookId is required", "BAD_REQUEST")); }
    let now = crate::state::now_ms();
    let note = json!({
        "id": body.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| Uuid::new_v4().to_string()),
        "bookId": book_id,
        "type": body.get("type").and_then(Value::as_str).unwrap_or("note"),
        "locator": body.get("locator").and_then(Value::as_str).unwrap_or(""),
        "text": body.get("text").and_then(Value::as_str).unwrap_or(""),
        "comment": body.get("comment").and_then(Value::as_str).unwrap_or(""),
        "color": body.get("color").and_then(Value::as_str).unwrap_or(""),
        "updatedAt": body.get("updatedAt").and_then(Value::as_i64).unwrap_or(now),
        "deleted": false,
    });
    push(state, user_id, &[], std::slice::from_ref(&note))?;
    Ok(note)
}

pub fn delete_note(state: &AppState, user_id: &str, id: &str) -> ApiResult<Value> {
    let existing = state.db.one(
        "SELECT book_id, type, locator, text, comment, color FROM notes WHERE id = ?1 AND user_id = ?2",
        rusqlite::params![id, user_id],
    )?;
    let Some(row) = existing else { return Ok(json!({ "ok": true })); };
    let note = json!({
        "id": id,
        "bookId": db::s(&row, "book_id"),
        "type": db::s(&row, "type"),
        "locator": db::s(&row, "locator"),
        "text": db::s(&row, "text"),
        "comment": db::s(&row, "comment"),
        "color": db::s(&row, "color"),
        "updatedAt": crate::state::now_ms(),
        "deleted": true,
    });
    push(state, user_id, &[], &[note])?;
    Ok(json!({ "ok": true }))
}

pub fn recently_read(state: &AppState, user_id: &str, limit: i64) -> ApiResult<Value> {
    let rows = state.db.query(
        "SELECT p.book_id, p.percentage, p.chapter_title, p.updated_at
         FROM reading_progress p
         JOIN user_books ub ON ub.book_id = p.book_id AND ub.user_id = p.user_id
         WHERE p.user_id = ?1 AND ub.hidden = 0
           AND (EXISTS (SELECT 1 FROM book_files f WHERE f.book_id = p.book_id AND f.missing = 0)
                OR EXISTS (SELECT 1 FROM acquired_files a WHERE a.book_id = p.book_id)
                OR EXISTS (SELECT 1 FROM chapter_publications c WHERE c.book_id = p.book_id AND c.user_id = p.user_id))
         ORDER BY p.updated_at DESC LIMIT ?2",
        rusqlite::params![user_id, limit.clamp(1, 100)],
    )?;
    let mut items = Vec::new();
    for r in rows {
        let id = db::s(&r, "book_id");
        let book = state.db.one(
            &format!("SELECT {} FROM books WHERE id = ?1", dto::BOOK_COLUMNS_PUB),
            [&id],
        )?;
        if let Some(book_row) = book {
            let mut dto = dto::book_dto(&state.db, &book_row)?;
            if let Some(obj) = dto.as_object_mut() {
                obj.insert("percentage".into(), json!(db::f(&r, "percentage")));
                obj.insert("chapterTitle".into(), json!(db::s(&r, "chapter_title")));
                obj.insert("lastReadAt".into(), json!(db::i(&r, "updated_at")));
            }
            items.push(dto);
        }
    }
    Ok(json!({ "items": items }))
}
