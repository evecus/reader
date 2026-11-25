//! 引擎通用工具：对齐 Legado `StringExtensions` / `NetworkUtils` / Java 正则语义。

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ───────────────────────── 字符串判定 ─────────────────────────

/// 对应 `String.isJson()`：首尾是 `{}` 或 `[]`
pub fn is_json(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'))
}

pub fn is_json_object(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('{') && t.ends_with('}')
}

pub fn is_json_array(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('[') && t.ends_with(']')
}

pub fn is_xml(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('<') && t.ends_with('>')
}

/// 对应 `String?.isTrue(nullIsTrue)`：空/"null" 取 null_is_true，false|no|not|0|0.0 为假
pub fn is_true(s: &str, null_is_true: bool) -> bool {
    let t = s.trim();
    if t.is_empty() || t == "null" { return null_is_true; }
    !matches!(t.to_ascii_lowercase().as_str(), "false" | "no" | "not" | "0" | "0.0")
}

/// `StringEscapeUtils.unescapeHtml4`
pub fn unescape_html(s: &str) -> String {
    if !s.contains('&') { return s.to_string(); }
    html_escape::decode_html_entities(s).into_owned()
}

// ───────────────────────── URL ─────────────────────────

/// 对齐 `AnalyzeUrl.paramPattern`（`\s*,\s*(?=\{)`）：返回（URL 部分, 选项部分起点）
pub fn split_url_option(s: &str) -> (&str, Option<&str>) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b',' {
            // 向后跳空白看是不是 '{'
            let mut j = i + 1;
            while j < b.len() && (b[j] as char).is_whitespace() { j += 1; }
            if j < b.len() && b[j] == b'{' {
                // 向前吃掉逗号前的空白
                let mut st = i;
                while st > 0 && (b[st - 1] as char).is_whitespace() { st -= 1; }
                return (&s[..st], Some(&s[j..]));
            }
        }
        i += 1;
    }
    (s, None)
}

fn is_abs_url(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

fn is_data_url(s: &str) -> bool {
    s.len() >= 5 && s[..5].eq_ignore_ascii_case("data:")
}

/// 对齐 `NetworkUtils.getAbsoluteURL(baseURL, relativePath)`：
/// - base 为空：原样返回（trim）
/// - base 里 `,` 之后的内容（URL 选项）被丢弃
/// - 绝对/ data: 直接返回；`javascript` 开头返回空
/// - 否则按 RFC3986 解析（`../`、`./`、`?q`、`//host`、`/path` 都正确）
pub fn absolute_url(base: &str, relative: &str) -> String {
    let rel = relative.trim();
    if base.is_empty() { return rel.to_string(); }
    let base = base.split(',').next().unwrap_or(base);
    let Ok(b) = url::Url::parse(base) else { return rel.to_string(); };
    if is_abs_url(rel) || is_data_url(rel) { return rel.to_string(); }
    if rel.to_ascii_lowercase().starts_with("javascript") { return String::new(); }
    match b.join(relative) {
        Ok(u) => u.to_string(),
        Err(_) => rel.to_string(),
    }
}

/// 对齐 `NetworkUtils.getBaseUrl`：`scheme://authority`
pub fn base_url_of(url: &str) -> Option<String> {
    let u = match url.to_ascii_lowercase().find("@js:") { Some(i) => &url[..i], None => url };
    let lower = u.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) { return None; }
    let parsed = url::Url::parse(u).ok()?;
    let host = parsed.host_str()?;
    if host.is_empty() { return None; }
    let scheme = &u[..u.find("://")?];
    let auth = match parsed.port() {
        Some(p) => format!("{}:{}", host, p),
        None => host.to_string(),
    };
    Some(format!("{}://{}", scheme, auth))
}

// ───────────────────────── Java 风格正则 ─────────────────────────

/// 同时支持 `regex` 与 `fancy-regex`（前/后断言、反向引用）。
/// Java 的 `Pattern` 有这些特性，书源里很常见；先走快的 `regex`，编译失败再回退。
pub enum JRegex {
    Std(Regex),
    Fancy(FancyRegex),
}

/// 一次匹配：各捕获组的字节区间（0 号为整体）
pub type Caps = Vec<Option<(usize, usize)>>;

impl JRegex {
    pub fn new(pat: &str) -> Option<JRegex> {
        if let Ok(r) = Regex::new(pat) { return Some(JRegex::Std(r)); }
        FancyRegex::new(pat).ok().map(JRegex::Fancy)
    }

    /// 所有匹配（不重叠，从左到右）
    pub fn captures_all(&self, text: &str) -> Vec<Caps> {
        match self {
            JRegex::Std(r) => r.captures_iter(text)
                .map(|c| c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect())
                .collect(),
            JRegex::Fancy(r) => {
                let mut out = Vec::new();
                for c in r.captures_iter(text).flatten() {
                    out.push(c.iter().map(|m| m.map(|m| (m.start(), m.end()))).collect());
                }
                out
            }
        }
    }

    pub fn is_match(&self, text: &str) -> bool {
        match self {
            JRegex::Std(r) => r.is_match(text),
            JRegex::Fancy(r) => r.is_match(text).unwrap_or(false),
        }
    }
}

/// 带缓存的编译（失败也缓存为 None）
pub fn jregex(pat: &str) -> Option<std::sync::Arc<JRegex>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<std::sync::Arc<JRegex>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = g.get(pat) { return v.clone(); }
    if g.len() > 512 { g.clear(); }
    let v = JRegex::new(pat).map(std::sync::Arc::new);
    g.insert(pat.to_string(), v.clone());
    v
}

/// 展开 Java `Matcher.appendReplacement` 风格的替换串：`$n` 取组，`\x` 转义。
/// `$12` 在组 12 存在时取 12 号组，否则取 1 号组后接字符 '2'（Java 语义）。
fn expand_replacement(text: &str, caps: &Caps, rep: &str, out: &mut String) {
    let rc: Vec<char> = rep.chars().collect();
    let mut i = 0;
    while i < rc.len() {
        let c = rc[i];
        if c == '\\' && i + 1 < rc.len() {
            out.push(rc[i + 1]);
            i += 2;
        } else if c == '$' && i + 1 < rc.len() && rc[i + 1].is_ascii_digit() {
            let mut n = rc[i + 1].to_digit(10).unwrap() as usize;
            i += 2;
            while i < rc.len() && rc[i].is_ascii_digit() {
                let nn = n * 10 + rc[i].to_digit(10).unwrap() as usize;
                if nn < caps.len() { n = nn; i += 1; } else { break; }
            }
            if let Some(Some((s, e))) = caps.get(n) { out.push_str(&text[*s..*e]); }
        } else {
            out.push(c);
            i += 1;
        }
    }
}

/// 对应 Kotlin `String.replace(Regex, replacement)`：全部替换
pub fn replace_all(text: &str, re: &JRegex, rep: &str) -> String {
    let all = re.captures_all(text);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in &all {
        let Some((s, e)) = caps[0] else { continue };
        if s < last { continue; }
        out.push_str(&text[last..s]);
        expand_replacement(text, caps, rep, &mut out);
        last = e;
    }
    out.push_str(&text[last..]);
    out
}

/// 只替换第一个匹配
pub fn replace_first(text: &str, re: &JRegex, rep: &str) -> String {
    let all = re.captures_all(text);
    let Some(caps) = all.first() else { return text.to_string(); };
    let Some((s, e)) = caps[0] else { return text.to_string(); };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..s]);
    expand_replacement(text, caps, rep, &mut out);
    out.push_str(&text[e..]);
    out
}

/// 对齐 `AnalyzeRule.replaceRegex`：
/// - `##re##rep`：全部替换；正则无效时退化为字面量替换
/// - `##re##rep###`（replace_first）：只保留第一个匹配并替换，没有匹配得空串；正则无效返回 rep
pub fn rule_replace(result: &str, regex: &str, replacement: &str, first: bool) -> String {
    if regex.is_empty() { return result.to_string(); }
    let re = jregex(regex);
    if first {
        match re {
            Some(re) => {
                let all = re.captures_all(result);
                match all.first().and_then(|c| c[0].map(|(s, e)| (c.clone(), s, e))) {
                    Some((caps, s, e)) => {
                        // Kotlin: matcher.group(0).replaceFirst(regex, replacement)
                        let whole = &result[s..e];
                        let sub_caps = re.captures_all(whole);
                        let _ = caps;
                        match sub_caps.first() {
                            Some(c) => {
                                let mut out = String::new();
                                let (ms, me) = c[0].unwrap_or((0, whole.len()));
                                out.push_str(&whole[..ms]);
                                expand_replacement(whole, c, replacement, &mut out);
                                out.push_str(&whole[me..]);
                                out
                            }
                            None => whole.to_string(),
                        }
                    }
                    None => String::new(),
                }
            }
            None => replacement.to_string(),
        }
    } else {
        match re {
            Some(re) => replace_all(result, &re, replacement),
            None => result.replace(regex, replacement),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_url_rfc3986() {
        assert_eq!(absolute_url("http://a.com/book/1.html", "2.html"), "http://a.com/book/2.html");
        assert_eq!(absolute_url("http://a.com/book/1/", "../2/"), "http://a.com/book/2/");
        assert_eq!(absolute_url("http://a.com/b/c", "/x"), "http://a.com/x");
        assert_eq!(absolute_url("http://a.com/b/c", "//cdn.com/x.jpg"), "http://cdn.com/x.jpg");
        assert_eq!(absolute_url("https://a.com/s?q=1", "?page=2"), "https://a.com/s?page=2");
        assert_eq!(absolute_url("http://a.com/", "javascript:void(0)"), "");
        assert_eq!(absolute_url("", " x "), "x");
        assert_eq!(absolute_url("http://a.com/x,{\"method\":\"POST\"}", "y"), "http://a.com/y");
        assert_eq!(absolute_url("http://a.com/", "data:image/png;base64,xx"), "data:image/png;base64,xx");
    }

    #[test]
    fn url_option_split() {
        assert_eq!(split_url_option("http://a.com/s, {\"a\":1}"), ("http://a.com/s", Some("{\"a\":1}")));
        assert_eq!(split_url_option("http://a.com/s?a=1,2"), ("http://a.com/s?a=1,2", None));
    }

    #[test]
    fn java_regex_features() {
        // 前/后断言是 Java 书源常用，Rust regex 不支持，应回退到 fancy-regex
        assert_eq!(rule_replace("价格:123元", r"^.*(?<=价格:)(\d+)(?=元).*$", "$1", false), "123");
        // 反向引用
        assert_eq!(rule_replace("aabbcd", r"(.)\1", "<$1>", false), "<a><b>cd");
        // $12 回退语义：只有 1 个组时 $12 = 组 1 + '2'
        assert_eq!(rule_replace("x", r"(x)", "$12", false), "x2");
        // 无效正则退化为字面量替换
        assert_eq!(rule_replace("a(b", "(", "[", false), "a[b");
    }

    #[test]
    fn replace_first_semantics() {
        // ###：只保留第一个匹配（替换后），没有匹配得空串
        assert_eq!(rule_replace("abc123def456", r"\d+", "N", true), "N");
        assert_eq!(rule_replace("abc", r"\d+", "N", true), "");
    }

    #[test]
    fn truthy() {
        assert!(!is_true("false", false));
        assert!(!is_true(" 0 ", false));
        assert!(is_true("1", false));
        assert!(is_true("", true));
        assert!(!is_true("null", false));
    }
}
