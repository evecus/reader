//! `AnalyzeByJSonPath.kt` 的移植（语义对齐 Jayway JsonPath）。
//!
//! 底层用 `serde_json_path`（RFC 9535），在其上补齐 Jayway/Legado 的习惯：
//! - 路径不以 `$`/`@` 开头时自动补 `$.`（`data.list` 等价 `$.data.list`）
//! - 点号属性名里允许 `-`、数字开头等（改写成 `['name']`）
//! - 确定性路径返回单值，不确定路径（`..` `*` 过滤 切片 联合）返回数组
//! - `&&` / `||` / `%%` 组合与 `{$.rule}` 内嵌规则（经 RuleAnalyzer 平衡切分）

use crate::engine::rule_analyzer::RuleAnalyzer;
use serde_json::Value;
use serde_json_path::JsonPath;

/// Jayway 风格路径 → RFC 9535 路径，并返回是否"不确定"（结果应为数组）
fn normalize(path: &str) -> (String, bool) {
    let p = path.trim();
    let p = if p.starts_with('$') || p.starts_with('@') {
        p.to_string()
    } else if p.starts_with('[') {
        format!("${}", p)
    } else {
        format!("$.{}", p)
    };

    let chars: Vec<char> = p.chars().collect();
    let mut out = String::new();
    let mut indefinite = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' => {
                // 复制整个括号段（引号感知），顺带判定是否不确定
                let mut depth = 0;
                let mut quote: Option<char> = None;
                let start = i;
                while i < chars.len() {
                    let d = chars[i];
                    if let Some(q) = quote {
                        if d == '\\' { i += 2; continue; }
                        if d == q { quote = None; }
                    } else if d == '\'' || d == '"' {
                        quote = Some(d);
                    } else if d == '[' {
                        depth += 1;
                    } else if d == ']' {
                        depth -= 1;
                        if depth == 0 { i += 1; break; }
                    }
                    i += 1;
                }
                let seg: String = chars[start..i.min(chars.len())].iter().collect();
                let inner = seg.trim_start_matches('[').trim_end_matches(']').trim();
                // 非引号内容里含 , : * ? 即不确定
                let mut q: Option<char> = None;
                for ch in inner.chars() {
                    if let Some(qq) = q { if ch == qq { q = None; } continue; }
                    if ch == '\'' || ch == '"' { q = Some(ch); continue; }
                    if matches!(ch, ',' | ':' | '*' | '?') { indefinite = true; }
                }
                out.push_str(&seg);
            }
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    indefinite = true;
                    out.push_str("..");
                    i += 2;
                    // `..` 后可能是 name / * / [
                    if i < chars.len() && chars[i] != '[' && chars[i] != '*' {
                        let (name, ni) = read_name(&chars, i);
                        out.push_str(&name_to_segment(&name, true));
                        i = ni;
                    }
                    continue;
                }
                i += 1;
                if i < chars.len() && chars[i] == '*' {
                    indefinite = true;
                    out.push_str(".*");
                    i += 1;
                    continue;
                }
                if i < chars.len() && chars[i] != '[' {
                    let (name, ni) = read_name(&chars, i);
                    if name.is_empty() { continue; }
                    out.push_str(&name_to_segment(&name, false));
                    i = ni;
                    continue;
                }
                // `.[`：直接让后面的 `[` 处理
            }
            _ => { out.push(c); i += 1; }
        }
    }
    (out, indefinite)
}

fn read_name(chars: &[char], mut i: usize) -> (String, usize) {
    let st = i;
    while i < chars.len() && chars[i] != '.' && chars[i] != '[' { i += 1; }
    (chars[st..i].iter().collect(), i)
}

fn is_shorthand(name: &str) -> bool {
    let mut it = name.chars();
    match it.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn name_to_segment(name: &str, descendant: bool) -> String {
    if is_shorthand(name) {
        if descendant { name.to_string() } else { format!(".{}", name) }
    } else {
        let esc = name.replace('\\', "\\\\").replace('\'', "\\'");
        format!("['{}']", esc)
    }
}

/// 读取 JSON：返回 Jayway 风格结果（确定路径单值；不确定路径数组）。找不到/路径非法返回 None。
pub fn read(json: &Value, path: &str) -> Option<Value> {
    let (norm, indefinite) = normalize(path);
    let jp = JsonPath::parse(&norm).ok()?;
    let nodes = jp.query(json);
    let all: Vec<&Value> = nodes.all();
    if indefinite {
        Some(Value::Array(all.into_iter().cloned().collect()))
    } else {
        all.into_iter().next().cloned()
    }
}

/// Java `Object.toString()`（JsonSmart）：字符串原样，其余 JSON 文本
pub fn val_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub struct AnalyzeByJsonPath {
    json: Value,
}

impl AnalyzeByJsonPath {
    pub fn new(json: Value) -> Self { Self { json } }

    pub fn from_str(s: &str) -> Self {
        Self { json: parse_lenient(s) }
    }

    fn list_to_string(v: &Value) -> String {
        match v {
            Value::Array(a) => a.iter().map(val_to_string).collect::<Vec<_>>().join("\n"),
            Value::Null => String::new(),
            other => val_to_string(other),
        }
    }

    pub fn get_string(&self, rule: &str) -> Option<String> {
        if rule.is_empty() { return None; }
        let mut ra = RuleAnalyzer::new(rule, true);
        let rules = ra.split_rule(&["&&", "||"]).unwrap_or_else(|_| vec![rule.to_string()]);
        if rules.len() == 1 {
            ra.reset_pos();
            let st = ra.inner_rule("{$.", 1, 1, |r| self.get_string(r));
            if st.is_empty() {
                return Some(read(&self.json, rule).map(|v| Self::list_to_string(&v)).unwrap_or_default());
            }
            return Some(st);
        }
        let et = ra.elements_type.clone();
        let mut texts = Vec::new();
        for rl in &rules {
            if let Some(t) = self.get_string(rl).filter(|t| !t.is_empty()) {
                texts.push(t);
                if et == "||" { break; }
            }
        }
        Some(texts.join("\n"))
    }

    pub fn get_string_list(&self, rule: &str) -> Vec<String> {
        let mut result = Vec::new();
        if rule.is_empty() { return result; }
        let mut ra = RuleAnalyzer::new(rule, true);
        let rules = ra.split_rule(&["&&", "||", "%%"]).unwrap_or_else(|_| vec![rule.to_string()]);
        if rules.len() == 1 {
            ra.reset_pos();
            let st = ra.inner_rule("{$.", 1, 1, |r| self.get_string(r));
            if st.is_empty() {
                if let Some(v) = read(&self.json, rule) {
                    match v {
                        Value::Array(a) => result.extend(a.iter().map(val_to_string)),
                        Value::Null => {}
                        other => result.push(val_to_string(&other)),
                    }
                }
            } else {
                result.push(st);
            }
            return result;
        }
        let et = ra.elements_type.clone();
        let mut groups: Vec<Vec<String>> = Vec::new();
        for rl in &rules {
            let t = self.get_string_list(rl);
            if !t.is_empty() {
                groups.push(t);
                if et == "||" { break; }
            }
        }
        merge(&mut result, groups, &et);
        result
    }

    /// 对应 `getList`：规则必须得到数组，否则为空
    pub fn get_list(&self, rule: &str) -> Vec<Value> {
        let mut result = Vec::new();
        if rule.is_empty() { return result; }
        let mut ra = RuleAnalyzer::new(rule, true);
        let rules = ra.split_rule(&["&&", "||", "%%"]).unwrap_or_else(|_| vec![rule.to_string()]);
        if rules.len() == 1 {
            return match read(&self.json, &rules[0]) {
                Some(Value::Array(a)) => a,
                _ => Vec::new(),
            };
        }
        let et = ra.elements_type.clone();
        let mut groups: Vec<Vec<Value>> = Vec::new();
        for rl in &rules {
            let t = self.get_list(rl);
            if !t.is_empty() {
                groups.push(t);
                if et == "||" { break; }
            }
        }
        merge(&mut result, groups, &et);
        result
    }

    pub fn get_object(&self, rule: &str) -> Option<Value> { read(&self.json, rule) }
}

fn merge<T: Clone>(out: &mut Vec<T>, groups: Vec<Vec<T>>, et: &str) {
    if groups.is_empty() { return; }
    if et == "%%" {
        for i in 0..groups[0].len() {
            for g in &groups { if i < g.len() { out.push(g[i].clone()); } }
        }
    } else {
        for g in groups { out.extend(g); }
    }
}

/// 宽松 JSON 解析（JsonSmart 习惯）：先严格解析，失败后去 BOM / JSONP 外壳 / 尾逗号再试
pub fn parse_lenient(s: &str) -> Value {
    fn attempt(t: &str) -> Option<Value> {
        if let Ok(v) = serde_json::from_str::<Value>(t) { return Some(v); }
        // 尾逗号
        let cleaned = regex::Regex::new(r",\s*([}\]])").unwrap().replace_all(t, "$1").to_string();
        serde_json::from_str::<Value>(&cleaned).ok()
    }
    let t = s.trim().trim_start_matches('\u{feff}');
    if let Some(v) = attempt(t) { return v; }
    // JSONP: callback({...});
    if let (Some(l), Some(r)) = (t.find('('), t.rfind(')')) {
        if l < r && t[..l].chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.') {
            if let Some(v) = attempt(t[l + 1..r].trim()) { return v; }
        }
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> AnalyzeByJsonPath { AnalyzeByJsonPath::from_str(s) }
    const J: &str = r#"{"data":{"list":[{"id":1,"n":"甲","tag-x":"t1"},{"id":2,"n":"乙","tag-x":"t2"}],"total":2},"msg":"ok"}"#;

    #[test]
    fn basic_and_auto_prefix() {
        let j = a(J);
        assert_eq!(j.get_string("$.msg").unwrap(), "ok");
        assert_eq!(j.get_string("msg").unwrap(), "ok"); // Jayway 自动补 $.
        assert_eq!(j.get_string("$.data.total").unwrap(), "2");
        assert_eq!(j.get_string("$.data.list[0].n").unwrap(), "甲");
        assert_eq!(j.get_string("$.data.list[*].n").unwrap(), "甲\n乙");
    }

    #[test]
    fn filters_and_recursive_descent() {
        let j = a(J);
        assert_eq!(j.get_string("$.data.list[?(@.id>1)].n").unwrap(), "乙");
        assert_eq!(j.get_string_list("$..n"), vec!["甲", "乙"]);
        assert_eq!(j.get_string("$.data.list[?(@.n=='甲')].id").unwrap(), "1");
        assert_eq!(j.get_string_list("$.data.list[0,1].id"), vec!["1", "2"]);
        assert_eq!(j.get_string_list("$.data.list[-1:].n"), vec!["乙"]);
    }

    #[test]
    fn odd_property_names() {
        let j = a(J);
        assert_eq!(j.get_string_list("$.data.list[*].tag-x"), vec!["t1", "t2"]);
        let j = a(r#"{"1a":"x","a b":"y"}"#);
        assert_eq!(j.get_string("$.1a").unwrap(), "x");
    }

    #[test]
    fn lists_and_objects() {
        let j = a(J);
        assert_eq!(j.get_list("$.data.list").len(), 2);
        assert!(j.get_list("$.data").is_empty()); // 对象不是 List
        assert_eq!(j.get_list("data.list[?(@.id==2)]").len(), 1);
        // 对象 toString 为 JSON 文本
        assert_eq!(a(r#"{"o":{"a":1}}"#).get_string("$.o").unwrap(), r#"{"a":1}"#);
    }

    #[test]
    fn combinators_and_inner_rules() {
        let j = a(J);
        assert_eq!(j.get_string("$.msg&&$.data.total").unwrap(), "ok\n2");
        assert_eq!(j.get_string("$.nope||$.msg").unwrap(), "ok");
        assert_eq!(j.get_string("id={$.data.total}&m={$.msg}").unwrap(), "id=2&m=ok");
        assert_eq!(j.get_string_list("$.data.list[*].id%%$.data.list[*].n"), vec!["1", "甲", "2", "乙"]);
    }

    #[test]
    fn lenient_parse() {
        assert_eq!(a(r#"cb({"a":1,});"#).get_string("$.a").unwrap(), "1");
        assert_eq!(a("\u{feff}{\"a\":2}").get_string("a").unwrap(), "2");
    }
}
