//! OPDS 1.2: client (browse/search/detail/acquire against remote catalogs)
//! and server (Atom feed over the local library, basic auth by catalog token).

use base64::{engine::general_purpose::STANDARD, Engine as _};
use axum::{
    body::Body,
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{
    auth::AuthUser,
    browse,
    error::{api_err, bad_request, not_found, ApiResult},
    state::AppState,
};

// ------------------------------------------------------------------ parsing

#[derive(Default)]
struct FeedEntry {
    id: String,
    title: String,
    authors: Vec<String>,
    summary: String,
    updated: String,
    links: Vec<(String, String, String)>, // rel, href, type
}

#[derive(Default)]
struct Feed {
    title: String,
    id: String,
    updated: String,
    entries: Vec<FeedEntry>,
}
/// Minimal streaming parse of an OPDS/Atom feed. Namespaces are stripped so
/// `entry`, `link` etc. match regardless of prefix.
fn parse_feed(xml: &str) -> Feed {
    let mut feed = Feed::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut path: Vec<String> = Vec::new();
    let mut current: Option<FeedEntry> = None;
    let mut buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = strip_ns(&String::from_utf8_lossy(e.name().as_ref()));
                path.push(name.clone());
                buf.clear();
                if name == "entry" {
                    current = Some(FeedEntry::default());
                }
                if name == "link" {
                    let mut rel = String::from("alternative");
                    let mut href = String::new();
                    let mut ty = String::new();
                    for attr in e.attributes().flatten() {
                        let key = strip_ns(&String::from_utf8_lossy(attr.key.as_ref()));
                        let value = String::from_utf8_lossy(&attr.value).into_owned();
                        match key.as_str() {
                            "rel" => rel = value,
                            "href" => href = value,
                            "type" => ty = value,
                            _ => {}
                        }
                    }
                    let link = (rel, href, ty);
                    if let Some(entry) = current.as_mut() {
                        entry.links.push(link);
                    } else if path.iter().any(|p| p == "feed") {
                        feed.entries.push(FeedEntry { links: vec![link], ..Default::default() });
                        // placeholder entry removed below by filtering empty ids
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name = strip_ns(&String::from_utf8_lossy(e.name().as_ref()));
                if name == "link" {
                    let mut rel = String::from("alternative");
                    let mut href = String::new();
                    let mut ty = String::new();
                    for attr in e.attributes().flatten() {
                        let key = strip_ns(&String::from_utf8_lossy(attr.key.as_ref()));
                        let value = String::from_utf8_lossy(&attr.value).into_owned();
                        match key.as_str() {
                            "rel" => rel = value,
                            "href" => href = value,
                            "type" => ty = value,
                            _ => {}
                        }
                    }
                    let link = (rel, href, ty);
                    if let Some(entry) = current.as_mut() {
                        entry.links.push(link);
                    }
                    // Links outside an entry (feed-level) are informational.
                }
            }
            Ok(Event::Text(t)) => {
                buf.push_str(&String::from_utf8_lossy(&t));
            }
            Ok(Event::CData(t)) => {
                buf.push_str(&String::from_utf8_lossy(&t));
            }
            Ok(Event::End(e)) => {
                let name = strip_ns(&String::from_utf8_lossy(e.name().as_ref()));
                let text = buf.trim().to_string();
                let in_entry = current.is_some();
                match name.as_str() {
                    "title" => {
                        if in_entry { current.as_mut().unwrap().title = text.clone(); }
                        else { feed.title = text.clone(); }
                    }
                    "id" => {
                        if in_entry { current.as_mut().unwrap().id = text.clone(); }
                        else { feed.id = text.clone(); }
                    }
                    "summary" | "content" => {
                        if in_entry && current.as_ref().unwrap().summary.is_empty() {
                            current.as_mut().unwrap().summary = text.clone();
                        }
                    }
                    "updated" => {
                        if in_entry { current.as_mut().unwrap().updated = text.clone(); }
                        else { feed.updated = text.clone(); }
                    }
                    "name" => {
                        if in_entry { current.as_mut().unwrap().authors.push(text.clone()); }
                    }
                    "entry" => {
                        if let Some(entry) = current.take() {
                            if !entry.id.is_empty() || !entry.title.is_empty() {
                                feed.entries.push(entry);
                            }
                        }
                    }
                    _ => {}
                }
                buf.clear();
                path.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    feed.entries.retain(|e| !e.id.is_empty() || !e.title.is_empty() || !e.links.is_empty() || !e.title.is_empty());
    feed.entries.retain(|e| !(e.title.is_empty() && e.links.len() == 1));
    feed
}

fn strip_ns(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

impl Feed {
    fn link_for(&self, entry: &FeedEntry, contains: &[&str]) -> Option<String> {
        entry.links.iter()
            .find(|(rel, href, ty)| {
                contains.iter().any(|c| rel.contains(c) || href.contains(c) || ty.contains(c))
                    || (rel == "alternate" && href.contains(".opds"))
            })
            .map(|(_, href, _)| href.clone())
    }

    fn to_json(&self) -> Vec<Value> {
        self.entries.iter().map(|e| {
            let acquisition = self.link_for(e, &["acquisition", "replies", ".epub", ".pdf", "image"]);
            let navigation = self.link_for(e, &["subsection", "catalog", ".opds"]);
            json!({
                "ref": e.id,
                "title": e.title,
                "authors": e.authors,
                "description": e.summary,
                "updated": e.updated,
                "catalogUrl": navigation,
                "acquisitionUrl": acquisition,
                "coverUrl": e.links.iter()
                    .find(|(rel, _, ty)| ty.starts_with("image/") || rel.contains("image"))
                    .map(|(_, href, _)| href.clone()),
            })
        }).collect()
    }
}

// ------------------------------------------------------------------- client

fn basic_auth_header(username: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{username}:{password}")))
}

fn catalog_config(ctx: &Value) -> (String, String, String) {
    let config = ctx.get("config").cloned().unwrap_or(json!({}));
    let url = config.get("url").or_else(|| config.get("baseUrl"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    let username = ctx.get("credentials")
        .and_then(|c| c.get("username")).and_then(Value::as_str).unwrap_or("").to_string();
    let password = ctx.get("credentials")
        .and_then(|c| c.get("password")).and_then(Value::as_str).unwrap_or("").to_string();
    (url, username, password)
}

async fn fetch_feed(state: &AppState, url: &str, username: &str, password: &str) -> ApiResult<Feed> {
    if url.is_empty() {
        return Err(bad_request("catalog url is not configured", "SOURCE_MISCONFIGURED"));
    }
    let mut request = state.http.get(url)
        .header(header::ACCEPT, "application/atom+xml,application/xml;q=0.9,*/*;q=0.8")
        .timeout(std::time::Duration::from_secs(20));
    if !username.is_empty() || !password.is_empty() {
        request = request.header(header::AUTHORIZATION, basic_auth_header(username, password));
    }
    let response = request.send().await
        .map_err(|e| api_err(502, "OPDS_UPSTREAM", format!("catalog request failed: {e}")))?;
    let status = response.status().as_u16();
    if status == 401 {
        return Err(api_err(401, "OPDS_AUTH", "catalog credentials rejected"));
    }
    if !(200..300).contains(&status) {
        return Err(api_err(502, "OPDS_UPSTREAM", format!("catalog responded {status}")));
    }
    let text = response.text().await
        .map_err(|e| api_err(502, "OPDS_UPSTREAM", format!("catalog body unreadable: {e}")))?;
    Ok(parse_feed(&text))
}

fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Some(idx) = base.rfind('/') {
        return format!("{}/{}", &base[..idx], href.trim_start_matches('/'));
    }
    href.to_string()
}

pub async fn client_browse(state: &AppState, _user: &AuthUser, ctx: &Value, params: &Value) -> ApiResult<Value> {
    let (base, username, password) = catalog_config(ctx);
    let url = params.get("ref").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let target = if url.is_empty() { base.clone() } else { resolve_url(&base, &url) };
    let feed = fetch_feed(state, &target, &username, &password).await?;
    let items = feed.to_json();
    let next = items.iter().find_map(|item| item.get("catalogUrl")).and_then(Value::as_str).map(str::to_string);
    Ok(json!({ "items": items, "nextCursor": next, "title": feed.title }))
}

pub async fn client_search(state: &AppState, _user: &AuthUser, params: &Value) -> ApiResult<Value> {
    let config = params.get("config").cloned().unwrap_or(json!({}));
    let url = config.get("url").or_else(|| config.get("baseUrl"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    let username = params.get("credentials")
        .and_then(|c| c.get("username")).and_then(Value::as_str).unwrap_or("").to_string();
    let password = params.get("credentials")
        .and_then(|c| c.get("password")).and_then(Value::as_str).unwrap_or("").to_string();
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    // OPDS search: first fetch the root to discover the search link, then hit it.
    let root = fetch_feed(state, &url, &username, &password).await?;
    let search_link = root.entries.iter().flat_map(|e| e.links.iter())
        .find(|(rel, _, _)| rel == "search")
        .map(|(_, href, _)| href.clone())
        .unwrap_or_else(|| format!("{url}?q={query}"));
    let target = resolve_url(&url, &search_link.replace("{searchTerms}", query));
    let feed = fetch_feed(state, &target, &username, &password).await?;
    let items = feed.to_json();
    Ok(json!({ "items": items, "batch": { "total": items.len(), "completed": items.len() }, "title": feed.title }))
}

pub fn client_detail(_state: &AppState, _user: &AuthUser, ctx: &Value, entry_ref: &str) -> ApiResult<Value> {
    let (base, _, _) = catalog_config(ctx);
    Ok(json!({
        "ref": entry_ref,
        "catalogUrl": resolve_url(&base, entry_ref),
    }))
}

/// Acquiring an OPDS publication downloads the file into the library.
pub async fn client_acquire(state: &AppState, user: &AuthUser, ctx: &Value, entry_ref: &str) -> ApiResult<Value> {
    let (base, username, password) = catalog_config(ctx);
    let url = resolve_url(&base, entry_ref);
    let mut request = state.http.get(&url).timeout(std::time::Duration::from_secs(120));
    if !username.is_empty() || !password.is_empty() {
        request = request.header(header::AUTHORIZATION, basic_auth_header(&username, &password));
    }
    let response = request.send().await
        .map_err(|e| api_err(502, "OPDS_DOWNLOAD", format!("download failed: {e}")))?;
    if !(200..300).contains(&response.status().as_u16()) {
        return Err(api_err(502, "OPDS_DOWNLOAD", format!("download responded {}", response.status())));
    }
    let content_type = response.headers().get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
    let disposition = response.headers().get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = response.bytes().await
        .map_err(|e| api_err(502, "OPDS_DOWNLOAD", format!("download body failed: {e}")))?;

    let filename = filename_from(&disposition, &url, &content_type);
    if !crate::indexer::supported_extension(&filename) {
        return Err(bad_request(format!("unsupported acquisition format: {filename}"), "UNSUPPORTED_FORMAT"));
    }
    let saved = crate::browse::save_upload(state, &filename, "opds", &bytes, "rename")?;
    let _ = user;
    Ok(json!({ "acquisition": saved }))
}

fn filename_from(disposition: &str, url: &str, content_type: &str) -> String {
    for part in disposition.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            return rest.trim_matches('"').to_string();
        }
    }
    let tail = url.split('/').next_back().unwrap_or("book").split('?').next().unwrap_or("book");
    if tail.contains('.') { return tail.to_string(); }
    let ext = match content_type.split(';').next().unwrap_or_default() {
        "application/epub+zip" => "epub",
        "application/pdf" => "pdf",
        "application/vnd.comicbook+zip" => "cbz",
        "text/plain" => "txt",
        _ => "bin",
    };
    format!("download.{ext}")
}

pub fn client_publication_manifest(_state: &AppState, _user: &AuthUser, _ctx: &Value, publication_ref: &str) -> ApiResult<Value> {
    // Chapter-style OPDS publications are not part of OPDS 1.2; the reader
    // treats acquired files as local books instead.
    Err(not_found(
        "OPDS 1.2 catalogs deliver files, not live publications",
        "UNSUPPORTED_OPERATION",
    ))
    .map(|_: Value| json!({ "publicationRef": publication_ref }))
}

pub fn client_publication_resource(_state: &AppState, _user: &AuthUser, _ctx: &Value, _publication_ref: &str, _ref: &str) -> ApiResult<Value> {
    Err(not_found("OPDS 1.2 catalogs deliver files, not live publications", "UNSUPPORTED_OPERATION"))
}

// ------------------------------------------------------------------- server

fn feed_xml(title: &str, id: &str, self_href: &str, entries: &[Value]) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
"#,
    );
    xml.push_str(&format!("  <id>{id}</id>\n"));
    xml.push_str(&format!("  <title>{}</title>\n", crate::indexer::escape_html(title)));
    xml.push_str(&format!("  <updated>{}</updated>\n", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)));
    xml.push_str(&format!("  <link rel=\"self\" href=\"{}\" type=\"application/atom+xml;profile=opds-catalog;kind=navigation\"/>\n", crate::indexer::escape_html(self_href)));
    for e in entries {
        let book_id = e.get("id").and_then(Value::as_str).unwrap_or("");
        let book_title = e.get("title").and_then(Value::as_str).unwrap_or("");
        let author = e.get("author").and_then(Value::as_str).unwrap_or("");
        xml.push_str("  <entry>\n");
        xml.push_str(&format!("    <id>urn:book:{}</id>\n", crate::indexer::escape_html(book_id)));
        xml.push_str(&format!("    <title>{}</title>\n", crate::indexer::escape_html(book_title)));
        if !author.is_empty() {
            xml.push_str(&format!("    <author><name>{}</name></author>\n", crate::indexer::escape_html(author)));
        }
        let content_type = e.get("format").and_then(Value::as_str).unwrap_or("");
        let ext = match content_type { "epub" => ".epub", "pdf" => ".pdf", "cbz" => ".cbz", "txt" => ".txt", _ => ".epub" };
        xml.push_str(&format!(
            "    <link rel=\"http://opds-spec.org/acquisition\" href=\"/opds/books/{}/content\" type=\"application/octet-stream\"/>\n",
            crate::indexer::escape_html(book_id)
        ));
        xml.push_str(&format!(
            "    <link rel=\"http://opds-spec.org/image\" href=\"/opds/books/{}/cover\" type=\"image/jpeg\"/>\n",
            crate::indexer::escape_html(book_id)
        ));
        let _ = ext;
        xml.push_str("  </entry>\n");
    }
    xml.push_str("</feed>\n");
    xml
}

fn catalog_user(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<AuthUser>> {
    let value = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    let token = value.strip_prefix("Basic ").unwrap_or_default();
    if token.is_empty() {
        return Ok(None);
    }
    let decoded = STANDARD.decode(token.trim())
        .map_err(|_| bad_request("invalid basic auth", "BAD_REQUEST"))?;
    let text = String::from_utf8_lossy(&decoded);
    let username = text.split(':').next().unwrap_or("");
    // The password field carries the OPDS catalog token; match its hash.
    let token_hash = crate::auth::hash_refresh_token(text.split_once(':').map(|(_, p)| p).unwrap_or(""));
    let row = state.db.one(
        "SELECT c.id, u.id AS uid, u.role FROM opds_credentials c JOIN users u ON u.id = c.user_id
         WHERE u.username = ?1 COLLATE NOCASE AND c.token_hash = ?2 AND c.expires_at > ?3",
        rusqlite::params![username, token_hash, crate::state::now_ms()],
    )?
    .ok_or_else(|| api_err(401, "OPDS_AUTH", "invalid catalog credentials"))?;
    Ok(Some(AuthUser {
        id: crate::db::s(&row, "uid"),
        role: crate::db::s(&row, "role"),
        ver: 0,
    }))
}

fn unauthorized_response() -> Response {
    let mut response = (
        axum::http::StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Basic realm=\"reader\", charset=\"UTF-8\"")],
    ).into_response();
    let _ = &mut response;
    response
}

pub async fn server_root(
    state: &AppState,
    query: &HashMap<String, String>,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let user = match catalog_user(state, headers)? {
        Some(u) => u,
        None => return Ok(unauthorized_response()),
    };
    let search = query.get("q").cloned().unwrap_or_default();
    let mut list_query = crate::dto::ListQuery::default();
    if !search.is_empty() { list_query.search = Some(search); }
    let result = crate::dto::list_books(&state.db, &user.id, &list_query)?;
    let entries: Vec<Value> = result.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let xml = feed_xml("reader 书架", "urn:reader:shelf", "/opds/", &entries);
    Ok(xml_response(xml))
}

pub async fn server_search_description(state: &AppState, headers: &HeaderMap) -> ApiResult<Response> {
    if catalog_user(state, headers)?.is_none() {
        return Ok(unauthorized_response());
    }
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
  <ShortName>reader</ShortName>
  <InputEncoding>UTF-8</InputEncoding>
  <Url type="application/atom+xml;profile=opds-catalog;kind=navigation" template="/opds/?q={searchTerms}"/>
</OpenSearchDescription>
"#;
    Ok(xml_response(xml.to_string()))
}

pub async fn server_book_content(
    state: &AppState,
    book_id: &str,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let user = catalog_user(state, headers)?.ok_or_else(|| api_err(401, "OPDS_AUTH", "credentials required"))?;
    let row = state.db.one(
        "SELECT rel_path, size, mtime_ms FROM book_files WHERE book_id = ?1 AND missing = 0 ORDER BY rel_path LIMIT 1",
        [book_id],
    )?
    .ok_or_else(|| not_found("book file not found", "NOT_FOUND"))?;
    let _ = user;
    let rel = crate::db::s(&row, "rel_path");
    let abs = browse::resolve_inside(&state.config.books_dir, &rel)?;
    let opts = crate::assets::AssetOpts {
        content_type: "application/octet-stream".into(),
        last_modified: Some(crate::db::i(&row, "mtime_ms")),
        filename: Some(rel.rsplit('/').next().unwrap_or(&rel).to_string()),
        seekable: true,
        ..Default::default()
    };
    Ok(crate::assets::send_asset(headers, crate::assets::AssetPayload::File { path: abs, size: crate::db::i(&row, "size") as u64 }, opts).await)
}

pub async fn server_book_cover(
    state: &AppState,
    book_id: &str,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    if catalog_user(state, headers)?.is_none() {
        return Ok(unauthorized_response());
    }
    let row = state.db.one("SELECT cover_path FROM books WHERE id = ?1", [book_id])?
        .ok_or_else(|| not_found("book not found", "NOT_FOUND"))?;
    let cover = crate::db::s(&row, "cover_path");
    if cover.is_empty() {
        return Err(not_found("no cover", "NO_COVER"));
    }
    let path = crate::indexer::cover_path(&state.config, &cover);
    let data = std::fs::read(&path).map_err(|_| not_found("no cover", "NO_COVER"))?;
    let content_type = mime_guess::from_path(&path).first_or_octet_stream().to_string();
    let opts = crate::assets::AssetOpts { content_type, ..Default::default() };
    Ok(crate::assets::send_asset(headers, crate::assets::AssetPayload::Memory(data), opts).await)
}

fn xml_response(xml: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/atom+xml;profile=opds-catalog;kind=navigation;charset=utf-8")],
        Body::from(xml),
    ).into_response()
}
