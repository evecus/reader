//! 轻量 Cookie jar：域级隔离、Set-Cookie 回写、请求头注入。
//! 对应 Legado 的 CookieStore / java.getCookie(tag)。
//! 不依赖 reqwest 自带 jar，因为异步 client 与 JS 回调的 blocking client 需要共享同一份状态。

use reqwest::header::{HeaderMap, COOKIE, SET_COOKIE};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 线程安全的共享 Cookie 存储。Clone 后指向同一份数据。
#[derive(Clone, Default)]
pub struct CookieStore {
    jars: Arc<Mutex<HashMap<String, HashMap<String, String>>>>,
}

impl CookieStore {
    pub fn new() -> Self { Self::default() }

    /// 从 URL 提取 host（不含端口）
    fn host(url: &str) -> Option<String> {
        let s = url.split("//").nth(1).unwrap_or(url);
        let host_port = s.split('/').next()?;
        let host = host_port.split('@').last()?; // 去掉 userinfo
        let host = host.split(':').next()?;      // 去掉端口
        if host.is_empty() { None } else { Some(host.to_ascii_lowercase()) }
    }

    /// host 及其可共享的父域（www.example.com → ["www.example.com", "example.com"]）
    fn applicable_domains(host: &str) -> Vec<String> {
        let mut out = vec![host.to_string()];
        let stripped = host.strip_prefix("www.").unwrap_or(host);
        if stripped != host { out.push(stripped.to_string()); }
        // 同二级域：a.example.com 与 example.com 共享
        let parts: Vec<&str> = stripped.split('.').collect();
        if parts.len() >= 2 {
            let parent = parts[parts.len() - 2..].join(".");
            if !out.contains(&parent) { out.push(parent); }
        }
        out
    }

    /// 生成请求的 Cookie 头值：`k=v; k2=v2`
    pub fn cookie_header(&self, url: &str) -> String {
        let Some(host) = Self::host(url) else { return String::new(); };
        let domains = Self::applicable_domains(&host);
        let jars = match self.jars.lock() { Ok(j) => j, Err(_) => return String::new() };
        let mut pairs: Vec<String> = Vec::new();
        for domain in domains {
            if let Some(jar) = jars.get(&domain) {
                for (k, v) in jar {
                    if !pairs.iter().any(|p| p.starts_with(&format!("{}=", k))) {
                        pairs.push(format!("{}={}", k, v));
                    }
                }
            }
        }
        pairs.join("; ")
    }

    /// 把 Cookie 头注入到请求 HeaderMap（若已有则不覆盖）
    pub fn inject(&self, headers: &mut HeaderMap, url: &str) {
        if headers.contains_key(COOKIE) { return; }
        let cookie = self.cookie_header(url);
        if !cookie.is_empty() {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&cookie) {
                headers.insert(COOKIE, v);
            }
        }
    }

    /// 从响应头收集 Set-Cookie 并回写
    pub fn store_response(&self, url: &str, headers: &HeaderMap) {
        let url_host = Self::host(url).unwrap_or_default();
        for value in headers.get_all(SET_COOKIE).iter() {
            let Ok(raw) = value.to_str() else { continue; };
            self.store_one(&url_host, raw);
        }
    }

    /// 解析单条 Set-Cookie：name=value; Path=/; Domain=...; Max-Age=0 / Expires=past → 删除
    fn store_one(&self, url_host: &str, raw: &str) {
        let mut segments = raw.split(';');
        let first = segments.next().unwrap_or("").trim();
        let Some(eq) = first.find('=') else { return; };
        let name = first[..eq].trim();
        let value = first[eq + 1..].trim();
        if name.is_empty() { return; }

        let mut domain = url_host.to_string();
        let mut expired = false;
        for attr in segments {
            let attr = attr.trim();
            let (key, val) = match attr.find('=') {
                Some(i) => (&attr[..i], Some(attr[i + 1..].trim())),
                None => (attr, None),
            };
            match key.to_ascii_lowercase().as_str() {
                "domain" => {
                    if let Some(v) = val {
                        let d = v.trim_start_matches('.').to_ascii_lowercase();
                        if !d.is_empty() { domain = d; }
                    }
                }
                "max-age" => {
                    if val.map(|v| v.parse::<i64>().unwrap_or(-1) <= 0).unwrap_or(false) {
                        expired = true;
                    }
                }
                "expires" => {
                    // 年份明显早于当前视为过期（粗略但足以覆盖 Max-Age=0 主路径）
                    if let Some(v) = val {
                        if v.contains("1970") { expired = true; }
                    }
                }
                _ => {}
            }
        }
        if domain.is_empty() { return; }

        let Ok(mut jars) = self.jars.lock() else { return; };
        let jar = jars.entry(domain).or_default();
        if expired { jar.remove(name); } else { jar.insert(name.to_string(), value.to_string()); }
    }

    /// java.getCookie(tag)：tag 可能是域名/URL/空；空返回全部 jar 的拼串
    pub fn get_by_tag(&self, tag: &str) -> String {
        let jars = match self.jars.lock() { Ok(j) => j, Err(_) => return String::new() };
        if tag.is_empty() {
            let mut pairs = Vec::new();
            for jar in jars.values() {
                for (k, v) in jar {
                    let p = format!("{}={}", k, v);
                    if !pairs.contains(&p) { pairs.push(p); }
                }
            }
            return pairs.join("; ");
        }
        let host = Self::host(tag).unwrap_or_else(|| tag.trim().to_ascii_lowercase());
        let domains = Self::applicable_domains(&host);
        let mut pairs: Vec<String> = Vec::new();
        for domain in domains {
            if let Some(jar) = jars.get(&domain) {
                for (k, v) in jar {
                    let p = format!("{}={}", k, v);
                    if !pairs.iter().any(|x| x.starts_with(&format!("{}=", k))) {
                        pairs.push(p);
                    }
                }
            }
        }
        pairs.join("; ")
    }

    /// 供测试/管理：清空所有 cookie
    pub fn clear(&self) {
        if let Ok(mut jars) = self.jars.lock() { jars.clear(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_inject() {
        let jar = CookieStore::new();
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, "session=abc123; Path=/; Domain=example.com".parse().unwrap());
        jar.store_response("https://www.example.com/book", &headers);
        assert_eq!(jar.cookie_header("https://www.example.com/ch"), "session=abc123");
        assert_eq!(jar.get_by_tag("example.com"), "session=abc123");

        // Max-Age=0 删除
        let mut h2 = HeaderMap::new();
        h2.append(SET_COOKIE, "session=; Max-Age=0; Domain=example.com".parse().unwrap());
        jar.store_response("https://example.com", &h2);
        assert!(jar.cookie_header("https://example.com").is_empty());
    }

    #[test]
    fn domain_isolation() {
        let jar = CookieStore::new();
        let mut h = HeaderMap::new();
        h.append(SET_COOKIE, "a=1; Domain=foo.com".parse().unwrap());
        jar.store_response("https://foo.com", &h);
        assert!(jar.cookie_header("https://bar.com").is_empty());
        assert_eq!(jar.cookie_header("https://foo.com"), "a=1");
    }
}
