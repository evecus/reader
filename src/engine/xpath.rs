//! 完整 XPath 1.0 支持。
//!
//! Legado 的 XPath 模式基于 JsoupXPath（XPath 1.0）。这里用 sxd-xpath 做真正的
//! XPath 1.0 求值，完整支持轴（child/descendant/parent/ancestor/following-sibling...）、
//! 谓词（[n]/[last()]/[@attr='v']/[contains(...)]/逻辑运算）、函数（text()/string()/
//! contains()/starts-with()/normalize-space()/count()/not()...）与节点集运算。
//!
//! 脏 HTML 的容错由 scraper(html5ever) 负责：先把 HTML 解析成规范 DOM，再按树结构
//! 逐节点复制成 sxd 的 XML DOM（不经字符串往返，避免实体/编码二次损坏），最后求值。

use crate::engine::value::EvalValue;
use crate::error::{AppError, AppResult};
use scraper::Html;
use sxd_document::dom::{Document, Element as SxdElement, Root as SxdRoot, Text as SxdText};
use sxd_document::Package;
use sxd_xpath::function::{register_core_functions, Error as FnError, Function};
use sxd_xpath::nodeset::Node;
use sxd_xpath::{EvaluationContext, Factory, Functions, Namespaces, Value as XpValue, Variables};

/// 从（可能很脏的）HTML 构建良构的 sxd XML DOM。
///
/// 采用深度优先遍历 scraper 的 ego-tree，用栈维护当前 sxd 父节点：
/// - 元素节点：create_element + 复制全部属性 + append 到父节点，入栈
/// - 文本节点：create_text + append 到父节点
/// - 关闭元素时出栈
/// Document/Fragment 根节点自身不映射，只遍历其子节点。
/// DOM 父节点抽象：顶层挂在 sxd 的 Root，其余挂在 Element。
/// 这样 `/html/body/...` 绝对路径能正确匹配（不引入多余的包裹元素）。
enum Parent<'d> {
    Root(SxdRoot<'d>),
    Element(SxdElement<'d>),
}

impl<'d> Parent<'d> {
    fn append_element(&self, el: SxdElement<'d>) {
        match self {
            Parent::Root(r) => { r.append_child(el); }
            Parent::Element(e) => { e.append_child(el); }
        }
    }
    fn append_text(&self, t: SxdText<'d>) {
        match self {
            Parent::Root(_) => { /* document root 不接受裸文本，忽略 */ }
            Parent::Element(e) => { e.append_child(t); }
        }
    }
}

fn build_dom(html: &str) -> Package {
    let scraper_doc = Html::parse_document(html);
    let package = Package::new();

    let root = scraper_doc.tree.root();
    let doc: Document = package.as_document();

    use ego_tree::iter::Edge;
    let mut stack: Vec<Parent> = vec![Parent::Root(doc.root())];
    // 每个 Open 是否入栈（供 Close 对称出栈）
    let mut opened: Vec<bool> = vec![false];

    for edge in root.traverse() {
        match edge {
            Edge::Open(n) => {
                match n.value() {
                    scraper::node::Node::Element(el) => {
                        let sxd_el = doc.create_element(el.name());
                        for (k, v) in el.attrs() {
                            sxd_el.set_attribute_value(k, v);
                        }
                        if let Some(parent) = stack.last() {
                            parent.append_element(sxd_el);
                        }
                        stack.push(Parent::Element(sxd_el));
                        opened.push(true);
                    }
                    scraper::node::Node::Text(t) => {
                        let txt = doc.create_text(t.text.as_ref());
                        if let Some(parent) = stack.last() {
                            parent.append_text(txt);
                        }
                        opened.push(false);
                    }
                    // Document / Fragment / Comment / PI 不映射为 sxd 节点
                    _ => { opened.push(false); }
                }
            }
            Edge::Close(_n) => {
                if opened.pop().unwrap_or(false) {
                    stack.pop();
                }
            }
        }
    }

    package
}

/// 把 XPath 值按 XPath 1.0 规则转成字符串（节点集取第一个节点的 string-value）。
fn xp_to_string(v: &XpValue) -> String {
    match v {
        XpValue::Nodeset(ns) => ns.iter().next().map(|n| n.string_value()).unwrap_or_default(),
        XpValue::String(s) => s.to_string(),
        XpValue::Number(n) => if n.fract() == 0.0 { format!("{:.0}", n) } else { n.to_string() },
        XpValue::Boolean(b) => b.to_string(),
    }
}

/// 双字符串谓词函数（contains / starts-with），自动把 nodeset 参数转成字符串。
/// sxd-xpath 0.2.1 的 core 版本不接受 nodeset，这里覆盖注册。
struct StrPredicate { mode: u8 } // 0=contains 1=starts-with 2=ends-with
impl Function for StrPredicate {
    fn evaluate<'a, 'd>(
        &self,
        _ctx: &EvaluationContext<'a, 'd>,
        args: Vec<XpValue<'d>>,
    ) -> Result<XpValue<'d>, FnError> {
        let a = args.get(0).map(xp_to_string).unwrap_or_default();
        let b = args.get(1).map(xp_to_string).unwrap_or_default();
        let r = match self.mode {
            0 => a.contains(&b),
            1 => a.starts_with(&b),
            _ => a.ends_with(&b),
        };
        Ok(XpValue::Boolean(r))
    }
}

/// 单字符串函数（normalize-space / string-length），nodeset 自动转字符串。
struct StrUnary { mode: u8 } // 0=normalize-space 1=string-length 2=string
impl Function for StrUnary {
    fn evaluate<'a, 'd>(
        &self,
        _ctx: &EvaluationContext<'a, 'd>,
        args: Vec<XpValue<'d>>,
    ) -> Result<XpValue<'d>, FnError> {
        let s = args.get(0).map(xp_to_string).unwrap_or_default();
        match self.mode {
            0 => Ok(XpValue::String(s.split_whitespace().collect::<Vec<_>>().join(" "))),
            1 => Ok(XpValue::Number(s.chars().count() as f64)),
            _ => Ok(XpValue::String(s)),
        }
    }
}

/// 注册核心函数 + 覆盖书源常用的字符串函数（修正 nodeset 强转问题）。
fn build_functions() -> Functions {
    let mut functions: Functions = Functions::new();
    register_core_functions(&mut functions);
    functions.insert("contains".into(), Box::new(StrPredicate { mode: 0 }));
    functions.insert("starts-with".into(), Box::new(StrPredicate { mode: 1 }));
    functions.insert("ends-with".into(), Box::new(StrPredicate { mode: 2 }));
    functions.insert("normalize-space".into(), Box::new(StrUnary { mode: 0 }));
    functions.insert("string-length".into(), Box::new(StrUnary { mode: 1 }));
    functions.insert("string".into(), Box::new(StrUnary { mode: 2 }));
    functions
}

/// 执行一条 XPath 规则，返回字符串列表（对应 Legado getStringList 语义）。
///
/// 规则形如：
/// - `//div[@class='content']`  → 元素节点集，取各节点 string-value（后代文本拼接）
/// - `//a/@href`               → 属性节点集，取属性值
/// - `//h1/text()`             → 文本节点集
/// - `string(//h1)`            → 单个字符串
/// - `count(//a)`              → 数字
pub fn xpath_select(html: &str, rule: &str) -> AppResult<EvalValue> {
    let rule = rule.trim();
    // 去掉可能的 @XPath: 前缀与开头的相对标记
    let mut expr = rule;
    if expr.to_ascii_lowercase().starts_with("@xpath:") {
        expr = &expr[7..];
    }
    // Legado 中 ".//" 表示相对当前节点；顶层求值时等价于 "//"
    if expr.starts_with(".//") {
        expr = &expr[1..];
    }
    expr = expr.trim();
    if expr.is_empty() {
        return Ok(EvalValue::List(vec![]));
    }

    let package = build_dom(html);
    let doc = package.as_document();

    let factory = Factory::new();
    let xpath = factory
        .build(expr)
        .map_err(|e| AppError::bad_request(format!("XPath '{}' 解析失败: {:?}", expr, e)))?
        .ok_or_else(|| AppError::bad_request(format!("XPath '{}' 为空表达式", expr)))?;

    let functions = build_functions();
    let variables: Variables = Variables::new();
    let namespaces: Namespaces = Namespaces::new();
    let context = EvaluationContext::new(doc.root(), &functions, &variables, &namespaces);

    let value = xpath
        .evaluate(&context)
        .map_err(|e| AppError::bad_request(format!("XPath '{}' 求值失败: {:?}", expr, e)))?;

    Ok(value_to_eval(&value))
}

/// 执行 XPath 并返回元素/节点的 outerXML 列表（用于 bookList / chapterList 这类"取元素集"场景）。
/// 对每个结果节点：元素→序列化其子树为 XML 字符串；属性/文本→其 string-value。
pub fn xpath_elements(html: &str, rule: &str) -> AppResult<Vec<String>> {
    let rule = rule.trim();
    let mut expr = rule;
    if expr.to_ascii_lowercase().starts_with("@xpath:") {
        expr = &expr[7..];
    }
    if expr.starts_with(".//") {
        expr = &expr[1..];
    }
    expr = expr.trim();
    if expr.is_empty() {
        return Ok(vec![]);
    }

    let package = build_dom(html);
    let doc = package.as_document();
    let factory = Factory::new();
    let xpath = factory
        .build(expr)
        .map_err(|e| AppError::bad_request(format!("XPath '{}' 解析失败: {:?}", expr, e)))?
        .ok_or_else(|| AppError::bad_request(format!("XPath '{}' 为空表达式", expr)))?;

    let functions = build_functions();
    let variables: Variables = Variables::new();
    let namespaces: Namespaces = Namespaces::new();
    let context = EvaluationContext::new(doc.root(), &functions, &variables, &namespaces);
    let value = xpath
        .evaluate(&context)
        .map_err(|e| AppError::bad_request(format!("XPath '{}' 求值失败: {:?}", expr, e)))?;

    let mut out = Vec::new();
    if let XpValue::Nodeset(ns) = &value {
        for node in ns.iter() {
            match node {
                Node::Element(el) => out.push(serialize_element(el)),
                other => {
                    let s = other.string_value();
                    if !s.trim().is_empty() {
                        out.push(s);
                    }
                }
            }
        }
    } else {
        let s = value.string();
        if !s.is_empty() {
            out.push(s);
        }
    }
    Ok(out)
}

/// 把 sxd XPath 的求值结果转成 EvalValue
fn value_to_eval(v: &XpValue) -> EvalValue {
    match v {
        XpValue::Nodeset(ns) => {
            let list: Vec<EvalValue> = ns
                .iter()
                .map(|n| {
                    let s = n.string_value();
                    EvalValue::Str(s.trim().to_string())
                })
                .filter(|e| !e.as_str().is_empty())
                .collect();
            EvalValue::List(list)
        }
        XpValue::String(s) => EvalValue::Str(s.to_string()),
        XpValue::Number(n) => EvalValue::Num(*n),
        XpValue::Boolean(b) => EvalValue::Bool(*b),
    }
}

/// 递归序列化一个 sxd 元素及其子树为 XML 字符串（outerXML）
fn serialize_element(el: SxdElement<'_>) -> String {
    let mut out = String::new();
    serialize_into(el, &mut out);
    out
}

fn serialize_into(el: SxdElement<'_>, out: &mut String) {
    let name = el.name().local_part();
    out.push('<');
    out.push_str(name);
    for attr in el.attributes() {
        out.push(' ');
        out.push_str(attr.name().local_part());
        out.push_str("=\"");
        out.push_str(&escape_xml(attr.value()));
        out.push('"');
    }
    out.push('>');
    for child in el.children() {
        match child {
            sxd_document::dom::ChildOfElement::Element(c) => serialize_into(c, out),
            sxd_document::dom::ChildOfElement::Text(t) => out.push_str(&escape_xml(t.text())),
            sxd_document::dom::ChildOfElement::Comment(_) | sxd_document::dom::ChildOfElement::ProcessingInstruction(_) => {}
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<!DOCTYPE html><html><body>
      <div id="list">
        <ul class="items">
          <li class="row odd" data-id="1"><a href="/book/1">书名甲</a><span class="author">作者甲</span></li>
          <li class="row even" data-id="2"><a href="/book/2">书名乙</a><span class="author">作者乙</span></li>
          <li class="row odd" data-id="3"><a href="/book/3">书名丙</a><span class="author">作者丙</span></li>
        </ul>
        <div id="content"><p>第一段</p><p>第二段</p><script>alert(1)</script></div>
      </div>
    </body></html>"#;

    fn val(rule: &str) -> Vec<String> {
        match xpath_select(HTML, rule).unwrap() {
            EvalValue::List(l) => l.into_iter().map(|e| e.as_str()).collect(),
            EvalValue::Str(s) => vec![s],
            e => vec![e.as_str()],
        }
    }

    #[test]
    fn absolute_and_descendant() {
        assert_eq!(val("//a"), vec!["书名甲", "书名乙", "书名丙"]);
        assert_eq!(val("/html/body//li/a/text()"), vec!["书名甲", "书名乙", "书名丙"]);
    }

    #[test]
    fn text_node_and_attribute() {
        assert_eq!(val("//li/a/text()"), vec!["书名甲", "书名乙", "书名丙"]);
        assert_eq!(val("//li/a/@href"), vec!["/book/1", "/book/2", "/book/3"]);
        assert_eq!(val("//li/@data-id"), vec!["1", "2", "3"]);
    }

    #[test]
    fn predicates() {
        // 属性等值
        assert_eq!(val("//li[@class='row odd']/a/text()"), vec!["书名甲", "书名丙"]);
        // contains()
        assert_eq!(val("//li[contains(@class,'even')]/a/text()"), vec!["书名乙"]);
        // 位置谓词
        assert_eq!(val("//li[1]/a/text()"), vec!["书名甲"]);
        assert_eq!(val("//li[last()]/a/text()"), vec!["书名丙"]);
        assert_eq!(val("//li[position()>1]/a/text()"), vec!["书名乙", "书名丙"]);
    }

    #[test]
    fn functions() {
        assert_eq!(val("string(//li[1]/a)"), vec!["书名甲"]);
        assert_eq!(val("count(//li)"), vec!["3"]);
        assert_eq!(val("//li[not(@data-id='2')]/a/text()"), vec!["书名甲", "书名丙"]);
        assert_eq!(val("normalize-space(//li[1]/a)"), vec!["书名甲"]);
    }

    #[test]
    fn elements_outer_xml() {
        let els = xpath_elements(HTML, "//div[@id='content']").unwrap();
        assert_eq!(els.len(), 1);
        assert!(els[0].contains("<p>第一段</p>"));
        assert!(els[0].starts_with("<div"));
    }

    #[test]
    fn relative_dot_slash() {
        assert_eq!(val(".//a/text()"), vec!["书名甲", "书名乙", "书名丙"]);
    }
}
