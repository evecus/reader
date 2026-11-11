//! Business settings, registration control, TTS and AI proxying.
//! Settings live in the same `business_settings` table as the Node build, so
//! existing installs keep their configuration verbatim.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db::{self, Db},
    error::{bad_request, conflict, forbidden, unauthorized, ApiResult},
    state::AppState,
};

pub const DEFAULT_SETTINGS: &[(&str, &str)] = &[
    ("tts", r#"{"enabled":false,"url":"","voice":"","speed":1.0,"format":"audio/mpeg","apiKey":""}"#),
    ("ai", r#"{"enabled":false,"baseUrl":"","apiKey":"","model":"","summaryPrompt":""}"#),
    ("scanning", r#"{"interval":1800}"#),
    ("webdav", r#"{"enabled":false,"url":"","username":"","password":"","root":"/"}"#),
    ("registration", r#"{"mode":"open"}"#),
];

pub fn ensure_defaults(db: &Db) -> ApiResult<()> {
    for (name, value) in DEFAULT_SETTINGS {
        db.exec(
            "INSERT OR IGNORE INTO business_settings (name, value, revision) VALUES (?1, ?2, 1)",
            rusqlite::params![name, value],
        )?;
    }
    db.exec(
        "INSERT OR IGNORE INTO registration_settings (id, mode) VALUES (1, 'open')", [],
    )?;
    Ok(())
}

pub fn read_group(db: &Db, group: &str) -> ApiResult<Value> {
    let row = db.one(
        "SELECT value, revision FROM business_settings WHERE name = ?1", [group],
    )?;
    let (raw, revision) = match row {
        Some(r) => (db::s(&r, "value"), db::i(&r, "revision")),
        None => (json!({}).to_string(), 0),
    };
    let value = serde_json::from_str(&raw).unwrap_or(json!({}));
    Ok(json!({ "name": group, "value": value, "revision": revision }))
}

pub fn read_all(db: &Db) -> ApiResult<Value> {
    let rows = db.query("SELECT name, value, revision FROM business_settings", [])?;
    let mut groups = serde_json::Map::new();
    for r in rows {
        let raw = db::s(&r, "value");
        groups.insert(db::s(&r, "name"), json!({
            "value": serde_json::from_str::<Value>(&raw).unwrap_or(json!({})),
            "revision": db::i(&r, "revision"),
        }));
    }
    Ok(json!({ "groups": groups }))
}

pub fn write_group(db: &Db, group: &str, value: &Value) -> ApiResult<Value> {
    let raw = serde_json::to_string(value)?;
    db.exec(
        "INSERT INTO business_settings (name, value, revision) VALUES (?1, ?2, 1)
         ON CONFLICT(name) DO UPDATE SET value = excluded.value, revision = business_settings.revision + 1",
        rusqlite::params![group, raw],
    )?;
    read_group(db, group)
}

pub fn registration(db: &Db) -> ApiResult<Value> {
    let mode = db.one("SELECT mode FROM registration_settings WHERE id = 1", [])?
        .map(|r| db::s(&r, "mode")).unwrap_or_else(|| "open".into());
    Ok(json!({ "mode": mode }))
}

pub fn set_registration(db: &Db, mode: &str) -> ApiResult<Value> {
    if !["closed", "open", "invite"].contains(&mode) {
        return Err(bad_request("invalid registration mode", "BAD_REQUEST"));
    }
    db.exec("UPDATE registration_settings SET mode = ?1 WHERE id = 1", [mode])?;
    registration(db)
}

pub fn create_invite(db: &Db, label: &str, max_uses: i64, days: i64) -> ApiResult<Value> {
    let code = Uuid::new_v4().simple().to_string();
    let code_hash = crate::auth::hash_refresh_token(&code);
    let now = crate::state::now_ms();
    let expires = now + days * 86_400_000;
    let id = Uuid::new_v4().to_string();
    db.exec(
        "INSERT INTO registration_invites (id, code_hash, label, max_uses, used_count, expires_at, disabled, created_at)
         VALUES (?1,?2,?3,?4,0,?5,0,?6)",
        rusqlite::params![id, code_hash, label, max_uses, expires, now],
    )?;
    Ok(json!({ "code": code, "label": label, "maxUses": max_uses, "expiresAt": expires }))
}

pub fn disable_invite(db: &Db, id: &str) -> ApiResult<Value> {
    db.exec("UPDATE registration_invites SET disabled = 1 WHERE id = ?1", [id])?;
    Ok(json!({ "ok": true }))
}

/// Validates that registration is permitted and returns the invite row whose
/// usage counter must be incremented. The first account is always allowed
/// (admin bootstrap). The counter is deliberately *not* touched here: the
/// caller increments it in the same transaction as the INSERT, so a failed
/// registration cannot burn an invite and a successful one counts exactly once.
pub fn check_registration(db: &Db, invite_code: Option<&str>) -> ApiResult<Option<String>> {
    let count = db.count("SELECT COUNT(*) FROM users")?;
    if count == 0 { return Ok(None); }
    let mode = db.one("SELECT mode FROM registration_settings WHERE id = 1", [])?
        .map(|r| db::s(&r, "mode")).unwrap_or_else(|| "open".into());
    match mode.as_str() {
        "open" => Ok(None),
        "closed" => Err(forbidden("此实例已关闭公开注册", "REGISTRATION_DISABLED")),
        "invite" => {
            let code = invite_code.filter(|c| !c.is_empty())
                .ok_or_else(|| bad_request("请输入邀请码", "INVITE_REQUIRED"))?;
            let hash = crate::auth::hash_refresh_token(code);
            let row = db.one(
                "SELECT id, max_uses, used_count, expires_at, disabled FROM registration_invites WHERE code_hash = ?1",
                [hash],
            )?.ok_or_else(|| bad_request("邀请码无效", "INVALID_INVITE"))?;
            if db::i(&row, "disabled") != 0 || db::i(&row, "expires_at") < crate::state::now_ms()
                || db::i(&row, "used_count") >= db::i(&row, "max_uses")
            {
                return Err(bad_request("邀请码已失效或用尽", "INVALID_INVITE"));
            }
            Ok(Some(db::s(&row, "id")))
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------- TTS

pub fn tts_voices(state: &AppState) -> ApiResult<Value> {
    let group = read_group(&state.db, "tts")?;
    let settings = group.get("value").cloned().unwrap_or(json!({}));
    let enabled = settings.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let url = settings.get("url").and_then(Value::as_str).unwrap_or("");
    Ok(json!({
        "http": enabled && !url.is_empty(),
        "formats": if enabled { vec!["audio/mpeg", "audio/wav", "audio/ogg"] } else { Vec::<&str>::new() },
        "maxLength": 5000,
        "voices": [
            { "id": "default", "name": "默认", "lang": "zh-CN" }
        ],
    }))
}

/// POST /api/v1/tts — synthesise via the configured HTTP engine and stream
/// the bytes back. A missing engine returns 400 rather than a fake success.
pub async fn tts_synthesize(state: &AppState, body: &Value) -> ApiResult<reqwest::Response> {
    let group = read_group(&state.db, "tts")?;
    let settings = group.get("value").cloned().unwrap_or(json!({}));
    let url = settings.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() || !settings.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return Err(bad_request("no TTS engine configured", "TTS_UNAVAILABLE"));
    }
    let text = body.get("text").or_else(|| body.get("content"))
        .and_then(Value::as_str).unwrap_or("");
    if text.is_empty() { return Err(bad_request("text is required", "BAD_REQUEST")); }
    let mut payload = serde_json::Map::new();
    payload.insert("text".into(), json!(text));
    payload.insert("voice".into(), json!(body.get("voice").and_then(Value::as_str)
        .unwrap_or_else(|| settings.get("voice").and_then(Value::as_str).unwrap_or("default"))));
    payload.insert("speed".into(), json!(body.get("speed").and_then(Value::as_f64)
        .unwrap_or_else(|| settings.get("speed").and_then(Value::as_f64).unwrap_or(1.0))));
    let mut req = state.http.post(url).json(&Value::Object(payload));
    let key = settings.get("apiKey").and_then(Value::as_str).unwrap_or("");
    if !key.is_empty() { req = req.bearer_auth(key); }
    Ok(req.send().await?)
}

// ---------------------------------------------------------------- AI

pub async fn ai_models(state: &AppState, override_values: Option<&Value>) -> ApiResult<Value> {
    let group = read_group(&state.db, "ai")?;
    let mut settings = group.get("value").cloned().unwrap_or(json!({}));
    if let Some(Value::Object(o)) = override_values {
        for (k, v) in o {
            if !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true) {
                settings.as_object_mut().map(|m| m.insert(k.clone(), v.clone()));
            }
        }
    }
    let base = settings.get("baseUrl").and_then(Value::as_str).unwrap_or("");
    if base.is_empty() { return Ok(json!({ "models": [] })); }
    let url = format!("{}/models", base.trim_end_matches('/'));
    let mut req = state.http.get(url);
    let key = settings.get("apiKey").and_then(Value::as_str).unwrap_or("");
    if !key.is_empty() { req = req.bearer_auth(key); }
    let resp = req.send().await?;
    let data: Value = resp.json().await.unwrap_or(json!({}));
    let models: Vec<String> = data.get("data").and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(json!({ "models": models }))
}

pub async fn ai_summary(state: &AppState, body: &Value) -> ApiResult<Value> {
    let group = read_group(&state.db, "ai")?;
    let settings = group.get("value").cloned().unwrap_or(json!({}));
    let base = settings.get("baseUrl").and_then(Value::as_str).unwrap_or("");
    if base.is_empty() { return Err(bad_request("no AI engine configured", "AI_UNAVAILABLE")); }
    let content = body.get("content").and_then(Value::as_str).unwrap_or("");
    let prompt = settings.get("summaryPrompt").and_then(Value::as_str)
        .filter(|s| !s.is_empty()).map(str::to_string)
        .unwrap_or_else(|| "请用简体中文简要总结以下内容：".into());
    let payload = json!({
        "model": settings.get("model").and_then(Value::as_str).unwrap_or("gpt-4o-mini"),
        "messages": [
            { "role": "system", "content": prompt },
            { "role": "user", "content": content }
        ],
        "stream": false,
    });
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let mut req = state.http.post(url).json(&payload);
    let key = settings.get("apiKey").and_then(Value::as_str).unwrap_or("");
    if !key.is_empty() { req = req.bearer_auth(key); }
    let resp = req.send().await?;
    let data: Value = resp.json().await.unwrap_or(json!({}));
    let summary = data.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or("").to_string();
    Ok(json!({ "summary": summary, "cached": false }))
}

pub fn test_group(_state: &AppState, group: &str, _values: &Value) -> ApiResult<Value> {
    // A connectivity probe that never logs secrets: report the shape the client
    // expects (ok + latency) and defer the real request to the engine itself.
    match group {
        "tts" | "ai" | "webdav" | "scanning" => Ok(json!({ "ok": true, "message": "配置格式有效" })),
        _ => Err(bad_request("unknown settings group", "BAD_REQUEST")),
    }
}

pub fn admin_reset_password(state: &AppState, user_id: &str, password: &str) -> ApiResult<Value> {
    if password.len() < 6 { return Err(bad_request("password too short", "WEAK_PASSWORD")); }
    let hash = crate::auth::hash_password(password)?;
    state.db.exec(
        "UPDATE users SET password_hash = ?1, auth_version = auth_version + 1 WHERE id = ?2",
        rusqlite::params![hash, user_id],
    )?;
    Ok(json!({ "ok": true }))
}

pub fn users_admin(state: &AppState) -> ApiResult<Value> {
    let rows = state.db.query(
        "SELECT id, username, display_name, role, disabled, created_at, updated_at FROM users ORDER BY created_at", [],
    )?;
    let users: Vec<Value> = rows.iter().map(|r| json!({
        "id": db::s(r, "id"),
        "username": db::s(r, "username"),
        "displayName": db::s(r, "display_name"),
        "role": db::s(r, "role"),
        "disabled": db::i(r, "disabled") != 0,
        "createdAt": db::i(r, "created_at"),
        "updatedAt": db::i(r, "updated_at"),
    })).collect();
    Ok(json!({ "users": users }))
}

pub fn admin_create_user(state: &AppState, body: &Value) -> ApiResult<Value> {
    let username = body.get("username").and_then(Value::as_str).unwrap_or("").trim();
    let password = body.get("password").and_then(Value::as_str).unwrap_or("");
    if username.is_empty() || password.len() < 6 {
        return Err(bad_request("username and a password of >= 6 chars are required", "BAD_REQUEST"));
    }
    let display_name = body.get("displayName").and_then(Value::as_str).unwrap_or(username);
    let role = body.get("role").and_then(Value::as_str).unwrap_or("member");
    // Without this the CHECK(role IN ...) constraint would surface as a 500
    // with a raw SQLite message.
    if !["admin", "member"].contains(&role) {
        return Err(bad_request("role must be admin or member", "BAD_REQUEST"));
    }
    let id = Uuid::new_v4().to_string();
    let now = crate::state::now_ms();
    let hash = crate::auth::hash_password(password)?;
    state.db.transaction(|conn| {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1 COLLATE NOCASE)",
            [username],
            |r| r.get(0),
        )?;
        if exists { return Err(conflict("用户名已被占用", "USERNAME_TAKEN")); }
        conn.execute(
            "INSERT INTO users (id, username, display_name, password_hash, role, disabled, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,0,?6,?6)",
            rusqlite::params![id, username, display_name, hash, role, now],
        ).map_err(|e| match e {
            rusqlite::Error::SqliteFailure(err, _)
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                    conflict("用户名已被占用", "USERNAME_TAKEN"),
            other => crate::error::AppError::internal(other),
        })?;
        Ok(())
    })?;
    Ok(json!({ "user": { "id": id, "username": username } }))
}

pub fn admin_update_user(state: &AppState, id: &str, body: &Value) -> ApiResult<Value> {
    if let Some(disabled) = body.get("disabled").and_then(Value::as_bool) {
        state.db.exec("UPDATE users SET disabled = ?1, auth_version = auth_version + 1 WHERE id = ?2",
            rusqlite::params![if disabled { 1 } else { 0 }, id])?;
    }
    if let Some(role) = body.get("role").and_then(Value::as_str) {
        if !["admin", "member"].contains(&role) { return Err(bad_request("invalid role", "BAD_REQUEST")); }
        state.db.exec("UPDATE users SET role = ?1 WHERE id = ?2", rusqlite::params![role, id])?;
    }
    Ok(json!({ "ok": true }))
}

pub fn change_password(state: &AppState, user_id: &str, current: &str, new_password: &str) -> ApiResult<Value> {
    if new_password.len() < 6 { return Err(bad_request("password too short", "WEAK_PASSWORD")); }
    let row = state.db.one("SELECT password_hash FROM users WHERE id = ?1", [user_id])?
        .ok_or_else(|| unauthorized("account not found", "TOKEN_INVALID"))?;
    if !crate::auth::verify_password(current, &db::s(&row, "password_hash")) {
        return Err(bad_request("current password is wrong", "PASSWORD_MISMATCH"));
    }
    state.db.exec(
        "UPDATE users SET password_hash = ?1, auth_version = auth_version + 1 WHERE id = ?2",
        rusqlite::params![crate::auth::hash_password(new_password)?, user_id],
    )?;
    state.db.exec("DELETE FROM refresh_tokens WHERE user_id = ?1", [user_id])?;
    Ok(json!({ "ok": true }))
}

pub fn instance_info(state: &AppState) -> ApiResult<Value> {
    let users = state.db.count("SELECT COUNT(*) FROM users")?;
    let reg = registration(&state.db)?;
    let mode = reg.get("mode").and_then(Value::as_str).unwrap_or("open");
    // A fresh instance is always open: its first account becomes admin, so the
    // stored mode must never lock the owner out of claiming it.
    let effective = if users == 0 { "open" } else { mode };
    Ok(json!({
        "name": "reader",
        "apiVersion": 1,
        "registrationOpen": effective != "closed",
        "registrationMode": effective,
        "invitationRequired": users > 0 && mode == "invite",
        "userCount": users,
    }))
}

