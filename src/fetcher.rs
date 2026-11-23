//! AnalyzeUrl：URL 变量替换（{{key}}/{{page}}）、请求头、编码处理、网络请求。

use crate::error::{AppError, AppResult};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT, REFERER};
use serde_json::Value;
use std::sync::OnceLock;

static UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

pub struct Fetcher {
    client: reqwest::Client,
}

impl Fetcher {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .gzip(true)
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    /// 替换 URL 中的 {{key}} / {{page}} / {{...}}
    pub fn prepare_url(url: &str, key: Option<&str>, page: Option<i32>) -> String {
        let mut result = url.to_string();
        if let Some(k) = key {
            result = result.replace("{{key}}", &urlencoding_encode(k));
        }
        if let Some(p) = page {
            // {{page}} 或 {{page:1,2,3}}
            let re = Regex::new(r"\{\{page(?::([^}]+))?\}\}").unwrap();
            result = re.replace_all(&result, |caps: &regex::Captures| -> String {
                if let Some(list) = caps.get(1) {
                    let pages: Vec<&str> = list.as_str().split(',').collect();
                    let idx = (p - 1).max(0) as usize;
                    pages.get(idx.min(pages.len().saturating_sub(1))).copied().unwrap_or("1").to_string()
                } else {
                    p.to_string()
                }
            }).to_string();
        }
        // 其他 {{var}} 置空
        let re2 = Regex::new(r"\{\{[a-zA-Z_][^}]*\}\}").unwrap();
        result = re2.replace_all(&result, "").to_string();
        result
    }

    /// 解析书源 header JSON 为 HeaderMap
    pub fn parse_headers(header_json: Option<&str>, url: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(UA));
        if let Some(h) = header_json {
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(h) {
                for (k, v) in map {
                    if let Some(s) = v.as_str() {
                        if let (Ok(name), Ok(val)) = (
                            HeaderName::from_bytes(k.as_bytes()),
                            HeaderValue::from_str(s),
                        ) {
                            headers.insert(name, val);
                        }
                    }
                }
            }
        }
        // 从 URL 提取 host 作为 Referer
        if let Some(host) = extract_host(url) {
            if !headers.contains_key(REFERER) {
                if let Ok(v) = HeaderValue::from_str(&format!("http://{}/", host)) {
                    headers.insert(REFERER, v);
                }
            }
        }
        headers
    }

    /// GET 请求，返回解码后的 body（自动检测 GBK/UTF-8）
    pub async fn get(&self, url: &str, headers: Option<&str>) -> AppResult<(String, String)> {
        let hm = Self::parse_headers(headers, url);
        let resp = self.client.get(url).headers(hm).send().await
            .map_err(|e| AppError::internal(format!("请求 {} 失败: {}", url, e)))?;
        let final_url = resp.url().to_string();
        let bytes = resp.bytes().await
            .map_err(|e| AppError::internal(format!("读取响应失败: {}", e)))?;
        let body = decode_bytes(&bytes);
        Ok((body, final_url))
    }

    /// 同步 GET（供 JS java.ajax 回调使用）
    pub fn get_blocking(url: &str, headers: Option<&str>) -> String {
        let hm = Self::parse_headers(headers, url);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .danger_accept_invalid_certs(true)
            .build().ok();
        let Some(client) = client else { return String::new(); };
        client.get(url).headers(hm).send()
            .ok().and_then(|r| r.bytes().ok())
            .map(|b| decode_bytes(&b)).unwrap_or_default()
    }
}

/// 编码检测：优先 UTF-8，失败则 GBK
pub fn decode_bytes(bytes: &[u8]) -> String {
    // 先查 meta charset
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
    let lower = head.to_ascii_lowercase();
    let is_gbk = lower.contains("charset=\"gb2312\"") || lower.contains("charset=gb2312")
        || lower.contains("charset=\"gbk\"") || lower.contains("charset=gbk")
        || lower.contains("charset=\"gb18030\"");
    if is_gbk {
        let (cow, _, _) = encoding_rs::GBK.decode(bytes);
        return cow.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (cow, _, _) = encoding_rs::GBK.decode(bytes);
            cow.into_owned()
        }
    }
}

fn extract_host(url: &str) -> Option<String> {
    let s = url.split("//").nth(1).unwrap_or(url);
    let host = s.split('/').next()?;
    if host.is_empty() { None } else { Some(host.to_string()) }
}

fn urlencoding_encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}
