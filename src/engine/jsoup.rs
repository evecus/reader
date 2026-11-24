//! Legado 默认规则语法（`AnalyzeByJSoup.kt` 的 Rust 移植）。
//!
//! 与 CSS 模式（`@CSS:` 前缀）不同，Legado 的默认模式是一套以 `@` 分步的链式语法：
//!
//! ```text
//! class.title@tag.a.0@text          按 class 取元素，再取 a 标签的第 0 个，最后取文本
//! tag.li!0:2                        排除第 0、2 个
//! tag.div[-1:0]                     反向
//! id.content@html                   按 id 取
//! text.下一页@href                  按“自身文本包含”取
//! children.1@text                   直接子元素的第 1 个
//! div.item > a@href                 其他情况回退为 CSS 选择器
//! ```
//!
//! 每一步由 `ElementsSingle`（取元素 + 索引筛选）完成，最后一步是输出修饰符
//! （`text` / `textNodes` / `ownText` / `html` / `all` / 属性名）。
//!
//! 元素在引擎里以 outerHTML 字符串传递，所以这里每次调用先把输入解析成一棵树，
//! 在同一棵树上完成整条链，最后把结果转回字符串。

use ego_tree::NodeId;
use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use std::collections::HashSet;
use std::sync::OnceLock;

const WRAP_ID: &str = "__rdr_w__";

/// 解析后的文档 + 当前规则的“根”节点
struct Doc {
    html: Html,
    root: NodeId,
}

fn first_tag(src: &str) -> String {
    let s = src.trim_start();
    let Some(rest) = s.strip_prefix('<') else { return String::new() };
    if rest.starts_with('!') || rest.starts_with('?') || rest.starts_with('/') {
        return String::new();
    }
    rest.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// 从 root 一路到 wrapper，沿途每一层是否都没有其他元素 / 非空白文本兄弟
fn is_sole_content(html: &Html, root: NodeId, wrapper: NodeId) -> bool {
    let mut cur = match html.tree.get(root) { Some(n) => n, None => return false };
    while cur.id() != wrapper {
        let Some(parent) = cur.parent() else { return false };
        for sib in parent.children() {
            if sib.id() == cur.id() { continue; }
            match sib.value() {
                Node::Element(_) => return false,
                Node::Text(t) if !t.trim().is_empty() => return false,
                _ => {}
            }
        }
        cur = parent;
    }
    true
}

impl Doc {
    fn parse(src: &str) -> Doc {
        let head: String = src.chars().take(512).collect::<String>().to_ascii_lowercase();
        if head.contains("<html") || head.contains("<!doctype") {
            let html = Html::parse_document(src);
            let root = html.tree.root().id();
            return Doc { html, root };
        }
        let tag = first_tag(src);
        let wrapper = if matches!(tag.as_str(), "tr" | "td" | "th" | "tbody" | "thead" | "tfoot") {
            "table"
        } else {
            "div"
        };
        let html = Html::parse_document(&format!(
            "<{w} id=\"{id}\">{src}</{w}>",
            w = wrapper,
            id = WRAP_ID,
            src = src
        ));
        // 找包装元素
        let mut wrapper_id = None;
        for n in html.tree.root().descendants() {
            if let Node::Element(e) = n.value() {
                if e.attr("id") == Some(WRAP_ID) {
                    wrapper_id = Some(n.id());
                    break;
                }
            }
        }
        let wrapper_id = wrapper_id.unwrap_or_else(|| html.tree.root().id());
        // 找真正的根元素：包装元素下文档序第一个同名元素
        let mut root = wrapper_id;
        if !tag.is_empty() {
            if let Some(w) = html.tree.get(wrapper_id) {
                for n in w.descendants().skip(1) {
                    if let Node::Element(e) = n.value() {
                        if e.name().eq_ignore_ascii_case(&tag) {
                            root = n.id();
                            break;
                        }
                    }
                }
            }
        }
        // 只有当内容正好是“单个元素”（元素自己的 outerHTML）时才以它为根；
        // 多个并列顶层节点（整页片段）则以包装元素为根，否则会丢掉兄弟节点。
        if root != wrapper_id && !is_sole_content(&html, root, wrapper_id) {
            root = wrapper_id;
        }
        Doc { html, root }
    }

    fn el(&self, id: NodeId) -> Option<ElementRef<'_>> {
        self.html.tree.get(id).and_then(ElementRef::wrap)
    }

    /// 节点自身及所有后代元素（文档序）
    fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        match self.html.tree.get(id) {
            Some(n) => n
                .descendants()
                .filter(|d| matches!(d.value(), Node::Element(_)))
                .map(|d| d.id())
                .collect(),
            None => vec![],
        }
    }

    fn children(&self, id: NodeId) -> Vec<NodeId> {
        match self.html.tree.get(id) {
            Some(n) => n
                .children()
                .filter(|d| matches!(d.value(), Node::Element(_)))
                .map(|d| d.id())
                .collect(),
            None => vec![],
        }
    }
}

// ───────────────────────── 切分 ─────────────────────────

/// 按 `@` 切分一条规则，忽略 `[]` `()` 与引号内的 `@`（对应 `RuleAnalyzer.splitRule("@")`）
fn split_at(rule: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut sq, mut par) = (0i32, 0i32);
    let mut quote: Option<char> = None;
    for c in rule.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q { quote = None; }
            continue;
        }
        match c {
            '\'' | '"' => { quote = Some(c); cur.push(c); }
            '[' => { sq += 1; cur.push(c); }
            ']' => { sq -= 1; cur.push(c); }
            '(' => { par += 1; cur.push(c); }
            ')' => { par -= 1; cur.push(c); }
            '@' if sq <= 0 && par <= 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// 对应 `RuleAnalyzer.trim()`：去掉规则前面的 `@` 和空白
fn trim_rule(rule: &str) -> &str {
    rule.trim_start_matches(|c: char| c == '@' || c < '!')
}

// ───────────────────────── 索引 ─────────────────────────

#[derive(Debug, Default, PartialEq)]
struct IndexSpec {
    /// '.' 选择 / '!' 排除 / ' ' 无索引
    split: char,
    before_rule: String,
    /// 老写法 `tag.div.-1:10:2`，按“逆向收集”的顺序保存
    index_default: Vec<i32>,
    /// `[...]` 写法，按“逆向收集”的顺序保存
    indexes: Vec<Idx>,
}

#[derive(Debug, PartialEq)]
enum Idx {
    One(i32),
    /// start, end, step
    Range(Option<i32>, Option<i32>, i32),
}

/// 对应 `ElementsSingle.findIndexSet`
fn find_index_set(rule: &str) -> IndexSpec {
    let rus: Vec<char> = rule.trim().chars().collect();
    let mut spec = IndexSpec { split: ' ', ..Default::default() };
    let full: String = rus.iter().collect();
    if rus.is_empty() {
        spec.before_rule = full;
        return spec;
    }
    let mut len = rus.len() as i64;
    let mut cur_minus = false;
    let mut cur_list: Vec<Option<i32>> = Vec::new();
    let mut l = String::new();
    let head = *rus.last().unwrap() == ']';

    let at = |i: i64| -> Option<char> { if i >= 0 { rus.get(i as usize).copied() } else { None } };
    let parse = |s: &str, minus: bool| -> Option<i32> {
        if s.is_empty() { None } else { s.parse::<i32>().ok().map(|v| if minus { -v } else { v }) }
    };

    if head {
        len -= 1; // 跳过尾部 ']'
        loop {
            let cond = len >= 0;
            len -= 1;
            if !cond { break; }
            let Some(mut rl) = at(len) else { break };
            if rl == ' ' { continue; }
            if rl.is_ascii_digit() {
                l.insert(0, rl);
            } else if rl == '-' {
                cur_minus = true;
            } else {
                let cur_int = parse(&l, cur_minus);
                if rl == ':' {
                    cur_list.push(cur_int);
                } else {
                    if cur_list.is_empty() {
                        match cur_int {
                            None => break, // 是 CSS 选择器而非索引列表
                            Some(v) => spec.indexes.push(Idx::One(v)),
                        }
                    } else {
                        let step = if cur_list.len() == 2 { cur_list[0].unwrap_or(1) } else { 1 };
                        spec.indexes.push(Idx::Range(cur_int, *cur_list.last().unwrap(), step));
                        cur_list.clear();
                    }
                    if rl == '!' {
                        spec.split = '!';
                        loop {
                            len -= 1;
                            match at(len) { Some(c) => rl = c, None => break }
                            if !(len > 0 && rl == ' ') { break; }
                        }
                    }
                    if rl == '[' {
                        spec.before_rule = rus[..len.max(0) as usize].iter().collect();
                        if spec.split != '!' { spec.split = '.'; }
                        return spec;
                    }
                    if rl != ',' { break; }
                }
                l.clear();
                cur_minus = false;
            }
        }
    } else {
        loop {
            let cond = len >= 0;
            len -= 1;
            if !cond { break; }
            let Some(rl) = at(len) else { break };
            if rl == ' ' { continue; }
            if rl.is_ascii_digit() {
                l.insert(0, rl);
            } else if rl == '-' {
                cur_minus = true;
            } else {
                if rl == '!' || rl == '.' || rl == ':' {
                    let Some(v) = parse(&l, cur_minus) else { break };
                    spec.index_default.push(v);
                    if rl != ':' {
                        spec.split = rl;
                        spec.before_rule = rus[..len as usize].iter().collect();
                        return spec;
                    }
                } else {
                    break;
                }
                l.clear();
                cur_minus = false;
            }
        }
    }
    // 不是索引结构：整条规则当选择器
    IndexSpec { split: ' ', before_rule: full, ..Default::default() }
}

/// 把索引规格展开成（保序、去重、不越界的）下标列表
fn resolve_indexes(spec: &IndexSpec, len: usize) -> Vec<usize> {
    let len_i = len as i32;
    let mut set: Vec<usize> = Vec::new();
    if len == 0 { return set; }
    fn push(v: usize, set: &mut Vec<usize>) { if !set.contains(&v) { set.push(v); } }
    // 注意：len > 0 时所有 push 的下标都已在 [0, len) 内

    if spec.indexes.is_empty() {
        for &it in spec.index_default.iter().rev() {
            if it >= 0 && it < len_i { push(it as usize, &mut set); }
            else if it < 0 && len_i >= -it { push((it + len_i) as usize, &mut set); }
        }
    } else {
        for idx in spec.indexes.iter().rev() {
            match idx {
                Idx::Range(sx, ex, stepx) => {
                    let mut start = sx.unwrap_or(0);
                    if start < 0 { start += len_i; }
                    let mut end = ex.unwrap_or(len_i - 1);
                    if end < 0 { end += len_i; }
                    if (start < 0 && end < 0) || (start >= len_i && end >= len_i) { continue; }
                    if start >= len_i { start = len_i - 1; } else if start < 0 { start = 0; }
                    if end >= len_i { end = len_i - 1; } else if end < 0 { end = 0; }
                    if start == end || *stepx >= len_i {
                        push(start as usize, &mut set);
                        continue;
                    }
                    let step = if *stepx > 0 { *stepx } else if -*stepx < len_i { *stepx + len_i } else { 1 };
                    if end > start {
                        let mut i = start;
                        while i <= end { push(i as usize, &mut set); i += step; }
                    } else {
                        let mut i = start;
                        while i >= end { push(i as usize, &mut set); i -= step; }
                    }
                }
                Idx::One(it) => {
                    let it = *it;
                    if it >= 0 && it < len_i { push(it as usize, &mut set); }
                    else if it < 0 && len_i >= -it { push((it + len_i) as usize, &mut set); }
                }
            }
        }
    }
    set
}

// ───────────────────────── 取元素 ─────────────────────────

fn own_text_of(el: &ElementRef<'_>) -> String {
    let mut s = String::new();
    for c in el.children() {
        if let Node::Text(t) = c.value() { s.push_str(t); }
    }
    s
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space { out.push(' '); prev_space = true; }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim_end().to_string()
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "br" | "li" | "ul" | "ol" | "tr" | "table" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" | "dd" | "dt" | "dl" | "section" | "article" | "header" | "footer"
            | "blockquote" | "pre" | "hr" | "form" | "body" | "html" | "nav" | "aside" | "main"
    )
}

/// 近似 jsoup `Element.text()`：拼接所有文本，块级元素边界补空格，折叠空白
fn full_text(doc: &Doc, id: NodeId) -> String {
    fn walk(n: ego_tree::NodeRef<'_, Node>, out: &mut String) {
        match n.value() {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) => {
                let name = e.name();
                if matches!(name, "script" | "style") { return; }
                let blk = is_block(name);
                if blk { out.push(' '); }
                for c in n.children() { walk(c, out); }
                if blk { out.push(' '); }
            }
            _ => { for c in n.children() { walk(c, out); } }
        }
    }
    let mut s = String::new();
    if let Some(n) = doc.html.tree.get(id) { walk(n, &mut s); }
    collapse_ws(s.trim_start())
}

/// 对应 `ElementsSingle.getElementsSingle`
fn elements_single(doc: &Doc, node: NodeId, rule: &str) -> Vec<NodeId> {
    let spec = find_index_set(rule);

    let elements: Vec<NodeId> = if spec.before_rule.is_empty() {
        doc.children(node)
    } else {
        let parts: Vec<&str> = spec.before_rule.split('.').collect();
        let arg = parts.get(1).copied().unwrap_or("");
        match parts[0] {
            "children" => doc.children(node),
            "class" => doc
                .descendants(node)
                .into_iter()
                .filter(|&i| {
                    doc.el(i)
                        .map(|e| e.value().classes().any(|c| c.eq_ignore_ascii_case(arg)))
                        .unwrap_or(false)
                })
                .collect(),
            "tag" => doc
                .descendants(node)
                .into_iter()
                .filter(|&i| doc.el(i).map(|e| e.value().name().eq_ignore_ascii_case(arg)).unwrap_or(false))
                .collect(),
            "id" => doc
                .descendants(node)
                .into_iter()
                .filter(|&i| doc.el(i).map(|e| e.value().id() == Some(arg)).unwrap_or(false))
                .collect(),
            "text" => {
                let needle = arg.to_lowercase();
                doc.descendants(node)
                    .into_iter()
                    .filter(|&i| {
                        doc.el(i)
                            .map(|e| own_text_of(&e).to_lowercase().contains(&needle))
                            .unwrap_or(false)
                    })
                    .collect()
            }
            _ => css_in(doc, node, &spec.before_rule),
        }
    };

    let idx = resolve_indexes(&spec, elements.len());
    match spec.split {
        '!' => elements
            .iter()
            .enumerate()
            .filter(|(i, _)| !idx.contains(i))
            .map(|(_, id)| *id)
            .collect(),
        '.' => idx.into_iter().map(|i| elements[i]).collect(),
        _ => elements,
    }
}

/// 在 node 子树内（含自身）执行 CSS 选择器
fn css_in(doc: &Doc, node: NodeId, selector: &str) -> Vec<NodeId> {
    let Ok(sel) = Selector::parse(selector.trim()) else { return vec![] };
    let scope: HashSet<NodeId> = doc.descendants(node).into_iter().collect();
    doc.html
        .select(&sel)
        .map(|e| e.id())
        .filter(|id| scope.contains(id))
        .collect()
}

// ───────────────────────── 输出 ─────────────────────────

fn script_style_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<script[\s\S]*?</script>|<style[\s\S]*?</style>").unwrap())
}

/// 对应 `getResultLast`
fn result_last(doc: &Doc, elements: &[NodeId], last_rule: &str) -> Vec<String> {
    let mut out = Vec::new();
    match last_rule {
        "text" => {
            for &id in elements {
                let t = full_text(doc, id);
                if !t.is_empty() { out.push(t); }
            }
        }
        "textNodes" => {
            for &id in elements {
                let Some(e) = doc.el(id) else { continue };
                let parts: Vec<String> = e
                    .children()
                    .filter_map(|c| match c.value() {
                        Node::Text(t) => Some(collapse_ws(t.trim())),
                        _ => None,
                    })
                    .filter(|t| !t.is_empty())
                    .collect();
                if !parts.is_empty() { out.push(parts.join("\n")); }
            }
        }
        "ownText" => {
            for &id in elements {
                let Some(e) = doc.el(id) else { continue };
                let t = collapse_ws(own_text_of(&e).trim_start());
                if !t.is_empty() { out.push(t); }
            }
        }
        "html" => {
            let joined: Vec<String> = elements
                .iter()
                .filter_map(|&id| doc.el(id).map(|e| script_style_re().replace_all(&e.html(), "").to_string()))
                .collect();
            let html = joined.join("\n");
            if !html.is_empty() { out.push(html); }
        }
        "all" => {
            let joined: Vec<String> = elements.iter().filter_map(|&id| doc.el(id).map(|e| e.html())).collect();
            out.push(joined.join("\n"));
        }
        attr => {
            for &id in elements {
                let Some(e) = doc.el(id) else { continue };
                let v = e.value().attr(attr).unwrap_or("");
                if v.trim().is_empty() || out.iter().any(|x| x == v) { continue; }
                out.push(v.to_string());
            }
        }
    }
    out
}

// ───────────────────────── 对外接口 ─────────────────────────

/// 对应 `AnalyzeByJSoup.getElements`（单条规则，不含 `&&`/`||`/`%%`）：返回元素的 outerHTML
pub fn elements(src: &str, rule: &str) -> Vec<String> {
    let rule = trim_rule(rule);
    if rule.is_empty() { return vec![]; }
    let doc = Doc::parse(src);
    let steps = split_at(rule);
    let mut current = vec![doc.root];
    for step in &steps {
        let mut next: Vec<NodeId> = Vec::new();
        for &n in &current {
            for r in elements_single(&doc, n, step) {
                if !next.contains(&r) { next.push(r); }
            }
        }
        current = next;
        if current.is_empty() { break; }
    }
    current.into_iter().filter_map(|id| doc.el(id).map(|e| e.html())).collect()
}

/// 对应 `AnalyzeByJSoup.getResultList`（单条规则）：最后一步是输出修饰符
pub fn result_list(src: &str, rule: &str) -> Vec<String> {
    let rule = trim_rule(rule);
    if rule.is_empty() { return vec![]; }
    let doc = Doc::parse(src);
    let steps = split_at(rule);
    let last = steps.len() - 1;
    let mut current = vec![doc.root];
    for step in &steps[..last] {
        let mut next: Vec<NodeId> = Vec::new();
        for &n in &current {
            next.extend(elements_single(&doc, n, step));
        }
        current = next;
    }
    if current.is_empty() { return vec![]; }
    result_last(&doc, &current, steps[last].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<div id="list">
        <ul class="books big">
          <li class="item"><a href="/b/1" class="t">书一</a><span class="au">作者甲</span></li>
          <li class="item"><a href="/b/2" class="t">书二</a><span class="au">作者乙</span></li>
          <li class="item last"><a href="/b/3" class="t">书三</a><span class="au">作者丙</span></li>
        </ul>
        <p>前缀 <b>加粗</b> 后缀</p><p>第二段</p>
      </div>"#;

    #[test]
    fn index_default_forms() {
        let s = find_index_set("tag.div.0");
        assert_eq!((s.split, s.before_rule.as_str()), ('.', "tag.div"));
        let s = find_index_set("tag.div!0:3");
        assert_eq!((s.split, s.before_rule.as_str()), ('!', "tag.div"));
        assert_eq!(s.index_default, vec![3, 0]);
        let s = find_index_set("tag.div.-1:10:2");
        assert_eq!(s.index_default, vec![2, 10, -1]);
        // 纯选择器：不是索引
        let s = find_index_set("div.item");
        assert_eq!((s.split, s.before_rule.as_str()), (' ', "div.item"));
        let s = find_index_set("a.col-3");
        assert_eq!(s.split, ' ');
    }

    #[test]
    fn index_bracket_forms() {
        let s = find_index_set("tag.div[-1, 3:-2:-10, 2]");
        assert_eq!(s.before_rule, "tag.div");
        assert_eq!(s.indexes.len(), 3);
        let s = find_index_set("tag.li[!0,2]");
        assert_eq!((s.split, s.before_rule.as_str()), ('!', "tag.li"));
        // CSS 属性选择器不应被当索引
        let s = find_index_set("a[href^=\"http\"]");
        assert_eq!(s.split, ' ');
    }

    #[test]
    fn class_tag_chain() {
        assert_eq!(result_list(HTML, "class.item@tag.a@text"), vec!["书一", "书二", "书三"]);
        assert_eq!(result_list(HTML, "class.item@tag.a@href"), vec!["/b/1", "/b/2", "/b/3"]);
        assert_eq!(result_list(HTML, "class.item.1@tag.a@text"), vec!["书二"]);
        assert_eq!(result_list(HTML, "class.item.-1@tag.a@text"), vec!["书三"]);
        assert_eq!(result_list(HTML, "id.list@tag.p.0@text"), vec!["前缀 加粗 后缀"]);
    }

    #[test]
    fn exclude_and_slice() {
        assert_eq!(result_list(HTML, "class.item!0@tag.a@text"), vec!["书二", "书三"]);
        assert_eq!(result_list(HTML, "class.item[!0,2]@tag.a@text"), vec!["书二"]);
        assert_eq!(result_list(HTML, "class.item[-1:0]@tag.a@text"), vec!["书三", "书二", "书一"]);
        // 老写法 `.0:2` 是“索引列表”（第 0 和第 2 个），不是区间
        assert_eq!(result_list(HTML, "class.item.0:2@tag.a@text"), vec!["书一", "书三"]);
    }

    #[test]
    fn children_and_text_step() {
        assert_eq!(result_list(HTML, "class.books@children.0@tag.a@text"), vec!["书一"]);
        assert_eq!(result_list(HTML, "text.第二@text"), vec!["第二段"]);
    }

    #[test]
    fn css_fallback_and_attr() {
        assert_eq!(result_list(HTML, "li.item a@text"), vec!["书一", "书二", "书三"]);
        assert_eq!(result_list(HTML, "ul > li:nth-child(2) a@href"), vec!["/b/2"]);
        // 单段规则：作用在根元素上（取属性/文本）
        assert_eq!(result_list("<a href=\"/x\">标题</a>", "href"), vec!["/x"]);
        assert_eq!(result_list("<a href=\"/x\">标题</a>", "text"), vec!["标题"]);
    }

    #[test]
    fn elements_for_list() {
        let els = elements(HTML, "class.item");
        assert_eq!(els.len(), 3);
        // 回到元素上继续取值
        assert_eq!(result_list(&els[2], "tag.a@text"), vec!["书三"]);
        assert_eq!(result_list(&els[2], "class.au@text"), vec!["作者丙"]);
        assert_eq!(elements(HTML, "tag.li.0").len(), 1);
        assert_eq!(elements(HTML, "class.item!0").len(), 2);
    }

    #[test]
    fn table_rows() {
        let page = "<table><tr><td><a href='/1'>甲</a></td></tr><tr><td><a href='/2'>乙</a></td></tr></table>";
        let rows = elements(page, "tag.tr");
        assert_eq!(rows.len(), 2);
        assert_eq!(result_list(&rows[1], "tag.a@href"), vec!["/2"]);
        assert_eq!(result_list(&rows[1], "tag.td.0@text"), vec!["乙"]);
    }

    #[test]
    fn multi_root_fragment_keeps_siblings() {
        let page = r#"<ul class="list"><li>a</li></ul><a class="next" href="/p2">下一页</a>"#;
        assert_eq!(result_list(page, "class.next@href"), vec!["/p2"]);
        assert_eq!(result_list(page, "tag.li@text"), vec!["a"]);
        // 单元素则以它自身为根（children 相对该元素）
        let one = "<ul><li>x</li><li>y</li></ul>";
        assert_eq!(result_list(one, "children.1@text"), vec!["y"]);
    }

    /// 随机拼接规则片段，保证解析器对任意（含畸形）输入都不会 panic
    #[test]
    fn malformed_rules_never_panic() {
        let toks = [
            "class.", "tag.", "id.", "text.", "children", "li", "a", "div", ".", "!", "[", "]", ":",
            "-", "0", "1", "2", "9", ",", " ", "@", "text", "href", "(", ")", "'", "\"", ">", "#",
            "*", "html", "ownText", "999999999999", "[-1:0]", "[!0,1]", ".0", "!0:3",
        ];
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = || { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) as usize };
        for _ in 0..3000 {
            let n = 1 + next() % 8;
            let rule: String = (0..n).map(|_| toks[next() % toks.len()]).collect();
            let _ = result_list(HTML, &rule);
            let _ = elements(HTML, &rule);
        }
    }

    #[test]
    fn modifiers() {
        let h = "<div class='c'>头<b>粗</b>尾<script>x()</script></div>";
        assert_eq!(result_list(h, "class.c@ownText"), vec!["头尾"]);
        assert_eq!(result_list(h, "class.c@textNodes"), vec!["头\n尾"]);
        assert!(!result_list(h, "class.c@html")[0].contains("script"));
        assert!(result_list(h, "class.c@all")[0].contains("script"));
    }

    #[test]
    fn full_document_root() {
        let page = "<html><head><title>T</title></head><body><div id='a'>内容</div></body></html>";
        assert_eq!(result_list(page, "id.a@text"), vec!["内容"]);
        assert_eq!(result_list(page, "tag.title@text"), vec!["T"]);
    }
}
