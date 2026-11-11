use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{
    config::Config,
    error::{api_err, ApiResult},
    state::AppState,
};

// ---------------------------------------------------------------- identity

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn compute_book_id(identifier: Option<&str>, content_hash: &str) -> String {
    let anchor = identifier.unwrap_or("").trim().to_lowercase();
    sha256_hex(format!("{anchor}\0{content_hash}").as_bytes())
}

pub fn compute_file_id(rel_path: &str) -> String {
    sha256_hex(rel_path.as_bytes())
}

pub fn resolve_book_id(
    identifier: Option<&str>,
    content_hash: &str,
    previous_id: Option<&str>,
    previous_identifier: Option<&str>,
) -> String {
    let ident = identifier.unwrap_or("").trim().to_lowercase();
    let prev_ident = previous_identifier.unwrap_or("").trim().to_lowercase();
    if let Some(prev) = previous_id {
        if ident == prev_ident {
            return prev.to_string();
        }
    }
    compute_book_id(identifier, content_hash)
}

// ---------------------------------------------------------------- natural sort

/// Orders "vol 2" before "vol 10" by comparing digit runs numerically.
pub fn natural_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    while i < ac.len() && j < bc.len() {
        if ac[i].is_ascii_digit() && bc[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < ac.len() && ac[i].is_ascii_digit() { i += 1; }
            while j < bc.len() && bc[j].is_ascii_digit() { j += 1; }
            let na: String = ac[si..i].iter().collect();
            let nb: String = bc[sj..j].iter().collect();
            let ta = na.trim_start_matches('0');
            let tb = nb.trim_start_matches('0');
            let ord = match (ta.len(), tb.len()) {
                (x, y) if x != y => x.cmp(&y),
                _ => ta.cmp(tb),
            };
            if ord != std::cmp::Ordering::Equal { return ord; }
            if na.len() != nb.len() { return na.len().cmp(&nb.len()); }
        } else {
            let ca = ac[i].to_ascii_lowercase();
            let cb = bc[j].to_ascii_lowercase();
            if ca != cb { return ca.cmp(&cb); }
            i += 1;
            j += 1;
        }
    }
    ac.len().cmp(&bc.len())
}

pub fn natural_sort(values: &mut [String]) {
    values.sort_by(|a, b| natural_compare(a, b));
}

// ---------------------------------------------------------------- metadata

#[derive(Default, Clone)]
pub struct Metadata {
    pub identifier: String,
    pub title: String,
    pub author: String,
    pub publisher: String,
    pub language: String,
    pub isbn: String,
    pub description: String,
    pub series: String,
    pub series_index: Option<f64>,
    pub tags: Vec<String>,
    pub pubdate: String,
    pub source: String,
}

impl Metadata {
    pub fn to_json(&self) -> Value {
        json!({
            "identifier": self.identifier,
            "title": self.title,
            "author": self.author,
            "publisher": self.publisher,
            "language": self.language,
            "isbn": self.isbn,
            "description": self.description,
            "series": self.series,
            "seriesIndex": self.series_index,
            "tags": self.tags,
            "pubdate": self.pubdate,
            "source": self.source,
        })
    }
}

/// Conservative filename parsing: without evidence the whole stem is the
/// title and author stays empty. Matches the Node scanner's stated rule.
pub fn filename_metadata(stem: &str) -> Metadata {
    let mut m = Metadata { source: "filename".into(), ..Default::default() };
    let mut title = stem.trim().to_string();
    // "Title (Author)" trailing marker.
    if let Some(open) = title.rfind(['（', '(']) {
        let close = title.rfind(['）', ')']);
        if let Some(close) = close {
            if close > open && close + 1 >= title.len() - 1 {
                let inner = title[open + 1..close].trim().to_string();
                if !inner.is_empty() && !inner.chars().all(|c| c.is_ascii_digit()) {
                    m.author = inner;
                    title = title[..open].trim().to_string();
                }
            }
        }
    }
    // "[Series 03] Title" / "《Series》 卷3 Title" style series hints.
    if let Some(rest) = title.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let inner = &rest[..end];
            for marker in ["第", "卷", "册", "部", "集"] {
                if let Some(pos) = inner.rfind(marker) {
                    let digits: String = inner[pos + marker.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(n) = digits.parse::<f64>() {
                        m.series = inner[..pos].trim().to_string();
                        m.series_index = Some(n);
                        title = rest[end + 1..].trim().to_string();
                    }
                    break;
                }
            }
        }
    }
    m.title = title;
    m
}

// ---------------------------------------------------------------- text decoding

pub fn decode_text(buf: &[u8]) -> (String, String) {
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(&buf[3..]).into_owned(), "utf-8".into());
    }
    if buf.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return (String::from_utf16_lossy(&u16s), "utf-16le".into());
    }
    if let Ok(s) = std::str::from_utf8(buf) {
        return (s.to_string(), "utf-8".into());
    }
    let (s, _, _) = encoding_rs::GB18030.decode(buf);
    (s.into_owned(), "gb18030".into())
}

fn is_chapter_heading(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() || t.chars().count() > 60 { return false; }
    if t.chars().any(|c| "。！？!?；;".contains(c)) { return false; }
    const UNIT: &str = "章节回卷篇部集";
    const NUM: &[char] = &['0', '1', '2', '3', '4', '5', '5', '6', '7', '8', '9',
        '一', '二', '三', '四', '五', '六', '七', '八', '九', '十', '百', '千', '零', '〇', '两'];
    // Anchored "第<num><unit>[ short title]" — prose that merely contains
    // "第一章" mid-sentence must not split.
    if let Some(after) = t.strip_prefix("第") {
        let numeral_end = after.find(|c: char| !NUM.contains(&c)).unwrap_or(after.len());
        if numeral_end == 0 { return false; }
        let rest = &after[numeral_end..];
        if let Some(unit) = rest.chars().next() {
            if UNIT.contains(unit) {
                return rest[unit.len_utf8()..].chars().count() <= 30;
            }
        }
        return false;
    }
    // "Chapter 12 [title]"
    for marker in ["Chapter", "CHAPTER", "chapter"] {
        if let Some(rest) = t.strip_prefix(marker) {
            let trimmed = rest.trim_start();
            if trimmed.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

pub fn split_chapters(text: &str) -> Vec<(String, String)> {
    let mut chapters: Vec<(String, String)> = Vec::new();
    let mut current_title = String::from("正文");
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        if is_chapter_heading(line) && !current.is_empty() {
            chapters.push((current_title.clone(), current.join("\n")));
            current_title = line.trim().to_string();
            current = Vec::new();
        }
        current.push(line);
    }
    if !current.is_empty() || chapters.is_empty() {
        chapters.push((current_title, current.join("\n")));
    }
    chapters
}

// ---------------------------------------------------------------- XML helper

/// Collect text of elements whose local name (namespace stripped) matches a key.
pub fn xml_collect(xml: &str, wanted: &[&str]) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut stack: Vec<(String, String)> = Vec::new(); // (local name, text)
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e) | Event::Empty(e)) => {
                let raw = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let local = raw.rsplit(':').next().unwrap_or(&raw).to_string();
                stack.push((local, String::new()));
            }
            Ok(Event::Text(e)) => {
                if let Some(last) = stack.last_mut() {
                    last.1.push_str(&e.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(_)) => {
                if let Some((name, text)) = stack.pop() {
                    if wanted.contains(&name.as_str()) && !text.trim().is_empty() {
                        out.push((name, text.trim().to_string()));
                    }
                    if let Some(parent) = stack.last_mut() {
                        parent.1.push(' ');
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
        buf.clear();
    }
    out
}

fn first(wanted: &[(String, String)], key: &str) -> String {
    wanted.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_default()
}

fn all(wanted: &[(String, String)], key: &str) -> Vec<String> {
    wanted.iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone()).collect()
}

// ---------------------------------------------------------------- zip helper

fn zip_read(archive: &mut ZipArchive<File>, name: &str) -> Option<Vec<u8>> {
    let mut f = archive.by_name(name).ok()?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

fn zip_names(archive: &mut ZipArchive<File>) -> Vec<String> {
    (0..archive.len()).filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string())).collect()
}

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp", ".avif"].iter().any(|e| lower.ends_with(e))
}

fn image_content_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".png") { "image/png" }
    else if lower.ends_with(".webp") { "image/webp" }
    else if lower.ends_with(".gif") { "image/gif" }
    else if lower.ends_with(".bmp") { "image/bmp" }
    else if lower.ends_with(".avif") { "image/avif" }
    else { "image/jpeg" }
}

fn normalize_zip_path(base: &str, href: &str) -> String {
    let joined = if base.is_empty() { href.to_string() } else { format!("{base}/{href}") };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ---------------------------------------------------------------- parsed source

pub struct ParsedBook {
    pub format: String,
    pub content_hash: String,
    pub page_count: Option<i64>,
    pub metadata: Metadata,
    pub cover: Option<(Vec<u8>, String)>,
    pub structure: Value,
}

fn empty_structure(kind: &str, total: i64) -> Value {
    json!({
        "kind": kind,
        "total": total,
        "groups": [],
        "items": [],
        "toc": [],
        "files": [],
        "resources": {},
        "chapterTexts": {},
        "zip": false,
        "source": "",
    })
}

fn group(title: &str, count: i64, offset: i64) -> Value {
    json!({ "id": format!("g{offset}"), "seq": offset, "title": title, "count": count, "offset": offset })
}

// ---------------------------------------------------------------- EPUB

fn parse_epub(path: &Path, bytes: &[u8], stem: &str) -> ApiResult<ParsedBook> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let names = zip_names(&mut archive);

    let container = zip_read(&mut archive, "META-INF/container.xml").unwrap_or_default();
    let opf_path = {
        let text = String::from_utf8_lossy(&container);
        // <rootfile full-path="..."/>
        let mut found = String::new();
        if let Some(pos) = text.find("full-path") {
            let rest = &text[pos..];
            if let Some(q) = rest.find('"') {
                let after = &rest[q + 1..];
                if let Some(end) = after.find('"') {
                    found = after[..end].to_string();
                }
            }
        }
        if found.is_empty() {
            names.iter().find(|n| n.ends_with(".opf")).cloned().unwrap_or_default()
        } else { found }
    };
    let opf_bytes = zip_read(&mut archive, &opf_path).unwrap_or_default();
    let opf = String::from_utf8_lossy(&opf_bytes).into_owned();
    let base = opf_path.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default();

    let meta = xml_collect(&opf, &[
        "title", "creator", "identifier", "language", "publisher", "description",
        "date", "subject", "contributor", "rights",
    ]);
    let mut m = Metadata {
        source: "embedded".into(),
        title: first(&meta, "title"),
        author: first(&meta, "creator"),
        identifier: first(&meta, "identifier"),
        language: first(&meta, "language"),
        publisher: first(&meta, "publisher"),
        description: first(&meta, "description"),
        pubdate: first(&meta, "date"),
        ..Default::default()
    };
    m.tags = all(&meta, "subject");
    m.isbn = meta.iter()
        .find(|(k, v)| k == "identifier" && (v.to_lowercase().contains("isbn") || v.chars().filter(|c| c.is_ascii_digit()).count() >= 10))
        .map(|(_, v)| v.clone()).unwrap_or_default();
    if m.title.is_empty() { m.title = stem.to_string(); m.source = "filename".into(); }

    // Manifest id -> (href, media-type, properties)
    let mut manifest: std::collections::HashMap<String, (String, String, String)> = std::collections::HashMap::new();
    let mut cover_href: Option<String> = None;
    {
        use quick_xml::events::Event;
        use quick_xml::Reader;
        let mut reader = Reader::from_str(&opf);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Eof) => break,
                Ok(Event::Start(e) | Event::Empty(e)) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    let local = name.rsplit(':').next().unwrap_or(&name).to_string();
                    let attrs: std::collections::HashMap<String, String> = e.attributes().filter_map(|a| {
                        let a = a.ok()?;
                        Some((String::from_utf8_lossy(a.key.as_ref()).into_owned(), a.unescape_value().unwrap_or_default().into_owned()))
                    }).collect();
                    if local == "item" {
                        let id = attrs.get("id").cloned().unwrap_or_default();
                        let href = attrs.get("href").cloned().unwrap_or_default();
                        let media = attrs.get("media-type").cloned().unwrap_or_default();
                        let props = attrs.get("properties").cloned().unwrap_or_default();
                        if props.contains("cover-image") { cover_href = Some(href.clone()); }
                        manifest.insert(id, (href, media, props));
                    } else if local == "meta" && attrs.get("name").map(|s| s.as_str()) == Some("cover") {
                        if let Some(id) = attrs.get("content") {
                            if let Some((href, _, _)) = manifest.get(id) { cover_href = Some(href.clone()); }
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
            buf.clear();
        }
    }
    // Spine idrefs -> hrefs
    let mut spine: Vec<String> = Vec::new();
    {
        use quick_xml::events::Event;
        use quick_xml::Reader;
        let mut reader = Reader::from_str(&opf);
        let mut in_spine = false;
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Eof) => break,
                Ok(Event::Start(e)) => {
                    let n = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    if n.rsplit(':').next() == Some("spine") { in_spine = true; }
                    else if in_spine && n.rsplit(':').next() == Some("itemref") {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"idref" {
                                let id = a.unescape_value().unwrap_or_default().into_owned();
                                if let Some((href, _, _)) = manifest.get(&id) { spine.push(normalize_zip_path(&base, href)); }
                            }
                        }
                    }
                }
                Ok(Event::End(e)) => {
                    if String::from_utf8_lossy(e.name().as_ref()).rsplit(':').next() == Some("spine") { in_spine = false; }
                }
                Ok(_) => {}
                Err(_) => break,
            }
            buf.clear();
        }
    }
    if spine.is_empty() {
        // Fall back to every xhtml/html file in the archive, naturally ordered.
        let mut docs: Vec<String> = names.iter().filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".xhtml") || l.ends_with(".html") || l.ends_with(".htm")
        }).cloned().collect();
        natural_sort(&mut docs);
        spine = docs;
    }

    // TOC from nav.xhtml or the NCX referenced by the spine.
    let mut toc: Vec<Value> = Vec::new();
    let nav_name = names.iter().find(|n| n.to_ascii_lowercase().ends_with("nav.xhtml")).cloned();
    let ncx_name = names.iter().find(|n| n.ends_with(".ncx")).cloned();
    let toc_source = nav_name.or(ncx_name);
    if let Some(name) = toc_source {
        if let Some(td) = zip_read(&mut archive, &name) {
            let text = String::from_utf8_lossy(&td).into_owned();
            let labels = xml_collect(&text, &["text", "label", "title", "content"]);
            let base_dir = name.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default();
            // Extract src attributes in document order for NCX/nav.
            let mut idx = 0usize;
            for (k, label) in labels {
                if k != "text" && k != "label" && k != "title" { continue; }
                // Find a following src="..." after the label text.
                if let Some(pos) = text.find(&label) {
                    let rest = &text[pos..];
                    if let Some(sp) = rest.find("src=\"") {
                        let after = &rest[sp + 5..];
                        if let Some(end) = after.find('"') {
                            let href = &after[..end];
                            let full = normalize_zip_path(&base_dir, href.split('#').next().unwrap_or(href));
                            toc.push(json!({ "href": format!("xhtml:{full}"), "title": label, "level": 1, "spine": idx }));
                            idx += 1;
                            continue;
                        }
                    }
                }
                toc.push(json!({ "href": format!("xhtml:{}", spine.get(idx).cloned().unwrap_or_default()), "title": label, "level": 1, "spine": idx }));
                idx += 1;
            }
        }
    }
    if toc.is_empty() {
        for (i, s) in spine.iter().enumerate() {
            toc.push(json!({ "href": format!("xhtml:{s}"), "title": format!("第 {} 节", i + 1), "level": 0, "spine": i }));
        }
    }

    let items: Vec<Value> = spine.iter().enumerate().map(|(i, s)| {
        json!({
            "id": format!("xhtml:{s}"),
            "seq": i,
            "title": toc.get(i).and_then(|t| t.get("title")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            "kind": "chapter",
            "mediaType": "application/xhtml+xml",
            "href": format!("xhtml:{s}"),
        })
    }).collect();

    let mut resources = serde_json::Map::new();
    for (i, s) in spine.iter().enumerate() {
        resources.insert(format!("xhtml:{s}"), Value::String(s.clone()));
        let _ = i;
    }
    for (href, _, _) in manifest.values() {
        if !href.is_empty() {
            let full = normalize_zip_path(&base, href);
            resources.insert(format!("asset:{full}"), Value::String(full.clone()));
            resources.insert(full.clone(), Value::String(full));
        }
    }

    let cover = cover_href.and_then(|href| {
        let full = normalize_zip_path(&base, &href);
        zip_read(&mut archive, &full).map(|data| (data, image_content_type(&full).to_string()))
    }).or_else(|| {
        // Fall back to the first image in the archive.
        let mut imgs: Vec<String> = names.iter().filter(|n| is_image(n)).cloned().collect();
        natural_sort(&mut imgs);
        imgs.first().and_then(|n| zip_read(&mut archive, n).map(|d| (d, image_content_type(n).to_string())))
    });

    let total = items.len() as i64;
    let mut structure = empty_structure("reflowable", total);
    let obj = structure.as_object_mut().unwrap();
    obj.insert("groups".into(), json!([group("正文", total, 0)]));
    obj.insert("items".into(), Value::Array(items));
    obj.insert("toc".into(), Value::Array(toc));
    obj.insert("resources".into(), Value::Object(resources));
    obj.insert("zip".into(), Value::Bool(true));

    Ok(ParsedBook {
        format: "epub".into(),
        content_hash: sha256_hex(bytes),
        page_count: Some(total),
        metadata: m,
        cover,
        structure,
    })
}

// ---------------------------------------------------------------- TXT

fn parse_text(path: &Path, bytes: &[u8], stem: &str) -> ApiResult<ParsedBook> {
    let (text, encoding) = decode_text(bytes);
    let chapters = split_chapters(&text);
    let mut m = filename_metadata(stem);
    m.language = if encoding == "gb18030" { "zh".into() } else { m.language };
    let items: Vec<Value> = chapters.iter().enumerate().map(|(i, (title, _))| {
        json!({
            "id": format!("chapter:{i}"),
            "seq": i,
            "title": title,
            "kind": "chapter",
            "mediaType": "text/plain; charset=utf-8",
            "href": format!("chapter:{i}"),
            "format": "text",
        })
    }).collect();
    let toc: Vec<Value> = items.iter().map(|it| json!({
        "href": it.get("href"), "title": it.get("title"), "level": 0, "spine": it.get("seq")
    })).collect();
    let mut texts = serde_json::Map::new();
    for (i, (_, body)) in chapters.iter().enumerate() {
        texts.insert(format!("chapter:{i}"), Value::String(body.clone()));
        texts.insert(format!("chapter-full:{i}"), Value::String(format!("<p>{}</p>", escape_html(body).replace('\n', "</p><p>"))));
    }
    // Continuous text uses chunk:offset for the reader's virtual scroll.
    let chunk_size = 12_000usize;
    let chars: Vec<char> = text.chars().collect();
    for (offset, chunk) in chars.chunks(chunk_size).enumerate() {
        texts.insert(format!("chunk:{}", offset * chunk_size), Value::String(chunk.iter().collect::<String>()));
    }
    let total = items.len() as i64;
    let mut structure = empty_structure("text", total);
    let obj = structure.as_object_mut().unwrap();
    obj.insert("groups".into(), json!([group("正文", total, 0)]));
    obj.insert("items".into(), Value::Array(items));
    obj.insert("toc".into(), Value::Array(toc));
    obj.insert("chapterTexts".into(), Value::Object(texts));
    obj.insert("encoding".into(), Value::String(encoding));
    let _ = path;
    Ok(ParsedBook {
        format: "txt".into(),
        content_hash: sha256_hex(bytes),
        page_count: Some(total),
        metadata: m,
        cover: None,
        structure,
    })
}

pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------- comic zip (CBZ/ZIP)

fn parse_comic_archive(path: &Path, bytes: &[u8], stem: &str, ext: &str) -> ApiResult<ParsedBook> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut images: Vec<String> = zip_names(&mut archive).into_iter().filter(|n| is_image(n)).collect();
    natural_sort(&mut images);
    if images.is_empty() {
        return Err(api_err(400, "NOT_A_COMIC", "zip has no page images"));
    }
    // Group by directory (volume). A flat archive is one group.
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for name in &images {
        let dir = name.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        if let Some((_, list)) = groups.iter_mut().find(|(g, _)| *g == dir) {
            list.push(name.clone());
        } else {
            groups.push((dir, vec![name.clone()]));
        }
    }
    let mut items = Vec::new();
    let mut resources = serde_json::Map::new();
    let mut group_values = Vec::new();
    let mut seq = 0i64;
    for (gi, (dir, list)) in groups.iter().enumerate() {
        let offset = seq;
        for (pi, name) in list.iter().enumerate() {
            let r = format!("page:{gi}:{pi}");
            items.push(json!({
                "id": r, "seq": seq, "title": format!("第 {} 页", pi + 1),
                "kind": "page", "mediaType": image_content_type(name), "href": r,
            }));
            resources.insert(r, Value::String(name.clone()));
            seq += 1;
        }
        group_values.push(group(&if dir.is_empty() { stem.to_string() } else { dir.clone() }, list.len() as i64, offset));
    }
    let cover = images.first().and_then(|n| zip_read(&mut archive, n).map(|d| (d, image_content_type(n).to_string())));
    let total = items.len() as i64;
    let mut structure = empty_structure("paged", total);
    let obj = structure.as_object_mut().unwrap();
    obj.insert("groups".into(), Value::Array(group_values));
    obj.insert("items".into(), Value::Array(items));
    obj.insert("resources".into(), Value::Object(resources));
    obj.insert("zip".into(), Value::Bool(true));
    let m = filename_metadata(stem);
    Ok(ParsedBook {
        format: if ext == "cbz" { "cbz".into() } else { "zip".into() },
        content_hash: sha256_hex(bytes),
        page_count: Some(total),
        metadata: m.tap(|m| m.source = "filename".into()),
        cover,
        structure,
    })
}

trait Tap: Sized { fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self { f(&mut self); self } }
impl<T> Tap for T {}

// ---------------------------------------------------------------- PDF / image

fn parse_opaque(path: &Path, bytes: &[u8], stem: &str, ext: &str) -> ApiResult<ParsedBook> {
    let is_pdf = ext == "pdf";
    let kind = if is_pdf { "document" } else { "single-image" };
    let r = "document".to_string();
    let items = vec![json!({
        "id": r, "seq": 0, "title": stem, "kind": "page",
        "mediaType": if is_pdf { "application/pdf" } else { image_content_type(&format!(".{ext}")) },
        "href": r,
    })];
    let mut resources = serde_json::Map::new();
    resources.insert(r.clone(), Value::String(String::new()));
    let mut structure = empty_structure(kind, 1);
    let obj = structure.as_object_mut().unwrap();
    obj.insert("groups".into(), json!([group(stem, 1, 0)]));
    obj.insert("items".into(), Value::Array(items));
    obj.insert("toc".into(), json!([{"href": r, "title": stem, "level": 0, "spine": 0}]));
    obj.insert("resources".into(), Value::Object(resources));
    let _ = path;
    Ok(ParsedBook {
        format: if is_pdf { "pdf".into() } else { ext.to_string() },
        content_hash: sha256_hex(bytes),
        page_count: if is_pdf { None } else { Some(1) },
        metadata: filename_metadata(stem),
        cover: if is_pdf { None } else { Some((bytes.to_vec(), image_content_type(&format!(".{ext}")).to_string())) },
        structure,
    })
}

// ---------------------------------------------------------------- image directory

fn parse_image_directory(abs: &Path, rel: &str) -> ApiResult<ParsedBook> {
    let mut images: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(abs)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_image(&name) { images.push(name); }
        }
    }
    natural_sort(&mut images);
    if images.is_empty() { return Err(api_err(400, "EMPTY_DIRECTORY", "no page images")); }
    let mut items = Vec::new();
    let mut resources = serde_json::Map::new();
    for (i, name) in images.iter().enumerate() {
        let r = format!("page:0:{i}");
        items.push(json!({ "id": r, "seq": i, "title": format!("第 {} 页", i + 1), "kind": "page", "mediaType": image_content_type(name), "href": r }));
        resources.insert(r, Value::String(name.clone()));
    }
    let first = images.first().cloned().unwrap();
    let cover = std::fs::read(abs.join(&first)).ok().map(|d| (d, image_content_type(&first).to_string()));
    let total = items.len() as i64;
    let mut structure = empty_structure("paged", total);
    let obj = structure.as_object_mut().unwrap();
    obj.insert("groups".into(), json!([group(rel, total, 0)]));
    obj.insert("items".into(), Value::Array(items));
    obj.insert("resources".into(), Value::Object(resources));
    let stem = Path::new(rel).file_name().and_then(|s| s.to_str()).unwrap_or(rel);
    Ok(ParsedBook {
        format: "comic-dir".into(),
        content_hash: sha256_hex(images.join("\0").as_bytes()),
        page_count: Some(total),
        metadata: filename_metadata(stem),
        cover,
        structure,
    })
}

// ---------------------------------------------------------------- public parse

pub fn parse_file(abs: &Path, rel: &str) -> ApiResult<ParsedBook> {
    let bytes = std::fs::read(abs)?;
    let ext = Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let stem = Path::new(rel).file_stem().and_then(|s| s.to_str()).unwrap_or(rel).to_string();
    match ext.as_str() {
        "epub" => parse_epub(abs, &bytes, &stem),
        "txt" | "text" | "md" => parse_text(abs, &bytes, &stem),
        "cbz" | "zip" => parse_comic_archive(abs, &bytes, &stem, &ext),
        "pdf" => parse_opaque(abs, &bytes, &stem, &ext),
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "avif" => parse_opaque(abs, &bytes, &stem, &ext),
        _ => Err(api_err(400, "UNSUPPORTED_FORMAT", format!("unsupported format: {ext}"))),
    }
}

pub fn parse_directory(abs: &Path, rel: &str) -> ApiResult<ParsedBook> {
    parse_image_directory(abs, rel)
}

pub fn supported_extension(rel: &str) -> bool {
    let ext = Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "epub" | "txt" | "text" | "md" | "cbz" | "zip" | "pdf" | "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "avif")
}

// ---------------------------------------------------------------- structure cache

fn cache_path(config: &Config, book_id: &str) -> PathBuf {
    config.data_dir.join("books").join(format!("{book_id}.json"))
}

pub fn save_structure(config: &Config, book_id: &str, structure: &Value) -> ApiResult<()> {
    let path = cache_path(config, book_id);
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(&path, serde_json::to_vec(structure)?)?;
    Ok(())
}

pub fn load_structure(config: &Config, book_id: &str) -> ApiResult<Value> {
    let path = cache_path(config, book_id);
    let bytes = std::fs::read(&path).map_err(|_| api_err(404, "CONTENT_MISSING", "book content cache is missing; run a scan"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn save_cover(config: &Config, book_id: &str, cover: &(Vec<u8>, String)) -> ApiResult<String> {
    let ext = match cover.1.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        _ => "jpg",
    };
    let dir = config.data_dir.join("covers");
    std::fs::create_dir_all(&dir)?;
    let name = format!("{book_id}.{ext}");
    std::fs::write(dir.join(&name), &cover.0)?;
    Ok(name)
}

pub fn cover_path(config: &Config, name: &str) -> PathBuf {
    config.data_dir.join("covers").join(name)
}

// ---------------------------------------------------------------- scanner

pub struct ScanSummary {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub failed: usize,
}

/// A directory whose immediate children are only page images is one comic book
/// (format `comic-dir`), not a folder of loose files. A nested folder or a real
/// book file inside makes it an ordinary directory that we descend into.
fn looks_like_comic_directory(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    let mut images = 0usize;
    for entry in entries {
        let Ok(entry) = entry else { return false };
        let Ok(ft) = entry.file_type() else { return false };
        if ft.is_symlink() || ft.is_dir() { return false; }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_image(&name) { images += 1; }
        else if supported_extension(&name) { return false; }
    }
    images > 0
}

fn walk(
    dir: &Path,
    base: &Path,
    files: &mut Vec<(String, PathBuf)>,
    comic_dirs: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let abs = entry.path();
        // Never follow symlinks (matches the Node scanner's containment rule).
        if ft.is_symlink() { continue; }
        let rel = abs.strip_prefix(base).unwrap_or(&abs).to_string_lossy().replace('\\', "/");
        if ft.is_dir() {
            if looks_like_comic_directory(&abs) {
                comic_dirs.push((rel, abs));
            } else {
                walk(&abs, base, files, comic_dirs)?;
            }
        } else if ft.is_file() && supported_extension(&rel) {
            files.push((rel, abs));
        }
    }
    Ok(())
}

fn modified_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Overwriting a page in place does not change its directory's mtime, so the
/// fast-path stamp of a comic directory is the sum of its page sizes plus the
/// newest page mtime. Regular file books use the file's own stat.
fn stamp(abs: &Path, is_dir: bool) -> Option<(i64, i64)> {
    if !is_dir {
        let meta = std::fs::metadata(abs).ok()?;
        return Some((meta.len() as i64, modified_ms(&meta)));
    }
    let mut size = 0i64;
    let mut newest = 0i64;
    for entry in std::fs::read_dir(abs).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_image(&name) { continue; }
        let meta = entry.metadata().ok()?;
        if !meta.is_file() { continue; }
        size += meta.len() as i64;
        newest = newest.max(modified_ms(&meta));
    }
    Some((size, newest))
}

fn unchanged(by_path: &std::collections::HashMap<String, crate::db::Row>, rel: &str, size: i64, mtime: i64) -> bool {
    by_path.get(rel).is_some_and(|prev| {
        crate::db::i(prev, "size") == size
            && crate::db::i(prev, "mtime_ms") == mtime
            && crate::db::i(prev, "missing") == 0
    })
}


/// Index (or re-index) one file. Returns true when the fast path skipped it.
pub fn index_file(
    state: &AppState,
    rel: &str,
    abs: &Path,
    by_path: &mut std::collections::HashMap<String, crate::db::Row>,
    sum: &mut ScanSummary,
) -> ApiResult<bool> {
    let Some((size, mtime)) = stamp(abs, false) else { sum.failed += 1; return Ok(true); };
    if unchanged(by_path, rel, size, mtime) { return Ok(true); }
    let parsed = match parse_file(abs, rel) {
        Ok(p) => p,
        Err(_) => { sum.failed += 1; return Ok(true); }
    };
    persist_book(state, rel, parsed, size, mtime, by_path, sum)?;
    Ok(false)
}

/// Index one comic directory (a folder of page images) as a single book.
fn index_directory(
    state: &AppState,
    rel: &str,
    abs: &Path,
    by_path: &mut std::collections::HashMap<String, crate::db::Row>,
    sum: &mut ScanSummary,
) -> ApiResult<bool> {
    let Some((size, mtime)) = stamp(abs, true) else { sum.failed += 1; return Ok(true); };
    if unchanged(by_path, rel, size, mtime) { return Ok(true); }
    let parsed = match parse_directory(abs, rel) {
        Ok(p) => p,
        Err(_) => { sum.failed += 1; return Ok(true); }
    };
    persist_book(state, rel, parsed, size, mtime, by_path, sum)?;
    Ok(false)
}

/// Write one parsed book (file or comic directory) into the DB and structure cache.
fn persist_book(
    state: &AppState,
    rel: &str,
    parsed: ParsedBook,
    size: i64,
    mtime: i64,
    by_path: &mut std::collections::HashMap<String, crate::db::Row>,
    sum: &mut ScanSummary,
) -> ApiResult<()> {
    let config = &state.config;
    let previous = by_path.get(rel);
    let previous_id = previous.map(|r| crate::db::s(r, "book_id"));
    let previous_identifier = previous.map(|r| crate::db::s(r, "book_identifier"));
    let book_id = resolve_book_id(
        Some(parsed.metadata.identifier.as_str()).filter(|s| !s.is_empty()),
        &parsed.content_hash,
        previous_id.as_deref(),
        previous_identifier.as_deref(),
    );
    let cover_name = parsed.cover.as_ref()
        .and_then(|c| save_cover(config, &book_id, c).ok());
    let file_id = compute_file_id(rel);
    let tags = serde_json::to_string(&parsed.metadata.tags)?;
    let meta_json = parsed.metadata.to_json().to_string();
    let m = &parsed.metadata;
    let page_count = parsed.page_count;
    let now = crate::state::now_ms();
    let exists = state.db.one("SELECT id FROM books WHERE id = ?1", [&book_id])?.is_some();
    let result: ApiResult<()> = state.db.transaction(|conn| {
        if exists {
            conn.execute(
                "UPDATE books SET title=?1, author=?2, publisher=?3, language=?4, isbn=?5, description=?6,
                   series=?7, series_index=?8, tags=?9, pubdate=?10, page_count=?11, file_size=?12, meta_json=?13,
                   cover_path=COALESCE(?14, cover_path), source=?15, content_hash=?16, identifier=?17,
                   content_hash_kind='file', updated_at=?18 WHERE id=?19",
                rusqlite::params![m.title, m.author, m.publisher, m.language, m.isbn, m.description,
                    m.series, m.series_index, tags, m.pubdate, page_count, size, meta_json,
                    cover_name, m.source, parsed.content_hash, m.identifier, now, book_id],
            )?;
            sum.updated += 1;
        } else {
            conn.execute(
                "INSERT INTO books (id, identifier, content_hash, content_hash_kind, format, title, author, publisher,
                   language, isbn, description, series, series_index, tags, pubdate, cover_path, file_size,
                   page_count, meta_json, source, created_at, updated_at)
                 VALUES (?1,?2,?3,'file',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                rusqlite::params![book_id, m.identifier, parsed.content_hash, parsed.format, m.title, m.author,
                    m.publisher, m.language, m.isbn, m.description, m.series, m.series_index, tags, m.pubdate,
                    cover_name, size, page_count, meta_json, m.source, now, now],
            )?;
            sum.added += 1;
            // Every account sees a newly indexed book on their shelf.
            conn.execute(
                "INSERT OR IGNORE INTO user_books (user_id, book_id, added_at) SELECT id, ?1, ?2 FROM users",
                rusqlite::params![book_id, now],
            )?;
        }
        conn.execute(
            "INSERT INTO book_files (id, book_id, rel_path, size, mtime_ms, inode, missing, first_seen, last_seen)
             VALUES (?1,?2,?3,?4,?5,'',0,?6,?6)
             ON CONFLICT(rel_path) DO UPDATE SET book_id=excluded.book_id, size=excluded.size,
               mtime_ms=excluded.mtime_ms, missing=0, last_seen=excluded.last_seen",
            rusqlite::params![file_id, book_id, rel, size, mtime, now],
        )?;
        Ok(())
    });
    if result.is_err() { sum.failed += 1; }
    let _ = save_structure(config, &book_id, &parsed.structure);
    Ok(())
}

/// Re-parse every file of one book (the POST /books/:id/refresh path).
pub fn rescan_book(state: &AppState, book_id: &str) -> ApiResult<()> {
    let rows = state.db.query(
        "SELECT rel_path FROM book_files WHERE book_id = ?1 AND missing = 0",
        [book_id],
    )?;
    let stored = state.db.query(
        "SELECT f.id, f.book_id, f.rel_path, f.size, f.mtime_ms, f.missing, b.content_hash AS book_content_hash, b.identifier AS book_identifier FROM book_files f JOIN books b ON b.id = f.book_id",
        [],
    )?;
    let mut by_path: std::collections::HashMap<String, crate::db::Row> = std::collections::HashMap::new();
    for r in &stored {
        by_path.insert(crate::db::s(r, "rel_path"), r.clone());
    }
    let mut sum = ScanSummary { scanned: 0, added: 0, updated: 0, removed: 0, failed: 0 };
    for row in rows {
        let rel = crate::db::s(&row, "rel_path");
        let abs = state.config.books_dir.join(&rel);
        if abs.is_dir() {
            let _ = index_directory(state, &rel, &abs, &mut by_path, &mut sum)?;
        } else {
            let _ = index_file(state, &rel, &abs, &mut by_path, &mut sum)?;
        }
    }
    Ok(())
}

pub fn scan(state: &AppState) -> ApiResult<ScanSummary> {
    let config = &state.config;
    let mut sum = ScanSummary { scanned: 0, added: 0, updated: 0, removed: 0, failed: 0 };
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut comic_dirs: Vec<(String, PathBuf)> = Vec::new();
    walk(&config.books_dir, &config.books_dir, &mut files, &mut comic_dirs)?;

    let stored = state.db.query(
        "SELECT f.id, f.book_id, f.rel_path, f.size, f.mtime_ms, f.missing, b.content_hash AS book_content_hash, b.identifier AS book_identifier FROM book_files f JOIN books b ON b.id = f.book_id",
        [],
    )?;
    let mut by_path: std::collections::HashMap<String, crate::db::Row> = std::collections::HashMap::new();
    for r in &stored {
        by_path.insert(crate::db::s(r, "rel_path"), r.clone());
    }

    let mut seen: Vec<String> = Vec::new();
    for (rel, abs) in files {
        sum.scanned += 1;
        seen.push(rel.clone());
        if index_file(state, &rel, &abs, &mut by_path, &mut sum)? {
            continue;
        }
    }
    for (rel, abs) in comic_dirs {
        sum.scanned += 1;
        seen.push(rel.clone());
        if index_directory(state, &rel, &abs, &mut by_path, &mut sum)? {
            continue;
        }
    }

    // Mark files that disappeared; never delete the book or its progress.
    for (rel, r) in &by_path {
        if !seen.contains(rel) && crate::db::i(r, "missing") == 0 {
            state.db.exec("UPDATE book_files SET missing = 1 WHERE id = ?1", [crate::db::s(r, "id")])?;
            sum.removed += 1;
        }
    }
    Ok(sum)
}

pub fn scan_async(state: AppState) {
    tokio::task::spawn_blocking(move || {
        {
            // The scan lock is process-local and short; use a blocking lock.
            let mut guard = state.scan.blocking_lock();
            guard.running = true;
            guard.started_at = crate::state::now_ms();
            guard.last_error = None;
        }
        let result = scan(&state);
        let mut guard = state.scan.blocking_lock();
        guard.running = false;
        guard.finished_at = crate::state::now_ms();
        match result {
            Ok(s) => {
                guard.scanned = s.scanned;
                guard.added += s.added;
                guard.updated += s.updated;
                guard.removed = s.removed;
            }
            Err(e) => guard.last_error = Some(e.to_string()),
        }
    });
}

pub fn scan_status(state: &AppState) -> Value {
    let guard = match state.scan.try_lock() {
        Ok(g) => g,
        Err(_) => return json!({ "running": true }),
    };
    json!({
        "running": guard.running,
        "startedAt": guard.started_at,
        "finishedAt": guard.finished_at,
        "scanned": guard.scanned,
        "added": guard.added,
        "updated": guard.updated,
        "removed": guard.removed,
        "lastError": guard.last_error,
    })
}

// ---------------------------------------------------------------- asset resolution

/// Resolve an opaque `ref` from a book's structure into bytes + content type.
pub fn resolve_asset(state: &AppState, book_id: &str, ref_value: &str) -> ApiResult<(Vec<u8>, String)> {
    let book = state.db.one(
        "SELECT id, format, meta_json FROM books WHERE id = ?1",
        [book_id],
    )?.ok_or_else(|| api_err(404, "NOT_FOUND", "book not found"))?;
    let files = state.db.query(
        "SELECT rel_path FROM book_files WHERE book_id = ?1 AND missing = 0 ORDER BY rel_path LIMIT 1",
        [book_id],
    )?;
    let rel = files.first().map(|r| crate::db::s(r, "rel_path")).unwrap_or_default();
    let abs = state.config.books_dir.join(&rel);
    let structure = load_structure(&state.config, book_id)?;
    let format = crate::db::s(&book, "format");
    let is_zip = structure.get("zip").and_then(Value::as_bool).unwrap_or(false);

    // TXT chapter / chunk renditions live in the structure cache.
    if let Some(texts) = structure.get("chapterTexts").and_then(Value::as_object) {
        if let Some(v) = texts.get(ref_value).and_then(Value::as_str) {
            let html = ref_value.starts_with("chapter-full:");
            return Ok((v.as_bytes().to_vec(), if html { "text/html; charset=utf-8" } else { "text/plain; charset=utf-8" }.into()));
        }
    }

    // Opaque document / single image: the whole file.
    if ref_value == "document" || format == "pdf" || ref_value.starts_with("raw:") {
        let bytes = std::fs::read(&abs)?;
        let ct = if format == "pdf" { "application/pdf" } else { image_content_type(&rel) };
        return Ok((bytes, ct.into()));
    }

    let resources = structure.get("resources").and_then(Value::as_object);
    let target = resources
        .and_then(|m| m.get(ref_value).or_else(|| m.get(&format!("asset:{ref_value}"))))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| resources.and_then(|m| m.get(&format!("xhtml:{ref_value}"))).and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| ref_value.to_string());

    if is_zip {
        let file = File::open(&abs)?;
        let mut archive = ZipArchive::new(file)?;
        let data = zip_read(&mut archive, &target)
            .ok_or_else(|| api_err(404, "RESOURCE_GONE", format!("resource not found: {target}")))?;
        let ct = content_type_for(&target, &format, ref_value);
        if ref_value.starts_with("xhtml:") || target.ends_with(".xhtml") || target.ends_with(".html") {
            let html = rewrite_relative(&String::from_utf8_lossy(&data), book_id, &target);
            return Ok((html.into_bytes(), "application/xhtml+xml; charset=utf-8".into()));
        }
        return Ok((data, ct));
    }

    // Directory / loose image: target is relative to the book's directory.
    let base = if format == "comic-dir" { abs.clone() } else { abs.parent().unwrap_or(Path::new(".")).to_path_buf() };
    let candidate = base.join(&target);
    if candidate.is_file() {
        let data = std::fs::read(&candidate)?;
        return Ok((data, content_type_for(&target, &format, ref_value)));
    }
    let bytes = std::fs::read(&abs)?;
    Ok((bytes, content_type_for(&rel, &format, ref_value)))
}

fn content_type_for(name: &str, format: &str, ref_value: &str) -> String {
    if ref_value.starts_with("xhtml:") || name.ends_with(".xhtml") || name.ends_with(".html") {
        return "application/xhtml+xml; charset=utf-8".into();
    }
    if name.ends_with(".css") { return "text/css; charset=utf-8".into(); }
    if name.ends_with(".js") { return "text/javascript; charset=utf-8".into(); }
    if is_image(name) { return image_content_type(name).into(); }
    if name.ends_with(".svg") { return "image/svg+xml".into(); }
    if name.ends_with(".woff2") { return "font/woff2".into(); }
    if name.ends_with(".woff") { return "font/woff".into(); }
    if name.ends_with(".ttf") || name.ends_with(".otf") { return "font/ttf".into(); }
    if format == "pdf" { return "application/pdf".into(); }
    "application/octet-stream".into()
}

/// Rewrite relative src/href in an EPUB chapter to the authenticated asset
/// endpoint, then strip scripts and event handlers. The reader mounts the
/// chapter in a Shadow DOM, so a relative URL would resolve against the page.
fn rewrite_relative(html: &str, book_id: &str, chapter_path: &str) -> String {
    // Lightweight sanitiser: drop script/style blocks and event-handler
    // attributes. EPUB content is trusted user data, but a chapter mounted in
    // a Shadow DOM must never be able to run script or reach the page's scope.
    let mut cleaned = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut i = 0usize;
    while i < html.len() {
        if lower[i..].starts_with("<script") || lower[i..].starts_with("<style") {
            let tag = if lower[i..].starts_with("<script") { "script" } else { "style" };
            if let Some(end) = lower[i..].find(&format!("</{tag}")) {
                i += end + tag.len() + 3;
                // skip to '>' of the closing tag
                while i < html.len() && html.as_bytes()[i] != b'>' { i += 1; }
                i += 1;
                continue;
            }
        }
        // Strip on*="..." / on*='...' attributes inside any tag.
        let ch = html[i..].chars().next().unwrap();
        cleaned.push(ch);
        i += ch.len_utf8();
    }
    // Remove inline event handler attributes with a simple scan over tags.
    cleaned = strip_event_attrs(&cleaned);
    let base = chapter_path.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default();
    let mut out = String::with_capacity(cleaned.len());
    let bytes = cleaned.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if cleaned[i..].starts_with("src=\"") || cleaned[i..].starts_with("href=\"") {
            let is_src = cleaned[i..].starts_with("src=\"");
            let keyword_len = if is_src { 5 } else { 6 };
            out.push_str(&cleaned[i..i + keyword_len]);
            i += keyword_len;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' { i += 1; }
            let value = &cleaned[start..i];
            out.push_str(&rewrite_url(value, book_id, &base));
            if i < bytes.len() { out.push('"'); i += 1; }
            continue;
        }
        let ch = cleaned[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn strip_event_attrs(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    let mut in_tag = false;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            in_tag = true;
            out.push('<');
            i += 1;
            continue;
        }
        if bytes[i] == b'>' {
            in_tag = false;
            out.push('>');
            i += 1;
            continue;
        }
        if in_tag && (bytes[i] == b' ' || bytes[i] == b'\n' || bytes[i] == b'\t' || bytes[i] == b'\r') {
            // Look ahead for on[a-z]+=
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\n' || bytes[j] == b'\t' || bytes[j] == b'\r') { j += 1; }
            let rest = &html[j..];
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("on") && lower.as_bytes().get(2).map(|c| c.is_ascii_lowercase()).unwrap_or(false) {
                // consume attribute name
                let mut k = j + 2;
                while k < bytes.len() && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'-' || bytes[k] == b'_') { k += 1; }
                // optional = "value"
                while k < bytes.len() && bytes[k] != b'=' && bytes[k] != b'>' && bytes[k] != b' ' && bytes[k] != b'/' { k += 1; }
                if k < bytes.len() && bytes[k] == b'=' {
                    k += 1;
                    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') { k += 1; }
                    let quote = if k < bytes.len() && (bytes[k] == b'"' || bytes[k] == b'\'') { Some(bytes[k]) } else { None };
                    if let Some(q) = quote {
                        k += 1;
                        while k < bytes.len() && bytes[k] != q { k += 1; }
                        if k < bytes.len() { k += 1; }
                    } else {
                        while k < bytes.len() && bytes[k] != b' ' && bytes[k] != b'>' && bytes[k] != b'/' { k += 1; }
                    }
                }
                // keep the whitespace that preceded the attribute, drop the rest
                out.push(' ');
                i = k;
                continue;
            }
        }
        let ch = html[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn rewrite_url(value: &str, book_id: &str, base: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") || value.starts_with("data:")
        || value.starts_with('#') || value.starts_with("mailto:") || value.starts_with("/api/")
    {
        return value.to_string();
    }
    let (path, fragment) = value.split_once('#').unwrap_or((value, ""));
    if path.is_empty() { return value.to_string(); }
    let full = normalize_zip_path(base, path);
    let encoded = percent_encoding::utf8_percent_encode(&full, percent_encoding::NON_ALPHANUMERIC);
    let mut url = format!("/api/v1/books/{}/assets?ref=asset:{encoded}", percent_encoding::utf8_percent_encode(book_id, percent_encoding::NON_ALPHANUMERIC));
    if !fragment.is_empty() { url.push('#'); url.push_str(fragment); }
    url
}

pub fn manifest(state: &AppState, book_id: &str, group: Option<i64>) -> ApiResult<Value> {
    let structure = load_structure(&state.config, book_id)?;
    let items = structure.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let total = items.len() as i64;
    let filtered = match group {
        Some(g) => {
            let groups = structure.get("groups").and_then(Value::as_array);
            if let Some(gs) = groups {
                if let Some(gv) = gs.get(g as usize) {
                    let offset = gv.get("offset").and_then(Value::as_i64).unwrap_or(0);
                    let count = gv.get("count").and_then(Value::as_i64).unwrap_or(0);
                    items.into_iter().skip(offset as usize).take(count as usize).collect::<Vec<_>>()
                } else { Vec::new() }
            } else { Vec::new() }
        }
        None => items,
    };
    let content = json!({
        "revision": structure.get("revision"),
        "kind": structure.get("kind").cloned().unwrap_or(json!("text")),
        "total": total,
        "groups": structure.get("groups").cloned().unwrap_or(json!([])),
        "items": filtered,
        "group": group,
    });
    Ok(content)
}

pub fn toc(state: &AppState, book_id: &str) -> ApiResult<Value> {
    let structure = load_structure(&state.config, book_id)?;
    Ok(structure.get("toc").cloned().unwrap_or(json!([])))
}

pub fn search_in_book(state: &AppState, book_id: &str, query: &str, cursor: Option<i64>) -> ApiResult<Value> {
    let structure = load_structure(&state.config, book_id)?;
    let q_lower = query.to_lowercase();
    let start = cursor.unwrap_or(0) as usize;
    let limit = 20usize;
    let mut results = Vec::new();
    let mut next: Option<i64> = None;
    if let Some(texts) = structure.get("chapterTexts").and_then(Value::as_object) {
        let keys: Vec<String> = texts.keys().filter(|k| k.starts_with("chapter:")).cloned().collect();
        let mut idx = 0usize;
        for key in keys {
            let body = texts.get(&key).and_then(Value::as_str).unwrap_or("");
            if let Some(pos) = body.to_lowercase().find(&q_lower) {
                if idx >= start && results.len() < limit {
                    let from = pos.saturating_sub(40);
                    let to = (pos + q_lower.len() + 60).min(body.len());
                    results.push(json!({ "href": key, "excerpt": &body[from..to], "title": key }));
                } else if idx >= start + limit {
                    next = Some(idx as i64);
                    break;
                }
                idx += 1;
            }
        }
    } else if let Some(items) = structure.get("items").and_then(Value::as_array) {
        for (i, it) in items.iter().enumerate().skip(start) {
            if results.len() >= limit { next = Some(i as i64); break; }
            let href = it.get("href").and_then(Value::as_str).unwrap_or("");
            if let Ok((bytes, _)) = resolve_asset(state, book_id, href) {
                let text = String::from_utf8_lossy(&bytes).to_lowercase();
                if let Some(pos) = text.find(&q_lower) {
                    let from = pos.saturating_sub(40);
                    let to = (pos + q_lower.len() + 60).min(text.len());
                    results.push(json!({ "href": href, "excerpt": &text[from..to], "title": it.get("title") }));
                }
            }
        }
    }
    Ok(json!({ "items": results, "nextCursor": next }))
}
