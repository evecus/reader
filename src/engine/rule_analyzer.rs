//! `RuleAnalyzer.kt` 的逐行移植：平衡感知的规则切分与内嵌规则替换。
//!
//! 关键点（与朴素 `split("&&")` 的区别）：
//! - 分隔符落在 `[]` / `()` 内（CSS 属性选择器、索引、JS 调用、正则）时不切；
//! - `code=true` 时按"代码平衡"处理引号与转义（JSONPath / JS），否则按"规则平衡"；
//! - `elements_type` 记录实际命中的分隔符（`&&` / `||` / `%%`），供上层决定合并方式。

const ESC: char = '\\';

pub struct RuleAnalyzer {
    queue: Vec<char>,
    pos: usize,
    start: usize,
    start_x: usize,
    rule: Vec<String>,
    step: usize,
    pub elements_type: String,
    code: bool,
}

impl RuleAnalyzer {
    pub fn new(data: &str, code: bool) -> Self {
        Self {
            queue: data.chars().collect(), pos: 0, start: 0, start_x: 0,
            rule: Vec::new(), step: 0, elements_type: String::new(), code,
        }
    }

    fn sub(&self, a: usize, b: usize) -> String {
        let b = b.min(self.queue.len());
        let a = a.min(b);
        self.queue[a..b].iter().collect()
    }

    fn rest(&self, a: usize) -> String { self.sub(a, self.queue.len()) }

    /// 修剪当前规则之前的 `@` 或空白符
    pub fn trim(&mut self) {
        let is_skip = |c: char| c == '@' || c < '!';
        if self.pos < self.queue.len() && is_skip(self.queue[self.pos]) {
            self.pos += 1;
            while self.pos < self.queue.len() && is_skip(self.queue[self.pos]) { self.pos += 1; }
            self.start = self.pos;
            self.start_x = self.pos;
        }
    }

    pub fn reset_pos(&mut self) {
        self.pos = 0;
        self.start_x = 0;
    }

    fn index_of(&self, seq: &[char], from: usize) -> Option<usize> {
        if seq.is_empty() { return Some(from.min(self.queue.len())); }
        if self.queue.len() < seq.len() { return None; }
        let last = self.queue.len() - seq.len();
        (from..=last).find(|&i| self.queue[i..i + seq.len()] == *seq)
    }

    /// 拉出一个字符串，直到（不含）匹配序列
    fn consume_to(&mut self, seq: &str) -> bool {
        self.start = self.pos;
        let s: Vec<char> = seq.chars().collect();
        match self.index_of(&s, self.pos) {
            Some(off) => { self.pos = off; true }
            None => false,
        }
    }

    fn consume_to_any(&mut self, seqs: &[&str]) -> bool {
        let seqs: Vec<Vec<char>> = seqs.iter().map(|s| s.chars().collect()).collect();
        let mut pos = self.pos;
        while pos != self.queue.len() {
            for s in &seqs {
                if pos + s.len() <= self.queue.len() && self.queue[pos..pos + s.len()] == s[..] {
                    self.step = s.len();
                    self.pos = pos;
                    return true;
                }
            }
            pos += 1;
        }
        false
    }

    fn find_to_any(&self, seq: &[char]) -> Option<usize> {
        let mut pos = self.pos;
        while pos != self.queue.len() {
            if seq.contains(&self.queue[pos]) { return Some(pos); }
            pos += 1;
        }
        None
    }

    /// 代码平衡组（含转义），用于 JSONPath / JS
    fn chomp_code_balanced(&mut self, open: char, close: char) -> bool {
        let mut pos = self.pos;
        let mut depth = 0i32;
        let mut other = 0i32;
        let (mut sq, mut dq) = (false, false);
        loop {
            if pos == self.queue.len() { break; }
            let c = self.queue[pos];
            pos += 1;
            if c != ESC {
                if c == '\'' && !dq { sq = !sq; }
                else if c == '"' && !sq { dq = !dq; }
                if sq || dq {
                    if depth > 0 || other > 0 { continue; } else { break; }
                }
                if c == '[' { depth += 1; }
                else if c == ']' { depth -= 1; }
                else if depth == 0 {
                    if c == open { other += 1; }
                    else if c == close { other -= 1; }
                }
            } else {
                pos += 1;
            }
            if !(depth > 0 || other > 0) { break; }
        }
        if depth > 0 || other > 0 { false } else { self.pos = pos.min(self.queue.len()); true }
    }

    /// 规则平衡组：XPath / JSoup 里引号内转义无效
    fn chomp_rule_balanced(&mut self, open: char, close: char) -> bool {
        let mut pos = self.pos;
        let mut depth = 0i32;
        let (mut sq, mut dq) = (false, false);
        loop {
            if pos == self.queue.len() { break; }
            let c = self.queue[pos];
            pos += 1;
            if c == '\'' && !dq { sq = !sq; }
            else if c == '"' && !sq { dq = !dq; }
            if sq || dq {
                if depth > 0 { continue; } else { break; }
            } else if c == '\\' {
                pos += 1;
                if depth > 0 { continue; } else { break; }
            }
            if c == open { depth += 1; }
            else if c == close { depth -= 1; }
            if depth <= 0 { break; }
        }
        if depth > 0 { false } else { self.pos = pos.min(self.queue.len()); true }
    }

    fn chomp_balanced(&mut self, open: char, close: char) -> bool {
        if self.code { self.chomp_code_balanced(open, close) } else { self.chomp_rule_balanced(open, close) }
    }

    /// 首段匹配（对应 `splitRule(vararg split)`）
    pub fn split_rule(&mut self, split: &[&str]) -> Result<Vec<String>, String> {
        if split.len() == 1 {
            self.elements_type = split[0].to_string();
            let et = self.elements_type.clone();
            return if !self.consume_to(&et) {
                let r = self.rest(self.start_x);
                self.rule.push(r);
                Ok(self.rule.clone())
            } else {
                self.step = et.chars().count();
                self.split_rule_next()
            };
        } else if !self.consume_to_any(split) {
            let r = self.rest(self.start_x);
            self.rule.push(r);
            return Ok(self.rule.clone());
        }

        let end = self.pos;
        self.pos = self.start;

        loop {
            let st = self.find_to_any(&['[', '(']);
            let Some(st) = st else {
                self.rule = vec![self.sub(self.start_x, end)];
                self.elements_type = self.sub(end, end + self.step);
                self.pos = end + self.step;
                let et = self.elements_type.clone();
                while self.consume_to(&et) {
                    let s = self.sub(self.start, self.pos);
                    self.rule.push(s);
                    self.pos += self.step;
                }
                let r = self.rest(self.pos);
                self.rule.push(r);
                return Ok(self.rule.clone());
            };

            if st > end {
                self.rule = vec![self.sub(self.start_x, end)];
                self.elements_type = self.sub(end, end + self.step);
                self.pos = end + self.step;
                let et = self.elements_type.clone();
                while self.consume_to(&et) && self.pos < st {
                    let s = self.sub(self.start, self.pos);
                    self.rule.push(s);
                    self.pos += self.step;
                }
                return if self.pos > st {
                    self.start_x = self.start;
                    self.split_rule_next()
                } else {
                    let r = self.rest(self.pos);
                    self.rule.push(r);
                    Ok(self.rule.clone())
                };
            }

            self.pos = st;
            let open = self.queue[self.pos];
            let close = if open == '[' { ']' } else { ')' };
            if !self.chomp_balanced(open, close) {
                return Err(format!("{}后未平衡", self.sub(0, self.start)));
            }
            if !(end > self.pos) { break; }
        }

        self.start = self.pos;
        self.split_rule(split)
    }

    /// 二段匹配（对应私有的 `splitRule()`）：elements_type 已确定
    fn split_rule_next(&mut self) -> Result<Vec<String>, String> {
        let end = self.pos;
        self.pos = self.start;
        let et = self.elements_type.clone();

        loop {
            let st = self.find_to_any(&['[', '(']);
            let Some(st) = st else {
                let s = self.sub(self.start_x, end);
                self.rule.push(s);
                self.pos = end + self.step;
                while self.consume_to(&et) {
                    let s = self.sub(self.start, self.pos);
                    self.rule.push(s);
                    self.pos += self.step;
                }
                let r = self.rest(self.pos);
                self.rule.push(r);
                return Ok(self.rule.clone());
            };

            if st > end {
                let s = self.sub(self.start_x, end);
                self.rule.push(s);
                self.pos = end + self.step;
                while self.consume_to(&et) && self.pos < st {
                    let s = self.sub(self.start, self.pos);
                    self.rule.push(s);
                    self.pos += self.step;
                }
                return if self.pos > st {
                    self.start_x = self.start;
                    self.split_rule_next()
                } else {
                    let r = self.rest(self.pos);
                    self.rule.push(r);
                    Ok(self.rule.clone())
                };
            }

            self.pos = st;
            let open = self.queue[self.pos];
            let close = if open == '[' { ']' } else { ')' };
            if !self.chomp_balanced(open, close) {
                return Err(format!("{}后未平衡", self.sub(0, self.start)));
            }
            if !(end > self.pos) { break; }
        }

        self.start = self.pos;
        if !self.consume_to(&et) {
            let r = self.rest(self.start_x);
            self.rule.push(r);
            Ok(self.rule.clone())
        } else {
            self.split_rule_next()
        }
    }

    /// 便捷封装：切分失败（括号不平衡）时退化为整条规则，避免因脏规则整体报错
    pub fn split_or_whole(data: &str, code: bool, seps: &[&str]) -> (Vec<String>, String) {
        let mut a = RuleAnalyzer::new(data, code);
        match a.split_rule(seps) {
            Ok(v) => (v, a.elements_type.clone()),
            Err(_) => (vec![data.to_string()], String::new()),
        }
    }

    /// 替换内嵌规则 `{$.xxx}`：`inner` 为起始标志（如 `{$.`），`start_step`/`end_step` 为不属于规则体的前后缀长度。
    /// 无任何成功替换时返回空串（与官方一致，调用方据此判断是否走整体路径）。
    pub fn inner_rule<F>(&mut self, inner: &str, start_step: usize, end_step: usize, mut fr: F) -> String
    where F: FnMut(&str) -> Option<String> {
        let mut st = String::new();
        let inner_len = inner.chars().count();
        while self.consume_to(inner) {
            let pos_pre = self.pos;
            if self.chomp_code_balanced('{', '}') {
                let body = self.sub(pos_pre + start_step, self.pos.saturating_sub(end_step));
                if let Some(v) = fr(&body).filter(|v| !v.is_empty()) {
                    st.push_str(&self.sub(self.start_x, pos_pre));
                    st.push_str(&v);
                    self.start_x = self.pos;
                    continue;
                }
            }
            self.pos += inner_len;
        }
        if self.start_x == 0 { String::new() } else { st.push_str(&self.rest(self.start_x)); st }
    }

    /// 替换内嵌规则 `startStr ... endStr`（如 `{{` `}}`），无替换时返回原串
    pub fn inner_rule_pair<F>(&mut self, start_str: &str, end_str: &str, mut fr: F) -> String
    where F: FnMut(&str) -> String {
        let mut st = String::new();
        let sl = start_str.chars().count();
        let el = end_str.chars().count();
        while self.consume_to(start_str) {
            self.pos += sl;
            let pos_pre = self.pos;
            if self.consume_to(end_str) {
                let v = fr(&self.sub(pos_pre, self.pos));
                st.push_str(&self.sub(self.start_x, pos_pre - sl));
                st.push_str(&v);
                self.pos += el;
                self.start_x = self.pos;
            }
        }
        if self.start_x == 0 { self.queue.iter().collect() } else { st.push_str(&self.rest(self.start_x)); st }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str, seps: &[&str], code: bool) -> (Vec<String>, String) {
        let mut a = RuleAnalyzer::new(s, code);
        let v = a.split_rule(seps).unwrap();
        (v, a.elements_type.clone())
    }

    #[test]
    fn basic_split() {
        assert_eq!(split("a&&b&&c", &["&&", "||", "%%"], false), (vec!["a".into(), "b".into(), "c".into()], "&&".into()));
        assert_eq!(split("a||b", &["&&", "||"], false).1, "||");
        assert_eq!(split("abc", &["&&", "||"], false).0, vec!["abc"]);
    }

    #[test]
    fn separator_inside_brackets_is_kept() {
        // 属性选择器 / 索引 / 函数调用里的分隔符不切
        let (v, _) = split("a[href*='&&']@text&&b", &["&&", "||", "%%"], false);
        assert_eq!(v, vec!["a[href*='&&']@text", "b"]);
        let (v, _) = split("//a[contains(@x,'||')]&&//b", &["&&", "||"], false);
        assert_eq!(v, vec!["//a[contains(@x,'||')]", "//b"]);
        // JSONPath 过滤表达式里的 && 属于 jsonpath 自身
        let (v, _) = split("$.a[?(@.x>1 && @.y<2)].n||$.b", &["&&", "||"], true);
        assert_eq!(v, vec!["$.a[?(@.x>1 && @.y<2)].n", "$.b"]);
    }

    #[test]
    fn at_split_for_jsoup_steps() {
        let mut a = RuleAnalyzer::new("@class.x@tag.a[0]@text", false);
        a.trim();
        assert_eq!(a.split_rule(&["@"]).unwrap(), vec!["class.x", "tag.a[0]", "text"]);
    }

    #[test]
    fn inner_rule_braces() {
        let mut a = RuleAnalyzer::new("https://x/{$.id}/{$.name}.html", true);
        let out = a.inner_rule("{$.", 1, 1, |r| Some(format!("<{}>", r)));
        assert_eq!(out, "https://x/<$.id>/<$.name>.html");
        let mut a = RuleAnalyzer::new("no inner here", true);
        assert_eq!(a.inner_rule("{$.", 1, 1, |_| Some("x".into())), "");
    }

    #[test]
    fn inner_rule_pair_double_braces() {
        let mut a = RuleAnalyzer::new("p={{page+1}}&k={{key}}", false);
        let out = a.inner_rule_pair("{{", "}}", |e| format!("[{}]", e));
        assert_eq!(out, "p=[page+1]&k=[key]");
        let mut a = RuleAnalyzer::new("plain", false);
        assert_eq!(a.inner_rule_pair("{{", "}}", |_| "x".into()), "plain");
    }
}
