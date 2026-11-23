//! Legado 数据实体：与书源 JSON 字段对应。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Legado 书源里数值字段可能是数字也可能是字符串，宽松反序列化
fn flexible_i64<'de, D>(d: D) -> Result<i64, D::Error>
where D: Deserializer<'de> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    })
}
fn flexible_i32<'de, D>(d: D) -> Result<i32, D::Error>
where D: Deserializer<'de> {
    Ok(flexible_i64(d)? as i32)
}
fn flexible_bool<'de, D>(d: D) -> Result<bool, D::Error>
where D: Deserializer<'de> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::String(s) => s == "true" || s == "1",
        _ => true,
    })
}

/// 书源（对应 Legado BookSource 实体）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookSource {
    #[serde(deserialize_with = "flexible_string")]
    pub book_source_url: String,
    #[serde(deserialize_with = "flexible_string")]
    pub book_source_name: String,
    #[serde(deserialize_with = "flexible_string")]
    pub book_source_group: String,
    #[serde(default, deserialize_with = "flexible_i32")]
    pub book_source_type: i32,
    pub book_url_pattern: Option<String>,
    #[serde(default, deserialize_with = "flexible_i32")]
    pub custom_order: i32,
    #[serde(default = "default_true", deserialize_with = "flexible_bool")]
    pub enabled: bool,
    #[serde(default = "default_true", deserialize_with = "flexible_bool")]
    pub enabled_explore: bool,
    pub js_lib: Option<String>,
    pub header: Option<String>,
    pub book_source_comment: Option<String>,
    #[serde(default, deserialize_with = "flexible_i64")]
    pub last_update_time: i64,
    #[serde(default = "default_respond", deserialize_with = "flexible_i64")]
    pub respond_time: i64,
    #[serde(default, deserialize_with = "flexible_i32")]
    pub weight: i32,
    pub explore_url: Option<String>,
    pub search_url: Option<String>,
    pub login_check_js: Option<String>,
    pub rule_search: Option<SearchRule>,
    pub rule_book_info: Option<BookInfoRule>,
    pub rule_toc: Option<TocRule>,
    pub rule_content: Option<ContentRule>,
    pub rule_explore: Option<ExploreRule>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

fn flexible_string<'de, D>(d: D) -> Result<String, D::Error>
where D: Deserializer<'de> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

fn default_group() -> String { String::new() }
fn default_true() -> bool { true }
fn default_respond() -> i64 { 180000 }

/// 书籍列表规则（搜索/发现共用）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookListRule {
    pub book_list: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub book_url: Option<String>,
    pub cover_url: Option<String>,
    pub word_count: Option<String>,
}

pub type SearchRule = BookListRule;
pub type ExploreRule = BookListRule;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookInfoRule {
    pub init: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub intro: Option<String>,
    pub kind: Option<String>,
    pub last_chapter: Option<String>,
    pub update_time: Option<String>,
    pub cover_url: Option<String>,
    pub toc_url: Option<String>,
    pub word_count: Option<String>,
    pub can_re_name: Option<String>,
    pub download_urls: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TocRule {
    pub pre_update_js: Option<String>,
    pub chapter_list: Option<String>,
    pub chapter_name: Option<String>,
    pub chapter_url: Option<String>,
    pub format_js: Option<String>,
    pub is_volume: Option<String>,
    pub is_vip: Option<String>,
    pub is_pay: Option<String>,
    pub update_time: Option<String>,
    pub next_toc_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ContentRule {
    pub content: Option<String>,
    pub sub_content: Option<String>,
    pub title: Option<String>,
    pub next_content_url: Option<String>,
    pub web_js: Option<String>,
    pub source_regex: Option<String>,
    pub replace_regex: Option<String>,
    pub image_style: Option<String>,
    pub image_decode: Option<String>,
    pub pay_action: Option<String>,
    pub call_back_js: Option<String>,
    pub content_batch: Option<String>,
    pub max_batch_size: Option<i32>,
}

/// 搜索结果 / 书架书籍
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Book {
    pub book_url: String,
    pub origin: String,
    pub origin_name: String,
    pub name: String,
    pub author: String,
    pub kind: String,
    pub word_count: String,
    pub last_chapter: String,
    pub intro: String,
    pub cover_url: String,
    pub toc_url: String,
    /// 多个书源命中同一本书时的来源 URL 列表
    pub origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct BookChapter {
    pub index: usize,
    pub title: String,
    pub url: String,
    pub is_volume: bool,
    pub is_vip: bool,
}

/// 书架条目
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ShelfItem {
    pub id: String,
    pub book: Book,
    pub dur_chapter_index: usize,
    pub dur_chapter_title: String,
    pub added_at: i64,
    pub updated_at: i64,
}

/// 离线缓存章节
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CachedChapter {
    pub book_url: String,
    pub chapter_url: String,
    pub title: String,
    pub content: String,
    pub cached_at: i64,
}

/// 发现分类入口
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ExploreEntry {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub style: Value,
}
