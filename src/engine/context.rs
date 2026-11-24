//! AnalyzeRule 求值器：规则切分 → 模式路由 → 内联 JS → 替换清洗。

use crate::cookie::CookieStore;
use crate::engine::analyzer::{parse_rule, Mode, ParsedRule, RulePiece, Segment};
use crate::engine::js::{self, new_var_store, JsContext, VarStore};
use crate::engine::selector::{apply_replace, css_select, json_select, regex_extract};
use crate::engine::jsoup;
use crate::engine::value::{css_elements, EvalValue};
use crate::engine::xpath::{xpath_elements, xpath_select};
use crate::error::{AppError, AppResult};
use serde_json::Value;

pub struct RuleContext {
    pub html: String,
    pub base_url: String,
    pub js_lib: String,
    /// 与 JS 层共享的变量表（java.put / source.setVariable / 规则 @put 共用）
    pub variables: VarStore,
    pub fetch: Option<crate::engine::js::JsFetchFn>,
    pub webview: Option<crate::engine::js::JsWebviewFn>,
    pub cookies: CookieStore,
    pub chapter_title: String,
}

impl RuleContext {
    pub fn new(html: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            html: html.into(), base_url: base_url.into(), js_lib: String::new(),
            variables: new_var_store(), fetch: None, webview: None, cookies: CookieStore::new(),
            chapter_title: String::new(),
        }
    }

    fn var(&self, key: &str) -> Option<String> {
        self.variables.lock().ok().and_then(|m| m.get(key).cloned())
    }
    fn set_var(&self, key: String, value: String) {
        if let Ok(mut m) = self.variables.lock() { m.insert(key, value); }
    }

    fn js_ctx(&self, result: impl Into<String>) -> JsContext {
        let login_info = new_var_store();
        if let Some(raw) = self.var("loginInfo") {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
                if let Ok(mut m) = login_info.lock() {
                    for (k, v) in map {
                        m.insert(k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string());
                    }
                }
            }
        }
        JsContext {
            js_lib: self.js_lib.clone(),
            result: result.into(),
            base_url: self.base_url.clone(),
            source_url: self.var("sourceUrl").unwrap_or_default(),
            source_name: self.var("sourceName").unwrap_or_default(),
            book_name: self.var("bookName").unwrap_or_default(),
            chapter_title: self.chapter_title.clone(),
            variables: self.variables.clone(),
            fetch: self.fetch.clone(),
            webview: self.webview.clone(),
            cookies: self.cookies.clone(),
            key: self.var("key").unwrap_or_default(),
            page: self.var("page").and_then(|v| v.parse().ok()).unwrap_or(1),
            login_info,
            ..Default::default()
        }
    }

    pub fn get_string(&mut self, rule: &str) -> AppResult<String> {
        if rule.is_empty() { return Ok(String::new()); }
        let parsed = parse_rule(rule);
        self.eval_string(&parsed)
    }

    pub fn get_string_list(&mut self, rule: &str) -> AppResult<Vec<String>> {
        if rule.is_empty() { return Ok(vec![]); }
        let parsed = parse_rule(rule);
        self.eval_list(&parsed)
    }

    /// 求元素列表（返回每个元素的 outerHTML）。组合符语义同 eval_pieces。
    pub fn get_elements(&mut self, rule: &str) -> AppResult<Vec<String>> {
        if rule.is_empty() { return Ok(vec![]); }
        let parsed = parse_rule(rule);
        let pieces = &parsed.pieces;
        let mut results: Vec<Vec<String>> = Vec::new();
        for (i, piece) in pieces.iter().enumerate() {
            if i > 0
                && parsed.separators.get(i - 1).map(|s| s.as_str()) == Some("||")
                && !results.is_empty()
            {
                break;
            }
            let els = if let Some(js) = &piece.js_prefix {
                // <js>...</js> 前缀：先执行脚本，结果作为后续选择器的求值内容
                let jc = self.js_ctx(self.html.clone());
                let result = js::eval_script(js, &jc)?;
                let mut sub = RuleContext::new(result, self.base_url.clone());
                sub.js_lib = self.js_lib.clone();
                sub.variables = self.variables.clone();
                sub.fetch = self.fetch.clone();
                sub.webview = self.webview.clone();
                sub.cookies = self.cookies.clone();
                let selector = sub.expand_segments(piece)?;
                sub.select_elements(&selector, piece.mode)?
            } else {
                let selector = self.expand_segments(piece)?;
                self.select_elements(&selector, piece.mode)?
            };
            for (k, expr) in &piece.put_map {
                let v = self.get_string(expr)?;
                self.set_var(k.clone(), v);
            }
            if !els.is_empty() { results.push(els); }
        }
        Ok(merge_results(results, &parsed.separators))
    }

    /// 从元素列表取字段值
    pub fn element_values(&mut self, elements: &[String], rule: &str) -> AppResult<Vec<String>> {
        if rule.is_empty() { return Ok(vec![]); }
        let is_json = rule == "$" || rule.starts_with("$.") || rule.starts_with("$[")
            || rule.to_ascii_lowercase().starts_with("@json:");
        let lower = rule.to_ascii_lowercase();
        let is_js = lower.starts_with("<js>") && lower.ends_with("</js>");
        if is_json || is_js {
            // JSON 元素 / 纯 JS 规则：用完整规则引擎（支持 $.field / <js> 脚本）
            let mut out = Vec::new();
            for html in elements {
                let mut sub = RuleContext::new(html.clone(), self.base_url.clone());
                sub.js_lib = self.js_lib.clone();
                sub.variables = self.variables.clone(); // 共享同一张表，JS 写入自动可见
                sub.fetch = self.fetch.clone();
                sub.webview = self.webview.clone();
                sub.cookies = self.cookies.clone();
                let values = sub.get_string_list(rule)?;
                for v in values { if !v.is_empty() { out.push(v); } }
            }
            return Ok(out);
        }
        // HTML/CSS 元素：走属性/文本提取器
        if let Some(idx) = rule.find("@js:") {
            let base_rule = &rule[..idx];
            let js_expr = &rule[idx + 4..];
            let mut out = Vec::new();
            for html in elements {
                for v in jsoup::result_list(html, base_rule) {
                    let jc = self.js_ctx(&v);
                    let r = js::eval(js_expr, &jc)?;
                    if !r.is_empty() { out.push(r); }
                }
            }
            return Ok(out);
        }
        let mut out = Vec::new();
        for html in elements { out.extend(jsoup::result_list(html, rule)); }
        Ok(out)
    }

    /// 逐 piece 求值并按分隔符合并。语义对齐 Legado `AnalyzeByJSoup`：
    /// - `&&`（默认）：所有非空结果顺序拼接（并集）
    /// - `||`（择一）：遇到第一个非空结果即停止
    /// - `%%`（交织）：按索引轮流从每组结果取值
    fn eval_pieces(&mut self, parsed: &ParsedRule) -> AppResult<Vec<String>> {
        let mut results: Vec<Vec<String>> = Vec::new();
        for (i, piece) in parsed.pieces.iter().enumerate() {
            // || 择一：前一分隔符为 || 且已有非空结果，停止求值后续规则
            if i > 0
                && parsed.separators.get(i - 1).map(|s| s.as_str()) == Some("||")
                && !results.is_empty()
            {
                break;
            }
            let val = self.eval_piece_value(piece)?;
            let mut group = Vec::new();
            for v in val.as_list() {
                let s = apply_piece_replace(&v.as_str(), piece);
                if !s.is_empty() { group.push(s); }
            }
            self.apply_puts(piece)?;
            if !group.is_empty() { results.push(group); }
        }
        Ok(merge_results(results, &parsed.separators))
    }

    fn eval_string(&mut self, parsed: &ParsedRule) -> AppResult<String> {
        Ok(self.eval_pieces(parsed)?.join("\n"))
    }

    fn eval_list(&mut self, parsed: &ParsedRule) -> AppResult<Vec<String>> {
        self.eval_pieces(parsed)
    }

    fn apply_puts(&mut self, piece: &RulePiece) -> AppResult<()> {
        for (k, expr) in &piece.put_map {
            let v = self.get_string(expr)?;
            self.set_var(k.clone(), v);
        }
        Ok(())
    }

    fn eval_piece_value(&mut self, piece: &RulePiece) -> AppResult<EvalValue> {
        let raw_selector = self.expand_segments(piece)?;

        // 处理 @js: 后缀（如 "#count@text@js:qsKind(result)"）
        if let Some(idx) = raw_selector.rfind("@js:") {
            let prefix = &raw_selector[..idx];
            let expr = &raw_selector[idx + 4..];
            let base_val = self.eval_selector(prefix, piece.mode)?;
            let mut out = Vec::new();
            for v in base_val.as_list() {
                let s = v.as_str();
                let jc = self.js_ctx(&s);
                let r = js::eval(expr, &jc)?;
                if !r.is_empty() { out.push(EvalValue::Str(r)); }
            }
            return Ok(EvalValue::List(out));
        }

        // Legado：默认模式下规则含 {{js}} / @get 时整体按 Regex 模式处理，
        // 内联求值后的文本就是结果本身（不再当选择器）。
        let has_inline = piece.segments.iter()
            .any(|s| matches!(s, Segment::Js(_) | Segment::GetVar(_)));
        if has_inline && piece.mode == Mode::Default {
            return Ok(EvalValue::Str(raw_selector));
        }

        self.eval_selector(&raw_selector, piece.mode)
    }

    fn eval_selector(&mut self, selector: &str, mode: Mode) -> AppResult<EvalValue> {
        match mode {
            Mode::Js => {
                let jc = self.js_ctx(self.html.clone());
                Ok(EvalValue::Str(js::eval(selector, &jc)?))
            }
            Mode::Json => {
                let json: Value = serde_json::from_str(&self.html).unwrap_or(Value::Null);
                Ok(json_select(&json, selector))
            }
            Mode::XPath => xpath_select(&self.html, selector),
            Mode::Regex => regex_extract(&self.html, selector, ""),
            Mode::WebJs => {
                let jc = self.js_ctx(self.html.clone());
                Ok(EvalValue::Str(js::eval(selector, &jc)?))
            }
            Mode::Default => {
                // Legado 默认语法（class./tag./id./children/索引 + CSS 回退）
                Ok(EvalValue::List(
                    jsoup::result_list(&self.html, selector)
                        .into_iter().map(EvalValue::Str).collect(),
                ))
            }
            Mode::Css => {
                match css_select(&self.html, selector) {
                    Ok(v) => Ok(v),
                    Err(_) => {
                        // 内联 {{...}} 求值后可能是纯文本（如 "{{$.origin}} {{$.kind}}"），
                        // 不是合法 CSS 选择器，此时直接把展开结果作为值返回
                        Ok(EvalValue::Str(selector.to_string()))
                    }
                }
            }
        }
    }

    fn expand_segments(&mut self, piece: &RulePiece) -> AppResult<String> {
        let mut s = String::new();
        for seg in &piece.segments {
            match seg {
                Segment::Text(t) => s.push_str(t),
                Segment::Js(expr) => {
                    let jc = self.js_ctx(self.html.clone());
                    s.push_str(&js::eval(expr, &jc)?);
                }
                Segment::GetVar(name) => {
                    if let Some(v) = self.var(name) { s.push_str(&v); }
                }
                Segment::RegexGroup(n) => s.push_str(&format!("${}", n)),
            }
        }
        Ok(s)
    }

    fn select_elements(&self, selector: &str, mode: Mode) -> AppResult<Vec<String>> {
        if selector.is_empty() { return Ok(vec![]); }
        match mode {
            Mode::Default => Ok(jsoup::elements(&self.html, selector)),
            Mode::Css => {
                css_elements(&self.html, selector.trim()).map_err(AppError::bad_request)
            }
            Mode::XPath => xpath_elements(&self.html, selector),
            Mode::Json => {
                let json: Value = serde_json::from_str(&self.html).unwrap_or(Value::Null);
                let val = json_select(&json, selector);
                Ok(val.as_list().iter().map(|v| v.as_str()).collect())
            }
            Mode::Js | Mode::WebJs => {
                let jc = self.js_ctx(String::new());
                let r = js::eval(selector, &jc)?;
                if r.is_empty() { Ok(vec![]) } else { Ok(vec![r]) }
            }
            Mode::Regex => Ok(vec![]),
        }
    }
}

fn apply_piece_replace(s: &str, piece: &crate::engine::analyzer::RulePiece) -> String {
    apply_replace(s.to_string(), &piece.replace_regex, &piece.replacement, piece.replace_first)
}

/// 按分隔符合并各规则段的结果，对齐 Legado `AnalyzeByJSoup`：
/// - 任一分隔符为 `%%`：按索引交织（i=0 取各组第 0 个，i=1 取各组第 1 个……）
/// - 其余（`&&` 或单规则）：顺序拼接所有非空组
/// （`||` 择一已在调用方循环里通过 break 实现）
pub(crate) fn merge_results(results: Vec<Vec<String>>, separators: &[String]) -> Vec<String> {
    if results.is_empty() { return Vec::new(); }
    if separators.iter().any(|s| s == "%%") {
        let max = results.iter().map(|g| g.len()).max().unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..max {
            for group in &results {
                if i < group.len() { out.push(group[i].clone()); }
            }
        }
        out
    } else {
        results.into_iter().flatten().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sep(s: &str) -> Vec<String> { vec![s.to_string()] }

    #[test]
    fn merge_and_concatenates() {
        let r = merge_results(vec![vec!["a".into(), "b".into()], vec!["c".into()]], &sep("&&"));
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn merge_interleave() {
        // 官方 %% 交织：i=0 取各组第0个，i=1 取各组第1个……
        let r = merge_results(
            vec![vec!["a1".into(), "a2".into()], vec!["b1".into(), "b2".into()]],
            &sep("%%"),
        );
        assert_eq!(r, vec!["a1", "b1", "a2", "b2"]);
        // 长度不等时不越界
        let r2 = merge_results(
            vec![vec!["a1".into(), "a2".into()], vec!["b1".into()]],
            &sep("%%"),
        );
        assert_eq!(r2, vec!["a1", "b1", "a2"]);
    }

    #[test]
    fn chained_text_rule() {
        // a@text 多步链：从 <div><a>标题</a></div> 中取 a 的文本
        let out = jsoup::result_list("<div><a href='/x'>标题</a></div>", "a@text");
        assert_eq!(out, vec!["标题"]);
        let out2 = jsoup::result_list("<div><a href='/x'>标题</a></div>", "a@href");
        assert_eq!(out2, vec!["/x"]);
    }

    #[test]
    fn eval_and_or_rules() {
        let mut ctx = RuleContext::new("<div><p class='a'>A</p><p class='b'>B</p></div>", "http://x/");
        // && 并集：两个选择器结果拼接
        let s = ctx.get_string("p.a@text&&p.b@text").unwrap();
        assert_eq!(s, "A\nB");
        // || 择一：第一个有结果即停
        let s2 = ctx.get_string("p.a@text||p.b@text").unwrap();
        assert_eq!(s2, "A");
        // %% 交织
        let s3 = ctx.get_string("p.a@text%%p.b@text").unwrap();
        assert_eq!(s3, "A\nB");
    }
}
