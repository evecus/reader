//! QuickJS 求值：执行 jsLib + @js: 表达式，注入 Legado 兼容的绑定变量。
//!
//! 变量体系：JS 层（java.put / source.setVariable）与规则层（@put）共用一张
//! `VarStore = Arc<Mutex<HashMap<String,String>>>`，因此 JS 写入的变量能被
//! 规则层 @get 读到，并在业务链路结束时由 BookService 统一落库。

use crate::cookie::CookieStore;
use crate::error::{AppError, AppResult};
use rquickjs::{Context, Function, Object, Runtime, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 官方 Legado 内置 CryptoJS 源码（64KB，自包含：MD5/SHA/AES/Hmac/Hex/Base64）。
/// 与上游 `assets/scripts/cryptojs.min.js` 完全一致，确保签名型书源语义一致。
const CRYPTO_JS_SOURCE: &str = include_str!("../assets/cryptojs.min.js");

/// 共享变量表：java.put / source.setVariable / 规则 @put 共用
pub type VarStore = Arc<Mutex<HashMap<String, String>>>;

pub fn new_var_store() -> VarStore { Arc::new(Mutex::new(HashMap::new())) }

pub struct JsContext {
    pub js_lib: String,
    pub result: String,
    pub base_url: String,
    pub source_url: String,
    pub source_name: String,
    pub book_name: String,
    pub chapter_title: String,
    /// 共享变量（与规则层 RuleContext 同一张表）
    pub variables: VarStore,
    pub fetch: Option<std::sync::Arc<dyn Fn(&str, &str) -> String + Send + Sync>>,
    /// jsUrl 专用：搜索关键字、页码
    pub key: String,
    pub page: i32,
    /// 共享 Cookie jar（java.getCookie / java.ajax 复用）
    pub cookies: CookieStore,
    /// 登录信息（getLoginInfoMap 返回）
    pub login_info: VarStore,
}

impl Default for JsContext {
    fn default() -> Self {
        Self {
            js_lib: String::new(), result: String::new(), base_url: String::new(),
            source_url: String::new(), source_name: String::new(), book_name: String::new(),
            chapter_title: String::new(), variables: new_var_store(), fetch: None,
            key: String::new(), page: 1, cookies: CookieStore::new(),
            login_info: new_var_store(),
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
    // 对象/数组需调用方用 Ctx::json_stringify 处理；此处返回空串
    String::new()
}

/// 执行 JS 表达式，返回结果字符串
pub fn eval(expr: &str, ctx: &JsContext) -> AppResult<String> {
    eval_inner(expr, ctx, true)
}

/// 规则求值：表达式包进 IIFE，兼容 @js: 复杂的 replace/正则链（返回表达式结果）
fn eval_inner(expr: &str, ctx: &JsContext, script_mode: bool) -> AppResult<String> {
    let runtime = Runtime::new().map_err(|e| AppError::internal(format!("QuickJS runtime: {e}")))?;
    let context = Context::full(&runtime).map_err(|e| AppError::internal(format!("QuickJS context: {e}")))?;

    context.with(|js| {
        // 内置官方 CryptoJS（对齐 Legado）：每次新建 Runtime 后注入一次全局 CryptoJS。
        // 仅当脚本/jsLib 实际引用 CryptoJS 时才 eval，避免无谓开销。
        let uses_crypto = expr.contains("CryptoJS") || ctx.js_lib.contains("CryptoJS");
        if uses_crypto {
            let _: Result<Value, _> = js.eval(CRYPTO_JS_SOURCE);
        }
        inject_bindings(&js, ctx)?;
        let script = if script_mode {
            let expr_json = serde_json::to_string(expr).unwrap_or_else(|_| "\"\"".into());
            format!("{}\n;var __reader_result = eval({});\n__reader_result;", ctx.js_lib, expr_json)
        } else {
            format!("{}\n;var __reader_result = (function(){{ return ({}); }})();\n__reader_result;", ctx.js_lib, expr)
        };
        let val: Value = js.eval(script).map_err(|e| {
            let detail = catch_js_error(&js);
            AppError::bad_request(format!("JS 执行失败: {} · {}", format_js_err(&e), detail))
        })?;
        Ok(js_value_to_string(&val))
    })
}

/// 执行 JS 语句并返回结果（用于 @js: 含多条语句的场景）
pub fn eval_statement(expr: &str, ctx: &JsContext) -> AppResult<String> { eval(expr, ctx) }

/// 执行多语句脚本（jsUrl / <js>...</js> 前缀），返回最后表达式的值
pub fn eval_script(expr: &str, ctx: &JsContext) -> AppResult<String> { eval_inner(expr, ctx, true) }

/// 求值 jsUrl：解析 <js>...</js> 或 @js: 包装的脚本，返回最终 URL 字符串。
pub fn eval_js_url(raw: &str, ctx: &JsContext) -> AppResult<Option<String>> {
    let t = raw.trim();
    let expr = if t.to_ascii_lowercase().starts_with("<js>") && t.to_ascii_lowercase().ends_with("</js>") {
        t[4..t.len() - 5].trim()
    } else if t.to_ascii_lowercase().starts_with("@js:") {
        t[4..].trim()
    } else {
        return Ok(None);
    };
    Ok(Some(eval_inner(expr, ctx, true)?))
}

fn format_js_err(e: &rquickjs::Error) -> String { format!("{e}") }

fn catch_js_error(js: &rquickjs::Ctx<'_>) -> String {
    let exc = js.catch();
    if let Some(obj) = exc.as_object() {
        let msg = obj.get::<_, Value>("message").ok()
            .map(|v| js_value_to_string(&v)).unwrap_or_default();
        let stack = obj.get::<_, Value>("stack").ok()
            .map(|v| js_value_to_string(&v)).unwrap_or_default();
        if !msg.is_empty() {
            return if stack.is_empty() { msg } else { format!("{} | {}", msg, stack.chars().take(300).collect::<String>()) };
        }
    }
    js_value_to_string(&exc)
}

/// 读取 VarStore 的 JSON 对象字符串
fn vars_json(store: &VarStore) -> String {
    let map = store.lock().ok().map(|g| g.clone()).unwrap_or_default();
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".into())
}

fn inject_bindings(js: &rquickjs::Ctx<'_>, jc: &JsContext) -> AppResult<()> {
    js.globals().set("result", jc.result.clone()).ok();
    js.globals().set("baseUrl", jc.base_url.clone()).ok();
    js.globals().set("bookName", jc.book_name.clone()).ok();
    js.globals().set("title", jc.chapter_title.clone()).ok();
    js.globals().set("sourceUrl", jc.source_url.clone()).ok();

    let java = Object::new(js.clone()).map_err(|e| AppError::internal(format!("java obj: {e}")))?;

    // java.ajax(url) / java.get(url)：走共享 Cookie jar
    if let Some(fetch) = &jc.fetch {
        let cookies = jc.cookies.clone();
        let fetch_ajax = fetch.clone();
        let ajax = Function::new(js.clone(), move |url: Value| -> String {
            let url_s = js_value_to_string(&url);
            // 对象参数 {url, method, body, headers, charset...}：重建为 `url,{...}` 交给
            // RequestSpec 统一解析，避免 headers/method/body 被丢弃。
            let raw = if url_s.trim_start().starts_with('{') {
                match serde_json::from_str::<serde_json::Value>(&url_s) {
                    Ok(mut v) => {
                        let u = v.get("url").and_then(|x| x.as_str()).map(|s| s.to_string());
                        if let Some(u) = u {
                            if let Some(obj) = v.as_object_mut() { obj.remove("url"); }
                            let has_opts = v.as_object().map(|m| !m.is_empty()).unwrap_or(false);
                            if has_opts {
                                format!("{},{}", u, serde_json::to_string(&v).unwrap_or_else(|_| "{}".into()))
                            } else { u }
                        } else { url_s }
                    }
                    _ => url_s,
                }
            } else { url_s };
            // 同时兼容直接传入 `url,{"method":...}` 字符串（已含选项，原样透传）
            let r = fetch_ajax(&raw, "{}");
            let _ = &cookies; // Cookie 注入/回写由 Fetcher 层完成
            r
        }).map_err(|e| AppError::internal(format!("ajax: {e}")))?;
        java.set("ajax", ajax).ok();

        let fetch_get = fetch.clone();
        let get = Function::new(js.clone(), move |url: String| -> String {
            fetch_get(&url, "{}")
        }).map_err(|e| AppError::internal(format!("get: {e}")))?;
        java.set("get", get).ok();
    }

    // base64 / log / getString
    use base64::Engine;
    let enc = base64::engine::general_purpose::STANDARD;
    let enc_fn = Function::new(js.clone(), move |s: String| -> String { enc.encode(s.as_bytes()) }).ok();
    if let Some(f) = enc_fn { java.set("base64Encode", f).ok(); }
    let dec = base64::engine::general_purpose::STANDARD;
    let dec_fn = Function::new(js.clone(), move |s: String| -> String {
        dec.decode(s).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default()
    }).ok();
    if let Some(f) = dec_fn { java.set("base64Decode", f).ok(); }
    let log = Function::new(js.clone(), |msg: String| {
        tracing::debug!(target: "js", "{}", msg);
    }).ok();
    if let Some(f) = log { java.set("log", f).ok(); }
    let to_str = Function::new(js.clone(), |v: Value| -> String { js_value_to_string(&v) }).ok();
    if let Some(f) = to_str { java.set("getString", f).ok(); }

    // java.getCookie(tag) —— 真实 CookieStore 查询（不再恒空）
    let cookie_store = jc.cookies.clone();
    let cookie_fn = Function::new(js.clone(), move |tag: String| -> String {
        cookie_store.get_by_tag(&tag)
    }).ok();
    if let Some(f) = cookie_fn { java.set("getCookie", f).ok(); }

    // java.put / java.get —— 写入共享变量表（Rust 闭包，真正回传给规则层与落库）
    let put_store = jc.variables.clone();
    let put_fn = Function::new(js.clone(), move |k: String, v: Value| -> String {
        let s = js_value_to_string(&v);
        if let Ok(mut m) = put_store.lock() { m.insert(k, s.clone()); }
        s
    }).ok();
    if let Some(f) = put_fn { java.set("put", f).ok(); }
    let get_store = jc.variables.clone();
    let get_fn = Function::new(js.clone(), move |k: String| -> String {
        get_store.lock().ok().and_then(|m| m.get(&k).cloned()).unwrap_or_default()
    }).ok();
    if let Some(f) = get_fn { java.set("get", f).ok(); }

    // 加密扩展：对齐 Legado JsEncodeUtils（摘要 / HMac / AES）
    inject_crypto_bindings(js, &java);

    js.globals().set("java", java).ok();

    // jsUrl 变量：key / page
    js.globals().set("key", jc.key.clone()).ok();
    js.globals().set("page", jc.page).ok();

    // JSON 规则中的内联 {{$.field}}：注入 $ = JSON.parse(result)
    let dollar_bootstrap = format!(
        "var $ = (function(){{ try{{ return JSON.parse({}); }}catch(e){{ return {{}}; }} }})();",
        serde_json::to_string(&jc.result).unwrap_or_else(|_| "\"\"".into())
    );
    let _: Result<Value, _> = js.eval(dollar_bootstrap);

    // cookie / cache 占位对象（保留兼容；真实 cookie 走 java.getCookie）
    let obj = Object::new(js.clone()).ok();
    if let Some(o) = obj {
        js.globals().set("cookie", o.clone()).ok();
        js.globals().set("cache", o).ok();
    }

    // source 对象：getVariable / setVariable / getLoginInfoMap / 书源信息
    let source = Object::new(js.clone()).ok();
    if let Some(s) = source {
        s.set("bookSourceUrl", jc.source_url.clone()).ok();
        s.set("bookSourceName", jc.source_name.clone()).ok();

        // getVariable：返回共享变量表的 JSON 字符串（书源通常 getVariable()['key']）
        let get_var_store = jc.variables.clone();
        let get_variable = Function::new(js.clone(), move || -> String { vars_json(&get_var_store) }).ok();
        if let Some(f) = get_variable { s.set("getVariable", f).ok(); }

        // setVariable：Rust 闭包只接收字符串；JS 包装层负责把对象 JSON.stringify
        let set_var_store = jc.variables.clone();
        let set_variable = Function::new(js.clone(), move |raw: String| -> String {
            let obj: serde_json::Value = serde_json::from_str(&raw)
                .unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
            if let Some(map) = obj.as_object() {
                if let Ok(mut m) = set_var_store.lock() {
                    for (k, val) in map {
                        m.insert(k.clone(), match val {
                            serde_json::Value::String(x) => x.clone(),
                            other => other.to_string(),
                        });
                    }
                }
            }
            raw
        }).ok();
        if let Some(f) = set_variable { s.set("__readerSetVar", f).ok(); }
        let _: Result<Value, _> = js.eval(
            "source.setVariable = function(v){ return __readerSetVar(typeof v === 'string' ? v : JSON.stringify(v)); };"
        );

        // getLoginInfoMap().get(k)：注入登录信息快照（书源只读场景足够）
        let login_snapshot = vars_json(&jc.login_info);
        let login_bootstrap = format!(
            "var __LOGIN = {}; \
             source.getLoginInfoMap = function() {{ \
               return {{ get: function(k) {{ \
                 return Object.prototype.hasOwnProperty.call(__LOGIN, k) ? __LOGIN[k] : null; \
               }} }}; \
             }};",
            login_snapshot
        );
        let _: Result<Value, _> = js.eval(login_bootstrap);

        js.globals().set("source", s).ok();
    }

    Ok(())
}

/// 注入 java.* 加密方法，对齐 Legado `JsEncodeUtils.kt`：
/// - md5Encode / md5Encode16
/// - digestHex / digestBase64Str(data, algorithm)
/// - HMacHex / HMacBase64(data, algorithm, key)
/// - aesEncodeToString / aesEncodeToBase64String / aesEncodeArgsBase64Str
/// - aesDecodeToString / aesBase64DecodeToString / aesDecodeArgsBase64Str
fn inject_crypto_bindings<'js>(js: &rquickjs::Ctx<'js>, java: &Object<'js>) {
    use crate::engine::crypto as c;

    macro_rules! bind1 {
        ($name:expr, $f:expr) => {
            if let Some(f) = Function::new(js.clone(), move |s: String| -> String {
                $f(&s).unwrap_or_default()
            }).ok() { java.set($name, f).ok(); }
        };
    }
    macro_rules! bind2 {
        ($name:expr, $f:expr) => {
            if let Some(f) = Function::new(js.clone(), move |a: String, b: String| -> String {
                $f(&a, &b).unwrap_or_default()
            }).ok() { java.set($name, f).ok(); }
        };
    }
    macro_rules! bind3 {
        ($name:expr, $f:expr) => {
            if let Some(f) = Function::new(js.clone(), move |a: String, b: String, c3: String| -> String {
                $f(&a, &b, &c3).unwrap_or_default()
            }).ok() { java.set($name, f).ok(); }
        };
    }

    bind1!("md5Encode", |s: &String| -> AppResult<String> { Ok(c::md5_encode(s)) });
    bind1!("md5Encode16", |s: &String| -> AppResult<String> { Ok(c::md5_encode16(s)) });
    bind2!("digestHex", c::digest_hex);
    bind2!("digestBase64Str", c::digest_base64);
    bind3!("HMacHex", c::hmac_hex);
    bind3!("HMacBase64", c::hmac_base64);

    // AES：data, key, transformation, iv（key/iv 为原始字符串字节）
    // aesEncodeToString 官方语义是"加密后 Base64"（aesEncodeToBase64String 的别名）
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, t: String, iv: String| -> String {
        c::aes_encrypt_base64(&data, &key, &t, &iv, false).unwrap_or_default()
    }).ok() { java.set("aesEncodeToString", f).ok(); }
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, t: String, iv: String| -> String {
        c::aes_encrypt_base64(&data, &key, &t, &iv, false).unwrap_or_default()
    }).ok() { java.set("aesEncodeToBase64String", f).ok(); }
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, t: String, iv: String| -> String {
        c::aes_encrypt_hex(&data, &key, &t, &iv, false).unwrap_or_default()
    }).ok() { java.set("aesEncodeToHex", f).ok(); }
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, t: String, iv: String| -> String {
        c::aes_decrypt_base64_to_string(&data, &key, &t, &iv, false).unwrap_or_default()
    }).ok() { java.set("aesDecodeToString", f).ok(); }
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, t: String, iv: String| -> String {
        c::aes_decrypt_base64_to_string(&data, &key, &t, &iv, false).unwrap_or_default()
    }).ok() { java.set("aesBase64DecodeToString", f).ok(); }
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, t: String, iv: String| -> String {
        c::aes_decrypt_hex_to_string(&data, &key, &t, &iv, false).unwrap_or_default()
    }).ok() { java.set("aesDecodeHexToString", f).ok(); }

    // ArgsBase64 变体：key/iv 为 Base64（对齐 aesEncodeArgsBase64Str / aesDecodeArgsBase64Str）
    // 签名 (data, key, mode, padding, iv)：mode+padding 拼成 transformation
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, mode: String, padding: String, iv: String| -> String {
        let t = format!("AES/{}/{}", mode, padding);
        c::aes_encrypt_base64(&data, &key, &t, &iv, true).unwrap_or_default()
    }).ok() { java.set("aesEncodeArgsBase64Str", f).ok(); }
    if let Some(f) = Function::new(js.clone(), |data: String, key: String, mode: String, padding: String, iv: String| -> String {
        let t = format!("AES/{}/{}", mode, padding);
        c::aes_decrypt_base64_to_string(&data, &key, &t, &iv, true).unwrap_or_default()
    }).ok() { java.set("aesDecodeArgsBase64Str", f).ok(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> JsContext { JsContext::default() }

    #[test]
    fn java_md5_bindings() {
        let c = ctx();
        assert_eq!(eval("java.md5Encode('abc')", &c).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(eval("java.md5Encode16('abc')", &c).unwrap(), "3cd24fb0d6963f7d");
        assert_eq!(eval("java.digestHex('abc','SHA-256')", &c).unwrap(),
                   "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn java_hmac_binding() {
        let c = ctx();
        assert_eq!(eval("java.HMacHex('The quick brown fox jumps over the lazy dog','md5','key')", &c).unwrap(),
                   "80070713463e7749b90c2dc24911e275");
    }

    #[test]
    fn java_aes_binding() {
        let c = ctx();
        let expr = "java.aesDecodeToString(java.aesEncodeToString('hello','1234567890abcdef','AES/ECB/PKCS5Padding',''),'1234567890abcdef','AES/ECB/PKCS5Padding','')";
        assert_eq!(eval(expr, &c).unwrap(), "hello");
    }

    #[test]
    fn builtin_cryptojs_injected() {
        let c = ctx();
        // 官方 cryptojs.min.js 应在脚本引用 CryptoJS 时自动注入
        assert_eq!(eval("CryptoJS.MD5('abc').toString()", &c).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(eval("CryptoJS.SHA256('abc').toString()", &c).unwrap(),
                   "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
