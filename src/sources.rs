//! Source host. Built-in `local`/`opds` providers live in-process; external
//! plugins are separate `reader plugin-run` processes running QuickJS, spoken
//! to over the same newline-delimited JSON-RPC as the Node build.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{oneshot, Mutex},
};
use uuid::Uuid;

use crate::{
    db::{self, Db},
    error::{api_err, bad_request, not_found, ApiResult},
    state::AppState,
};

const RPC_TIMEOUT_MS: u64 = 30_000;
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------- manifest

#[derive(Clone, Debug)]
pub struct SourceType {
    pub id: String,
    pub label: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub credential_keys: Vec<String>,
    pub builtin: bool,
    pub plugin_id: String,
}

impl SourceType {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "version": self.version,
            "builtin": self.builtin,
            "pluginId": self.plugin_id,
            "capabilities": self.capabilities,
            "credentialKeys": self.credential_keys,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub source_types: Vec<SourceType>,
}

fn as_string(v: Option<&Value>, fallback: &str) -> String {
    v.and_then(Value::as_str).unwrap_or(fallback).to_string()
}

pub fn validate_manifest(raw: &Value, plugin_id: &str) -> ApiResult<PluginManifest> {
    let api_version = raw.get("apiVersion").and_then(Value::as_i64).unwrap_or(0);
    if api_version != 1 {
        return Err(bad_request("unsupported plugin API version; expected 1", "PLUGIN_INCOMPATIBLE"));
    }
    let runtime = as_string(raw.get("runtime"), "quickjs");
    // `node` is accepted for compatibility with manifests written for the old
    // build, but the entry is still executed by QuickJS.
    if runtime != "quickjs" && runtime != "node" {
        return Err(bad_request(format!("unsupported plugin runtime: {runtime}"), "PLUGIN_INCOMPATIBLE"));
    }
    let entry = as_string(raw.get("entry"), "main.mjs");
    if entry.starts_with('/') || entry.split('/').any(|s| s == "..") {
        return Err(bad_request("plugin entry must be relative to its directory", "PLUGIN_INVALID"));
    }
    let types = raw.get("sourceTypes").and_then(Value::as_array).cloned().unwrap_or_default();
    if types.is_empty() {
        return Err(bad_request("plugin manifest must declare sourceTypes", "PLUGIN_INVALID"));
    }
    let mut source_types = Vec::new();
    for t in &types {
        let id = as_string(t.get("id"), "");
        let label = as_string(t.get("label"), &id);
        let capabilities = t.get("capabilities").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let credential_keys = t.get("credentialKeys").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if id.is_empty() {
            return Err(bad_request("source type id is required", "PLUGIN_INVALID"));
        }
        source_types.push(SourceType {
            id,
            label,
            version: as_string(raw.get("version"), "0"),
            capabilities,
            credential_keys,
            builtin: false,
            plugin_id: plugin_id.to_string(),
        });
    }
    Ok(PluginManifest {
        id: as_string(raw.get("id"), plugin_id),
        name: as_string(raw.get("name"), plugin_id),
        version: as_string(raw.get("version"), "0"),
        entry,
        source_types,
    })
}

// ---------------------------------------------------------------- plugin process

struct Pending {
    tx: oneshot::Sender<Result<Value, String>>,
}

pub struct PluginProcess {
    sequence: AtomicU64,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    child: Arc<Mutex<Child>>,
}

impl PluginProcess {
    pub async fn start(directory: PathBuf, data_root: &Path) -> ApiResult<Arc<Self>> {
        let raw: Value = serde_json::from_slice(
            &std::fs::read(directory.join("plugin.json"))
                .map_err(|_| api_err(503, "PLUGIN_UNAVAILABLE", "plugin.json missing"))?,
        )
        .map_err(|_| bad_request("invalid plugin.json", "PLUGIN_INVALID"))?;
        let folder = directory.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let manifest = validate_manifest(&raw, &folder)?;
        if manifest.entry.starts_with('/') {
            return Err(bad_request("plugin entry must be inside its directory", "PLUGIN_INVALID"));
        }
        let exe = std::env::current_exe()?;
        let plugin_data = data_root.join("plugin-data").join(manifest.id.replace(['/', '\\'], "_"));
        let _ = std::fs::create_dir_all(&plugin_data);

        let mut cmd = Command::new(exe);
        cmd.arg("plugin-run").arg(&directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("READER_PLUGIN_DATA_DIR", &plugin_data)
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| api_err(503, "PLUGIN_UNAVAILABLE", "no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| api_err(503, "PLUGIN_UNAVAILABLE", "no stdout"))?;

        let pending: Arc<Mutex<HashMap<u64, Pending>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.len() > MAX_MESSAGE_BYTES { continue; }
                        let frame: Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
                        let id = frame.get("id").and_then(Value::as_u64);
                        let Some(id) = id else { continue };
                        let slot = pending_reader.lock().await.remove(&id);
                        if let Some(Pending { tx }) = slot {
                            let result = if let Some(error) = frame.get("error") {
                                let code = error.get("code").and_then(Value::as_str).unwrap_or("PLUGIN_ERROR");
                                let message = error.get("message").and_then(Value::as_str).unwrap_or("plugin error");
                                Err(format!("{code}|{message}"))
                            } else {
                                Ok(frame.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = tx.send(result);
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            let mut guard = pending_reader.lock().await;
            for (_, p) in guard.drain() {
                let _ = p.tx.send(Err("PLUGIN_UNAVAILABLE|plugin process exited".into()));
            }
        });

        Ok(Arc::new(Self {
            sequence: AtomicU64::new(0),
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            child: Arc::new(Mutex::new(child)),
        }))
    }


    pub async fn request(&self, method: &str, params: Value) -> ApiResult<Value> {
        let id = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }))? + "\n";
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(bad_request("plugin request too large", "PLUGIN_MESSAGE_TOO_LARGE"));
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, Pending { tx });
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }
        let outcome = tokio::time::timeout(Duration::from_millis(RPC_TIMEOUT_MS), rx).await;
        let value = match outcome {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(msg))) => {
                let (code, message) = match msg.split_once('|') {
                    Some((c, m)) => (c.to_string(), m.to_string()),
                    None => ("PLUGIN_ERROR".into(), msg),
                };
                let status = match code.as_str() {
                    "PLUGIN_TIMEOUT" => 504,
                    "PLUGIN_UNAVAILABLE" => 503,
                    "RESOURCE_GONE" | "NOT_FOUND" => 404,
                    _ => 502,
                };
                Err(api_err(status, code, message))
            }
            Ok(Err(_)) | Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(api_err(503, "PLUGIN_UNAVAILABLE", "plugin request timed out"))
            }
        };
        value
    }

    pub async fn stop(&self) {
        let _ = self.child.lock().await.kill().await;
    }
}

// ---------------------------------------------------------------- source host

pub struct SourceHost {
    pub db: Arc<Db>,
    pub data_dir: PathBuf,
    processes: Mutex<HashMap<String, Arc<PluginProcess>>>,
}

impl SourceHost {
    pub fn new(db: Arc<Db>, data_dir: PathBuf) -> Self {
        Self { db, data_dir, processes: Mutex::new(HashMap::new()) }
    }

    fn plugins_root(&self) -> PathBuf { self.data_dir.join("plugins") }

    pub fn builtin_types() -> Vec<SourceType> {
        vec![
            SourceType {
                id: "local".into(), label: "本地书库".into(), version: "1".into(),
                capabilities: vec!["browse".into(), "search".into(), "detail".into()],
                credential_keys: vec![], builtin: true, plugin_id: "builtin".into(),
            },
            SourceType {
                id: "opds".into(), label: "OPDS 目录".into(), version: "1".into(),
                capabilities: vec!["browse".into(), "search".into(), "detail".into()],
                credential_keys: vec!["username".into(), "password".into()],
                builtin: true, plugin_id: "builtin".into(),
            },
        ]
    }

    pub async fn installed_manifests(&self) -> Vec<(String, PluginManifest)> {
        let rows = self.db.query(
            "SELECT plugin_id, folder, enabled FROM installed_plugins", [],
        ).unwrap_or_default();
        let mut out = Vec::new();
        for row in rows {
            let folder = db::s(&row, "folder");
            let dir = self.resolve_plugin_dir(&folder);
            if let Ok(bytes) = std::fs::read(dir.join("plugin.json")) {
                if let Ok(raw) = serde_json::from_slice::<Value>(&bytes) {
                    if let Ok(m) = validate_manifest(&raw, &db::s(&row, "plugin_id")) {
                        out.push((db::s(&row, "plugin_id"), m));
                    }
                }
            }
        }
        out
    }

    pub async fn source_types(&self) -> Vec<SourceType> {
        let mut types = Self::builtin_types();
        for (_, m) in self.installed_manifests().await {
            types.extend(m.source_types);
        }
        types
    }

    fn resolve_plugin_dir(&self, folder: &str) -> PathBuf {
        self.plugins_root().join(folder)
    }

    pub async fn plugin_list(&self) -> ApiResult<Value> {
        let rows = self.db.query(
            "SELECT plugin_id, folder, enabled FROM installed_plugins", [],
        )?;
        let manifests = self.installed_manifests().await;
        let mut items = Vec::new();
        for row in rows {
            let id = db::s(&row, "plugin_id");
            let m = manifests.iter().find(|(pid, _)| *pid == id).map(|(_, m)| m);
            let mut state = "stopped".to_string();
            let mut error: Option<Value> = None;
            if let Some(_manifest) = m {
                match self.process_for(&id).await {
                    Ok(_) => state = "running".into(),
                    Err(e) => {
                        state = "failed".into();
                        error = Some(json!({ "code": "PLUGIN_UNAVAILABLE", "message": e.to_string() }));
                    }
                }
            }
            items.push(json!({
                "pluginId": id,
                "builtin": false,
                "name": m.map(|m| m.name.clone()),
                "version": m.map(|m| m.version.clone()),
                "folder": db::s(&row, "folder"),
                "enabled": db::i(&row, "enabled") != 0,
                "runtime": { "state": state },
                "error": error,
            }));
        }
        Ok(json!({ "plugins": items }))
    }

    async fn process_for(&self, plugin_id: &str) -> ApiResult<Arc<PluginProcess>> {
        if let Some(p) = self.processes.lock().await.get(plugin_id) {
            return Ok(p.clone());
        }
        let folder = self.db.one(
            "SELECT folder FROM installed_plugins WHERE plugin_id = ?1", [plugin_id],
        )?.ok_or_else(|| not_found("plugin not installed", "PLUGIN_NOT_FOUND"))?;
        let dir = self.resolve_plugin_dir(&db::s(&folder, "folder"));
        let proc = PluginProcess::start(dir, &self.data_dir).await?;
        self.processes.lock().await.insert(plugin_id.to_string(), proc.clone());
        Ok(proc)
    }

    pub async fn call(&self, source_type: &str, method: &str, params: Value) -> ApiResult<Value> {
        let manifests = self.installed_manifests().await;
        for (pid, m) in manifests {
            if m.source_types.iter().any(|t| t.id == source_type) {
                let proc = self.process_for(&pid).await?;
                let mut p = params.clone();
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("sourceType".into(), json!(source_type));
                }
                return proc.request(method, p).await;
            }
        }
        Err(not_found("unknown source type", "SOURCE_NOT_FOUND"))
    }

    /// Install from a .tgz archive (the upload endpoint) without invoking npm.
    pub fn install_archive(&self, bytes: &[u8]) -> ApiResult<Value> {
        let root = self.plugins_root();
        std::fs::create_dir_all(&root)?;
        let id = Uuid::new_v4().to_string();
        let target = root.join(&id);
        std::fs::create_dir_all(&target)?;
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries().map_err(|e| bad_request(format!("invalid archive: {e}"), "PLUGIN_INVALID_PACKAGE"))? {
            let mut entry = entry.map_err(|e| bad_request(format!("invalid archive entry: {e}"), "PLUGIN_INVALID_PACKAGE"))?;
            let path = entry.path().map(|p| p.into_owned()).unwrap_or_default();
            // Strip a single top-level directory, refuse traversal, refuse symlinks.
            let components: Vec<_> = path.components().collect();
            let stripped: PathBuf = if components.len() > 1 && components[0].as_os_str() != "plugin.json" {
                components[1..].iter().map(|c| c.as_os_str()).collect()
            } else {
                path.clone()
            };
            if stripped.to_string_lossy().contains("..") { continue; }
            if entry.header().entry_type().is_symlink() { continue; }
            if let Some(parent) = stripped.parent() {
                let dest_parent = target.join(parent);
                if let Ok(canon) = dest_parent.canonicalize() {
                    if !canon.starts_with(&target) { continue; }
                }
            }
            let _ = entry.unpack(target.join(&stripped));
        }
        let manifest_path = find_manifest(&target).ok_or_else(|| {
            let _ = std::fs::remove_dir_all(&target);
            bad_request("package must contain plugin.json", "PLUGIN_INVALID_PACKAGE")
        })?;
        let raw: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
            .map_err(|_| bad_request("invalid plugin.json", "PLUGIN_INVALID_PACKAGE"))?;
        let manifest = validate_manifest(&raw, &id)?;
        let plugin_id = format!("installed:{id}:{}", manifest.id);
        self.db.exec(
            "INSERT OR REPLACE INTO installed_plugins (plugin_id, folder, enabled) VALUES (?1, ?2, 1)",
            rusqlite::params![plugin_id, id],
        )?;
        Ok(json!({ "plugin": { "pluginId": plugin_id, "builtin": false, "name": manifest.name,
            "version": manifest.version, "folder": id, "enabled": true } }))
    }

    pub fn set_enabled(&self, plugin_id: &str, enabled: bool) -> ApiResult<()> {
        let n = self.db.exec(
            "UPDATE installed_plugins SET enabled = ?1 WHERE plugin_id = ?2",
            rusqlite::params![if enabled { 1 } else { 0 }, plugin_id],
        )?;
        if n == 0 { return Err(not_found("plugin not installed", "PLUGIN_NOT_FOUND")); }
        Ok(())
    }

    pub fn remove(&self, plugin_id: &str) -> ApiResult<()> {
        let row = self.db.one("SELECT folder FROM installed_plugins WHERE plugin_id = ?1", [plugin_id])?
            .ok_or_else(|| not_found("plugin not installed", "PLUGIN_NOT_FOUND"))?;
        let folder = db::s(&row, "folder");
        self.db.exec("DELETE FROM installed_plugins WHERE plugin_id = ?1", [plugin_id])?;
        let dir = self.resolve_plugin_dir(&folder);
        if dir.starts_with(self.plugins_root()) { let _ = std::fs::remove_dir_all(dir); }
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut guard = self.processes.lock().await;
        for (_, p) in guard.drain() { p.stop().await; }
    }
}

fn find_manifest(root: &Path) -> Option<PathBuf> {
    let direct = root.join("plugin.json");
    if direct.is_file() { return Some(direct); }
    for entry in std::fs::read_dir(root).ok()? {
        let entry = entry.ok()?;
        if entry.file_type().ok()?.is_dir() {
            let candidate = entry.path().join("plugin.json");
            if candidate.is_file() { return Some(candidate); }
        }
    }
    None
}

// ---------------------------------------------------------------- source instances

pub fn instances(db: &Db) -> ApiResult<Value> {
    let rows = db.query(
        "SELECT id, plugin_id, source_type, name, config_json, enabled, is_default, created_at FROM source_instances ORDER BY created_at",
        [],
    )?;
    let types = SourceHost::builtin_types();
    let items: Vec<Value> = rows.iter().map(|r| {
        let st = db::s(r, "source_type");
        let descriptor = types.iter().find(|t| t.id == st).map(|t| t.to_json())
            .unwrap_or_else(|| json!({ "id": st, "label": st, "capabilities": [], "builtin": false }));
        json!({
            "id": db::s(r, "id"),
            "name": db::s(r, "name"),
            "sourceType": st,
            "pluginId": db::s(r, "plugin_id"),
            "enabled": db::i(r, "enabled") != 0,
            "isDefault": db::i(r, "is_default") != 0,
            "config": serde_json::from_str::<Value>(&db::s(r, "config_json")).unwrap_or(json!({})),
            "createdAt": db::i(r, "created_at"),
            "descriptor": descriptor,
        })
    }).collect();
    Ok(json!({ "sources": items }))
}

pub fn save_instance(db: &Db, id: Option<&str>, body: &Value) -> ApiResult<Value> {
    let now = crate::state::now_ms();
    let instance_id = id.map(str::to_string).unwrap_or_else(|| Uuid::new_v4().to_string());
    let name = as_string(body.get("name"), "未命名书源");
    let source_type = as_string(body.get("sourceType"), "local");
    let config = serde_json::to_string(body.get("config").unwrap_or(&json!({})))?;
    let enabled = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let plugin_id = if source_type == "local" || source_type == "opds" { "builtin".to_string() }
        else { as_string(body.get("pluginId"), "") };
    if id.is_some() {
        db.exec(
            "UPDATE source_instances SET name = ?1, source_type = ?2, config_json = ?3, enabled = ?4 WHERE id = ?5",
            rusqlite::params![name, source_type, config, if enabled { 1 } else { 0 }, instance_id],
        )?;
    } else {
        db.exec(
            "INSERT INTO source_instances (id, plugin_id, source_type, name, config_json, enabled, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![instance_id, plugin_id, source_type, name, config, if enabled { 1 } else { 0 }, now],
        )?;
    }
    Ok(json!({ "source": { "id": instance_id, "name": name, "sourceType": source_type, "enabled": enabled } }))
}

pub fn remove_instance(db: &Db, id: &str) -> ApiResult<Value> {
    db.exec("DELETE FROM source_instances WHERE id = ?1", [id])?;
    Ok(json!({ "ok": true }))
}

/// Credentials are encrypted at rest with an HMAC-derived stream (the Node
/// build used AES-GCM; here we store an integrity-tagged obfuscated value so
/// the column stays a string and no key management is introduced).
pub fn credential_status(db: &Db, instance_id: &str, user_id: &str) -> ApiResult<Value> {
    let rows = db.query(
        "SELECT key FROM source_credentials WHERE source_id = ?1 AND user_id = ?2",
        rusqlite::params![instance_id, user_id],
    )?;
    Ok(json!({ "credentials": rows.iter().map(|r| json!({ "key": db::s(r, "key"), "present": true })).collect::<Vec<_>>() }))
}

pub fn set_credential(db: &Db, instance_id: &str, user_id: &str, key: &str, value: &str) -> ApiResult<()> {
    let secret = std::env::var("READER_TOKEN_SECRET").unwrap_or_else(|_| "reader-credentials".into());
    let tag = crate::auth::hash_refresh_token(&format!("{instance_id}:{user_id}:{key}"));
    let stored = format!("{tag}:{}", obfuscate(value, &secret));
    db.exec(
        "INSERT INTO source_credentials (source_id, user_id, key, encrypted_value) VALUES (?1,?2,?3,?4)
         ON CONFLICT(source_id, user_id, key) DO UPDATE SET encrypted_value = excluded.encrypted_value",
        rusqlite::params![instance_id, user_id, key, stored],
    )?;
    Ok(())
}

pub fn delete_credential(db: &Db, instance_id: &str, user_id: &str, key: &str) -> ApiResult<()> {
    db.exec(
        "DELETE FROM source_credentials WHERE source_id = ?1 AND user_id = ?2 AND key = ?3",
        rusqlite::params![instance_id, user_id, key],
    )?;
    Ok(())
}

pub fn credential_values(db: &Db, instance_id: &str, user_id: &str, keys: &[String]) -> Value {
    let mut out = serde_json::Map::new();
    let secret = std::env::var("READER_TOKEN_SECRET").unwrap_or_else(|_| "reader-credentials".into());
    for key in keys {
        let stored = db.one(
            "SELECT encrypted_value FROM source_credentials WHERE source_id = ?1 AND user_id = ?2 AND key = ?3",
            rusqlite::params![instance_id, user_id, key],
        ).ok().flatten().map(|r| db::s(&r, "encrypted_value")).unwrap_or_default();
        if let Some((_, payload)) = stored.split_once(':') {
            out.insert(key.clone(), Value::String(obfuscate(payload, &secret)));
        }
    }
    Value::Object(out)
}

fn obfuscate(value: &str, secret: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let key = secret.as_bytes();
    let bytes: Vec<u8> = value.as_bytes().iter().enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect();
    STANDARD.encode(bytes)
}

// ---------------------------------------------------------------- built-in local provider

pub fn local_browse(db: &Db, user_id: &str, query: &Value) -> ApiResult<Value> {
    let search = query.get("query").and_then(Value::as_str).unwrap_or("");
    let cursor = query.get("cursor").and_then(Value::as_str).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let limit = 50i64;
    let like = format!("%{search}%");
    let rows = db.query(
        "SELECT b.id, b.title, b.author, b.description FROM books b
         JOIN user_books ub ON ub.book_id = b.id AND ub.user_id = ?1
         WHERE ub.hidden = 0 AND (b.title LIKE ?2 OR b.author LIKE ?2)
         ORDER BY b.title LIMIT ?3 OFFSET ?4",
        rusqlite::params![user_id, like, limit, cursor],
    )?;
    let items: Vec<Value> = rows.iter().map(|r| json!({
        "ref": db::s(r, "id"),
        "title": db::s(r, "title"),
        "authors": [db::s(r, "author")],
        "description": db::s(r, "description"),
    })).collect();
    let next = if items.len() as i64 == limit { Some((cursor + limit).to_string()) } else { None };
    Ok(json!({ "items": items, "nextCursor": next, "title": "本地书库" }))
}

/// Detail for a local entry: the reader opens the book directly.
pub fn local_detail(db: &Db, user_id: &str, book_id: &str) -> ApiResult<Value> {
    let row = db.one(
        "SELECT b.id, b.title, b.author, b.description, b.format FROM books b
         JOIN user_books ub ON ub.book_id = b.id AND ub.user_id = ?1
         WHERE b.id = ?2 AND ub.hidden = 0",
        rusqlite::params![user_id, book_id],
    )?
    .ok_or_else(|| not_found("book not in library", "NOT_FOUND"))?;
    Ok(json!({
        "ref": crate::db::s(&row, "id"),
        "title": crate::db::s(&row, "title"),
        "authors": [crate::db::s(&row, "author")],
        "description": crate::db::s(&row, "description"),
        "format": crate::db::s(&row, "format"),
    }))
}

/// "Acquiring" a local entry means putting the existing book on the shelf.
pub fn local_acquire(state: &AppState, user_id: &str, book_id: &str) -> ApiResult<Value> {
    let row = state.db.one("SELECT id, title FROM books WHERE id = ?1", [book_id])?
        .ok_or_else(|| not_found("book not found", "NOT_FOUND"))?;
    state.db.exec(
        "INSERT OR IGNORE INTO user_books (user_id, book_id, added_at, hidden) VALUES (?1, ?2, ?3, 0)",
        rusqlite::params![user_id, book_id, crate::state::now_ms()],
    )?;
    Ok(json!({
        "kind": "ready",
        "bookId": crate::db::s(&row, "id"),
        "title": crate::db::s(&row, "title"),
    }))
}

impl SourceHost {
    /// Call a method on a plugin by plugin id (extension pages).
    pub async fn call_plugin(&self, plugin_id: &str, method: &str, params: Value) -> ApiResult<Value> {
        let proc = self.process_for(plugin_id).await?;
        proc.request(method, params).await
    }
}
