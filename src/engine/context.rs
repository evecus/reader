//! AnalyzeRule 求值器：规则切分 → 模式路由 → 内联 JS → 替换清洗。

use crate::engine::analyzer::{parse_rule, Mode, ParsedRule, RulePiece, Segment};
use crate::engine::js::{self, JsContext};
use crate::engine::selector::{apply_replace, css_select, json_select, regex_extract};
use crate::engine::value::{css_elements, element_attr_or_text, EvalValue};
use crate::engine::xpath::{xpath_elements, xpath_select};
use crate::error::{AppError, AppResult};
use serde_json::Value;
use std::collections::HashMap;

pub struct RuleContext {
    pub html: String,
    pub base_url: String,
    pub js_lib: String,
    pub variables: HashMap<String, String>,
    pub fetch: Option<std::sync::Arc<dyn Fn(&str, &str) -> String + Send + Sync>>,
    pub chapter_title: String,
}

impl RuleContext {
    pub fn new(html: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            html: html.into(), base_url: base_url.into(), js_lib: String::new(),
            variables: HashMap::new(), fetch: None, chapter_title: String::new(),
        }
    }

    fn js_ctx(&self, result: impl Into<String>) -> JsContext {
        JsContext {
            js_lib: self.js_lib.clone(),
            result: result.into(),
            base_url: self.base_url.clone(),
            source_url: String::new(),
            source_name: String::new(),
            book_name: self.variables.get("bookName").cloned().unwrap_or_default(),
            chapter_title: self.chapter_title.clone(),
            variables: self.variables.clone(),
            fetch: self.fetch.clone(),
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

    /// 求元素列表（返回每个元素的 outerHTML）
    pub fn get_elements(&mut self, rule: &str) -> AppResult<Vec<String>> {
        if rule.is_empty() { return Ok(vec![]); }
        let pieces: Vec<RulePiece> = parse_rule(rule).pieces;
        let mut acc: Vec<String> = Vec::new();
        for piece in &pieces {
            let selector = self.expand_segments(piece)?;
            let els = self.select_elements(&selector, piece.mode)?;
            for (k, expr) in &piece.put_map {
                let v = self.get_string(expr)?;
                self.variables.insert(k.clone(), v);
            }
            acc.extend(els);
        }
        Ok(acc)
    }

    /// 从元素 HTML 列表取属性/文本
    pub fn element_values(&mut self, elements: &[String], rule: &str) -> AppResult<Vec<String>> {
        if let Some(idx) = rule.find("@js:") {
            let base_rule = &rule[..idx];
            let js_expr = &rule[idx + 4..];
            let mut out = Vec::new();
            for html in elements {
                let base_vals = element_attr_or_text(html, base_rule);
                for v in base_vals {
                    let jc = self.js_ctx(&v);
                    let r = js::eval(js_expr, &jc)?;
                    if !r.is_empty() { out.push(r); }
                }
            }
            return Ok(out);
        }
        let mut out = Vec::new();
        for html in elements { out.extend(element_attr_or_text(html, rule)); }
        Ok(out)
    }

    fn eval_string(&mut self, parsed: &ParsedRule) -> AppResult<String> {
        let mut outputs: Vec<String> = Vec::new();
        for (i, piece) in parsed.pieces.iter().enumerate() {
            if i > 0 && parsed.separators.get(i - 1).map(|s| s.as_str()) == Some("||")
                && !outputs.is_empty() { break; }
            let val = self.eval_piece_value(piece)?;
            for v in val.as_list() {
                let s = apply_piece_replace(&v.as_str(), piece);
                if !s.is_empty() { outputs.push(s); }
            }
            self.apply_puts(piece)?;
        }
        Ok(outputs.join("\n"))
    }

    fn eval_list(&mut self, parsed: &ParsedRule) -> AppResult<Vec<String>> {
        let mut outputs: Vec<String> = Vec::new();
        for (i, piece) in parsed.pieces.iter().enumerate() {
            if i > 0 && parsed.separators.get(i - 1).map(|s| s.as_str()) == Some("||")
                && !outputs.is_empty() { break; }
            let val = self.eval_piece_value(piece)?;
            for v in val.as_list() {
                let s = apply_piece_replace(&v.as_str(), piece);
                if !s.is_empty() { outputs.push(s); }
            }
            self.apply_puts(piece)?;
        }
        Ok(outputs)
    }

    fn apply_puts(&mut self, piece: &RulePiece) -> AppResult<()> {
        for (k, expr) in &piece.put_map {
            let v = self.get_string(expr)?;
            self.variables.insert(k.clone(), v);
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

        self.eval_selector(&raw_selector, piece.mode)
    }

    fn eval_selector(&mut self, selector: &str, mode: Mode) -> AppResult<EvalValue> {
        match mode {
            Mode::Js => {
                let jc = self.js_ctx(String::new());
                Ok(EvalValue::Str(js::eval(selector, &jc)?))
            }
            Mode::Json => {
                let json: Value = serde_json::from_str(&self.html).unwrap_or(Value::Null);
                Ok(json_select(&json, selector))
            }
            Mode::XPath => xpath_select(&self.html, selector),
            Mode::Regex => regex_extract(&self.html, selector, ""),
            Mode::WebJs => {
                let jc = self.js_ctx(String::new());
                Ok(EvalValue::Str(js::eval(selector, &jc)?))
            }
            Mode::Default => css_select(&self.html, selector),
        }
    }

    fn expand_segments(&mut self, piece: &RulePiece) -> AppResult<String> {
        let mut s = String::new();
        for seg in &piece.segments {
            match seg {
                Segment::Text(t) => s.push_str(t),
                Segment::Js(expr) => {
                    let jc = self.js_ctx(String::new());
                    s.push_str(&js::eval(expr, &jc)?);
                }
                Segment::GetVar(name) => {
                    if let Some(v) = self.variables.get(name).cloned() { s.push_str(&v); }
                }
                Segment::RegexGroup(n) => s.push_str(&format!("${}", n)),
            }
        }
        Ok(s)
    }

    fn select_elements(&self, selector: &str, mode: Mode) -> AppResult<Vec<String>> {
        if selector.is_empty() { return Ok(vec![]); }
        match mode {
            Mode::Default => {
                let sel = selector.trim_start_matches("@CSS:").trim_start_matches("@@");
                css_elements(&self.html, sel).map_err(AppError::bad_request)
            }
            Mode::XPath => xpath_elements(&self.html, selector),
            _ => Ok(vec![]),
        }
    }
}

fn apply_piece_replace(s: &str, piece: &RulePiece) -> String {
    apply_replace(s.to_string(), &piece.replace_regex, &piece.replacement, piece.replace_first)
}
