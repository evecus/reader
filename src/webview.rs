//! WebView 渲染引擎：chromiumoxide + 系统 Chrome（headless）。
//!
//! 对齐 legado `BackstageWebView` / `JsExtensions`：
//! - webView(html, url, js)：加载页面后执行 js 取结果；js 为空返回整页源码
//! - webViewGetSource(html, url, js, sourceRegex)：嗅探加载的请求 URL（资源/文档），
//!   全串匹配 sourceRegex 时返回该 URL（官方 SnifferWebClient onLoadResource 语义）
//! - webViewGetOverrideUrl(html, url, js, overrideUrlRegex)：嗅探主框架跳转，
//!   匹配时返回跳转 URL（官方 shouldOverrideUrlLoading 语义）
//! - regex 匹配对齐 Kotlin `Regex.matches()`：全串匹配（自动包裹 ^(?:...)$ ）
//! - AnalyzeUrl 选项 {"webView": true}：请求经浏览器渲染后取源码（fetcher.rs 路由）
//!
//! 无浏览器环境优雅降级：call 返回 Err，上层回退普通请求或返回空串；不 panic。
//! 环境变量：READER_CHROME_PATH 指定浏览器路径；READER_WEBVIEW_TIMEOUT 覆写超时秒数。

use chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent;
use chromiumoxide::cdp::browser_protocol::page::{EventFrameNavigated, NavigateParams};
use chromiumoxide::cdp::browser_protocol::target::CreateTargetParams;
use chromiumoxide::listeners::EventStream;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use regex::Regex;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// 模式：0=webView（js 结果/整页源码）1=webViewGetSource（请求嗅探）2=webViewGetOverrideUrl（跳转嗅探）
pub fn call(mode: u8, html: &str, url: &str, js: &str, regex: &str) -> Result<String, String> {
    let (html, url, js, regex) = (html.to_string(), url.to_string(), js.to_string(), regex.to_string());
    run_sync(move || Box::pin(async move { call_async(mode, &html, &url, &js, &regex).await }))
}

// ---------------- 浏览器探测 ----------------

fn find_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("READER_CHROME_PATH") {
        let pb = PathBuf::from(&p);
        if pb.exists() { return Some(pb); }
        tracing::warn!(target: "webview", "READER_CHROME_PATH 指向的路径不存在: {p}");
    }
    for name in [
        "chrome-headless-shell", "chrome", "google-chrome", "google-chrome-stable",
        "chromium", "chromium-browser",
    ] {
        if let Some(p) = which_path(name) { return Some(p); }
    }
    [
        "/opt/chrome-headless-shell-linux64/chrome-headless-shell",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/snap/bin/chromium",
    ].iter().map(PathBuf::from).find(|p| p.exists())
}

fn which_path(name: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok()?.split(':')
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.exists())
}

/// 测试用：本环境是否具备浏览器
#[cfg(test)]
pub fn chrome_available() -> bool { find_chrome().is_some() }

fn timeout_dur() -> Duration {
    let secs: u64 = std::env::var("READER_WEBVIEW_TIMEOUT").ok()
        .and_then(|s| s.parse().ok()).unwrap_or(60);
    Duration::from_secs(secs.clamp(1, 300))
}

// ---------------- CDP 运行时与浏览器实例 ----------------

/// 独立 tokio runtime：CDP 连接/事件分发专用
static CDP_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn runtime() -> &'static tokio::runtime::Runtime {
    CDP_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("创建 webview tokio runtime 失败")
    })
}

static BROWSER: OnceLock<tokio::sync::Mutex<Option<Arc<Browser>>>> = OnceLock::new();

async fn get_browser() -> Result<Arc<Browser>, String> {
    let slot = BROWSER.get_or_init(|| tokio::sync::Mutex::new(None));
    let mut guard = slot.lock().await;
    if let Some(b) = guard.as_ref() { return Ok(b.clone()); }

    let chrome = find_chrome().ok_or_else(|| {
        "未找到 Chrome/Chromium，webView 不可用（可设置 READER_CHROME_PATH 指定浏览器路径）".to_string()
    })?;
    let chrome_path = chrome.clone();
    let config = BrowserConfig::builder()
        .chrome_executable(chrome)
        .no_sandbox()
        .window_size(1280, 800)
        .launch_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("浏览器配置错误: {e}"))?;
    let (browser, mut handler) = Browser::launch(config).await
        .map_err(|e| format!("启动浏览器失败: {e}"))?;
    // 必须持续消费 handler，CDP 事件（请求/跳转）才能到达监听器
    tokio::spawn(async move {
        while let Some(_msg) = handler.next().await {}
    });
    tracing::info!(target: "webview", "已启动浏览器: {}", chrome_path.display());
    let b = Arc::new(browser);
    *guard = Some(b.clone());
    Ok(b)
}

/// 浏览器进程不可用（崩溃/退出）时清空缓存实例，下次调用重新拉起
async fn invalidate_browser() {
    if let Some(lock) = BROWSER.get() {
        *lock.lock().await = None;
    }
}

// ---------------- 同步桥 ----------------

/// JS 绑定闭包是同步调用（且可能在 async 工作线程内求值），
/// 统一丢到独立线程 block_on，规避 runtime 嵌套限制。
fn run_sync(make: impl FnOnce() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + 'static) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("webview-sync".into()).spawn(move || {
        let out = runtime().block_on(make());
        let _ = tx.send(out);
    }).map_err(|e| format!("webview 线程创建失败: {e}"))?;
    // 内部已有 tokio 超时兜底，这里再留 15s 余量防止悬死
    let cap = timeout_dur() + Duration::from_secs(15);
    match rx.recv_timeout(cap) {
        Ok(r) => r,
        Err(_) => Err("webView 执行超时".into()),
    }
}

// ---------------- 核心流程 ----------------

async fn call_async(mode: u8, html: &str, url: &str, js: &str, regex: &str) -> Result<String, String> {
    let dur = timeout_dur();
    let fut = async {
        let browser = get_browser().await?;
        let params = CreateTargetParams::builder().url("about:blank").build()
            .map_err(|e| format!("页面参数错误: {e}"))?;
        let page = match browser.new_page(params).await {
            Ok(p) => p,
            Err(e) => {
                // 浏览器进程可能已退出，清空实例待下次调用重新拉起
                invalidate_browser().await;
                return Err(format!("打开页面失败: {e}"));
            }
        };
        dispatch(&page, mode, html, url, js, regex).await
    };
    match tokio::time::timeout(dur, fut).await {
        Ok(r) => r,
        Err(_) => Err("webView 执行超时".into()),
    }
}

/// 官方语义：sourceRegex/overrideUrlRegex 均为空时走 HtmlWebViewClient（js 结果/源码），
/// 因此嗅探模式下 regex 为空回退到 mode 0。
async fn dispatch(page: &Page, mode: u8, html: &str, url: &str, js: &str, regex: &str) -> Result<String, String> {
    if matches!(mode, 1 | 2) && regex.trim().is_empty() {
        return render_result(page, html, url, js).await;
    }
    match mode {
        // webViewGetSource：订阅全量请求事件（先订阅再加载，避免漏掉早期资源）
        1 => {
            let mut stream = page.event_listener::<EventRequestWillBeSent>().await
                .map_err(|e| format!("订阅请求事件失败: {e}"))?;
            load(page, html, url).await?;
            Ok(wait_match(&mut stream, regex, |ev| Some(ev.request.url.clone())).await)
        }
        // webViewGetOverrideUrl：订阅主框架跳转事件
        2 => {
            let mut stream = page.event_listener::<EventFrameNavigated>().await
                .map_err(|e| format!("订阅跳转事件失败: {e}"))?;
            load(page, html, url).await?;
            Ok(wait_match(&mut stream, regex, |ev| {
                if ev.frame.parent_id.is_none() { Some(ev.frame.url.clone()) } else { None }
            }).await)
        }
        // webView：js 结果；js 为空返回整页源码
        _ => render_result(page, html, url, js).await,
    }
}

/// mode 0：加载页面后执行 js 取结果；js 为空返回整页源码
async fn render_result(page: &Page, html: &str, url: &str, js: &str) -> Result<String, String> {
    load(page, html, url).await?;
    if js.is_empty() {
        page.content().await.map_err(|e| format!("获取页面源码失败: {e}"))
    } else {
        Ok(eval_result(page, js).await)
    }
}

async fn load(page: &Page, html: &str, url: &str) -> Result<(), String> {
    if html.is_empty() {
        if url.is_empty() {
            return Err("webView 需要提供 html 或 url".into());
        }
        let params = NavigateParams::builder().url(url).build()
            .map_err(|e| format!("导航参数错误: {e}"))?;
        page.goto(params).await.map_err(|e| format!("加载 {url} 失败: {e}"))?;
    } else {
        page.set_content(html).await.map_err(|e| format!("载入 html 失败: {e}"))?;
    }
    Ok(())
}

/// js 求值结果转字符串：字符串原样，数字/布尔转文本，对象 JSON 序列化
async fn eval_result(page: &Page, js: &str) -> String {
    // 用 evaluate_expression 绕过 evaluate 的"函数式写法"启发式转换
    let params = chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
        .expression(js)
        .return_by_value(true)
        .build();
    let params = match params { Ok(p) => p, Err(e) => {
        tracing::debug!(target: "webview", "eval 参数错误: {e}");
        return String::new();
    } };
    match page.evaluate_expression(params).await {
        Ok(res) => match res.into_value::<serde_json::Value>() {
            Ok(serde_json::Value::String(s)) => s,
            Ok(serde_json::Value::Null) => String::new(),
            Ok(other) => other.to_string(),
            Err(_) => String::new(),
        },
        Err(e) => {
            tracing::debug!(target: "webview", "webView 执行 js 失败: {e}");
            String::new()
        }
    }
}

/// Kotlin `Regex.matches()` 语义 = 全串匹配
fn full_match(pattern: &str, candidate: &str) -> bool {
    Regex::new(&format!("^(?:{})$", pattern))
        .map(|re| re.is_match(candidate))
        .unwrap_or_else(|e| {
            tracing::debug!(target: "webview", "regex 无效: {pattern} · {e}");
            false
        })
}

/// 在事件流上等待第一个全串匹配 regex 的 URL；超时返回空串
async fn wait_match<T, F>(stream: &mut EventStream<T>, regex: &str, get: F) -> String
where
    T: chromiumoxide::cdp::IntoEventKind + Unpin + Send + Sync + 'static,
    F: Fn(&T) -> Option<String>,
{
    let deadline = tokio::time::Instant::now() + timeout_dur();
    loop {
        let remaining = deadline.checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            tracing::debug!(target: "webview", "嗅探超时未匹配: {regex}");
            return String::new();
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(ev)) => {
                if let Some(u) = get(&ev) {
                    if full_match(regex, &u) { return u; }
                }
            }
            Ok(None) => return String::new(), // 事件流关闭
            Err(_) => return String::new(),   // 达到 deadline
        }
    }
}

// ---------------- 测试（需真实浏览器；无浏览器时跳过） ----------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_html_and_eval_js() {
        if !chrome_available() { eprintln!("跳过 webview 测试：环境无 Chrome/Chromium"); return; }
        let out = call(0, "<b id='x'>ok-渲染</b>", "", "document.getElementById('x').textContent", "")
            .expect("渲染失败");
        assert_eq!(out, "ok-渲染");
    }

    #[test]
    fn render_url_get_source() {
        if !chrome_available() { return; }
        // data URL 需显式声明 charset（data 默认 US-ASCII）
        let out = call(0, "", "data:text/html;charset=utf-8,<html><head><title>t1</title></head><body>正文</body></html>", "", "")
            .expect("渲染失败");
        assert!(out.contains("正文"), "应返回整页源码: {}", out);
    }

    #[test]
    fn get_source_by_request_sniffing() {
        if !chrome_available() { return; }
        // data URL 主文档请求 URL 即为 data:…，全串匹配
        let url = "data:text/html,<img src='data:image/png;base64,iVBORw0KGgo='><p>sniff</p>";
        let out = call(1, "", url, "", &format!("^{}$", regex::escape(url))).expect("嗅探失败");
        assert_eq!(out, url);
    }

    #[test]
    fn override_url_on_js_navigation() {
        if !chrome_available() { return; }
        let html = r#"<script>setTimeout(()=>{location.href='https://example.com/target/abc123';},80);</script>"#;
        let out = call(2, html, "", "", r".*target/abc123").expect("嗅探跳转失败");
        assert!(out.contains("target/abc123"), "应返回跳转 URL: {}", out);
    }

    #[test]
    fn missing_browser_degrades_gracefully() {
        // 强制指向不存在的路径：应返回 Err 而非 panic
        // （仅当环境中也没有其它可用浏览器时才验证降级）
        if chrome_available() { return; }
        let r = call(0, "<b>x</b>", "", "1", "");
        assert!(r.is_err());
    }
}
