//! HTML 正文格式化，对齐官方 legado `utils/HtmlFormatter.kt`（formatKeepImg 语义）。
//! 供 `java.htmlFormat(str, redirectUrl)` 使用。

use regex::Regex;
use std::sync::{Mutex, OnceLock};

fn re(pat: &str) -> Regex {
    static CACHE: OnceLock<Mutex<Vec<(String, Regex)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::<(String, Regex)>::new()));
    let mut guard = cache.lock().unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
    if let Some((_, r)) = guard.iter().find(|(p, _)| p == pat) {
        return r.clone();
    }
    let r = Regex::new(pat).expect("invalid regex in htmlfmt");
    guard.push((pat.to_string(), r.clone()));
    r
}

/// 清理转义换行符（官方 cleanBlankTags 的前置处理）
fn clean_blank_tags(html: &str) -> String {
    html.replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// 图片 src 解析优先级（对齐官方 imgSearchRegex 三条规则）：
/// data-src / data-original / srcset > src > 其他 data-* 属性
fn extract_img_src(img_html: &str) -> Option<String> {
    let data_src = re(r#"(?i)\s(?:data-src|data-original|srcset)\s*=\s*['"]([^'"]+)['"]"#);
    if let Some(m) = data_src.captures(img_html) {
        return Some(m[1].to_string());
    }
    let plain_src = re(r#"(?i)\ssrc\s*=\s*['"]([^'"]+)['"]"#);
    if let Some(m) = plain_src.captures(img_html) {
        return Some(m[1].to_string());
    }
    let data_attr = re(r#"(?i)\s(data-[\w-]*)\s*=\s*['"]([^'"]+)['"]"#);
    if let Some(m) = data_attr.captures(img_html) {
        return Some(m[2].to_string());
    }
    None
}

/// 相对 URL → 绝对 URL；剥离官方 paramPattern（`,{...}` 选项后缀）
fn absolutize(base: Option<&str>, src: &str) -> String {
    // 先去掉 ,{...} 选项（AnalyzeUrl.paramPattern）
    let url_part = if let Some(idx) = src.rfind(",{") { &src[..idx] } else { src };
    if url_part.starts_with("http://") || url_part.starts_with("https://") || base.is_none() {
        return url_part.to_string();
    }
    match url::Url::parse(base.unwrap()) {
        Ok(b) => b.join(url_part.trim()).map(|u| u.to_string()).unwrap_or_else(|_| url_part.to_string()),
        Err(_) => url_part.to_string(),
    }
}

/// 对齐 HtmlFormatter.formatKeepImg：
/// 保留 <img> 标签、剥离其余 HTML、段落换行缩进、图片 src 绝对化
pub fn format_keep_img(html: &str, redirect_url: Option<&str>) -> String {
    let s = clean_blank_tags(html);

    // 实体/不可打印字符
    let s = re(r"(&nbsp;)+").replace_all(&s, " ").to_string();
    let s = re(r"(&ensp;|&emsp;)").replace_all(&s, " ").to_string();
    let s = re(r"(&thinsp;|&zwnj;|&zwj;|\u{2009}|\u{200C}|\u{200D})")
        .replace_all(&s, "")
        .to_string();

    // 块级元素 → 换行（对齐 wrapHtmlRegex）
    let s = re(r#"(?i)</?(?:div|p|br|hr|h[1-6]|article|dd|dl|blockquote)[^>]*>"#)
        .replace_all(&s, "\n")
        .to_string();

    // 注释剥离（官方 commentRegex）
    let s = re(r#"<!--[^>]*-->"#).replace_all(&s, "").to_string();

    // 保护 img 标签后剥离其余标签
    let img_re = re(r"(?i)<img[^>]*>");
    let mut placeholders = Vec::new();
    let mut protected = String::new();
    let mut last = 0;
    for m in img_re.find_iter(&s) {
        placeholders.push(m.as_str().to_string());
        protected.push_str(&s[last..m.start()]);
        protected.push_str(&format!("\u{0}IMG{}\u{0}", placeholders.len() - 1));
        last = m.end();
    }
    protected.push_str(&s[last..]);

    let stripped = re(r"(?i)</?[a-zA-Z]+[^>]*>").replace_all(&protected, "").to_string();

    // 恢复 img，并做 src 绝对化
    let mut out = stripped;
    for (i, img) in placeholders.iter().enumerate() {
        let processed = if let Some(src) = extract_img_src(img) {
            let abs = absolutize(redirect_url, &src);
            img.replace(&src, &abs)
        } else {
            img.clone()
        };
        out = out.replace(&format!("\u{0}IMG{}\u{0}", i), &processed);
    }

    // 缩进与空行收敛（对齐 indentRegex / indent2Regex / lastRegex）
    let out = re(r"\s*\n+\s*").replace_all(&out, "\n").to_string();
    let mut out = re(r"^[\n\s]+").replace(&out, "").to_string();
    out = re(r"[\n\s]+$").replace(&out, "").to_string();
    out.replace('\n', "\n　　")
}
