//! `AnalyzeRule.kt` 的移植：规则切分 → 模式路由 → 内联求值 → 结果管线。
//!
//! 与官方一致的核心语义：
//! - 规则先按 `<js>…</js>` / `@js:` / `@webjs:` 切成若干 SourceRule，**上一段的结果作为下一段的输入**；
//! - 每段 SourceRule 内部识别模式前缀（`@CSS:` `@@` `@XPath:` `@Json:` `$.` `/`），内容是 JSON 时默认走 JSONPath；
//! - `@put:{"k":"规则"}` 写变量，`@get:{k}` / `{{js|规则}}` 内联求值，`$1` 引用上一步（`:` 整体正则）的分组；
//! - `##正则##替换`（`###` 表示只取第一个匹配）；
//! - 变量分三层：章节 → 书 → 书源（`put` 写到最内层存在者，`get` 由内向外找）。

use crate::cookie::CookieStore;
use crate::engine::js::{self, new_var_store, JsBook, JsChapter, JsContext, JsFetchFn, JsWebviewFn, VarStore};
use crate::engine::jsonpath::{parse_lenient, AnalyzeByJsonPath};
use crate::engine::jsoup;
use crate::engine::rule_analyzer::RuleAnalyzer;
use crate::engine::util::{self, absolute_url, jregex, rule_replace, unescape_html};
use crate::engine::value::{EvalValue, Val};
use crate::engine::xpath::{xpath_elements, xpath_select};
use crate::error::{AppError, AppResult};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode { XPath, Json, Default, Js, Regex, WebJs }

fn js_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?is)<js>([\w\W]*?)</js>|@js:([\w\W]*)").unwrap())
}
fn webjs_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?is)@webjs:([\w\W]{5,})").unwrap())
}
fn put_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)@put:(\{[^}]+?\})").unwrap())
}
fn eval_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)@get:\{[^}]+?\}|\{\{[\w\W]*?\}\}").unwrap())
}
fn regex_group_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\$\d{1,2}").unwrap())
}

const GET_RULE_TYPE: i32 = -2;
const JS_RULE_TYPE: i32 = -1;
const DEFAULT_RULE_TYPE: i32 = 0;

/// 规则段（对应 `AnalyzeRule.SourceRule`）
#[derive(Debug, Clone)]
pub struct SourceRule {
    pub mode: Mode,
    pub rule: String,
    replace_regex: String,
    replacement: String,
    replace_first: bool,
    put_map: Vec<(String, String)>,
    rule_param: Vec<String>,
    rule_type: Vec<i32>,
}

/// `@put:{...}` 里的 JSON：严格 JSON 优先，失败后用 json5 容忍单引号/无引号键
fn parse_put_json(s: &str) -> Option<Vec<(String, String)>> {
    let v: Value = serde_json::from_str(s).ok().or_else(|| json5::from_str(s).ok())?;
    let Value::Object(m) = v else { return None };
    Some(m.into_iter().map(|(k, v)| (k, match v {
        Value::String(s) => s,
        other => other.to_string(),
    })).collect())
}

impl SourceRule {
    fn new(rule_str: &str, mode: Mode, is_json: bool) -> SourceRule {
        let lower = rule_str.to_ascii_lowercase();
        let mut mode = mode;
        let rule: String = if mode == Mode::Js || mode == Mode::Regex {
            rule_str.to_string()
        } else if lower.starts_with("@css:") {
            mode = Mode::Default;
            rule_str.to_string()
        } else if rule_str.starts_with("@@") {
            mode = Mode::Default;
            rule_str[2..].to_string()
        } else if lower.starts_with("@xpath:") {
            mode = Mode::XPath;
            rule_str[7..].to_string()
        } else if lower.starts_with("@json:") {
            mode = Mode::Json;
            rule_str[6..].to_string()
        } else if is_json || rule_str.starts_with("$.") || rule_str.starts_with("$[") {
            mode = Mode::Json;
            rule_str.to_string()
        } else if rule_str.starts_with('/') {
            mode = Mode::XPath;
            rule_str.to_string()
        } else {
            rule_str.to_string()
        };

        let mut sr = SourceRule {
            mode, rule: String::new(), replace_regex: String::new(), replacement: String::new(),
            replace_first: false, put_map: Vec::new(), rule_param: Vec::new(), rule_type: Vec::new(),
        };

        // 分离 @put
        let mut rule = rule;
        let puts: Vec<(String, String)> = put_pattern().captures_iter(&rule)
            .map(|c| (c[0].to_string(), c[1].to_string())).collect();
        for (whole, json) in puts {
            rule = rule.replacen(&whole, "", 1);
            if let Some(m) = parse_put_json(&json) { sr.put_map.extend(m); }
        }
        sr.rule = rule.clone();

        // @get / {{ }} 拆分
        let mut start = 0usize;
        let mut first = true;
        for m in eval_pattern().find_iter(&rule) {
            if first {
                let tmp = &rule[..m.start()];
                if sr.mode != Mode::Js && sr.mode != Mode::Regex
                    && (m.start() == 0 || !tmp.contains("##"))
                {
                    sr.mode = Mode::Regex;
                }
                first = false;
            }
            if m.start() > start { sr.split_regex(&rule[start..m.start()]); }
            let tmp = m.as_str();
            if tmp.to_ascii_lowercase().starts_with("@get:") {
                sr.rule_type.push(GET_RULE_TYPE);
                sr.rule_param.push(tmp[6..tmp.len() - 1].to_string());
            } else if tmp.starts_with("{{") {
                sr.rule_type.push(JS_RULE_TYPE);
                sr.rule_param.push(tmp[2..tmp.len() - 2].to_string());
            } else {
                sr.split_regex(tmp);
            }
            start = m.end();
        }
        if rule.len() > start {
            sr.split_regex(&rule[start..]);
        }
        sr
    }

    /// 拆分 `$\d{1,2}`
    fn split_regex(&mut self, rule_str: &str) {
        let head = rule_str.split("##").next().unwrap_or("");
        let mut start = 0usize;
        let mut found = false;
        for m in regex_group_pattern().find_iter(head) {
            if !found {
                found = true;
                if self.mode != Mode::Js && self.mode != Mode::Regex { self.mode = Mode::Regex; }
            }
            if m.start() > start {
                self.rule_type.push(DEFAULT_RULE_TYPE);
                self.rule_param.push(rule_str[start..m.start()].to_string());
            }
            self.rule_type.push(m.as_str()[1..].parse().unwrap_or(0));
            self.rule_param.push(m.as_str().to_string());
            start = m.end();
        }
        if rule_str.len() > start {
            self.rule_type.push(DEFAULT_RULE_TYPE);
            self.rule_param.push(rule_str[start..].to_string());
        }
    }

    fn param_size(&self) -> usize { self.rule_param.len() }
}

/// 内联 `{{...}}` 是规则还是 JS（官方 `isRule`）
fn is_inline_rule(s: &str) -> bool {
    s.starts_with('@') || s.starts_with("$.") || s.starts_with("$[") || s.starts_with("//")
}

/// 整数值的 JS 数字不带 ".0"
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{:.0}", n) } else { n.to_string() }
}

// ───────────────────────── RuleContext ─────────────────────────

#[derive(Clone)]
pub struct RuleContext {
    pub content: Val,
    pub base_url: String,
    pub redirect_url: Option<String>,
    pub js_lib: String,
    /// 书源级变量（`source.put/get`、登录信息等）
    pub variables: VarStore,
    /// 书级变量（`@put` / `java.put` 默认写这里）
    pub book_vars: Option<VarStore>,
    /// 章节级变量
    pub chapter_vars: Option<VarStore>,
    pub fetch: Option<JsFetchFn>,
    pub webview: Option<JsWebviewFn>,
    pub cookies: CookieStore,
    pub source_url: String,
    pub source_name: String,
    pub key: String,
    pub page: i32,
    pub book: Option<JsBook>,
    pub chapter: Option<JsChapter>,
    pub next_chapter_url: Option<String>,
    is_regex: bool,
    local: HashMap<String, String>,
}

impl RuleContext {
    pub fn new(html: impl Into<String>, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        Self {
            content: Val::Str(html.into()), redirect_url: None, base_url, js_lib: String::new(),
            variables: new_var_store(), book_vars: None, chapter_vars: None,
            fetch: None, webview: None, cookies: CookieStore::new(),
            source_url: String::new(), source_name: String::new(), key: String::new(), page: 1,
            book: None, chapter: None, next_chapter_url: None,
            is_regex: false, local: HashMap::new(),
        }
    }

    /// 兼容旧调用：章节标题
    pub fn set_chapter_title(&mut self, title: &str) {
        let mut c = self.chapter.take().unwrap_or_default();
        c.title = title.to_string();
        self.chapter = Some(c);
    }

    pub fn set_content(&mut self, content: Val) -> &mut Self {
        self.content = content;
        self
    }

    pub fn set_content_str(&mut self, content: impl Into<String>) -> &mut Self {
        self.content = Val::Str(content.into());
        self
    }

    /// 对应 `setRedirectUrl`：data: 不覆盖
    pub fn set_redirect_url(&mut self, url: &str) {
        if url.len() >= 5 && url[..5].eq_ignore_ascii_case("data:") { return; }
        self.redirect_url = Some(url.to_string());
    }

    pub fn set_local(&mut self, key: &str, value: &str) {
        self.local.insert(key.to_string(), value.to_string());
    }

    /// 从当前内容派生一个子上下文（共享变量/网络/Cookie，内容替换）
    pub fn with_content(&self, content: Val) -> RuleContext {
        let mut c = self.clone();
        c.content = content;
        c.local.clear();
        c
    }

    // ───── 变量 ─────

    /// 对应 `AnalyzeRule.put`：章节 → 书 → 书源
    pub fn put(&mut self, key: &str, value: String) -> String {
        let store = self.chapter_vars.as_ref()
            .or(self.book_vars.as_ref())
            .unwrap_or(&self.variables);
        if let Ok(mut m) = store.lock() { m.insert(key.to_string(), value.clone()); }
        value
    }

    /// 对应 `AnalyzeRule.get`：本地绑定 → 书名/章节名 → 章节 → 书 → 书源
    pub fn get(&self, key: &str) -> String {
        if let Some(v) = self.local.get(key) { return v.clone(); }
        match key {
            "bookName" => if let Some(b) = &self.book { return b.name.clone(); },
            "title" => if let Some(c) = &self.chapter { return c.title.clone(); },
            _ => {}
        }
        for store in [self.chapter_vars.as_ref(), self.book_vars.as_ref(), Some(&self.variables)].into_iter().flatten() {
            if let Some(v) = store.lock().ok().and_then(|m| m.get(key).cloned()).filter(|v| !v.is_empty()) {
                return v;
            }
        }
        String::new()
    }

    fn put_rule(&mut self, map: &[(String, String)]) -> AppResult<()> {
        for (k, v) in map {
            let val = self.get_string(v)?;
            self.put(k, val);
        }
        Ok(())
    }

    // ───── JS ─────

    fn js_ctx(&self, result: &Val) -> JsContext {
        let login_info = new_var_store();
        if let Some(raw) = self.variables.lock().ok().and_then(|m| m.get("loginInfo").cloned()) {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
                if let Ok(mut m) = login_info.lock() {
                    for (k, v) in map {
                        m.insert(k.clone(), v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()));
                    }
                }
            }
        }
        let needs_host = |s: &str| s.contains("getString") || s.contains("getElement") || s.contains("setContent");
        let host = if needs_host(&self.js_lib) || self.local.contains_key("__needs_host") {
            Some(Arc::new(Mutex::new(self.clone())))
        } else { None };
        JsContext {
            js_lib: self.js_lib.clone(),
            result: result.to_text(),
            result_json: match result { Val::Str(_) | Val::Null => None, other => Some(other.to_json().to_string()) },
            base_url: self.base_url.clone(),
            source_url: self.source_url.clone(),
            source_name: self.source_name.clone(),
            book_name: self.book.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
            chapter_title: self.chapter.as_ref().map(|c| c.title.clone()).unwrap_or_default(),
            variables: self.variables.clone(),
            book_vars: self.book_vars.clone(),
            chapter_vars: self.chapter_vars.clone(),
            fetch: self.fetch.clone(),
            webview: self.webview.clone(),
            cookies: self.cookies.clone(),
            key: self.key.clone(),
            page: self.page,
            login_info,
            src: self.content.to_text(),
            next_chapter_url: self.next_chapter_url.clone().unwrap_or_default(),
            book: self.book.clone(),
            chapter: self.chapter.clone(),
            host,
            ..Default::default()
        }
    }

    /// 对应 `evalJS(jsStr, result)`
    pub fn eval_js(&mut self, script: &str, result: &Val) -> AppResult<Val> {
        let mut jc = self.js_ctx(result);
        if jc.host.is_none() && script.contains("java.get") && (script.contains("getString") || script.contains("getElement")) {
            jc.host = Some(Arc::new(Mutex::new(self.clone())));
        }
        if jc.host.is_none() && script.contains("setContent") {
            jc.host = Some(Arc::new(Mutex::new(self.clone())));
        }
        js::eval_val(script, &jc)
    }

    fn web_js_result(&self, js_str: &str, result: &Val) -> AppResult<Val> {
        let Some(wv) = &self.webview else { return Ok(Val::Str(String::new())) };
        let script = format!("var result = {};\n{}", result.to_json(), js_str);
        wv(0, &self.content.to_text(), &self.base_url, &script, "")
            .map(Val::Str)
            .map_err(AppError::internal)
    }

    // ───── 规则切分 ─────

    fn content_is_json(&self) -> bool { self.content.looks_like_json() }

    /// 对应 `splitSourceRule`
    pub fn split_source_rule(&mut self, rule_str: &str, all_in_one: bool) -> Vec<SourceRule> {
        let mut out = Vec::new();
        if rule_str.is_empty() { return out; }
        let is_json = self.content_is_json();
        let mut m_mode = Mode::Default;
        let mut start = 0usize;
        if all_in_one && rule_str.starts_with(':') {
            m_mode = Mode::Regex;
            self.is_regex = true;
            start = 1;
        } else if self.is_regex {
            m_mode = Mode::Regex;
        }
        for caps in js_pattern().captures_iter(rule_str) {
            let m = caps.get(0).unwrap();
            if m.start() > start {
                let tmp = rule_str[start..m.start()].trim();
                if !tmp.is_empty() { out.push(SourceRule::new(tmp, m_mode, is_json)); }
            }
            let body = caps.get(2).or(caps.get(1)).map(|g| g.as_str()).unwrap_or("");
            out.push(SourceRule::new(body, Mode::Js, is_json));
            start = m.end();
        }
        for caps in webjs_pattern().captures_iter(rule_str) {
            let m = caps.get(0).unwrap();
            if m.start() > start {
                let tmp = rule_str[start..m.start()].trim();
                if !tmp.is_empty() { out.push(SourceRule::new(tmp, m_mode, is_json)); }
            }
            out.push(SourceRule::new(caps.get(1).map(|g| g.as_str()).unwrap_or(""), Mode::WebJs, is_json));
            start = m.end();
        }
        if rule_str.len() > start {
            let tmp = rule_str[start..].trim();
            if !tmp.is_empty() { out.push(SourceRule::new(tmp, m_mode, is_json)); }
        }
        out
    }

    /// 对应 `SourceRule.makeUpRule`：展开 `$n` / `@get` / `{{}}`，再拆 `##`
    fn make_up_rule(&mut self, sr: &mut SourceRule, result: &Val) -> AppResult<()> {
        if !sr.rule_param.is_empty() {
            let mut info = String::new();
            let mut index = sr.rule_param.len();
            while index > 0 {
                index -= 1;
                let t = sr.rule_type[index];
                let piece: String = if t > DEFAULT_RULE_TYPE {
                    match result {
                        Val::List(items) => items.get(t as usize)
                            .filter(|v| !v.is_null()).map(|v| v.to_text()).unwrap_or_default(),
                        _ => sr.rule_param[index].clone(),
                    }
                } else if t == JS_RULE_TYPE {
                    let p = sr.rule_param[index].clone();
                    if is_inline_rule(&p) {
                        let mut sub = self.split_source_rule(&p, false);
                        // 内联规则一律当作整体规则求值（不套用 isRegex 之类的状态）
                        self.get_string_rules(&mut sub, None, false, true)?
                    } else {
                        match self.eval_js(&p, result)? {
                            Val::Null => String::new(),
                            other => other.to_text(),
                        }
                    }
                } else if t == GET_RULE_TYPE {
                    self.get(&sr.rule_param[index])
                } else {
                    sr.rule_param[index].clone()
                };
                info.insert_str(0, &piece);
            }
            sr.rule = info;
        }
        // 分离正则替换
        let parts: Vec<String> = sr.rule.split("##").map(|s| s.to_string()).collect();
        sr.rule = parts[0].trim().to_string();
        if parts.len() > 1 { sr.replace_regex = parts[1].clone(); }
        if parts.len() > 2 { sr.replacement = parts[2].clone(); }
        if parts.len() > 3 { sr.replace_first = true; }
        Ok(())
    }

    fn replace_regex(result: &str, sr: &SourceRule) -> String {
        rule_replace(result, &sr.replace_regex, &sr.replacement, sr.replace_first)
    }

    // ───── 内容转换 ─────

    fn xpath_html(text: &str) -> String {
        let mut h = text.to_string();
        if h.ends_with("</td>") { h = format!("<tr>{}</tr>", h); }
        if h.ends_with("</tr>") || h.ends_with("</tbody>") { h = format!("<table>{}</table>", h); }
        h
    }

    fn xpath_strings(text: &str, rule: &str) -> AppResult<Vec<String>> {
        let html = Self::xpath_html(text);
        let (parts, et) = RuleAnalyzer::split_or_whole(rule, false, &["&&", "||", "%%"]);
        if parts.len() == 1 {
            return Ok(match xpath_select(&html, &parts[0])? {
                EvalValue::List(l) => l.into_iter().map(|e| e.as_str()).collect(),
                EvalValue::Null => vec![],
                other => vec![other.as_str()],
            });
        }
        let mut groups: Vec<Vec<String>> = Vec::new();
        for p in &parts {
            let t = Self::xpath_strings(text, p)?;
            if !t.is_empty() {
                groups.push(t);
                if et == "||" { break; }
            }
        }
        Ok(merge_groups(groups, &et))
    }

    fn xpath_elems(text: &str, rule: &str) -> AppResult<Vec<String>> {
        let html = Self::xpath_html(text);
        let (parts, et) = RuleAnalyzer::split_or_whole(rule, false, &["&&", "||", "%%"]);
        if parts.len() == 1 { return xpath_elements(&html, &parts[0]); }
        let mut groups: Vec<Vec<String>> = Vec::new();
        for p in &parts {
            let t = xpath_elements(&html, p)?;
            if !t.is_empty() {
                groups.push(t);
                if et == "||" { break; }
            }
        }
        Ok(merge_groups(groups, &et))
    }

    fn json_of(&self, v: &Val) -> AnalyzeByJsonPath {
        match v {
            Val::Json(j) => AnalyzeByJsonPath::new(j.clone()),
            other => AnalyzeByJsonPath::new(parse_lenient(&other.to_text())),
        }
    }

    // ───── 取值 ─────

    pub fn get_string(&mut self, rule: &str) -> AppResult<String> {
        if rule.is_empty() { return Ok(String::new()); }
        let mut rules = self.split_source_rule(rule, false);
        self.get_string_rules(&mut rules, None, false, true)
    }

    /// 带显式内容（对应 `getString(ruleStr, mContent)`）
    pub fn get_string_with(&mut self, rule: &str, content: &Val) -> AppResult<String> {
        if rule.is_empty() { return Ok(String::new()); }
        let mut rules = self.split_source_rule(rule, false);
        self.get_string_rules(&mut rules, Some(content), false, true)
    }

    /// 取 URL：结果按 redirectUrl 补全，空则回落 baseUrl
    pub fn get_string_url(&mut self, rule: &str) -> AppResult<String> {
        if rule.is_empty() { return Ok(String::new()); }
        let mut rules = self.split_source_rule(rule, false);
        self.get_string_rules(&mut rules, None, true, true)
    }

    /// 正文专用：不做 HTML 实体反转义（官方 `getString(rule, unescape=false)`）
    pub fn get_string_raw(&mut self, rule: &str) -> AppResult<String> {
        if rule.is_empty() { return Ok(String::new()); }
        let mut rules = self.split_source_rule(rule, false);
        self.get_string_rules(&mut rules, None, false, false)
    }

    pub fn get_string_rules(
        &mut self, rules: &mut Vec<SourceRule>, m_content: Option<&Val>, is_url: bool, unescape: bool,
    ) -> AppResult<String> {
        let content = m_content.cloned().unwrap_or_else(|| self.content.clone());
        let mut result: Val = Val::Null;
        if !matches!(content, Val::Null) && !rules.is_empty() {
            result = content.clone();
            for sr in rules.iter_mut() {
                self.put_rule(&sr.put_map.clone())?;
                self.make_up_rule(sr, &result)?;
                if result.is_null() { continue; }
                let rule = sr.rule.clone();
                if !rule.trim().is_empty() || sr.replace_regex.is_empty() {
                    result = match sr.mode {
                        Mode::WebJs => self.web_js_result(&rule, &result)?,
                        Mode::Js => self.eval_js(&rule, &result)?,
                        Mode::Json => match self.json_of(&result).get_string(&rule) {
                            Some(s) => Val::Str(s), None => Val::Null,
                        },
                        Mode::XPath => {
                            let l = Self::xpath_strings(&result.to_text(), &rule)?;
                            Val::Str(l.join("\n"))
                        }
                        Mode::Default => {
                            let text = result.to_text();
                            if is_url {
                                jsoup::result_list(&text, &rule).into_iter().next().map(Val::Str).unwrap_or(Val::Str(String::new()))
                            } else {
                                match jsoup::result_string(&text, &rule) { Some(s) => Val::Str(s), None => Val::Null }
                            }
                        }
                        Mode::Regex => Val::Str(rule),
                    };
                }
                if !result.is_null() && !sr.replace_regex.is_empty() {
                    result = Val::Str(Self::replace_regex(&result.to_text(), sr));
                }
            }
        }
        let text = if result.is_null() { String::new() } else { result.to_text() };
        let s = if unescape && text.contains('&') { unescape_html(&text) } else { text };
        if is_url {
            return Ok(if s.trim().is_empty() {
                self.base_url.clone()
            } else {
                self.abs_url(&s)
            });
        }
        Ok(s)
    }

    fn abs_url(&self, rel: &str) -> String {
        match &self.redirect_url {
            Some(r) => absolute_url(r, rel),
            None => rel.trim().to_string(),
        }
    }

    pub fn get_string_list(&mut self, rule: &str) -> AppResult<Vec<String>> {
        if rule.is_empty() { return Ok(vec![]); }
        let mut rules = self.split_source_rule(rule, false);
        Ok(self.get_string_list_rules(&mut rules, false)?.unwrap_or_default())
    }

    pub fn get_string_list_url(&mut self, rule: &str) -> AppResult<Vec<String>> {
        if rule.is_empty() { return Ok(vec![]); }
        let mut rules = self.split_source_rule(rule, false);
        Ok(self.get_string_list_rules(&mut rules, true)?.unwrap_or_default())
    }

    pub fn get_string_list_rules(&mut self, rules: &mut Vec<SourceRule>, is_url: bool) -> AppResult<Option<Vec<String>>> {
        let content = self.content.clone();
        let mut result: Val = Val::Null;
        if !matches!(content, Val::Null) && !rules.is_empty() {
            result = content.clone();
            for sr in rules.iter_mut() {
                self.put_rule(&sr.put_map.clone())?;
                self.make_up_rule(sr, &result)?;
                if result.is_null() { continue; }
                let rule = sr.rule.clone();
                if !rule.is_empty() {
                    result = match sr.mode {
                        Mode::WebJs => {
                            let r = self.web_js_result(&rule, &result)?;
                            match serde_json::from_str::<Value>(&r.to_text()) {
                                Ok(Value::Array(a)) => Val::List(a.into_iter().map(Val::from_json).collect()),
                                _ => r,
                            }
                        }
                        Mode::Js => self.eval_js(&rule, &result)?,
                        Mode::Json => {
                            Val::List(self.json_of(&result).get_string_list(&rule).into_iter().map(Val::Str).collect())
                        }
                        Mode::XPath => Val::List(Self::xpath_strings(&result.to_text(), &rule)?.into_iter().map(Val::Str).collect()),
                        Mode::Default => Val::List(jsoup::result_list(&result.to_text(), &rule).into_iter().map(Val::Str).collect()),
                        Mode::Regex => Val::Str(rule),
                    };
                }
                if !sr.replace_regex.is_empty() {
                    result = match &result {
                        Val::List(items) => Val::List(items.iter()
                            .map(|i| Val::Str(Self::replace_regex(&i.to_text(), sr))).collect()),
                        other => Val::Str(Self::replace_regex(&other.to_text(), sr)),
                    };
                }
            }
        }
        if result.is_null() { return Ok(None); }
        let list: Option<Vec<String>> = match result {
            Val::Str(s) => Some(s.split('\n').map(|x| x.to_string()).collect()),
            Val::List(items) => Some(items.iter().map(|v| v.to_text()).collect()),
            Val::Json(Value::Array(a)) => Some(a.iter().map(|v| Val::from_json(v.clone()).to_text()).collect()),
            Val::Json(_) => None,
            Val::Null => None,
        };
        if is_url {
            let mut urls: Vec<String> = Vec::new();
            for u in list.unwrap_or_default() {
                let abs = self.abs_url(&u);
                if !abs.is_empty() && !urls.contains(&abs) { urls.push(abs); }
            }
            return Ok(Some(urls));
        }
        Ok(list)
    }

    /// 对应 `getElement`（单个元素/对象，详情页 init 用）
    pub fn get_element(&mut self, rule: &str) -> AppResult<Val> {
        if rule.is_empty() { return Ok(Val::Null); }
        let mut rules = self.split_source_rule(rule, true);
        let mut result = self.content.clone();
        if rules.is_empty() { return Ok(result); }
        for sr in rules.iter_mut() {
            self.put_rule(&sr.put_map.clone())?;
            self.make_up_rule(sr, &result)?;
            if result.is_null() { continue; }
            let r = sr.rule.clone();
            result = match sr.mode {
                Mode::Regex => {
                    let regs = split_not_blank(&r, "&&");
                    match regex_get_element(&result.to_text(), &regs, 0) {
                        Some(g) => Val::List(g.into_iter().map(Val::Str).collect()),
                        None => Val::Null,
                    }
                }
                Mode::WebJs => {
                    let r = self.web_js_result(&r, &result)?;
                    match serde_json::from_str::<Value>(&r.to_text()) { Ok(v) => Val::from_json(v), Err(_) => Val::Null }
                }
                Mode::Js => self.eval_js(&r, &result)?,
                Mode::Json => match self.json_of(&result).get_object(&r) {
                    Some(v) => Val::from_json(v), None => Val::Null,
                },
                Mode::XPath => Val::Str(Self::xpath_elems(&result.to_text(), &r)?.join("\n")),
                Mode::Default => Val::Str(jsoup::elements(&result.to_text(), &r).join("\n")),
            };
            if !sr.replace_regex.is_empty() && !result.is_null() {
                result = Val::Str(Self::replace_regex(&result.to_text(), sr));
            }
        }
        Ok(result)
    }

    /// 对应 `getElements`：返回元素列表（HTML 字符串 / JSON 对象 / 正则分组列表）
    pub fn get_elements(&mut self, rule: &str) -> AppResult<Vec<Val>> {
        let mut result: Val = Val::Null;
        let mut rules = self.split_source_rule(rule, true);
        if !matches!(self.content, Val::Null) && !rules.is_empty() {
            result = self.content.clone();
            for sr in rules.iter_mut() {
                self.put_rule(&sr.put_map.clone())?;
                if result.is_null() { continue; }
                let r = sr.rule.clone();
                result = match sr.mode {
                    Mode::Regex => {
                        let regs = split_not_blank(&r, "&&");
                        Val::List(regex_get_elements(&result.to_text(), &regs, 0).into_iter()
                            .map(|g| Val::List(g.into_iter().map(Val::Str).collect())).collect())
                    }
                    Mode::WebJs => {
                        let w = self.web_js_result(&r, &result)?;
                        match serde_json::from_str::<Value>(&w.to_text()) { Ok(v) => Val::from_json(v), Err(_) => Val::Null }
                    }
                    Mode::Js => self.eval_js(&r, &result)?,
                    Mode::Json => Val::List(self.json_of(&result).get_list(&r).into_iter().map(Val::from_json).collect()),
                    Mode::XPath => Val::List(Self::xpath_elems(&result.to_text(), &r)?.into_iter().map(Val::Str).collect()),
                    Mode::Default => Val::List(jsoup::elements(&result.to_text(), &r).into_iter().map(Val::Str).collect()),
                };
            }
        }
        Ok(match result {
            Val::List(items) => items.into_iter().filter(|v| !v.is_null()).collect(),
            _ => Vec::new(),
        })
    }
}

fn merge_groups<T: Clone>(groups: Vec<Vec<T>>, et: &str) -> Vec<T> {
    let mut out = Vec::new();
    if groups.is_empty() { return out; }
    if et == "%%" {
        for i in 0..groups[0].len() {
            for g in &groups { if i < g.len() { out.push(g[i].clone()); } }
        }
    } else {
        for g in groups { out.extend(g); }
    }
    out
}

fn split_not_blank(s: &str, sep: &str) -> Vec<String> {
    s.split(sep).filter(|x| !x.trim().is_empty()).map(|x| x.to_string()).collect()
}

// ───────────────────────── AnalyzeByRegex ─────────────────────────

fn group_texts(text: &str, caps: &util::Caps) -> Vec<String> {
    caps.iter().map(|g| g.map(|(s, e)| text[s..e].to_string()).unwrap_or_default()).collect()
}

/// 对应 `AnalyzeByRegex.getElement`：多段正则逐级缩小，最后一段取分组
pub fn regex_get_element(res: &str, regs: &[String], index: usize) -> Option<Vec<String>> {
    let re = jregex(regs.get(index)?)?;
    let all = re.captures_all(res);
    if all.is_empty() { return None; }
    if index + 1 == regs.len() {
        return Some(group_texts(res, &all[0]));
    }
    let joined: String = all.iter().filter_map(|c| c[0].map(|(s, e)| &res[s..e])).collect();
    regex_get_element(&joined, regs, index + 1)
}

/// 对应 `AnalyzeByRegex.getElements`
pub fn regex_get_elements(res: &str, regs: &[String], index: usize) -> Vec<Vec<String>> {
    let Some(pat) = regs.get(index) else { return vec![] };
    let Some(re) = jregex(pat) else { return vec![] };
    let all = re.captures_all(res);
    if all.is_empty() { return vec![]; }
    if index + 1 == regs.len() {
        return all.iter().map(|c| group_texts(res, c)).collect();
    }
    let joined: String = all.iter().filter_map(|c| c[0].map(|(s, e)| &res[s..e])).collect();
    regex_get_elements(&joined, regs, index + 1)
}

#[allow(dead_code)]
fn _fmt_num_used(n: f64) -> String { fmt_num(n) }

#[cfg(test)]
mod legado_compat_tests {
    use super::*;
    fn ctx(html: &str) -> RuleContext { let mut c = RuleContext::new(html, "http://a.com/x/y.html"); c.set_redirect_url("http://a.com/x/y.html"); c }

    #[test]
    fn put_get_json_syntax() {
        let mut c = ctx("<div><a href='/b/1'>书名</a></div>");
        let r = c.get_string(r#"a@text@put:{"k":"a@href"}"#).unwrap();
        assert_eq!(r, "书名");
        assert_eq!(c.get_string("@get:{k}").unwrap(), "/b/1");
    }
    #[test]
    fn lookaround_and_first_only() {
        let mut c = ctx("<p>abc123def456</p>");
        assert_eq!(c.get_string(r"p@text##(?<=abc)\d+(?=def)##X").unwrap(), "abcXdef456");
        assert_eq!(c.get_string(r"p@text##\d+##N###").unwrap(), "N");
    }
    #[test]
    fn json_without_prefix_and_js_array() {
        let mut c = ctx(r#"{"data":{"list":[{"n":"a"},{"n":"b"}]}}"#);
        assert_eq!(c.get_string_list("data.list[*].n").unwrap(), vec!["a","b"]);
        assert_eq!(c.get_string("$.data.list[?(@.n=='b')].n").unwrap(), "b");
    }
    #[test]
    fn inline_rule_and_expr() {
        let mut c = ctx("<a href='/z'>t</a>");
        assert_eq!(c.get_string("{{@css:a@href}}!").unwrap(), "/z!");
        assert_eq!(c.get_string("{{1+2}}").unwrap(), "3");
    }
    #[test]
    fn relative_url() {
        let mut c = ctx("<a href='../z/1.html'>t</a>");
        assert_eq!(c.get_string_url("a@href").unwrap(), "http://a.com/z/1.html");
    }
}
