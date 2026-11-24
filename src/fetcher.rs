//! AnalyzeUrl：URL 变量替换（{{key}}/{{page}}）、请求选项解析（method/body/
//! headers/charset/retry/timeout）、编码处理、网络请求。
//!
//! 主链（async）与 JS java.ajax（blocking）共用同一个 `RequestSpec` 解析，
//! 确保 `url,{"method":"POST",...}` 选项在四条业务链与 java.ajax 语义一致。

use crate::cookie::CookieStore;
use crate::error::{AppError, AppResult};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, REFERER, USER_AGENT};
use serde_json::Value;

static UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 统一的请求规格：由 URL 选项 JSON + 书源 header 合并而来。
#[derive(Clone, Debug)]
pub struct RequestSpec {
    pub url: String,
    pub method: String,
    pub body: String,
    /// 合并后的 headers（书源 header + 选项 headers），JSON object 字符串
    pub headers_json: String,
    /// 响应解码 charset（如 "gbk" / "utf-8"），None 表示自动探测
    pub charset: Option<String>,
    pub retry: u32,
    pub timeout_secs: u64,
    /// URL 选项 {"webView": true}：请求经 headless Chrome 渲染后取源码
    pub webview: bool,
}

impl RequestSpec {
    /// 从 `raw`（可能是 `url,{...}`）解析出请求规格。
    /// `source_headers` 为书源级 header JSON，会被选项 headers 覆盖。
    pub fn parse(raw: &str, source_headers: Option<&str>) -> Self {
        let (url, opts) = split_options(raw);
        let opts: Option<Value> = opts.and_then(|o| serde_json::from_str(&o).ok());

        let mut method = "GET".to_string();
        let mut body = String::new();
        let mut charset = None;
        let mut retry = 0u32;
        let mut timeout_secs = 30u64;
        let mut webview = false;
        let mut extra_headers: Option<Value> = None;

        if let Some(o) = &opts {
            if let Some(m) = o.get("method").and_then(|v| v.as_str()) {
                method = m.to_ascii_uppercase();
            }
            if let Some(b) = o.get("body") {
                body = match b {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
            }
            if let Some(cs) = o.get("charset").and_then(|v| v.as_str()) {
                charset = Some(cs.to_string());
            }
            if let Some(r) = o.get("retry").and_then(|v| v.as_u64()) {
                retry = r.min(10) as u32; // 上限 10，防止恶意配置
            }
            if let Some(t) = o.get("timeout").and_then(|v| v.as_u64()) {
                timeout_secs = t.clamp(1, 300);
            }
            if let Some(w) = o.get("webView") {
                webview = w.as_bool().unwrap_or(false)
                    || w.as_str().map(|s| s.eq_ignore_ascii_case("true")).unwrap_or(false);
            }
            if let Some(h) = o.get("headers") {
                extra_headers = Some(h.clone());
            }
            // jsUrl 常见写法：bodyJs / js 在 URL 脚本阶段已处理，这里仅消费静态字段。
        }

        // 合并 headers：书源 header 为底，选项 headers 覆盖
        let merged = merge_headers(source_headers, extra_headers.as_ref());
        Self { url, method, body, headers_json: merged, charset, retry, timeout_secs, webview }
    }
}

/// 合并书源 header 与选项 header（选项优先），返回 JSON object 字符串
fn merge_headers(source: Option<&str>, extra: Option<&Value>) -> String {
    let mut map = serde_json::Map::new();
    if let Some(s) = source {
        if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(s) {
            for (k, v) in m { map.insert(k, v); }
        }
    }
    if let Some(Value::Object(m)) = extra {
        for (k, v) in m { map.insert(k.clone(), v.clone()); }
    }
    serde_json::to_string(&Value::Object(map)).unwrap_or_else(|_| "{}".into())
}

/// JS 层 `java.connect/get/post/head/ajaxAll` 返回的完整响应。
/// 对齐 legado `StrResponse`：body()/code()/header(k)/headers()/url()。
#[derive(Clone, Debug, serde::Serialize)]
pub struct FetchResponse {
    pub status: u16,
    /// 响应头（key 全小写；同名多值以 "," 合并）
    pub headers: serde_json::Map<String, Value>,
    pub body: String,
    pub url: String,
}

impl FetchResponse {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn error(msg: &str) -> Self {
        let mut m = serde_json::Map::new();
        m.insert("__err".into(), Value::String(msg.to_string()));
        Self { status: 0, headers: m, body: String::new(), url: String::new() }
    }
}

pub struct Fetcher {
    client: reqwest::Client,
    /// 进程级共享 Cookie jar：异步主链与 JS blocking 回调共用
    pub cookies: CookieStore,
}

impl Fetcher {
    pub fn new() -> Self { Self::with_cookies(CookieStore::new()) }

    pub fn with_cookies(cookies: CookieStore) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .gzip(true)
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, cookies }
    }

    /// 替换 URL 中的 {{key}} / {{page}} / {{...}}
    pub fn prepare_url(url: &str, key: Option<&str>, page: Option<i32>) -> String {
        let mut result = url.to_string();
        if let Some(k) = key {
            result = result.replace("{{key}}", &urlencoding_encode(k));
        }
        if let Some(p) = page {
            // {{page}} 或 {{page:1,2,3}}
            let re = Regex::new(r"\{\{page(?::([^}]+))?\}\}").unwrap();
            result = re.replace_all(&result, |caps: &regex::Captures| -> String {
                if let Some(list) = caps.get(1) {
                    let pages: Vec<&str> = list.as_str().split(',').collect();
                    let idx = (p - 1).max(0) as usize;
                    pages.get(idx.min(pages.len().saturating_sub(1))).copied().unwrap_or("1").to_string()
                } else {
                    p.to_string()
                }
            }).to_string();
        }
        // 其他 {{var}} 置空
        let re2 = Regex::new(r"\{\{[a-zA-Z_][^}]*\}\}").unwrap();
        re2.replace_all(&result, "").to_string()
    }

    /// 解析书源 header JSON 为 HeaderMap
    pub fn parse_headers(header_json: Option<&str>, url: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(UA));
        if let Some(h) = header_json {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(h) {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        if let (Ok(name), Ok(val)) = (
                            HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(s),
                        ) {
                            headers.insert(name, val);
                        }
                    }
                }
            }
        }
        // 从 URL 提取 host 作为 Referer（对齐官方：用 URL 的 scheme）
        if !headers.contains_key(REFERER) {
            if let Some((scheme, host)) = extract_origin(url) {
                if let Ok(v) = HeaderValue::from_str(&format!("{}://{}/", scheme, host)) {
                    headers.insert(REFERER, v);
                }
            }
        }
        headers
    }

    /// GET 请求（无选项），返回解码后的 body 与最终 URL。
    pub async fn get(&self, url: &str, headers: Option<&str>) -> AppResult<(String, String)> {
        self.request(url, headers).await
    }

    /// 异步请求：完整消费 RequestSpec（method/body/headers/charset/retry/timeout）。
    /// 这是四条业务链的主请求入口，支持 POST 搜索。
    pub async fn request(&self, raw: &str, source_headers: Option<&str>) -> AppResult<(String, String)> {
        let spec = RequestSpec::parse(raw, source_headers);
        // {"webView": true}：headless Chrome 渲染取源码；失败回退普通请求
        if spec.webview {
            let url = spec.url.clone();
            match tokio::task::spawn_blocking(move || crate::webview::call(1, "", &url, "", "")).await {
                Ok(Ok(source)) => return Ok((source, spec.url.clone())),
                Ok(Err(e)) => {
                    tracing::debug!(target: "fetcher", "webView 渲染失败，回退普通请求: {}", e);
                }
                Err(e) => {
                    tracing::debug!(target: "fetcher", "webView 任务失败，回退普通请求: {}", e);
                }
            }
        }
        let mut hm = Self::parse_headers(Some(&spec.headers_json), &spec.url);
        // 若指定 charset 且未声明 Content-Type，补一个 charset 提示（部分站点据此返回对应编码）
        if let Some(cs) = &spec.charset {
            if !hm.contains_key(CONTENT_TYPE) && !spec.body.is_empty() {
                if let Ok(v) = HeaderValue::from_str(&format!("application/x-www-form-urlencoded; charset={}", cs)) {
                    hm.insert(CONTENT_TYPE, v);
                }
            }
        }
        self.cookies.inject(&mut hm, &spec.url);

        let mut last_err: Option<AppError> = None;
        for attempt in 0..=spec.retry {
            let result = self.send_async(&spec, &hm).await;
            match result {
                Ok((resp_headers, final_url, bytes)) => {
                    self.cookies.store_response(&final_url, &resp_headers);
                    let body = decode_bytes(&bytes, spec.charset.as_deref());
                    return Ok((body, final_url));
                }
                Err(e) => {
                    if attempt < spec.retry { last_err = Some(e); continue; }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AppError::internal("请求失败")))
    }

    async fn send_async(&self, spec: &RequestSpec, hm: &HeaderMap)
        -> AppResult<(HeaderMap, String, Vec<u8>)>
    {
        // 复用 self.client（连接池）；按请求的 timeout 用 tokio::time::timeout 实现。
        let req = if spec.method == "POST" {
            self.client.post(&spec.url).headers(hm.clone()).body(spec.body.clone())
        } else if spec.method == "GET" {
            self.client.get(&spec.url).headers(hm.clone())
        } else {
            self.client.request(spec.method.parse().unwrap_or(reqwest::Method::GET), &spec.url)
                .headers(hm.clone()).body(spec.body.clone())
        };
        let fut = async {
            let resp = req.send().await.map_err(|e| AppError::internal(format!("请求 {} 失败: {}", spec.url, e)))?;
            let final_url = resp.url().to_string();
            let headers = resp.headers().clone();
            let bytes = resp.bytes().await.map_err(|e| AppError::internal(format!("读取响应失败: {}", e)))?;
            Ok((headers, final_url, bytes.to_vec()))
        };
        match tokio::time::timeout(std::time::Duration::from_secs(spec.timeout_secs), fut).await {
            Ok(r) => r,
            Err(_) => Err(AppError::timeout(format!("请求超时: {}", spec.url))),
        }
    }

    /// 通用同步请求：注入/回写共享 Cookie jar。供 JS java.ajax / fetch 闭包使用。
    /// 完整消费 RequestSpec（method/body/charset/headers），但 retry 固定 0（JS 回调场景）。
    pub fn request_blocking(&self, raw: &str, source_headers: Option<&str>) -> String {
        self.request_blocking_full(raw, source_headers, None)
            .map(|r| r.body)
            .unwrap_or_default()
    }

    /// 同步请求的完整响应版：供 java.connect/get/post/head/ajaxAll 使用。
    /// `timeout_override_ms`：JS 显式传入的 callTimeout（毫秒，对齐 okhttp callTimeout 语义）。
    pub fn request_blocking_full(
        &self,
        raw: &str,
        source_headers: Option<&str>,
        timeout_override_ms: Option<u64>,
    ) -> Result<FetchResponse, String> {
        let spec = RequestSpec::parse(raw, source_headers);
        let timeout_ms = timeout_override_ms.unwrap_or(spec.timeout_secs * 1000).clamp(1, 300_000);
        // {"webView": true}：headless Chrome 渲染取源码；失败回退普通请求
        if spec.webview {
            match crate::webview::call(1, "", &spec.url, "", "") {
                Ok(source) => {
                    let mut headers = serde_json::Map::new();
                    headers.insert("content-type".into(), Value::String("text/html".into()));
                    return Ok(FetchResponse { status: 200, headers, body: source, url: spec.url.clone() });
                }
                Err(e) => {
                    tracing::debug!(target: "fetcher", "webView 渲染失败，回退普通请求: {}", e);
                }
            }
        }
        let mut hm = Self::parse_headers(Some(&spec.headers_json), &spec.url);
        self.cookies.inject(&mut hm, &spec.url);
        let client = match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .danger_accept_invalid_certs(true).build()
        {
            Ok(c) => c,
            Err(e) => return Err(format!("HTTP 客户端构建失败: {e}")),
        };
        let req = if spec.method == "POST" {
            client.post(&spec.url).headers(hm).body(spec.body.clone())
        } else if spec.method == "GET" {
            client.get(&spec.url).headers(hm)
        } else {
            client.request(spec.method.parse().unwrap_or(reqwest::Method::GET), &spec.url)
                .headers(hm).body(spec.body.clone())
        };
        match req.send() {
            Ok(r) => {
                let status = r.status().as_u16();
                let final_url = r.url().to_string();
                let raw_headers = r.headers().clone();
                self.cookies.store_response(&final_url, &raw_headers);
                // 响应头 → JSON map（小写 key，多值合并）
                let mut headers = serde_json::Map::new();
                for (k, v) in raw_headers.iter() {
                    let val = v.to_str().unwrap_or("").to_string();
                    let entry = headers
                        .entry(k.as_str().to_ascii_lowercase())
                        .or_insert_with(|| Value::String(val.clone()));
                    if let Value::String(s) = entry {
                        if *s != val { *s = format!("{},{}", s, val); }
                    }
                }
                let body = r.bytes().ok()
                    .map(|b| decode_bytes(&b, spec.charset.as_deref()))
                    .unwrap_or_default();
                Ok(FetchResponse { status, headers, body, url: final_url })
            }
            Err(e) => Err(format!("请求 {} 失败: {}", spec.url, e)),
        }
    }
}

/// 拆分 `url,{"...":...}` → (url, Some(options_json))
fn split_options(raw: &str) -> (String, Option<String>) {
    let s = raw.trim();
    if let Some(idx) = s.rfind(",{") {
        let candidate = &s[idx + 1..];
        if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
            return (s[..idx].trim().to_string(), Some(candidate.to_string()));
        }
    }
    (s.to_string(), None)
}

/// 编码检测：优先显式 charset，其次 meta charset，最后 UTF-8→GBK 兜底
pub fn decode_bytes(bytes: &[u8], charset: Option<&str>) -> String {
    if let Some(cs) = charset {
        if let Some(enc) = encoding_rs::Encoding::for_label(cs.as_bytes()) {
            let (cow, _, _) = enc.decode(bytes);
            return cow.into_owned();
        }
    }
    // 先查 meta charset（含 gb18030 / big5）
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    let lower = head.to_ascii_lowercase();
        let hinted = ["gb2312", "gbk", "gb18030", "big5", "utf-16", "iso-8859-1", "windows-1252"]
        .iter().find(|cs| lower.contains(&format!("charset=\"{}\"", cs)) || lower.contains(&format!("charset={}", cs)));
    if let Some(cs) = hinted {
        if let Some(enc) = encoding_rs::Encoding::for_label(cs.as_bytes()) {
            let (cow, _, _) = enc.decode(bytes);
            return cow.into_owned();
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (cow, _, _) = encoding_rs::GBK.decode(bytes);
            cow.into_owned()
        }
    }
}

fn extract_origin(url: &str) -> Option<(String, String)> {
    if let Some(pos) = url.find("://") {
        let scheme = &url[..pos];
        let rest = &url[pos + 3..];
        let host = rest.split('/').next()?;
        if !host.is_empty() { return Some((scheme.to_string(), host.to_string())); }
    }
    None
}

fn urlencoding_encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_post_options() {
        let spec = RequestSpec::parse(
            r#"https://x.com/search,{"method":"POST","body":"q=a","charset":"gbk","retry":3,"timeout":9,"headers":{"X-Token":"t"}}"#,
            Some(r#"{"User-Agent":"UA","X-Old":"o"}"#),
        );
        assert_eq!(spec.url, "https://x.com/search");
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.body, "q=a");
        assert_eq!(spec.charset.as_deref(), Some("gbk"));
        assert_eq!(spec.retry, 3);
        assert_eq!(spec.timeout_secs, 9);
        let h: Value = serde_json::from_str(&spec.headers_json).unwrap();
        assert_eq!(h["X-Token"], "t");
        assert_eq!(h["X-Old"], "o"); // 选项 headers 覆盖但保留其它
    }

    #[test]
    fn parse_get_default() {
        let spec = RequestSpec::parse("https://x.com/a?b=c", None);
        assert_eq!(spec.method, "GET");
        assert!(spec.body.is_empty());
        assert_eq!(spec.retry, 0);
        assert_eq!(spec.timeout_secs, 30);
    }

    #[test]
    fn charset_decode() {
        // "中文" 的 GBK 编码
        let gbk = encoding_rs::GBK.encode("中文").0;
        assert_eq!(decode_bytes(&gbk, Some("gbk")), "中文");
        assert_eq!(decode_bytes(&gbk, Some("gb18030")), "中文");
        assert_eq!(decode_bytes("中文".as_bytes(), Some("utf-8")), "中文");
    }

    #[test]
    fn retry_clamped() {
        let spec = RequestSpec::parse(r#"https://x.com,{"retry":999,"timeout":9999}"#, None);
        assert_eq!(spec.retry, 10);
        assert_eq!(spec.timeout_secs, 300);
    }

    #[test]
    fn parse_webview_option() {
        assert!(RequestSpec::parse(r#"https://x.com,{"webView":true}"#, None).webview);
        assert!(RequestSpec::parse(r#"https://x.com,{"webView":"true"}"#, None).webview);
        assert!(!RequestSpec::parse(r#"https://x.com,{"webView":false}"#, None).webview);
        assert!(!RequestSpec::parse("https://x.com", None).webview);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// 起一个极简 HTTP server：回显 method/body/收到的 Cookie，并 Set-Cookie。
    fn spawn_echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let mut s = stream.unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let first = req.lines().next().unwrap_or("");
                let cookie = req.lines().find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                    .unwrap_or("").to_string();
                let body_start = req.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
                let sent_body = &req[body_start..];
                // body 里带 method / cookie / 回显内容
                let payload = format!("{}|{}|{}", first, cookie.trim(), sent_body.trim_end_matches('\0'));
                let _ = s.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nSet-Cookie: token=abc123; Path=/\r\nContent-Length: {}\r\n\r\n{}",
                    payload.as_bytes().len(), payload
                ).as_bytes());
                let _ = s.flush();
            }
        });
        port
    }

    #[test]
    fn blocking_post_and_cookie_jar() {
        let port = spawn_echo_server();
        let fetcher = Fetcher::new();
        let url = format!("http://127.0.0.1:{}/search", port);
        // 第一次 POST：带 body 与自定义 header
        let raw = format!(r#"{},{{"method":"POST","body":"q=hello","headers":{{"X-Test":"yes"}}}}"#, url);
        let out = fetcher.request_blocking(&raw, None);
        assert!(out.contains("POST"), "应发出 POST: {}", out);
        assert!(out.contains("q=hello"), "应携带 body: {}", out);
        // 服务端 Set-Cookie 已回写；第二次 GET 应带上 Cookie
        let out2 = fetcher.request_blocking(&url, None);
        assert!(out2.contains("token=abc123"), "第二次请求应带 Cookie: {}", out2);
    }
}
