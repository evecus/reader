//! Route assembly. Every reading endpoint of the Node server is registered
//! here; `/api/v1/media/*` is answered by [`media_fallback`] as an empty
//! compatibility stub. The client ships no media frontend at all.

use std::collections::HashMap;
use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as AxumPath, Query, RawQuery, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post, put},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    auth::{self, AuthUser},
    browse, dto,
    error::{api_err, bad_request, forbidden, not_found, ApiResult},
    indexer, opds, settings, sources,
    state::{now_ms, ok, AppState, Auth},
    sync, web,
};

const PLUGIN_UPLOAD_LIMIT: usize = 100 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let api_state = state.clone();
    let api = Router::new()
        // ---- auth ----
        .route("/api/v1/auth/register", post(auth_register))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/refresh", post(auth_refresh))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/password", post(auth_password))
        // ---- instance / misc ----
        .route("/api/v1/instance", get(instance))
        .route("/api/v1/health", get(health))
        .route("/api/v1/providers", get(providers))
        // ---- admin ----
        .route("/api/v1/admin/registration", get(admin_registration_get).patch(admin_registration_patch))
        .route("/api/v1/admin/invites", post(admin_invite_create))
        .route("/api/v1/admin/invites/{id}", delete(admin_invite_delete))
        .route("/api/v1/admin/users", get(admin_users_list).post(admin_users_create))
        .route("/api/v1/admin/users/{id}", patch(admin_users_patch))
        .route("/api/v1/admin/users/{id}/password", post(admin_user_password))
        // ---- settings / tts / ai ----
        .route("/api/v1/admin/settings", get(admin_settings_all))
        .route("/api/v1/admin/settings/{group}", patch(admin_settings_patch))
        .route("/api/v1/admin/settings/{group}/test", post(admin_settings_test))
        .route("/api/v1/admin/settings/tts/preview", post(admin_tts_preview))
        .route("/api/v1/admin/settings/tts/clear-cache", post(admin_tts_clear_cache))
        .route("/api/v1/admin/settings/webdav/upload", post(admin_webdav_upload))
        .route("/api/v1/tts/voices", get(tts_voices))
        .route("/api/v1/tts/test", post(tts_test))
        .route("/api/v1/tts", get(tts_get))
        .route("/api/v1/admin/ai/models", post(ai_models))
        .route("/api/v1/ai/summary", post(ai_summary))
        // ---- library ----
        .route("/api/v1/library/facets", get(library_facets))
        .route("/api/v1/library/browse", get(library_browse))
        .route("/api/v1/library/browse/move", post(library_browse_move))
        .route("/api/v1/library/browse/rename", post(library_browse_rename))
        .route("/api/v1/library/browse/mkdir", post(library_browse_mkdir))
        .route("/api/v1/library/browse/delete", post(library_browse_delete))
        .route("/api/v1/library/browse/metadata", post(library_browse_metadata))
        .route("/api/v1/library/browse/shelf", post(library_browse_shelf))
        .route("/api/v1/library/upload", post(library_upload).get(library_upload_targets))
        .route("/api/v1/library/scan", post(library_scan_start).get(library_scan_status))
        .route("/api/v1/library/formats", get(library_formats))
        .route("/api/v1/library/stats", get(library_stats))
        .route("/api/v1/library/continue", get(library_continue))
        .route("/api/v1/books", get(books_list))
        .route("/api/v1/books/{id}", get(book_get))
        .route("/api/v1/books/{id}/manifest", get(book_manifest))
        .route("/api/v1/books/{id}/refresh", post(book_refresh))
        .route("/api/v1/books/{id}/content", get(book_content))
        .route("/api/v1/books/{id}/items", get(book_items))
        .route("/api/v1/books/{id}/toc", get(book_toc))
        .route("/api/v1/books/{id}/search", get(book_search))
        .route("/api/v1/books/{id}/assets", get(book_assets))
        .route("/api/v1/books/{id}/file", get(book_file))
        .route("/api/v1/books/{id}/cover", get(book_cover))
        .route("/api/v1/books/{id}/metadata", patch(book_metadata_patch))
        .route("/api/v1/books/{id}/metadata/{field}", delete(book_metadata_delete))
        .route("/api/v1/books/{id}/reading-status", put(book_reading_status))
        .route("/api/v1/books/{id}/reading-overrides", get(overrides_get).put(overrides_put))
        .route("/api/v1/books/{id}/reading-overrides/undo", post(overrides_undo))
        .route("/api/v1/books/{id}/subscription", patch(book_subscription))
        // ---- sync / notes ----
        .route("/api/v1/sync", get(sync_get).post(sync_post))
        .route("/api/v1/sync/progress/{bookId}", get(progress_get).put(progress_put))
        .route("/api/v1/notes", get(notes_get).post(notes_post))
        .route("/api/v1/notes/{id}", delete(notes_delete))
        // ---- subscriptions ----
        .route("/api/v1/subscriptions", get(subscriptions_list))
        // ---- sources ----
        .route("/api/v1/sources/types", get(source_types))
        .route("/api/v1/sources", get(sources_list).post(sources_create))
        .route("/api/v1/sources/{id}", patch(source_patch).delete(source_delete))
        .route("/api/v1/sources/{id}/credentials", get(credentials_list))
        .route("/api/v1/sources/{id}/credentials/{key}", put(credential_put).delete(credential_delete))
        .route("/api/v1/sources/{id}/browse", get(source_browse))
        .route("/api/v1/sources/{id}/search", post(source_search))
        .route("/api/v1/sources/{id}/search/cancel", post(source_search_cancel))
        .route("/api/v1/sources/{id}/entries", get(source_entries))
        .route("/api/v1/sources/{id}/acquire", post(source_acquire))
        .route("/api/v1/sources/{id}/publications/{publicationRef}/manifest", get(publication_manifest))
        .route("/api/v1/sources/{id}/publications/{publicationRef}/resource", get(publication_resource))
        .route("/api/v1/sources/{id}/search-filters", get(search_filters))
        .route("/api/v1/sources/{id}/pages/{pageId}", get(source_page_get).post(source_page_post))
        .route("/api/v1/plugins/{id}/pages/{pageId}", get(plugin_page_get).post(plugin_page_post))
        // ---- plugin management ----
        .route("/api/v1/plugins", get(plugins_list).post(plugins_install))
        .route("/api/v1/plugins/upload", post(plugins_upload))
        .route("/api/v1/plugins/{id}", patch(plugins_patch).delete(plugins_delete))
        // ---- media: compatibility stub for old clients / external callers ----
        .route("/api/v1/media/{*path}", any(media_fallback))
        .route("/api/v1/media", any(media_fallback))
        .fallback(get(web::web_fallback))
        .layer(axum::middleware::from_fn_with_state(api_state.clone(), web::cors_guard))
        .layer(DefaultBodyLimit::max(PLUGIN_UPLOAD_LIMIT + 4 * 1024 * 1024))
        .with_state(api_state);

    let opds_state = state.clone();
    let opds_api = Router::new()
        .route("/opds", get(opds_root))
        .route("/opds/", get(opds_root))
        .route("/opds/search.xml", get(opds_search_description))
        .route("/opds/books/{id}/content", get(opds_book_content))
        .route("/opds/books/{id}/cover", get(opds_book_cover))
        .layer(axum::middleware::from_fn_with_state(opds_state.clone(), web::cors_guard))
        .with_state(opds_state);

    api.merge(opds_api)
}

// ------------------------------------------------------------------ helpers


fn require_string(v: Option<&Value>, key: &str) -> ApiResult<String> {
    let value = v
        .and_then(|b| b.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if value.is_empty() || value.len() > 16_384 {
        return Err(bad_request(format!("{key} must be a nonempty string"), "BAD_REQUEST"));
    }
    Ok(value.to_string())
}

fn qp(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query.get(key).filter(|s| !s.trim().is_empty()).cloned()
}

fn qp_int(query: &HashMap<String, String>, key: &str) -> ApiResult<Option<i64>> {
    match query.get(key) {
        Some(raw) if !raw.trim().is_empty() => raw.trim().parse::<i64>().map(Some)
            .map_err(|_| bad_request(format!("invalid {key}"), "BAD_REQUEST")),
        _ => Ok(None),
    }
}

fn parse_list_query(query: &HashMap<String, String>) -> ApiResult<dto::ListQuery> {
    let mut q = dto::ListQuery {
        search: qp(query, "search"),
        author: qp(query, "author"),
        series: qp(query, "series"),
        tag: qp(query, "tag"),
        format: qp(query, "format"),
        path: qp(query, "path"),
        status: qp(query, "status"),
        ..Default::default()
    };
    if let Some(s) = qp(query, "sort") { q.sort = s; }
    if let Some(s) = qp(query, "order") { q.order = s; }
    if let Some(s) = qp(query, "scope") { q.scope = s; }
    q.page = qp_int(query, "page")?.unwrap_or(1);
    q.page_size = qp_int(query, "pageSize")?.unwrap_or(50);
    if let Some(f) = qp(query, "fileFormats") {
        q.file_formats = Some(f.split(',').map(str::to_string).collect());
    }
    Ok(q)
}


// --------------------------------------------------------------------- auth

async fn auth_register(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let username = require_string(Some(&body), "username")?;
    let password = require_string(Some(&body), "password")?;
    if username.len() > 64 || !username.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '@') {
        return Err(bad_request("invalid username", "BAD_REQUEST"));
    }
    if password.len() < 8 { return Err(bad_request("password must be at least 8 characters", "BAD_REQUEST")); }
    // Invite code arrives in the body when registration mode is `invite`.
    let invite = body.get("inviteCode").and_then(Value::as_str);
    settings::check_registration(&state.db, invite)?;
    let first_user = state.db.count("SELECT COUNT(*) FROM users")? == 0;
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let hash = auth::hash_password(&password)?;
    state.db.exec(
        "INSERT INTO users (id, username, display_name, password_hash, role, disabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
        rusqlite::params![id, username, username, hash, if first_user { "admin" } else { "member" }, now],
    )?;
    if let Some(code) = invite.filter(|_| !invite.unwrap_or("").is_empty()) {
        let code_hash = auth::hash_refresh_token(code);
        state.db.exec(
            "UPDATE registration_invites SET used_count = used_count + 1 WHERE code_hash = ?1",
            [code_hash],
        )?;
    }
    let role = state.db.one("SELECT role FROM users WHERE id = ?1", [&id])?
        .map(|r| crate::db::s(&r, "role")).unwrap_or_else(|| "member".into());
    let user = AuthUser { id, role, ver: 0 };
    session_response(&state, &user, StatusCode::CREATED)
}

async fn auth_login(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let username = require_string(Some(&body), "username")?;
    let password = require_string(Some(&body), "password")?;
    let row = state.db.one(
        "SELECT id, password_hash, role, disabled, auth_version FROM users WHERE username = ?1 COLLATE NOCASE",
        [&username],
    )?
    .ok_or_else(unauthorized_login)?;
    let stored = crate::db::s(&row, "password_hash");
    let role = crate::db::s(&row, "role");
    let ver = crate::db::i(&row, "auth_version");
    let user = AuthUser { id: crate::db::s(&row, "id"), role, ver };
    if crate::db::i(&row, "disabled") != 0 {
        return Err(forbidden("account disabled", "ACCOUNT_DISABLED"));
    }
    if !auth::verify_password(&password, &stored) {
        return Err(unauthorized_login());
    }
    session_response(&state, &user, StatusCode::OK)
}

fn unauthorized_login() -> crate::error::AppError {
    crate::error::unauthorized("用户名或密码错误", "INVALID_CREDENTIALS")
}

/// Issues an access token plus a refresh token row; the body shape matches the
/// Node server exactly (`token`, `expiresAt`, `refreshToken`, `user`).
fn session_response(state: &AppState, user: &AuthUser, status: StatusCode) -> ApiResult<Response> {
    let (token, expires_at) = auth::sign_access_token(&state.config, user)?;
    let refresh = auth::new_refresh_token();
    let now = now_ms();
    let expires = now + state.config.refresh_ttl;
    state.db.exec(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, device, created_at, expires_at) VALUES (?1, ?2, ?3, '', ?4, ?5)",
        rusqlite::params![refresh.id, user.id, refresh.hash, now, expires],
    )?;
    let row = state.db.one("SELECT username, display_name FROM users WHERE id = ?1", [&user.id])?;
    let payload = json!({
        "token": token,
        "expiresAt": expires_at,
        "refreshToken": refresh.token,
        "user": {
            "id": user.id,
            "username": row.as_ref().map(|r| crate::db::s(r, "username")).unwrap_or_default(),
            "displayName": row.as_ref().map(|r| crate::db::s(r, "display_name")).unwrap_or_default(),
            "role": user.role,
        },
    });
    let mut response = (status, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

async fn auth_refresh(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let token = require_string(Some(&body), "refreshToken")?;
    let hash = auth::hash_refresh_token(&token);
    let row = state.db.one(
        "SELECT id, user_id, expires_at, revoked_at FROM refresh_tokens WHERE token_hash = ?1",
        [&hash],
    )?
    .ok_or_else(|| crate::error::unauthorized("refresh token not recognized", "REFRESH_INVALID"))?;
    if crate::db::i(&row, "revoked_at").is_positive() || crate::db::i(&row, "expires_at") <= now_ms() {
        return Err(crate::error::unauthorized("refresh token expired", "REFRESH_INVALID"));
    }
    let user_id = crate::db::s(&row, "id");
    state.db.exec(
        "UPDATE refresh_tokens SET revoked_at = ?1 WHERE id = ?2",
        rusqlite::params![now_ms(), user_id],
    )?;
    let user_row = state.db.one(
        "SELECT id, role, disabled, auth_version FROM users WHERE id = ?1",
        [crate::db::s(&row, "user_id")],
    )?
    .ok_or_else(|| crate::error::unauthorized("account no longer exists", "REFRESH_INVALID"))?;
    if crate::db::i(&user_row, "disabled") != 0 {
        return Err(forbidden("account disabled", "ACCOUNT_DISABLED"));
    }
    let user = AuthUser {
        id: crate::db::s(&user_row, "id"),
        role: crate::db::s(&user_row, "role"),
        ver: crate::db::i(&user_row, "auth_version"),
    };
    session_response(&state, &user, StatusCode::OK)
}

async fn auth_logout(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if let Some(token) = body.get("refreshToken").and_then(Value::as_str) {
        let hash = auth::hash_refresh_token(token);
        state.db.exec(
            "UPDATE refresh_tokens SET revoked_at = ?1 WHERE token_hash = ?2 AND user_id = ?3",
            rusqlite::params![now_ms(), hash, user.id],
        )?;
    }
        Ok(Json(ok()))
}

async fn auth_me(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    let row = state.db.one(
        "SELECT id, username, display_name, role, created_at FROM users WHERE id = ?1",
        [&user.id],
    )?
    .ok_or_else(|| not_found("user not found", "NOT_FOUND"))?;
        Ok(Json(json!({
        "user": {
            "id": crate::db::s(&row, "id"),
            "username": crate::db::s(&row, "username"),
            "displayName": crate::db::s(&row, "display_name"),
            "role": crate::db::s(&row, "role"),
            "createdAt": crate::db::i(&row, "created_at"),
        }
    })))
}

async fn auth_password(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let current = require_string(Some(&body), "currentPassword")?;
    let new_password = require_string(Some(&body), "newPassword")?;
    if new_password.len() < 8 { return Err(bad_request("password must be at least 8 characters", "BAD_REQUEST")); }
    settings::change_password(&state, &user.id, &current, &new_password)?;
        Ok(Json(ok()))
}

// --------------------------------------------------------- instance / admin

async fn instance(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    settings::instance_info(&state).map(Json)
}

async fn health(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let books = state.db.count("SELECT COUNT(*) FROM books").unwrap_or(-1);
        Ok(Json(json!({ "status": "ok", "books": books, "version": env!("CARGO_PKG_VERSION") })))
}

async fn providers(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let _ = &state;
        Ok(Json(json!({ "providers": [] })))
}

async fn admin_registration_get(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::registration(&state.db).map(Json)
}

async fn admin_registration_patch(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let mode = require_string(Some(&body), "mode")?;
    settings::set_registration(&state.db, &mode).map(Json)
}

async fn admin_invite_create(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let label = body.get("label").and_then(Value::as_str).unwrap_or("").to_string();
    let max_uses = body.get("maxUses").and_then(Value::as_i64).unwrap_or(1);
    let days = body.get("expiresInDays").and_then(Value::as_i64).unwrap_or(7);
    settings::create_invite(&state.db, &label, max_uses, days).map(Json)
}

async fn admin_invite_delete(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::disable_invite(&state.db, &id)?;
        Ok(Json(ok()))
}

async fn admin_users_list(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::users_admin(&state).map(Json)
}

async fn admin_users_create(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::admin_create_user(&state, &body).map(Json)
}

async fn admin_users_patch(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::admin_update_user(&state, &id, &body).map(Json)
}

async fn admin_user_password(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let password = require_string(Some(&body), "password")?;
    settings::admin_reset_password(&state, &id, &password).map(Json)
}

// -------------------------------------------------------- settings / tts/ai

async fn admin_settings_all(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::read_all(&state.db).map(Json)
}

async fn admin_settings_patch(State(state): State<AppState>, Auth(user): Auth, AxumPath(group): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::write_group(&state.db, &group, &body).map(Json)
}

async fn admin_settings_test(State(state): State<AppState>, Auth(user): Auth, AxumPath(group): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    settings::test_group(&state, &group, &body).map(Json)
}

async fn admin_tts_preview(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Response> {
    auth::require_admin(&user)?;
    let response = settings::tts_synthesize(&state, &body).await?;
    let bytes = response.bytes().await
        .map_err(|e| api_err(502, "TTS_UPSTREAM", format!("tts upstream failed: {e}")))?;
    let mut r = Response::new(Body::from(bytes));
    r.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("audio/mpeg"));
    Ok(r)
}

async fn admin_tts_clear_cache(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let dir = state.config.data_dir.join("tts-cache");
    let _ = std::fs::remove_dir_all(dir);
        Ok(Json(ok()))
}

async fn admin_webdav_upload(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    // WebDAV backup upload: POST the current DB snapshot to the configured server.
    let base = body.get("url").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if base.is_empty() { return Err(bad_request("url is required", "BAD_REQUEST")); }
    let username = body.get("username").and_then(Value::as_str).unwrap_or("").to_string();
    let password = body.get("password").and_then(Value::as_str).unwrap_or("").to_string();
    let snapshot = state.config.data_dir.join("backup-snapshot.db");
    std::fs::copy(state.config.data_dir.join("reader.db"), &snapshot)
        .map_err(|e| api_err(500, "BACKUP", format!("cannot snapshot database: {e}")))?;
    let bytes = std::fs::read(&snapshot)?;
    let url = format!("{}/reader-rs-backup.db", base.trim_end_matches('/'));
    let mut request = state.http.put(&url).header(header::CONTENT_TYPE, "application/octet-stream");
    if !username.is_empty() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let cred = STANDARD.encode(format!("{username}:{password}"));
        request = request.header(header::AUTHORIZATION, format!("Basic {cred}"));
    }
    let response = request.body(bytes).send().await
        .map_err(|e| api_err(502, "WEBDAV", format!("webdav request failed: {e}")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(api_err(502, "WEBDAV", format!("webdav responded {status}")));
    }
        Ok(Json(json!({ "ok": true, "status": status })))
}

async fn tts_voices(State(state): State<AppState>, Auth(_): Auth) -> ApiResult<Json<Value>> {
    settings::tts_voices(&state).map(Json)
}

async fn tts_test(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Response> {
    let response = settings::tts_synthesize(&state, &body).await?;
    let bytes = response.bytes().await
        .map_err(|e| api_err(502, "TTS_UPSTREAM", format!("tts upstream failed: {e}")))?;
    let mut r = Response::new(Body::from(bytes));
    r.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("audio/mpeg"));
    Ok(r)
}

async fn tts_get(State(state): State<AppState>, Auth(_): Auth, Query(query): Query<HashMap<String, String>>) -> ApiResult<Response> {
    let mut body = json!({});
    if let Some(text) = qp(&query, "text") { body["text"] = json!(text); }
    if let Some(voice) = qp(&query, "voice") { body["voice"] = json!(voice); }
    let response = settings::tts_synthesize(&state, &body).await?;
    let bytes = response.bytes().await
        .map_err(|e| api_err(502, "TTS_UPSTREAM", format!("tts upstream failed: {e}")))?;
    let mut r = Response::new(Body::from(bytes));
    r.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("audio/mpeg"));
    Ok(r)
}

async fn ai_models(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let models = settings::ai_models(&state, Some(&body)).await?;
        Ok(Json(json!({ "models": models })))
}

async fn ai_summary(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if body.get("bookId").and_then(Value::as_str).unwrap_or("").is_empty()
        || body.get("chapterId").and_then(Value::as_str).unwrap_or("").is_empty()
        || body.get("content").and_then(Value::as_str).unwrap_or("").is_empty()
    {
        return Err(bad_request("缺少章节内容", "BAD_REQUEST"));
    }
    settings::ai_summary(&state, &body).await.map(Json)
}

// ------------------------------------------------------------------ library

async fn library_facets(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    dto::facets(&state.db, &user.id).map(Json)
}

async fn library_browse(State(state): State<AppState>, Auth(user): Auth, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let page = qp_int(&query, "page")?.unwrap_or(1);
    browse::browse(&state, &qp(&query, "path").unwrap_or_default(), page).map(Json)
        .map(|mut v| { v["user"] = json!(user.id); v })
}

async fn library_browse_move(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let paths = body.get("paths").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
    browse::move_paths(&state, &paths, &require_string(Some(&body), "target")?).map(Json)
}

async fn library_browse_rename(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    browse::rename(&state, &require_string(Some(&body), "path")?, &require_string(Some(&body), "name")?).map(Json)
}

async fn library_browse_mkdir(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    browse::mkdir(&state, &require_string(Some(&body), "path")?, &require_string(Some(&body), "name")?).map(Json)
}

async fn library_browse_delete(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let paths = body.get("paths").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
    browse::delete_paths(&state, &paths).map(Json)
}

async fn library_browse_metadata(State(state): State<AppState>, Auth(_): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let paths = body.get("paths").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
    browse::batch_metadata(&state, &paths, body.get("fields").unwrap_or(&json!({}))).map(Json)
}

async fn library_browse_shelf(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    browse::batch_shelf(&state, &body, &user.id).map(Json)
}

async fn library_upload_targets(State(state): State<AppState>, Auth(_): Auth) -> ApiResult<Json<Value>> {
        Ok(Json(json!({ "writable": browse::is_writable(&state.config.books_dir), "formats": browse::formats() })))
}

async fn library_upload(
    State(state): State<AppState>,
    Auth(_): Auth,
    mut upload: Multipart,
) -> ApiResult<Json<Value>> {
    let mut target_dir = String::new();
    let mut on_conflict = "rename".to_string();
    let mut saved = Vec::new();
    while let Some(field) = upload.next_field().await
        .map_err(|e| bad_request(format!("invalid upload: {e}"), "BAD_REQUEST"))?
    {
        match field.name().unwrap_or_default() {
            "targetDir" | "path" => target_dir = field.text().await.unwrap_or_default(),
            "onConflict" => on_conflict = field.text().await.unwrap_or_default(),
            "file" | "files" => {
                let name = field.file_name().unwrap_or_default().to_string();
                if name.is_empty() { return Err(bad_request("missing filename", "BAD_REQUEST")); }
                if !indexer::supported_extension(&name) {
                    return Err(bad_request(format!("unsupported file type: {name}"), "UNSUPPORTED_FORMAT"));
                }
                let bytes = field.bytes().await
                    .map_err(|e| bad_request(format!("upload too large or broken: {e}"), "PAYLOAD_TOO_LARGE"))?;
                saved.push((name, bytes));
            }
            _ => {}
        }
    }
    if saved.is_empty() { return Err(bad_request("no file provided", "BAD_REQUEST")); }
    let mut results = Vec::new();
    for (name, bytes) in saved {
        let result = browse::save_upload(&state, &name, &target_dir, &bytes, &on_conflict)?;
        results.push(result);
    }
    Ok(Json(json!({ "saved": results })))
}

async fn library_scan_start(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    indexer::scan_async(state.clone());
        Ok(Json(indexer::scan_status(&state)))
}

async fn library_scan_status(State(state): State<AppState>, Auth(_): Auth) -> ApiResult<Json<Value>> {
        Ok(Json(indexer::scan_status(&state)))
}

async fn library_formats(State(_state): State<AppState>) -> ApiResult<Json<Value>> {
        Ok(Json(browse::formats()))
}

async fn library_stats(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    browse::stats(&state).map(|mut v| { v["userId"] = json!(user.id); v }).map(Json)
}

async fn library_continue(State(state): State<AppState>, Auth(user): Auth, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let limit = qp_int(&query, "limit")?.unwrap_or(12);
    sync::recently_read(&state, &user.id, limit).map(Json)
}

async fn books_list(State(state): State<AppState>, Auth(user): Auth, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let q = parse_list_query(&query)?;
    dto::list_books(&state.db, &user.id, &q).map(Json)
}

async fn book_get(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    dto::get_book(&state.db, &user.id, &id).map(Json)
}

async fn book_manifest(
    State(state): State<AppState>,
    Auth(user): Auth,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let _ = user;
    let group = qp_int(&query, "group")?;
    Ok(Json(indexer::manifest(&state, &id, group)?))
}

async fn book_refresh(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    // Re-parse every book file for this book from disk.
    ensure_book_access(&state, &user, &id)?;
    let rows = state.db.query(
        "SELECT rel_path FROM book_files WHERE book_id = ?1 AND missing = 0", [&id],
    )?;
    let _ = rows;
    // The scan pipeline is content-addressed; a targeted refresh re-runs the parser.
    indexer::rescan_book(&state, &id)?;
    indexer::manifest(&state, &id, None).map(Json)
}

fn ensure_book_access(state: &AppState, user: &AuthUser, book_id: &str) -> ApiResult<()> {
    let exists = state.db.one("SELECT id FROM books WHERE id = ?1", [book_id])?
        .ok_or_else(|| not_found("book not found", "NOT_FOUND"))?;
    let _ = exists;
    if user.role == "admin" { return Ok(()); }
    state.db.one(
        "SELECT hidden FROM user_books WHERE user_id = ?1 AND book_id = ?2",
        rusqlite::params![user.id, book_id],
    )?.ok_or_else(|| not_found("book not found", "NOT_FOUND"))?;
        Ok(())
}

async fn book_content(
    State(state): State<AppState>,
    Auth(_): Auth,
    AxumPath(id): AxumPath<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ref_value = raw.as_deref().unwrap_or("").split('&')
        .find_map(|pair| pair.strip_prefix("ref=").map(crate::web::url_decode))
        .unwrap_or_default();
    if ref_value.is_empty() { return Err(bad_request("ref is required", "BAD_REQUEST")); }
    let (data, content_type) = indexer::resolve_asset(&state, &id, &ref_value)?;
    let opts = crate::assets::AssetOpts {
        content_type,
        ..Default::default()
    };
    Ok(crate::assets::send_asset(&headers, crate::assets::AssetPayload::Memory(data), opts).await)
}

async fn book_items(State(state): State<AppState>, Auth(_): Auth, AxumPath(id): AxumPath<String>, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let group = qp_int(&query, "group")?;
    let manifest = indexer::manifest(&state, &id, group)?;
        Ok(Json(json!({ "items": manifest.get("items").cloned().unwrap_or(json!([])) })))
}

async fn book_toc(State(state): State<AppState>, Auth(_): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    indexer::toc(&state, &id).map(Json)
}

async fn book_search(State(state): State<AppState>, Auth(_): Auth, AxumPath(id): AxumPath<String>, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let q = qp(&query, "q").ok_or_else(|| bad_request("q is required", "BAD_REQUEST"))?;
    let cursor = qp_int(&query, "cursor")?;
    indexer::search_in_book(&state, &id, &q, cursor).map(Json)
}

async fn book_assets(State(state): State<AppState>, Auth(_): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    let structure = indexer::load_structure(&state.config, &id)?;
    let assets = structure.get("assets").cloned().unwrap_or(json!([]));
        Ok(Json(json!({ "assets": assets })))
}

async fn book_file(
    State(state): State<AppState>,
    Auth(_): Auth,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let row = state.db.one(
        "SELECT rel_path, size, mtime_ms FROM book_files WHERE book_id = ?1 AND missing = 0 ORDER BY rel_path LIMIT 1",
        [&id],
    )?
    .ok_or_else(|| not_found("book file not found", "NOT_FOUND"))?;
    let rel = crate::db::s(&row, "rel_path");
    let abs = browse::resolve_inside(&state.config.books_dir, &rel)?;
    let opts = crate::assets::AssetOpts {
        content_type: "application/octet-stream".into(),
        last_modified: Some(crate::db::i(&row, "mtime_ms")),
        filename: Some(rel.rsplit('/').next().unwrap_or(&rel).to_string()),
        seekable: true,
        not_modified_on: None,
        ..Default::default()
    };
    Ok(crate::assets::send_asset(&headers, crate::assets::AssetPayload::File { path: abs, size: crate::db::i(&row, "size") as u64 }, opts).await)
}

async fn book_cover(
    State(state): State<AppState>,
    Auth(_): Auth,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let row = state.db.one("SELECT cover_path FROM books WHERE id = ?1", [&id])?
        .ok_or_else(|| not_found("book not found", "NOT_FOUND"))?;
    let cover = crate::db::s(&row, "cover_path");
    if cover.is_empty() {
        return Err(not_found("no cover", "NO_COVER"));
    }
    let path = indexer::cover_path(&state.config, &cover);
    let data = std::fs::read(&path).map_err(|_| not_found("no cover", "NO_COVER"))?;
    let content_type = mime_guess::from_path(&path).first_or_octet_stream().to_string();
    let opts = crate::assets::AssetOpts { content_type, ..Default::default() };
    Ok(crate::assets::send_asset(&headers, crate::assets::AssetPayload::Memory(data), opts).await)
}

async fn book_metadata_patch(State(state): State<AppState>, Auth(_): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    apply_metadata(&state, &id, &body)?;
    dto::get_book(&state.db, "", &id).map(Json)
}

async fn book_metadata_delete(State(state): State<AppState>, Auth(_): Auth, AxumPath((id, field)): AxumPath<(String, String)>) -> ApiResult<Json<Value>> {
    state.db.exec("DELETE FROM metadata_overrides WHERE book_id = ?1 AND field = ?2", rusqlite::params![id, field])?;
        Ok(Json(ok()))
}

fn apply_metadata(state: &AppState, book_id: &str, body: &Value) -> ApiResult<()> {
    let obj = body.as_object()
        .ok_or_else(|| bad_request("body must be an object", "BAD_REQUEST"))?;
    for (field, value) in obj {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Null => continue,
            other => serde_json::to_string(other)?,
        };
        state.db.exec(
            "INSERT INTO metadata_overrides (book_id, field, value, updated_by, updated_at) VALUES (?1, ?2, ?3, 'manual', ?4)
             ON CONFLICT(book_id, field) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![book_id, field, text, now_ms()],
        )?;
    }
        Ok(())
}

async fn book_reading_status(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let status = require_string(Some(&body), "status")?;
    if !["unread", "reading", "finished"].contains(&status.as_str()) {
        return Err(bad_request("invalid status", "BAD_REQUEST"));
    }
    ensure_book_access(&state, &user, &id)?;
    // Reading status (reading/finished) is derived from progress; this
    // endpoint just makes sure the book is on the shelf.
    state.db.exec(
        "INSERT OR IGNORE INTO user_books (user_id, book_id, added_at, hidden) VALUES (?1, ?2, ?3, 0)",
        rusqlite::params![user.id, id, now_ms()],
    )?;
        Ok(Json(json!({ "status": status })))
}

async fn overrides_get(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    let map = dto::overrides(&state.db, &id)?;
    let user_row = state.db.one(
        "SELECT version, payload FROM reading_overrides WHERE user_id = ?1 AND book_id = ?2 ORDER BY version DESC LIMIT 1",
        rusqlite::params![user.id, id],
    )?;
    let payload = user_row
        .map(|r| serde_json::from_str::<Value>(&crate::db::s(&r, "payload")).unwrap_or(json!({})))
        .unwrap_or(json!({}));
        Ok(Json(json!({ "overrides": map, "reading": payload })))
}

async fn overrides_put(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    dto::set_overrides(&state, &id, &body, &user.id)?;
        Ok(Json(json!({ "overrides": dto::overrides(&state.db, &id)? })))
}

async fn overrides_undo(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let fields = body.get("fields").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
    for field in fields {
        let _ = user;
        dto::clear_override(&state, &id, &field)?;
    }
        Ok(Json(json!({ "overrides": dto::overrides(&state.db, &id)? })))
}

async fn book_subscription(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    ensure_book_access(&state, &user, &id)?;
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    let interval = body.get("intervalMinutes").and_then(Value::as_i64).unwrap_or(60).max(5);
    state.db.exec(
        "INSERT INTO chapter_subscriptions (book_id, enabled, interval_minutes, next_check_at) VALUES (?1, ?2, ?3, 0)
         ON CONFLICT(book_id) DO UPDATE SET enabled = excluded.enabled, interval_minutes = excluded.interval_minutes",
        rusqlite::params![id, if enabled { 1 } else { 0 }, interval],
    )?;
        Ok(Json(json!({ "subscription": { "bookId": id, "enabled": enabled, "intervalMinutes": interval } })))
}

async fn subscriptions_list(State(state): State<AppState>, Auth(_): Auth) -> ApiResult<Json<Value>> {
    let rows = state.db.query(
        "SELECT s.book_id, s.enabled, s.interval_minutes, s.next_check_at, s.last_check_at, s.last_error, b.title
         FROM chapter_subscriptions s JOIN books b ON b.id = s.book_id", [],
    )?;
    let items: Vec<Value> = rows.iter().map(|r| json!({
        "bookId": crate::db::s(r, "book_id"),
        "title": crate::db::s(r, "title"),
        "enabled": crate::db::i(r, "enabled") != 0,
        "intervalMinutes": crate::db::i(r, "interval_minutes"),
        "nextCheckAt": crate::db::i(r, "next_check_at"),
        "lastCheckAt": crate::db::i(r, "last_check_at"),
        "lastError": crate::db::s(r, "last_error"),
    })).collect();
        Ok(Json(json!({ "subscriptions": items })))
}

// -------------------------------------------------------------------- sync

async fn sync_get(State(state): State<AppState>, Auth(user): Auth, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let since = qp_int(&query, "since")?.unwrap_or(0);
    let book_id = qp(&query, "bookId");
    sync::pull(&state.db, &user.id, since, book_id.as_deref()).map(Json)
}

async fn sync_post(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let progress = body.get("progress").and_then(Value::as_array).cloned().unwrap_or_default();
    let notes = body.get("notes").and_then(Value::as_array).cloned().unwrap_or_default();
    let (applied_progress, applied_notes) = sync::push(&state, &user.id, &progress, &notes)?;
    let mut pull = sync::pull(&state.db, &user.id, body.get("since").and_then(Value::as_i64).unwrap_or(0), None)?;
    pull["applied"] = json!({ "progress": applied_progress, "notes": applied_notes });
        Ok(Json(pull))
}

async fn progress_get(State(state): State<AppState>, Auth(user): Auth, AxumPath(book_id): AxumPath<String>) -> ApiResult<Json<Value>> {
    let row = sync::progress_for(&state.db, &user.id, &book_id)?;
        Ok(Json(json!({ "progress": row })))
}

async fn progress_put(State(state): State<AppState>, Auth(user): Auth, AxumPath(book_id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut wrapped = body.as_object().cloned().unwrap_or_default();
    wrapped.insert("bookId".into(), json!(book_id));
    let (applied, _) = sync::push(&state, &user.id, &[Value::Object(wrapped)], &[])?;
    let row = sync::progress_for(&state.db, &user.id, &book_id)?;
        Ok(Json(json!({ "applied": applied, "progress": row })))
}

async fn notes_get(State(state): State<AppState>, Auth(user): Auth, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let since = qp_int(&query, "since")?.unwrap_or(0);
    sync::list_notes(&state.db, &user.id, qp(&query, "bookId").as_deref(), since).map(Json)
}

async fn notes_post(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    sync::create_note(&state, &user.id, &body).map(Json)
}

async fn notes_delete(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    sync::delete_note(&state, &user.id, &id)?;
        Ok(Json(ok()))
}

// ------------------------------------------------------------------ sources

async fn source_types(State(state): State<AppState>, Auth(_): Auth) -> ApiResult<Json<Value>> {
    let types = state.sources.source_types().await;
        Ok(Json(json!({ "types": types.iter().map(|t| t.to_json()).collect::<Vec<_>>() })))
}

async fn sources_list(State(state): State<AppState>, Auth(_): Auth) -> ApiResult<Json<Value>> {
    sources::instances(&state.db).map(Json)
}

async fn sources_create(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Response> {
    auth::require_admin(&user)?;
    let result = sources::save_instance(&state.db, None, &body)?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}

async fn source_patch(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    sources::save_instance(&state.db, Some(&id), &body).map(Json)
}

async fn source_delete(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    sources::remove_instance(&state.db, &id).map(Json)
}

async fn credentials_list(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    sources::credential_status(&state.db, &id, &user.id).map(Json)
}

async fn credential_put(State(state): State<AppState>, Auth(user): Auth, AxumPath((id, key)): AxumPath<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let value = body.get("value").and_then(Value::as_str)
        .ok_or_else(|| bad_request("credential value must be a string", "BAD_REQUEST"))?;
    sources::set_credential(&state.db, &id, &user.id, &key, value)?;
        Ok(Json(ok()))
}

async fn credential_delete(State(state): State<AppState>, Auth(user): Auth, AxumPath((id, key)): AxumPath<(String, String)>) -> ApiResult<Json<Value>> {
    sources::delete_credential(&state.db, &id, &user.id, &key)?;
        Ok(Json(ok()))
}

/// Resolves the instance row; callers match on the returned source type and
/// dispatch to builtin implementations or the plugin host themselves.
async fn resolve_source(state: &AppState, user: &AuthUser, id: &str) -> ApiResult<(String, Value)> {
    let row = state.db.one(
        "SELECT id, source_type, config_json, enabled FROM source_instances WHERE id = ?1",
        [id],
    )?
    .ok_or_else(|| not_found("source not found", "SOURCE_NOT_FOUND"))?;
    if crate::db::i(&row, "enabled") == 0 {
        return Err(api_err(409, "SOURCE_DISABLED", "source is disabled"));
    }
    let source_type = crate::db::s(&row, "source_type");
    let config: Value = serde_json::from_str(&crate::db::s(&row, "config_json")).unwrap_or(json!({}));
    let credentials = sources::credential_values(&state.db, id, &user.id, &[]);
    Ok((source_type.clone(), json!({ "userId": user.id, "config": config, "credentials": credentials })))
}

async fn source_browse(
    State(state): State<AppState>,
    Auth(user): Auth,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let (source_type, ctx) = resolve_source(&state, &user, &id).await?;
    let params = json!({
        "ref": qp(&query, "ref").unwrap_or_default(),
        "cursor": qp(&query, "cursor"),
        "limit": qp_int(&query, "limit")?,
    });
    match source_type.as_str() {
        "local" => sources::local_browse(&state.db, &user.id, &params).map(Json),
        "opds" => opds::client_browse(&state, &user, &ctx, &params).await.map(Json),
        _ => state.sources.call(&source_type, "browse", params).await.map(Json),
    }
}

async fn source_search(
    State(state): State<AppState>,
    Auth(user): Auth,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let query = require_string(Some(&body), "query")?;
    let session_id = require_string(Some(&body), "sessionId")?;
    let mut cursor = body.get("cursor").and_then(Value::as_str).map(str::to_string);
    let row = state.db.one(
        "SELECT source_type, config_json, enabled FROM source_instances WHERE id = ?1", [&id],
    )?
    .ok_or_else(|| not_found("source not found", "SOURCE_NOT_FOUND"))?;
    let source_type = crate::db::s(&row, "source_type");
    let config: Value = serde_json::from_str(&crate::db::s(&row, "config_json")).unwrap_or(json!({}));

    let stream = async_stream::stream! {
        yield Ok::<_, std::io::Error>(bytes::Bytes::from(": search connected\n\n".to_string()));
        loop {
            let params = json!({
                "query": query, "sessionId": session_id, "cursor": cursor, "sourceType": source_type,
                "config": config, "userId": user.id,
            });
            let page = match source_type.as_str() {
                "local" => sources::local_browse(&state.db, &user.id, &params),
                "opds" => opds::client_search(&state, &user, &params).await,
                _ => state.sources.call(&source_type, "search", params).await,
            };
            match page {
                Ok(page) => {
                    let frame = format!("event: results\ndata: {}\n\n", page);
                    yield Ok(bytes::Bytes::from(frame));
                    cursor = page.get("nextCursor").and_then(Value::as_str).map(str::to_string);
                    if cursor.is_none() {
                        let done = format!("event: done\ndata: {}\n\n", json!({ "reason": "complete" }));
                        yield Ok(bytes::Bytes::from(done));
                        break;
                    }
                }
                Err(e) => {
                    let err = format!("event: error\ndata: {}\n\n", json!({
                        "code": "SEARCH_FAILED", "message": e.to_string(), "status": 500,
                    }));
                    yield Ok(bytes::Bytes::from(err));
                    break;
                }
            }
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream; charset=utf-8"));
    response.headers_mut().insert(header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-transform"));
    Ok(response)
}

async fn source_search_cancel(State(state): State<AppState>, Auth(_): Auth, AxumPath(_id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    // Search sessions are connection-scoped here; closing the SSE connection
    // already stops pagination, so cancel just acknowledges.
    let _ = state;
    require_string(Some(&body), "sessionId")?;
        Ok(Json(ok()))
}

async fn source_entries(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Query(query): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let (source_type, ctx) = resolve_source(&state, &user, &id).await?;
    let entry_ref = qp(&query, "ref").ok_or_else(|| bad_request("ref is required", "BAD_REQUEST"))?;
    let params = json!({ "entryRef": entry_ref });
    match source_type.as_str() {
        "local" => sources::local_detail(&state.db, &user.id, &entry_ref).map(Json),
        "opds" => opds::client_detail(&state, &user, &ctx, &entry_ref).map(Json),
        _ => state.sources.call(&source_type, "detail", params).await.map(Json),
    }
}

async fn source_acquire(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let (source_type, ctx) = resolve_source(&state, &user, &id).await?;
    let entry_ref = require_string(Some(&body), "entryRef")?;
    let option_id = body.get("optionId").and_then(Value::as_str).unwrap_or("").to_string();
    let params = json!({ "entryRef": entry_ref, "optionId": option_id });
    match source_type.as_str() {
        "local" => sources::local_acquire(&state, &user.id, &entry_ref).map(Json),
        "opds" => opds::client_acquire(&state, &user, &ctx, &entry_ref).await.map(Json),
        _ => state.sources.call(&source_type, "acquire", params).await.map(Json),
    }
}

async fn publication_manifest(
    State(state): State<AppState>,
    Auth(user): Auth,
    AxumPath((id, publication_ref)): AxumPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (source_type, ctx) = resolve_source(&state, &user, &id).await?;
    let params = json!({ "publicationRef": publication_ref });
    match source_type.as_str() {
        "opds" => opds::client_publication_manifest(&state, &user, &ctx, &publication_ref).map(Json),
        _ => state.sources.call(&source_type, "manifest", params).await.map(Json),
    }
}

async fn publication_resource(
    State(state): State<AppState>,
    Auth(user): Auth,
    AxumPath((id, publication_ref)): AxumPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let (source_type, ctx) = resolve_source(&state, &user, &id).await?;
    let ref_value = qp(&query, "ref").ok_or_else(|| bad_request("ref is required", "BAD_REQUEST"))?;
    let params = json!({ "publicationRef": publication_ref, "ref": ref_value });
    match source_type.as_str() {
        "opds" => opds::client_publication_resource(&state, &user, &ctx, &publication_ref, &ref_value).map(Json),
        _ => state.sources.call(&source_type, "resource", params).await.map(Json),
    }
}

async fn search_filters(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    let (source_type, _ctx) = resolve_source(&state, &user, &id).await?;
    match source_type.as_str() {
        "local" | "opds" => Ok(Json(json!({ "filters": [] }))),
        _ => state.sources.call(&source_type, "search-filters", json!({})).await.map(Json),
    }
}

async fn source_page_get(State(state): State<AppState>, Auth(user): Auth, AxumPath((id, page_id)): AxumPath<(String, String)>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let (source_type, _ctx) = resolve_source(&state, &user, &id).await?;
    match source_type.as_str() {
        "local" | "opds" => Err(not_found("no extension pages", "NOT_FOUND")),
        _ => state.sources.call(&source_type, "page", json!({ "pageId": page_id })).await.map(Json),
    }
}

async fn source_page_post(State(state): State<AppState>, Auth(user): Auth, AxumPath((id, page_id)): AxumPath<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let (source_type, _ctx) = resolve_source(&state, &user, &id).await?;
    let action = require_string(Some(&body), "action")?;
    match source_type.as_str() {
        "local" | "opds" => Err(not_found("no extension pages", "NOT_FOUND")),
        _ => state.sources.call(&source_type, "page-action", json!({
            "pageId": page_id, "action": action, "values": body.get("values").unwrap_or(&json!({})),
        })).await.map(Json)
    }
}

async fn plugin_page_get(State(state): State<AppState>, Auth(user): Auth, AxumPath((id, page_id)): AxumPath<(String, String)>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let manifests = state.sources.installed_manifests().await;
    if !manifests.iter().any(|(pid, _)| pid == &id) {
        return Err(not_found("plugin not installed", "PLUGIN_NOT_FOUND"));
    }
    state.sources.call_plugin(&id, "page", json!({ "pageId": page_id })).await.map(Json)
}

async fn plugin_page_post(State(state): State<AppState>, Auth(user): Auth, AxumPath((id, page_id)): AxumPath<(String, String)>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let action = require_string(Some(&body), "action")?;
    state.sources.call_plugin(&id, "page-action", json!({
        "pageId": page_id, "action": action, "values": body.get("values").unwrap_or(&json!({})),
    })).await.map(Json)
}

async fn plugins_list(State(state): State<AppState>, Auth(user): Auth) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    state.sources.plugin_list().await.map(Json)
}

async fn plugins_install(State(state): State<AppState>, Auth(user): Auth, Json(body): Json<Value>) -> ApiResult<Response> {
    auth::require_admin(&user)?;
    if body.get("trusted").and_then(Value::as_bool) != Some(true) {
        return Err(bad_request("trusted must be true: installed plugins execute with server OS privileges", "PLUGIN_TRUST_REQUIRED"));
    }
    let folder = require_string(Some(&body), "folder")?;
    let dir: PathBuf = state.config.data_dir.join("plugins").join(&folder);
    let bytes = std::fs::read(dir.join("plugin.json"))
        .map_err(|_| not_found("plugin folder not found", "PLUGIN_NOT_FOUND"))?;
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|_| bad_request("invalid plugin.json", "PLUGIN_INVALID_PACKAGE"))?;
    let id = format!("installed:{folder}");
    let manifest = sources::validate_manifest(&raw, &id)?;
    state.db.exec(
        "INSERT OR REPLACE INTO installed_plugins (plugin_id, folder, enabled) VALUES (?1, ?2, 1)",
        rusqlite::params![id, folder],
    )?;
    let plugin = json!({
        "pluginId": id, "builtin": false,
        "name": manifest.name, "version": manifest.version, "folder": folder, "enabled": true,
    });
    Ok((StatusCode::CREATED, Json(json!({ "plugin": plugin }))).into_response())
}

async fn plugins_upload(
    State(state): State<AppState>,
    Auth(user): Auth,
    mut upload: Multipart,
) -> ApiResult<Response> {
    auth::require_admin(&user)?;
    let mut trusted = false;
    let mut bytes: Option<Vec<u8>> = None;
    let mut filename = String::new();
    while let Some(field) = upload.next_field().await
        .map_err(|e| bad_request(format!("invalid upload: {e}"), "NOT_MULTIPART"))?
    {
        match field.name().unwrap_or_default() {
            "trusted" => trusted = field.text().await.unwrap_or_default() == "true",
            "file" => {
                filename = field.file_name().unwrap_or_default().to_string();
                let data = field.bytes().await
                    .map_err(|_| bad_request("安装包不能超过 100 MiB。", "PLUGIN_PACKAGE_TOO_LARGE"))?;
                bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }
    if !trusted {
        return Err(bad_request("请确认信任插件代码。", "PLUGIN_TRUST_REQUIRED"));
    }
    if !filename.to_lowercase().ends_with(".tgz") || bytes.is_none() {
        return Err(bad_request("只支持 .tgz 安装包。", "PLUGIN_INVALID_PACKAGE"));
    }
    let plugin = state.sources.install_archive(&bytes.unwrap())?;
    Ok((StatusCode::CREATED, Json(plugin)).into_response())
}

async fn plugins_patch(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    let enabled = body.get("enabled").and_then(Value::as_bool)
        .ok_or_else(|| bad_request("enabled must be boolean", "BAD_REQUEST"))?;
    state.sources.set_enabled(&id, enabled)?;
        Ok(Json(json!({ "plugin": { "pluginId": id, "enabled": enabled } })))
}

async fn plugins_delete(State(state): State<AppState>, Auth(user): Auth, AxumPath(id): AxumPath<String>) -> ApiResult<Json<Value>> {
    auth::require_admin(&user)?;
    state.sources.remove(&id)?;
        Ok(Json(ok()))
}

// -------------------------------------------------------------- media compat

/// The reading client has no media frontend anymore. This stub only keeps old
/// clients and external callers from getting a hard failure; it never touches
/// media.db and never renders a page. Empty collections are the whole contract.
async fn media_fallback(AxumPath(path): AxumPath<String>) -> Response {
    let path = path.as_str();
    let body = if path.starts_with("libraries") || path.contains("/browse") || path.contains("/search") || path.contains("/items") {
        json!({ "items": [], "total": 0 })
    } else if path.contains("queue") || path.contains("continue") {
        json!({ "queue": [], "items": [] })
    } else if path.contains("preferences") || path.contains("preferenceScope") {
        json!({ "scope": "global" })
    } else {
        json!({ "items": [], "total": 0 })
    };
    (StatusCode::OK, Json(body)).into_response()
}

// ------------------------------------------------------------- opds serving

async fn opds_root(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    opds::server_root(&state, &query, &headers).await
}

async fn opds_search_description(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    opds::server_search_description(&state, &headers).await
}

async fn opds_book_content(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    opds::server_book_content(&state, &id, &headers).await
}

async fn opds_book_cover(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    opds::server_book_cover(&state, &id, &headers).await
}
