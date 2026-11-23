//! QuickJS 求值：执行 jsLib + @js: 表达式，注入 Legado 兼容的绑定变量。

use crate::error::{AppError, AppResult};
use rquickjs::{Context, Function, Object, Runtime, Value};
use std::collections::HashMap;

pub struct JsContext {
    pub js_lib: String,
    pub result: String,
    pub base_url: String,
    pub source_url: String,
    pub source_name: String,
    pub book_name: String,
    pub chapter_title: String,
    pub variables: HashMap<String, String>,
    pub fetch: Option<std::sync::Arc<dyn Fn(&str, &str) -> String + Send + Sync>>,
}

impl Default for JsContext {
    fn default() -> Self {
        Self {
            js_lib: String::new(), result: String::new(), base_url: String::new(),
            source_url: String::new(), source_name: String::new(), book_name: String::new(),
            chapter_title: String::new(), variables: HashMap::new(), fetch: None,
        }
    }
}

/// 把 JS Value 转成字符串（rquickjs 0.14 API）
fn js_value_to_string(v: &Value<'_>) -> String {
    if let Some(s) = v.as_string() {
        return s.to_string().unwrap_or_default();
    }
    if let Some(b) = v.as_bool() { return b.to_string(); }
    if let Some(i) = v.as_int() { return i.to_string(); }
    if let Some(n) = v.as_number() {
        return if n.fract() == 0.0 { format!("{:.0}", n) } else { n.to_string() };
    }
    if v.is_null() || v.is_undefined() { return String::new(); }
    // 对象/数组 → JSON 字符串（用 JSON.stringify）
    String::new()
}

/// 执行 JS 表达式，返回结果字符串
pub fn eval(expr: &str, ctx: &JsContext) -> AppResult<String> {
    let runtime = Runtime::new().map_err(|e| AppError::internal(format!("QuickJS runtime: {e}")))?;
    let context = Context::full(&runtime).map_err(|e| AppError::internal(format!("QuickJS context: {e}")))?;

    context.with(|js| {
        inject_bindings(&js, ctx)?;
        let script = format!(
            "{}\n;var __legado_result = (function(){{ return ({}); }})();\n__legado_result;",
            ctx.js_lib, expr
        );
        let val: Value = js.eval(script)
            .map_err(|e| AppError::bad_request(format!("JS 执行失败: {}", format_js_err(&e))))?;
        Ok(js_value_to_string(&val))
    })
}

/// 执行 JS 语句并返回结果（用于 @js: 含多条语句的场景）
pub fn eval_statement(expr: &str, ctx: &JsContext) -> AppResult<String> {
    eval(expr, ctx)
}

fn format_js_err(e: &rquickjs::Error) -> String { format!("{e}") }

fn inject_bindings(js: &rquickjs::Ctx<'_>, jc: &JsContext) -> AppResult<()> {
    js.globals().set("result", jc.result.clone()).ok();
    js.globals().set("baseUrl", jc.base_url.clone()).ok();
    js.globals().set("bookName", jc.book_name.clone()).ok();
    js.globals().set("title", jc.chapter_title.clone()).ok();
    js.globals().set("sourceUrl", jc.source_url.clone()).ok();

    let java = Object::new(js.clone()).map_err(|e| AppError::internal(format!("java obj: {e}")))?;

    // java.ajax(url) / java.get(url, headers)
    if let Some(fetch) = &jc.fetch {
        let fetch_ajax = fetch.clone();
        let ajax = Function::new(js.clone(), move |url: Value| -> String {
            let url_s = js_value_to_string(&url);
            let u = if url_s.starts_with('{') {
                serde_json::from_str::<serde_json::Value>(&url_s)
                    .ok().and_then(|v| v.get("url").and_then(|x| x.as_str()).map(|s| s.to_string()))
                    .unwrap_or(url_s)
            } else { url_s };
            fetch_ajax(&u, "{}")
        }).map_err(|e| AppError::internal(format!("ajax: {e}")))?;
        java.set("ajax", ajax).ok();

        let fetch_get = fetch.clone();
        let get = Function::new(js.clone(), move |url: String| -> String {
            fetch_get(&url, "{}")
        }).map_err(|e| AppError::internal(format!("get: {e}")))?;
        java.set("get", get).ok();
    }

    // java.base64Encode / base64Decode
    use base64::Engine;
    let enc = base64::engine::general_purpose::STANDARD;
    let enc_fn = Function::new(js.clone(), move |s: String| -> String { enc.encode(s.as_bytes()) }).ok();
    if let Some(f) = enc_fn { java.set("base64Encode", f).ok(); }
    let dec = base64::engine::general_purpose::STANDARD;
    let dec_fn = Function::new(js.clone(), move |s: String| -> String {
        dec.decode(s).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default()
    }).ok();
    if let Some(f) = dec_fn { java.set("base64Decode", f).ok(); }

    // java.log
    let log = Function::new(js.clone(), |msg: String| {
        tracing::debug!(target: "js", "{}", msg);
    }).ok();
    if let Some(f) = log { java.set("log", f).ok(); }

    // java.getString(v) - 字符串化
    let to_str = Function::new(js.clone(), |v: Value| -> String { js_value_to_string(&v) }).ok();
    if let Some(f) = to_str { java.set("getString", f).ok(); }

    // java.getCookie
    let cookie = Function::new(js.clone(), |_tag: String| -> String { String::new() }).ok();
    if let Some(f) = cookie { java.set("getCookie", f).ok(); }

    js.globals().set("java", java).ok();

    // 初始化变量对象 + 注入已保存变量（通过 JS bootstrap，避免闭包捕获 Ctx）
    let vars_init = serde_json::to_string(&jc.variables).unwrap_or_else(|_| "{}".to_string());
    let bootstrap = format!(
        "var __LEGADO_VARS = {}; \
         if (!globalThis.java) globalThis.java = {{}}; \
         java.put = function(k, v) {{ __LEGADO_VARS[k] = v; return v; }}; \
         java.getString = function(x) {{ return x == null ? '' : String(x); }};",
        vars_init
    );
    let _: Result<Value, _> = js.eval(bootstrap);

    // cookie / cache / source 占位
    let obj = Object::new(js.clone()).ok();
    if let Some(o) = obj {
        js.globals().set("cookie", o.clone()).ok();
        js.globals().set("cache", o).ok();
    }
    let source = Object::new(js.clone()).ok();
    if let Some(s) = source { js.globals().set("source", s).ok(); }

    Ok(())
}
