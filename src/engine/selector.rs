//! CSS / XPath / JSONPath / 正则 四种选择器。
//! CSS 用 scraper；XPath 做轻量实现（覆盖 //tag 和 //tag[@attr='v']）；JSONPath 自实现。

use crate::engine::value::EvalValue;
use crate::error::{AppError, AppResult};
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;

/// CSS 选择器 + 输出修饰符
/// rule 形如: "td.odd a@href" / "h1@text" / "#ccontent@html"
pub fn css_select(html: &str, rule: &str) -> AppResult<EvalValue> {
    let (selector_part, modifier) = match rule.rfind('@') {
        Some(i) => (&rule[..i], &rule[i + 1..]),
        None => (rule, ""),
    };
    let selector_part = selector_part.trim();
    if selector_part.is_empty() {
        return Ok(EvalValue::Str(html.to_string()));
    }
    // 完整 HTML 用 parse_document；片段需要包装上下文，
    // 否则 <tr>/<td> 在根级会被解析器丢弃。
    let head: String = html.chars().take(512).collect();
    let head_lower = head.to_ascii_lowercase();
    let is_full_doc = head_lower.contains("<html");
    let selector = Selector::parse(selector_part)
        .map_err(|e| AppError::bad_request(format!("CSS 选择器 '{}' 无效: {:?}", selector_part, e)))?;

    let elements: Vec<String> = if is_full_doc {
        let document = Html::parse_document(html);
        document.select(&selector).map(|e| e.html()).collect()
    } else {
        let wrapped = if head_lower.contains("<tr") || head_lower.contains("<td") {
            format!("<table>{}</table>", html)
        } else {
            format!("<div>{}</div>", html)
        };
        let doc = Html::parse_document(&wrapped);
        doc.select(&selector).map(|e| e.html()).collect()
    };

    if modifier.is_empty() {
        // 返回元素列表本身（供后续取属性/文本）
        return Ok(EvalValue::Elements(elements));
    }

    let mut results = Vec::new();
    let re_script = Regex::new(r"(?is)<script[\s\S]*?</script>|<style[\s\S]*?</style>").unwrap();
    match modifier {
        "text" | "ownText" => {
            for html in &elements {
                let t = crate::engine::value::element_text(html);
                if !t.is_empty() { results.push(EvalValue::Str(t)); }
            }
        }
        "textNodes" => {
            for html in &elements {
                let doc = Html::parse_fragment(html);
                let parts: Vec<String> = doc.root_element().text()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty()).collect();
                if !parts.is_empty() { results.push(EvalValue::Str(parts.join("\n"))); }
            }
        }
        "html" | "all" => {
            for html in &elements {
                let frag = re_script.replace_all(html, "").to_string();
                let frag = if frag.trim().is_empty() { html.clone() } else { frag };
                if !frag.trim().is_empty() { results.push(EvalValue::Str(frag)); }
            }
        }
        attr => {
            for html in &elements {
                for v in crate::engine::value::element_attr_or_text(html, attr) {
                    if !results.iter().any(|x| x.as_str() == v) {
                        results.push(EvalValue::Str(v));
                    }
                }
            }
        }
    }
    Ok(EvalValue::List(results))
}

/// 轻量 XPath：支持 //tag、//tag/@attr、//tag[cond]、相对路径 .//tag
pub fn xpath_select(html: &str, rule: &str) -> AppResult<EvalValue> {
    // 对于简单 //tag[@attr='v'] 用 CSS 转换
    let mut css = rule.to_string();
    if css.starts_with(".//") { css = css[2..].to_string(); }
    if css.starts_with("//") { css = css[2..].to_string(); }
    // 提取末尾 /@attr 或 /text()
    let modifier: String;
    if let Some(i) = css.rfind("/@") {
        modifier = css[i + 2..].to_string();
        css = css[..i].to_string();
    } else if css.ends_with("/text()") {
        modifier = "text".to_string();
        css = css[..css.len() - 7].to_string();
    } else {
        modifier = "text".to_string();
    }
    // [条件] 转 CSS: [@class='x'] -> [class="x"], [contains(@class,'x')] -> [class*="x"]
    let re_attr = Regex::new(r"\[@([\w-]+)\s*=\s*'([^']*)'\]").unwrap();
    css = re_attr.replace_all(&css, "[$1=\"$2\"]").to_string();
    let re_contains = Regex::new(r"\[contains\(@([\w-]+)\s*,\s*'([^']*)'\)\]").unwrap();
    css = re_contains.replace_all(&css, "[$1*=\"$2\"]").to_string();

    let full = format!("{}@{}", css, modifier);
    css_select(html, &full)
}

/// JSONPath：支持 $.a.b[0]、$.a[*].b、$.a[0]
pub fn json_select(json: &Value, path: &str) -> EvalValue {
    let mut path = path.trim();
    if path.starts_with("@Json:") { path = &path[6..]; }
    let tokens = tokenize_jsonpath(path);
    let mut current = vec![json.clone()];
    for tok in &tokens {
        let mut next = Vec::new();
        for cur in current {
            match tok {
                Tok::Key(k) => {
                    if let Value::Object(m) = cur {
                        if let Some(v) = m.get(k) { next.push(v.clone()); }
                    }
                }
                Tok::Index(i) => {
                    if let Value::Array(arr) = cur {
                        if let Some(v) = arr.get(*i) { next.push(v.clone()); }
                    }
                }
                Tok::Wildcard => {
                    match cur {
                        Value::Array(arr) => next.extend(arr.iter().cloned()),
                        Value::Object(m) => next.extend(m.values().cloned()),
                        _ => {}
                    }
                }
            }
        }
        current = next;
    }
    if current.len() == 1 {
        let v = current.remove(0);
        // JSONPath 根 `$` 指向数组时，展开为列表（Legado 书源常见写法：bookList="$"）
        if path.trim() == "$" {
            if let Value::Array(arr) = v {
                return EvalValue::List(arr.into_iter().map(EvalValue::Json).collect());
            }
        }
        EvalValue::Json(v)
    }
    else { EvalValue::List(current.into_iter().map(EvalValue::Json).collect()) }
}

#[derive(Debug)]
enum Tok { Key(String), Index(usize), Wildcard }

fn tokenize_jsonpath(path: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut s = path;
    if s.starts_with("$.") { s = &s[2..]; }
    else if s == "$" { return toks; }
    else if s.starts_with('$') { s = &s[1..]; }
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '.' { i += 1; continue; }
        if chars[i] == '[' {
            let end = chars.iter().position(|&c| c == ']').map(|p| p).unwrap_or(chars.len());
            let inner: String = chars[i + 1..end].iter().collect();
            if inner == "*" { toks.push(Tok::Wildcard); }
            else if let Ok(n) = inner.parse::<usize>() { toks.push(Tok::Index(n)); }
            else { toks.push(Tok::Key(inner.trim_matches('"').trim_matches('\'').to_string())); }
            i = end + 1;
        } else {
            let start = i;
            while i < chars.len() && chars[i] != '.' && chars[i] != '[' { i += 1; }
            let key: String = chars[start..i].iter().collect();
            if !key.is_empty() { toks.push(Tok::Key(key)); }
        }
    }
    toks
}

/// 正则提取：支持 "regex" 与 "regex##replacement"
pub fn regex_extract(text: &str, pattern: &str, replacement: &str) -> AppResult<EvalValue> {
    let re = Regex::new(pattern)
        .map_err(|e| AppError::bad_request(format!("正则无效 '{}': {}", pattern, e)))?;
    if !replacement.is_empty() {
        let out = re.replace_all(text, replacement).to_string();
        return Ok(EvalValue::Str(out));
    }
    // 有捕获组返回组列表，否则返回匹配列表
    let mut results = Vec::new();
    for cap in re.captures_iter(text) {
        if cap.len() > 1 {
            for g in cap.iter().skip(1).flatten() { results.push(EvalValue::Str(g.as_str().to_string())); }
        } else {
            results.push(EvalValue::Str(cap.get(0).unwrap().as_str().to_string()));
        }
    }
    Ok(EvalValue::List(results))
}

/// 对字符串做替换（##regex##replacement）
pub fn apply_replace(value: String, regex: &str, replacement: &str, first: bool) -> String {
    if regex.is_empty() { return value; }
    let Ok(re) = Regex::new(regex) else { return value; };
    if first { re.replace(&value, replacement).to_string() }
    else { re.replace_all(&value, replacement).to_string() }
}
