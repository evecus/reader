//! 求值过程中的动态值类型。
//! 不用 scraper 的 ElementRef（带生命周期，跨函数难传递），改存元素 HTML 字符串。

use scraper::{Html, Selector};
use serde_json::Value;

/// 规则求值的中间值
#[derive(Clone, Debug)]
pub enum EvalValue {
    Null,
    Str(String),
    List(Vec<EvalValue>),
    /// 元素节点集合：存每个元素的 outerHTML，后续再解析取属性/文本
    Elements(Vec<String>),
    /// JSON 对象
    Json(Value),
    Bool(bool),
    Num(f64),
}

impl EvalValue {
    pub fn as_str(&self) -> String {
        match self {
            EvalValue::Null => String::new(),
            EvalValue::Str(s) => s.clone(),
            EvalValue::Bool(b) => b.to_string(),
            EvalValue::Num(n) => if n.fract() == 0.0 { format!("{:.0}", n) } else { n.to_string() },
            EvalValue::Json(v) => match v {
                Value::String(s) => s.clone(),
                _ => v.to_string(),
            },
            EvalValue::Elements(es) => es.iter().map(|h| element_text(h)).collect::<Vec<_>>().join("\n"),
            EvalValue::List(l) => l.iter().map(|v| v.as_str()).collect::<Vec<_>>().join("\n"),
        }
    }

    pub fn as_list(&self) -> Vec<EvalValue> {
        match self {
            EvalValue::List(l) => l.clone(),
            EvalValue::Elements(es) => es.iter().map(|h| EvalValue::Str(element_text(h))).collect(),
            EvalValue::Str(s) => {
                if s.is_empty() { vec![] }
                else { s.split('\n').map(|x| EvalValue::Str(x.to_string())).collect() }
            }
            EvalValue::Null => vec![],
            other => vec![other.clone()],
        }
    }

    pub fn is_null(&self) -> bool { matches!(self, EvalValue::Null) }
}

/// 把可能的表片段包装成可解析的完整上下文
fn parse_element(html: &str) -> scraper::Html {
    let head: String = html.chars().take(256).collect();
    let hl = head.to_ascii_lowercase();
    if hl.contains("<tr") || hl.contains("<td") {
        Html::parse_document(&format!("<table>{}</table>", html))
    } else {
        Html::parse_document(&format!("<div>{}</div>", html))
    }
}

/// 从元素 HTML 提取文本
pub fn element_text(html: &str) -> String {
    let doc = parse_element(html);
    doc.root_element().text().collect::<String>().trim().to_string()
}

/// 从元素 HTML 取属性或文本
pub fn element_attr_or_text(html: &str, rule: &str) -> Vec<String> {
    let rule = rule.trim();
    if matches!(rule, "html" | "all") { return vec![html.to_string()]; }

    // 提取该片段的首个标签名，用选择器精确选中它（这样 attr 才取到目标元素）
    let tag: String = html.trim_start_matches('<')
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let selector_str = if tag.is_empty() { "*".to_string() } else { tag };
    let Ok(selector) = Selector::parse(&selector_str) else { return vec![]; };

    let doc = parse_element(html);
    let results: Vec<String> = if rule == "text" || rule.is_empty() {
        doc.select(&selector)
            .map(|e| e.text().collect::<String>().trim().to_string())
            .filter(|t| !t.is_empty()).collect()
    } else {
        doc.select(&selector)
            .filter_map(|e| e.value().attr(rule).map(|v| v.to_string()))
            .filter(|v| !v.is_empty()).collect()
    };
    results
}

/// 从 HTML 中按 CSS 选择器提取元素（返回 outerHTML 列表）
pub fn css_elements(html: &str, selector: &str) -> Result<Vec<String>, String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(selector).map_err(|e| format!("CSS '{}': {:?}", selector, e))?;
    Ok(doc.select(&sel).map(|e| e.html()).collect())
}
