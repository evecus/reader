//! `reader plugin-run <dir>` — the QuickJS side of the plugin boundary.
//!
//! The host process speaks newline-delimited JSON-RPC over stdio, exactly the
//! wire format the Node build used. Only the runtime inside this process is
//! different: instead of `node main.mjs`, the plugin's entry file is executed
//! by an embedded QuickJS engine. The host never needs Node installed.
//!
//! Plugin contract (v1, QuickJS):
//!   - The entry script assigns `globalThis.methods = { validateConfig, browse,
//!     search, detail, acquire, getManifest, readResource, ... }`.
//!   - Each method receives the JSON-RPC `params` object and returns a plain
//!     JSON-serialisable value (or throws with `error.code`).
//!   - Host capabilities are injected as `globalThis.host`:
//!     `host.fetchJson(url, optionsJson)`, `host.hash(algo, text)`,
//!     `host.log(message)`, `host.dataDir()`, `host.instanceDataDir()`.
//!
//! ESM npm imports are intentionally unsupported: QuickJS has no npm resolver.
//! A plugin is a self-contained script; host.fetchJson is the way out to the
//! network. This is a real sandbox boundary, unlike the Node process plugin.

use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

use rquickjs::{Context, Function, Object, Runtime};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn run(directory: &Path) -> Result<(), String> {
    let manifest_path = directory.join("plugin.json");
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|e| format!("cannot read plugin.json: {e}"))?,
    )
    .map_err(|e| format!("invalid plugin.json: {e}"))?;

    let api_version = manifest.get("apiVersion").and_then(Value::as_i64).unwrap_or(0);
    if api_version != 1 {
        return Err(format!("unsupported plugin API version: {api_version}"));
    }
    let runtime = manifest.get("runtime").and_then(Value::as_str).unwrap_or("quickjs");
    if runtime != "quickjs" && runtime != "node" {
        return Err(format!("unsupported plugin runtime: {runtime}"));
    }
    let entry = manifest.get("entry").and_then(Value::as_str).unwrap_or("main.mjs");
    if entry.starts_with('/') || entry.contains("..") {
        return Err("plugin entry must be inside its directory".into());
    }
    let entry_path = directory.join(entry);
    let source = std::fs::read_to_string(&entry_path)
        .map_err(|e| format!("cannot read plugin entry {}: {e}", entry_path.display()))?;

    let data_dir: PathBuf = std::env::var("READER_PLUGIN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| directory.join("data"));
    let instance_data_dir: PathBuf = std::env::var("READER_PLUGIN_INSTANCE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.clone());
    let _ = std::fs::create_dir_all(&data_dir);

    let rt = Runtime::new().map_err(|e| format!("quickjs runtime: {e}"))?;
    let ctx = Context::full(&rt).map_err(|e| format!("quickjs context: {e}"))?;

    let bootstrap = r#"
        globalThis.__dispatch = function(method, paramsJson) {
          try {
            const params = JSON.parse(paramsJson);
            const fn = globalThis.methods && globalThis.methods[method];
            if (typeof fn !== 'function') {
              throw Object.assign(new Error('unsupported method: ' + method), { code: 'UNSUPPORTED_OPERATION' });
            }
            const result = fn(params);
            return JSON.stringify({ ok: true, result: result === undefined ? null : result });
          } catch (e) {
            return JSON.stringify({
              ok: false,
              code: (e && e.code) ? String(e.code) : 'PLUGIN_ERROR',
              message: (e && e.message) ? String(e.message) : String(e)
            });
          }
        };
        true;
    "#;

    let load: Result<(), String> = ctx.with(|ctx| -> Result<(), String> {
        inject_host(&ctx, data_dir.clone(), instance_data_dir.clone())
            .map_err(|e| e.to_string())?;
        let console = Object::new(ctx.clone()).map_err(|e| e.to_string())?;
        let log = Function::new(ctx.clone(), |msg: String| {
            let _ = writeln!(io::stderr(), "READER_DIAGNOSTIC {}", json!({ "event": "console", "message": msg }));
        }).map_err(|e| e.to_string())?;
        console.set("log", log).map_err(|e| e.to_string())?;
        ctx.globals().set("console", console).map_err(|e| e.to_string())?;
        // `methods` must exist before the entry runs so a plugin can assign to
        // individual keys instead of replacing the object wholesale.
        let methods = Object::new(ctx.clone()).map_err(|e| e.to_string())?;
        ctx.globals().set("methods", methods).map_err(|e| e.to_string())?;
        ctx.eval::<rquickjs::Value, _>(source.clone())
            .map_err(|e| format!("plugin entry: {e}"))?;
        ctx.eval::<rquickjs::Value, _>(bootstrap.to_string())
            .map_err(|e| format!("plugin bootstrap: {e}"))?;
        Ok(())
    });
    if let Err(e) = &load {
        let _ = writeln!(io::stderr(), "READER_DIAGNOSTIC {}", json!({ "event": "load", "message": e }));
    }
    load?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() { continue; }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // malformed lines never break the RPC channel
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        if method == "$/cancelRequest" { continue; }
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let response = ctx.with(|ctx| -> Result<Value, String> {
            let dispatch: Function = ctx.globals().get("__dispatch")
                .map_err(|e| format!("plugin bootstrap missing: {e}"))?;
            let args = serde_json::to_string(&params).unwrap_or_else(|_| "{}".into());
            let out: String = dispatch.call((method.clone(), args))
                .map_err(|e| format!("{e}"))?;
            let frame: Value = serde_json::from_str(&out)
                .map_err(|e| format!("plugin returned invalid JSON: {e}"))?;
            if frame.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                Ok(frame.get("result").cloned().unwrap_or(Value::Null))
            } else {
                let code = frame.get("code").and_then(Value::as_str).unwrap_or("PLUGIN_ERROR");
                let message = frame.get("message").and_then(Value::as_str).unwrap_or("plugin error");
                Err(format!("{code}|{message}"))
            }
        });

        let frame = match response {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => {
                let (code, msg) = match message.split_once('|') {
                    Some((c, m)) => (c.to_string(), m.to_string()),
                    None => ("PLUGIN_ERROR".into(), message.clone()),
                };
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": clean_message(&msg) } })
            }
        };
        if writeln!(stdout, "{}", serde_json::to_string(&frame).unwrap_or_default()).is_err() { break; }
        let _ = stdout.flush();
    }
    Ok(())
}

fn clean_message(message: &str) -> String {
    message.replace("Error: ", "").chars().take(500).collect()
}

fn inject_host(
    ctx: &rquickjs::Ctx<'_>,
    data_dir: PathBuf,
    instance_data_dir: PathBuf,
) -> Result<(), rquickjs::Error> {
    let host = Object::new(ctx.clone())?;

    // Synchronous HTTP via reqwest::blocking. A plugin is a single-threaded
    // process; a blocking fetch is the honest model here and keeps the script
    // simple (no promises, no microtask pump in the runner).
    let fetch = Function::new(ctx.clone(), |url: String, options: String| -> String {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build();
        let client = match client { Ok(c) => c, Err(e) => return json!({"error": e.to_string()}).to_string() };
        let opts: Value = serde_json::from_str(&options).unwrap_or(json!({}));
        let method = opts.get("method").and_then(Value::as_str).unwrap_or("GET").to_ascii_uppercase();
        let mut req = match method.as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };
        if let Some(headers) = opts.get("headers").and_then(Value::as_object) {
            for (k, v) in headers {
                if let Some(s) = v.as_str() { req = req.header(k, s); }
            }
        }
        if let Some(body) = opts.get("body").and_then(Value::as_str) {
            req = req.body(body.to_string());
        }
        match req.send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let headers: serde_json::Map<String, Value> = resp.headers().iter()
                    .filter_map(|(k, v)| Some((k.to_string(), Value::String(v.to_str().ok()?.to_string()))))
                    .collect();
                let body = resp.text().unwrap_or_default();
                json!({ "status": status, "headers": headers, "body": body }).to_string()
            }
            Err(e) => json!({ "error": e.to_string() }).to_string(),
        }
    })?;
    host.set("fetchJson", fetch)?;

    let hash = Function::new(ctx.clone(), |algo: String, text: String| -> String {
        if algo == "sha256" {
            let mut h = Sha256::new();
            h.update(text.as_bytes());
            let bytes = h.finalize();
            let mut s = String::new();
            for b in bytes { s.push_str(&format!("{b:02x}")); }
            s
        } else {
            String::new()
        }
    })?;
    host.set("hash", hash)?;

    let log = Function::new(ctx.clone(), |msg: String| {
        let _ = writeln!(io::stderr(), "READER_DIAGNOSTIC {}", json!({ "event": "log", "message": msg }));
    })?;
    host.set("log", log)?;

    let dd = data_dir.clone();
    let data_dir_fn = Function::new(ctx.clone(), move || dd.to_string_lossy().into_owned())?;
    host.set("dataDir", data_dir_fn)?;
    let idd = instance_data_dir.clone();
    let instance_fn = Function::new(ctx.clone(), move || idd.to_string_lossy().into_owned())?;
    host.set("instanceDataDir", instance_fn)?;

    // A tiny `storage` namespace, scoped to the plugin's own data directory.
    let storage = Object::new(ctx.clone())?;
    let sd = data_dir.clone();
    let read = Function::new(ctx.clone(), move |key: String| -> String {
        safe_join(&sd, &key)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    })?;
    storage.set("read", read)?;
    let sd2 = data_dir.clone();
    let write = Function::new(ctx.clone(), move |key: String, value: String| -> bool {
        if let Some(p) = safe_join(&sd2, &key) {
            if let Some(parent) = p.parent() { let _ = std::fs::create_dir_all(parent); }
            return std::fs::write(p, value).is_ok();
        }
        false
    })?;
    storage.set("write", write)?;
    host.set("storage", storage)?;

    ctx.globals().set("host", host)?;
    Ok(())
}

fn safe_join(root: &Path, key: &str) -> Option<PathBuf> {
    let candidate = root.join(key.trim_start_matches('/').replace("..", ""));
    if candidate.starts_with(root) { Some(candidate) } else { None }
}
