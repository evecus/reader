//! RuleAnalyzer 的 Rust 移植：把一条规则字符串切成规则段。
//! 对应 Legado 的 RuleAnalyzer.kt + AnalyzeRule.SourceRule。
//!
//! 支持：
//! - 模式前缀 @CSS: @@ @XPath: @Json: / $. $[
//! - 复合段：selector##regex##replacement##first
//! - 内联求值：{{js}}、@get:{var}、@put:{var=expr}
//! - 正则组引用 $1~$99（此处只做识别，展开在 context）
//! - 逻辑分隔 && || %%

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Default, // Legado 默认语法（AnalyzeByJSoup：class./tag./id./children/索引，其余回退 CSS）
    Css,     // @CSS: 前缀：纯 CSS 选择器 + 末尾 @修饰符
    XPath,
    Json,
    Js,
    Regex,
    WebJs,
}

/// 内联片段类型
#[derive(Debug, Clone)]
pub enum Segment {
    Text(String),      // 普通文本 / 选择器 / 正则
    Js(String),        // {{...}} 中的 JS 表达式
    GetVar(String),    // @get:{name}
    RegexGroup(usize), // $1 ~ $99
}

/// 一个规则段（由 && / || / %% 分隔后的最小单元）
#[derive(Debug, Clone)]
pub struct RulePiece {
    pub mode: Mode,
    pub segments: Vec<Segment>,
    pub put_map: Vec<(String, String)>, // @put:{k=expr}
    pub replace_regex: String,         // ## 后第 1 段
    pub replacement: String,           // ## 后第 2 段
    pub replace_first: bool,           // ## 后第 3 段存在
    /// <js>...</js> 前缀脚本：先执行得到字符串，再作为后续规则的求值内容
    pub js_prefix: Option<String>,
}

impl RulePiece {
    /// 将 segments 拼成最终规则串（需要 context 求值内联片段）
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for seg in &self.segments {
            match seg {
                Segment::Text(t) => s.push_str(t),
                Segment::Js(j) => s.push_str(&format!("{{{{{}}}}}", j)),
                Segment::GetVar(n) => s.push_str(&format!("@get:{{{}}}", n)),
                Segment::RegexGroup(n) => s.push_str(&format!("${}", n)),
            }
        }
        s
    }
}

/// 完整的规则（一条 rule 字符串被解析后的结果）
#[derive(Debug, Clone)]
pub struct ParsedRule {
    pub pieces: Vec<RulePiece>,
    /// piece 之间的连接符（&& / || / %%），长度 = pieces.len()-1
    pub separators: Vec<String>,
}

static PUT_RE: OnceLock<Regex> = OnceLock::new();
static EVAL_RE: OnceLock<Regex> = OnceLock::new();
static REGEX_GROUP_RE: OnceLock<Regex> = OnceLock::new();

fn put_re() -> &'static Regex {
    PUT_RE.get_or_init(|| Regex::new(r"(?i)@put:\{([^}]+?)\}").unwrap())
}
fn eval_re() -> &'static Regex {
    // 匹配 @get:{...} 或 {{...}}（非贪婪，支持跨行）
    EVAL_RE.get_or_init(|| Regex::new(r"(?is)@get:\{[^}]+?\}|\{\{[\w\W]*?\}\}").unwrap())
}
fn regex_group_re() -> &'static Regex {
    REGEX_GROUP_RE.get_or_init(|| Regex::new(r"\$\d{1,2}").unwrap())
}

/// 主入口：解析一条规则字符串
pub fn parse_rule(input: &str) -> ParsedRule {
    let mut s = input.trim().to_string();
    // 首字符 '-' / '+' 是目录反转前缀，由业务层（BookService::chapters）处理；
    // 此处仅剥离，避免残留进选择器。reverse 语义不在规则解析器内生效。
    if s.starts_with('-') || s.starts_with('+') { s = s[1..].to_string(); }

    // 第一层：按 && / || / %% 分割（用平衡感知的切分）
    let (raw_pieces, separators) = split_logic(&s);
    let pieces = raw_pieces.into_iter().map(|p| parse_piece(&p)).collect();
    ParsedRule { pieces, separators }
}

/// 平衡感知的逻辑分割：&& || %%
fn split_logic(s: &str) -> (Vec<String>, Vec<String>) {
    let chars: Vec<char> = s.chars().collect();
    let mut pieces = Vec::new();
    let mut seps = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut depth_sq = 0i32; // []
    let mut depth_par = 0i32; // ()
    let mut depth_brace = 0i32; // {}
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_double { in_single = !in_single; }
        else if c == '"' && !in_single { in_double = !in_double; }
        if !in_single && !in_double {
            match c {
                '[' => depth_sq += 1,
                ']' => depth_sq -= 1,
                '(' => depth_par += 1,
                ')' => depth_par -= 1,
                '{' => depth_brace += 1,
                '}' => depth_brace -= 1,
                _ => {}
            }
            if depth_sq == 0 && depth_par == 0 && depth_brace == 0 {
                if c == '&' && i + 1 < chars.len() && chars[i + 1] == '&' {
                    pieces.push(chars[start..i].iter().collect());
                    seps.push("&&".into());
                    i += 2; start = i; continue;
                }
                if c == '|' && i + 1 < chars.len() && chars[i + 1] == '|' {
                    pieces.push(chars[start..i].iter().collect());
                    seps.push("||".into());
                    i += 2; start = i; continue;
                }
                if c == '%' && i + 1 < chars.len() && chars[i + 1] == '%' {
                    pieces.push(chars[start..i].iter().collect());
                    seps.push("%%".into());
                    i += 2; start = i; continue;
                }
            }
        }
        i += 1;
    }
    pieces.push(chars[start..].iter().collect());
    (pieces, seps)
}

fn parse_piece(raw: &str) -> RulePiece {
    let mut s = raw.trim().to_string();
    let mut mode = Mode::Default;
    let mut js_prefix: Option<String> = None;

    // <js>...</js> 前缀脚本（后面还接选择器，如 <js>...</js>$[*]）
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("<js>") {
        if let Some(end) = lower.find("</js>") {
            let body = s[4..end].to_string();
            let rest = s[end + 5..].trim().to_string();
            if !rest.is_empty() {
                js_prefix = Some(body);
                s = rest;
            } else {
                mode = Mode::Js;
                s = body;
            }
        }
    }

    // 模式前缀识别
    let lower = s.to_ascii_lowercase();
    if mode != Mode::Js && lower.starts_with("@js:") {
        mode = Mode::Js;
        s = s[4..].to_string();
    } else if lower.starts_with("@css:") {
        mode = Mode::Css;
        s = s[5..].trim().to_string();
    } else if s.starts_with("@@") {
        mode = Mode::Default;
        s = s[2..].to_string();
    } else if s.to_ascii_lowercase().starts_with("@xpath:") {
        mode = Mode::XPath;
        s = s[7..].to_string();
    } else if s.to_ascii_lowercase().starts_with("@json:") {
        mode = Mode::Json;
        s = s[6..].to_string();
    } else if s == "$" || s.starts_with("$.") || s.starts_with("$[") {
        mode = Mode::Json;
    } else if s.starts_with('/') {
        mode = Mode::XPath;
    }

    // @put:{} 提取（先收集再替换，避免借用冲突）
    let mut put_map = Vec::new();
    let put_matches: Vec<(String, String)> = put_re().captures_iter(&s).map(|cap| {
        (
            cap.get(0).unwrap().as_str().to_string(),
            cap.get(1).unwrap().as_str().to_string(),
        )
    }).collect();
    for (whole, body) in put_matches {
        if let Some(eq) = body.find('=') {
            put_map.push((body[..eq].trim().to_string(), body[eq + 1..].trim().to_string()));
        }
        s = s.replace(&whole, "");
    }

    // 拆分 ## ：selector##regex##replacement##first
    let mut parts = s.splitn(4, "##");
    let head = parts.next().unwrap_or("").to_string();
    let replace_regex = parts.next().unwrap_or("").to_string();
    let replacement = parts.next().unwrap_or("").to_string();
    let replace_first = parts.next().is_some();

    // 内联片段拆分：@get:{} / {{...}} / $N
    let segments = split_segments(&head, &mut mode);

    RulePiece {
        mode, segments, put_map, replace_regex, replacement, replace_first,
        js_prefix,
    }
}

fn split_segments(head: &str, mode: &mut Mode) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut last = 0usize;

    // 先处理 eval（@get / {{}}）
    let evals: Vec<(usize, usize, Segment)> = eval_re().find_iter(head).map(|m| {
        let txt = m.as_str();
        let seg = if txt.to_ascii_lowercase().starts_with("@get:") {
            Segment::GetVar(txt[6..txt.len() - 1].to_string())
        } else {
            Segment::Js(txt[2..txt.len() - 2].to_string())
        };
        (m.start(), m.end(), seg)
    }).collect();

    // 若存在内联 JS 且当前为默认模式，Legado 会切成 Regex 模式（本引擎按操作数处理，这里保留 Default）
    if !evals.is_empty() && *mode == Mode::Default {
        // 不强制改 mode：选择器可能是 "css{{js}}" 形式，求值后仍是 CSS
    }

    for (start, end, seg) in &evals {
        if *start > last {
            push_text_segments(&mut out, &head[last..*start], mode);
        }
        out.push(seg.clone());
        last = *end;
    }
    if last < head.len() {
        push_text_segments(&mut out, &head[last..], mode);
    }
    if out.is_empty() { out.push(Segment::Text(String::new())); }
    out
}

fn push_text_segments(out: &mut Vec<Segment>, text: &str, mode: &mut Mode) {
    let mut last = 0usize;
    for m in regex_group_re().find_iter(text) {
        if m.start() > last {
            out.push(Segment::Text(text[last..m.start()].to_string()));
        }
        let n: usize = m.as_str()[1..].parse().unwrap_or(0);
        if n > 0 {
            if *mode == Mode::Default || *mode == Mode::Js { *mode = Mode::Regex; }
            out.push(Segment::RegexGroup(n));
        }
        last = m.end();
    }
    if last < text.len() {
        out.push(Segment::Text(text[last..].to_string()));
    }
}
