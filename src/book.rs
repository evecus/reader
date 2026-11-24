//! 四条业务链路：搜索 / 详情 / 目录 / 正文。
//! 对应 Legado WebBook + BookList + BookInfo + BookChapterList + BookContent。

use crate::db::Db;
use crate::engine::js::{self, new_var_store, JsContext, VarStore};
use crate::engine::RuleContext;
use crate::error::{AppError, AppResult};
use crate::fetcher::{FetchResponse, Fetcher};
use crate::model::{Book, BookChapter, BookSource};
use futures::future::join_all;
use serde_json::Value;
use std::sync::Arc;

pub struct BookService {
    pub fetcher: Arc<Fetcher>,
    pub db: Arc<Db>,
}

impl BookService {
    pub fn new(fetcher: Arc<Fetcher>, db: Arc<Db>) -> Self { Self { fetcher, db } }

    /// 从 db 加载书源持久化变量到共享表，并注入书元信息
    fn load_vars(&self, source: &BookSource) -> VarStore {
        let store = new_var_store();
        if let Ok(Value::Object(map)) = self.db.get_source_variables(&source.book_source_url) {
            if let Ok(mut m) = store.lock() {
                for (k, v) in map {
                    m.insert(k.clone(), match v {
                        Value::String(s) => s,
                        other => other.to_string(),
                    });
                }
            }
        }
        if let Ok(mut m) = store.lock() {
            m.insert("sourceUrl".into(), source.book_source_url.clone());
            m.insert("sourceName".into(), source.book_source_name.clone());
        }
        store
    }

    /// 把共享变量表回写 db（剔除注入的只读书元信息）
    fn save_vars(&self, source: &BookSource, store: &VarStore) {
        let map = match store.lock() {
            Ok(m) => m.iter()
                .filter(|(k, _)| k.as_str() != "sourceUrl" && k.as_str() != "sourceName"
                    && k.as_str() != "key" && k.as_str() != "page")
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect::<serde_json::Map<String, Value>>(),
            Err(_) => return,
        };
        let _ = self.db.set_source_variables(&source.book_source_url, &Value::Object(map));
    }

    /// 构造 fetch 闭包：走实例方法以共享 Cookie jar
    fn fetch_closure(&self, source: &BookSource)
        -> std::sync::Arc<dyn Fn(&str, &str, u64) -> Result<FetchResponse, String> + Send + Sync>
    {
        let fetcher = self.fetcher.clone();
        let header = source.header.clone().unwrap_or_default();
        Arc::new(move |url: &str, _h: &str, timeout: u64| {
            fetcher.request_blocking_full(url, Some(&header), (timeout != 0).then_some(timeout))
        })
    }

    /// 构造 webview 闭包：走全局 headless Chrome 渲染（对齐 java.webView*）
    fn webview_closure(&self) -> crate::engine::js::JsWebviewFn {
        Arc::new(|mode: u8, html: &str, url: &str, js: &str, regex: &str| {
            crate::webview::call(mode, html, url, js, regex)
        })
    }

    fn make_context(&self, html: String, base_url: String, source: &BookSource, vars: &VarStore) -> RuleContext {
        let mut ctx = RuleContext::new(html, base_url);
        ctx.js_lib = source.js_lib.clone().unwrap_or_default();
        ctx.variables = vars.clone();
        ctx.cookies = self.fetcher.cookies.clone();
        ctx.fetch = Some(self.fetch_closure(source));
        ctx.webview = Some(self.webview_closure());
        ctx
    }

    /// 构造带书源绑定的 JS 上下文（供 jsUrl / 规则求值共用）
    fn js_context(&self, source: &BookSource, vars: &VarStore) -> crate::engine::js::JsContext {
        JsContext {
            js_lib: source.js_lib.clone().unwrap_or_default(),
            source_url: source.book_source_url.clone(),
            source_name: source.book_source_name.clone(),
            variables: vars.clone(),
            cookies: self.fetcher.cookies.clone(),
            fetch: Some(self.fetch_closure(source)),
            webview: Some(self.webview_closure()),
            ..Default::default()
        }
    }

    /// 构造仅用于 jsUrl 求值的变量表（带 key/page）
    fn js_url_vars(&self, source: &BookSource, key: Option<&str>, page: i32) -> VarStore {
        let vars = self.load_vars(source);
        if let Ok(mut m) = vars.lock() {
            m.insert("key".into(), key.unwrap_or("").to_string());
            m.insert("page".into(), page.to_string());
        }
        vars
    }

    /// 解析入口 URL：支持普通 URL / {{key}}{{page}} / <js> 脚本
    fn resolve_url(&self, source: &BookSource, raw: &str, key: Option<&str>, page: i32) -> AppResult<String> {
        if raw.trim().to_ascii_lowercase().starts_with("<js>")
            || raw.trim().to_ascii_lowercase().starts_with("@js:") {
            let vars = self.js_url_vars(source, key, page);
            let mut ctx = self.js_context(source, &vars);
            ctx.key = key.unwrap_or("").to_string();
            ctx.page = page;
            if let Some(u) = js::eval_js_url(raw, &ctx)? {
                // jsUrl 中 java.put / source.setVariable 的写入落库
                self.save_vars(source, &vars);
                return Ok(u);
            }
        }
        Ok(Fetcher::prepare_url(raw, key, Some(page)))
    }

    /// 搜索（单源）
    pub async fn search(&self, source: &BookSource, key: &str, page: i32) -> AppResult<Vec<Book>> {
        let search_url = source.search_url.as_ref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::bad_request("书源搜索地址为空"))?;
        let url = self.resolve_url(source, search_url, Some(key), page)?;
        let (body, final_url) = self.fetch_url(&url, source).await?;
        self.parse_book_list(source, body, final_url, true)
    }

    /// 发现（分类/榜单）
    pub async fn explore(&self, source: &BookSource, url: &str, page: i32) -> AppResult<Vec<Book>> {
        let url = self.resolve_url(source, url, None, page)?;
        let (body, final_url) = self.fetch_url(&url, source).await?;
        self.parse_book_list(source, body, final_url, false)
    }

    /// 请求 URL：完整消费 Legado 选项语法
    /// `url,{"method":"POST","body":"...","headers":{...},"charset":"gbk","retry":n,"timeout":n}`
    /// 支持 POST 搜索，语义由 Fetcher::request 统一处理。
    async fn fetch_url(&self, raw: &str, source: &BookSource) -> AppResult<(String, String)> {
        self.fetcher.request(raw, source.header.as_deref()).await
    }

    /// 多源并发搜索
    pub async fn search_multi(&self, sources: Vec<BookSource>, key: &str, page: i32) -> Vec<(String, AppResult<Vec<Book>>)> {
        let futures = sources.into_iter().map(|s| {
            let name = s.book_source_name.clone();
            let svc = &self;
            async move {
                let r = svc.search(&s, key, page).await;
                (name, r)
            }
        });
        join_all(futures).await
    }

    /// 解析书单页（搜索/发现共用）
    fn parse_book_list(&self, source: &BookSource, body: String, base_url: String, is_search: bool) -> AppResult<Vec<Book>> {
        let vars = self.load_vars(source);
        let mut ctx = self.make_context(body.clone(), base_url.clone(), source, &vars);
        let rule = if is_search {
            source.rule_search.clone()
        } else {
            source.rule_explore.clone().or_else(|| source.rule_search.clone())
        }.ok_or_else(|| AppError::bad_request("书源缺少书单规则"))?;

        let list_rule = rule.book_list.as_deref().unwrap_or("");
        let elements = ctx.get_elements(list_rule)?;
        let mut books = Vec::new();

        for el in &elements {
            if let Some(book) = self.parse_book_item(source, &mut ctx, el, &base_url, &rule)? {
                books.push(book);
            }
        }

        // 去重（书名+作者）
        let mut seen = std::collections::HashSet::new();
        books.retain(|b| seen.insert(format!("{}|{}", b.name, b.author)));

        // 如果列表为空且是搜索，尝试当作详情页解析
        if books.is_empty() && is_search {
            if let Some(b) = self.parse_info(source, body, &base_url, &vars)? {
                books.push(b);
            }
        }
        self.save_vars(source, &vars);
        Ok(books)
    }

    fn parse_book_item(&self, source: &BookSource, ctx: &mut RuleContext, el: &String, base_url: &str, rule: &crate::model::BookListRule) -> AppResult<Option<Book>> {
        // 把单个元素作为上下文内容
        let html = el.clone();
        let mut item_ctx = RuleContext::new(html, base_url.to_string());
        item_ctx.js_lib = ctx.js_lib.clone();
        item_ctx.variables = ctx.variables.clone();
        item_ctx.fetch = ctx.fetch.clone();

        let name = get_s(&mut item_ctx, rule.name.as_deref())?;
        if name.is_empty() { return Ok(None); }

        let book_url_raw = get_s(&mut item_ctx, rule.book_url.as_deref())?;
        let book_url = absolute_url(base_url, &book_url_raw);

        let author = get_s(&mut item_ctx, rule.author.as_deref())?;
        let kind = get_list(&mut item_ctx, rule.kind.as_deref())?.join(",");
        let word_count = get_s(&mut item_ctx, rule.word_count.as_deref())?;
        let last_chapter = get_s(&mut item_ctx, rule.last_chapter.as_deref())?;
        let cover_url = absolute_url(base_url, &get_s(&mut item_ctx, rule.cover_url.as_deref())?);
        let intro = get_s(&mut item_ctx, rule.intro.as_deref())?;

        ctx.variables = item_ctx.variables.clone();
        Ok(Some(Book {
            book_url, origin: source.book_source_url.clone(),
            origin_name: source.book_source_name.clone(), name, author, kind,
            word_count, last_chapter, intro, cover_url,
            toc_url: String::new(), origins: vec![source.book_source_url.clone()],
        }))
    }

    /// 书籍详情
    pub async fn book_info(&self, source: &BookSource, book_url: &str) -> AppResult<Book> {
        let (body, final_url) = self.fetch_url(book_url, source).await?;
        let vars = self.load_vars(source);
        let book = self.parse_info(source, body, &final_url, &vars)?
            .ok_or_else(|| AppError::internal("解析详情失败"))?;
        self.save_vars(source, &vars);
        Ok(book)
    }

    fn parse_info(&self, source: &BookSource, body: String, base_url: &str, vars: &VarStore) -> AppResult<Option<Book>> {
        let rule = source.rule_book_info.as_ref().ok_or_else(|| AppError::bad_request("书源缺少详情规则"))?;
        let mut ctx = self.make_context(body.clone(), base_url.to_string(), source, vars);

        // init 规则
        if let Some(init) = rule.init.as_deref().filter(|s| !s.is_empty()) {
            let init_result = ctx.get_string(init)?;
            if !init_result.is_empty() {
                ctx = self.make_context(init_result, base_url.to_string(), source, vars);
            }
        }

        let name = get_s(&mut ctx, rule.name.as_deref())?;
        if name.is_empty() { return Ok(None); }

        let author = get_s(&mut ctx, rule.author.as_deref())?;
        let kind = get_list(&mut ctx, rule.kind.as_deref())?.join(",");
        let word_count = get_s(&mut ctx, rule.word_count.as_deref())?;
        let last_chapter = get_s(&mut ctx, rule.last_chapter.as_deref())?;
        let intro = get_s(&mut ctx, rule.intro.as_deref())?;
        let cover_url = absolute_url(base_url, &get_s(&mut ctx, rule.cover_url.as_deref())?);
        let mut toc_url = get_s(&mut ctx, rule.toc_url.as_deref())?;
        if toc_url.is_empty() { toc_url = base_url.to_string(); }
        else { toc_url = absolute_url(base_url, &toc_url); }

        Ok(Some(Book {
            book_url: base_url.to_string(), origin: source.book_source_url.clone(),
            origin_name: source.book_source_name.clone(), name, author, kind,
            word_count, last_chapter, intro, cover_url, toc_url,
            origins: vec![source.book_source_url.clone()],
        }))
    }

    /// 目录（含 nextTocUrl 翻页）
    ///
    /// 对齐 Legado `BookChapterList.analyzeChapterList`：
    /// - 下一页规则返回 1 个 URL：顺序翻页，直到没有下一页或遇到已访问过的 URL（防循环）
    /// - 返回多个 URL：并发抓取所有分页（保持顺序），分页内不再继续翻页
    /// - 目录列表规则以 `-` 开头表示站点目录是倒序的，需要反转；无前缀保持页面顺序
    pub async fn chapters(&self, source: &BookSource, book: &Book) -> AppResult<Vec<BookChapter>> {
        let rule = source.rule_toc.as_ref().ok_or_else(|| AppError::bad_request("书源缺少目录规则"))?;
        let url = if book.toc_url.is_empty() { &book.book_url } else { &book.toc_url };
        let (body, final_url) = self.fetch_url(url, source).await?;
        let vars = self.load_vars(source);

        // 官方 BookChapterList.kt：`-` 前缀 → reverse=true；最终顺序 = 页面顺序（无前缀）或其反转（`-`）
        let raw_list_rule = rule.chapter_list.as_deref().unwrap_or("");
        let (list_rule, should_reverse) = if let Some(rest) = raw_list_rule.strip_prefix('-') {
            (rest, true)
        } else if let Some(rest) = raw_list_rule.strip_prefix('+') {
            (rest, false)
        } else {
            (raw_list_rule, false)
        };

        let mut visited: Vec<String> = vec![final_url.clone()];
        let (mut chapters, nexts) =
            self.parse_toc_page(source, rule, &vars, body, &final_url, list_rule, true)?;

        match nexts.len() {
            0 => {}
            1 => {
                let mut next = nexts.into_iter().next().unwrap_or_default();
                while !next.is_empty() && !visited.contains(&next) && visited.len() < MAX_PAGES {
                    visited.push(next.clone());
                    // 首页之后的分页出错不拖垮整个目录，保留已取到的部分
                    let (b, u) = match self.fetch_url(&next, source).await {
                        Ok(r) => r,
                        Err(e) => { tracing::warn!("目录翻页失败 {}: {}", next, e.message); break; }
                    };
                    let (c, n) = self.parse_toc_page(source, rule, &vars, b, &u, list_rule, true)?;
                    chapters.extend(c);
                    next = n.into_iter().next().unwrap_or_default();
                }
            }
            _ => {
                let urls: Vec<String> = nexts.into_iter().take(MAX_PAGES).collect();
                let fetched = self.fetch_many(&urls, source).await;
                for (u, r) in urls.iter().zip(fetched) {
                    match r {
                        Ok((b, fu)) => {
                            let (c, _) = self.parse_toc_page(source, rule, &vars, b, &fu, list_rule, false)?;
                            chapters.extend(c);
                        }
                        Err(e) => tracing::warn!("目录分页抓取失败 {}: {}", u, e.message),
                    }
                }
            }
        }

        if chapters.is_empty() { return Err(AppError::internal("目录为空")); }

        // 按章节 URL 去重（保留首次出现），对应官方 LinkedHashSet
        let mut seen = std::collections::HashSet::new();
        chapters.retain(|c| seen.insert(c.url.clone()));

        if should_reverse { chapters.reverse(); }
        for (i, c) in chapters.iter_mut().enumerate() { c.index = i; }
        self.save_vars(source, &vars);
        Ok(chapters)
    }

    /// 并发抓取多个分页（按批 `PAGE_CONCURRENCY` 并发，结果保持输入顺序）
    async fn fetch_many(&self, urls: &[String], source: &BookSource) -> Vec<AppResult<(String, String)>> {
        let mut out = Vec::with_capacity(urls.len());
        for chunk in urls.chunks(PAGE_CONCURRENCY) {
            out.extend(join_all(chunk.iter().map(|u| self.fetch_url(u, source))).await);
        }
        out
    }

    /// 解析一页目录，返回（章节列表，下一页 URL 列表）
    fn parse_toc_page(
        &self, source: &BookSource, rule: &crate::model::TocRule, vars: &VarStore,
        body: String, page_url: &str, list_rule: &str, get_next: bool,
    ) -> AppResult<(Vec<BookChapter>, Vec<String>)> {
        let mut ctx = self.make_context(body, page_url.to_string(), source, vars);
        let elements = ctx.get_elements(list_rule)?;

        let mut nexts: Vec<String> = Vec::new();
        if get_next {
            if let Some(r) = rule.next_toc_url.as_deref().filter(|s| !s.is_empty()) {
                for u in ctx.get_string_list(r)? {
                    let abs = absolute_url(page_url, &u);
                    if !abs.is_empty() && abs != page_url && !nexts.contains(&abs) { nexts.push(abs); }
                }
            }
        }

        let name_rule = rule.chapter_name.as_deref().filter(|s| !s.is_empty()).unwrap_or("text");
        let url_rule = rule.chapter_url.as_deref().filter(|s| !s.is_empty()).unwrap_or("href");
        let mut chapters = Vec::new();
        for el in &elements {
            // 每个元素用独立上下文求值，规则才能完整支持 && / || / ## / {{js}} 等
            let mut item = RuleContext::new(el.clone(), page_url.to_string());
            item.js_lib = ctx.js_lib.clone();
            item.variables = ctx.variables.clone();
            item.fetch = ctx.fetch.clone();
            item.cookies = ctx.cookies.clone();
            let title = get_s(&mut item, Some(name_rule))?;
            if title.is_empty() { continue; }
            let raw_url = get_s(&mut item, Some(url_rule))?;
            let chapter_url = if raw_url.is_empty() { page_url.to_string() } else { absolute_url(page_url, &raw_url) };
            chapters.push(BookChapter {
                index: 0, title, url: chapter_url,
                is_volume: false, is_vip: false,
            });
        }
        Ok((chapters, nexts))
    }

    /// 正文
    pub async fn content(&self, source: &BookSource, book: &Book, chapter: &BookChapter) -> AppResult<String> {
        self.content_with_next(source, book, chapter, None).await
    }

    /// 正文（含 nextContentUrl 翻页）。
    /// `next_chapter_url`：下一章的 URL，用于在“下一页”指向下一章时停止翻页（官方同样处理）。
    pub async fn content_with_next(
        &self, source: &BookSource, _book: &Book, chapter: &BookChapter, next_chapter_url: Option<&str>,
    ) -> AppResult<String> {
        let rule = source.rule_content.as_ref().ok_or_else(|| AppError::bad_request("书源缺少正文规则"))?;
        let content_rule = rule.content.as_deref().unwrap_or("");
        if content_rule.is_empty() { return Ok(chapter.url.clone()); }

        let (body, final_url) = self.fetch_url(&chapter.url, source).await?;
        let vars = self.load_vars(source);
        let next_chapter_abs = next_chapter_url
            .filter(|s| !s.is_empty())
            .map(|u| absolute_url(&final_url, u));

        let mut visited: Vec<String> = vec![final_url.clone()];
        let (first, nexts) = self.parse_content_page(source, rule, &vars, chapter, body, &final_url, true)?;
        let mut pages = vec![first];

        match nexts.len() {
            0 => {}
            1 => {
                let mut next = nexts.into_iter().next().unwrap_or_default();
                while !next.is_empty() && !visited.contains(&next) && visited.len() < MAX_PAGES {
                    if next_chapter_abs.as_deref() == Some(next.as_str()) { break; }
                    visited.push(next.clone());
                    let (b, u) = match self.fetch_url(&next, source).await {
                        Ok(r) => r,
                        Err(e) => { tracing::warn!("正文翻页失败 {}: {}", next, e.message); break; }
                    };
                    let (text, n) = self.parse_content_page(source, rule, &vars, chapter, b, &u, true)?;
                    pages.push(text);
                    next = n.into_iter().next().unwrap_or_default();
                }
            }
            _ => {
                let urls: Vec<String> = nexts.into_iter().take(MAX_PAGES).collect();
                let fetched = self.fetch_many(&urls, source).await;
                for (u, r) in urls.iter().zip(fetched) {
                    match r {
                        Ok((b, fu)) => {
                            let (text, _) = self.parse_content_page(source, rule, &vars, chapter, b, &fu, false)?;
                            pages.push(text);
                        }
                        Err(e) => tracing::warn!("正文分页抓取失败 {}: {}", u, e.message),
                    }
                }
            }
        }

        let mut text = pages.join("\n");

        // 全文替换：把 replaceRegex（形如 `##正则##替换`）作用在拼好的正文上
        if let Some(replace) = rule.replace_regex.as_deref().filter(|s| !s.is_empty()) {
            let mut sub = self.make_context(text.clone(), final_url.clone(), source, &vars);
            if let Ok(r) = sub.get_string(replace) { text = r; }
        }
        self.save_vars(source, &vars);
        Ok(text)
    }

    /// 解析一页正文，返回（正文文本，下一页 URL 列表）
    fn parse_content_page(
        &self, source: &BookSource, rule: &crate::model::ContentRule, vars: &VarStore,
        chapter: &BookChapter, body: String, page_url: &str, get_next: bool,
    ) -> AppResult<(String, Vec<String>)> {
        let mut ctx = self.make_context(body, page_url.to_string(), source, vars);
        ctx.chapter_title = chapter.title.clone();
        let text = ctx.get_string(rule.content.as_deref().unwrap_or(""))?;
        let mut nexts: Vec<String> = Vec::new();
        if get_next {
            if let Some(r) = rule.next_content_url.as_deref().filter(|s| !s.is_empty()) {
                for u in ctx.get_string_list(r)? {
                    let abs = absolute_url(page_url, &u);
                    if !abs.is_empty() && abs != page_url && !nexts.contains(&abs) { nexts.push(abs); }
                }
            }
        }
        Ok((text, nexts))
    }
}

/// 翻页上限（防止站点无限生成下一页链接）
const MAX_PAGES: usize = 2000;
/// 多个分页 URL 并发抓取的并发度
const PAGE_CONCURRENCY: usize = 6;

fn get_s(ctx: &mut RuleContext, rule: Option<&str>) -> AppResult<String> {
    match rule { Some(r) if !r.is_empty() => ctx.get_string(r), _ => Ok(String::new()) }
}
fn get_list(ctx: &mut RuleContext, rule: Option<&str>) -> AppResult<Vec<String>> {
    match rule { Some(r) if !r.is_empty() => ctx.get_string_list(r), _ => Ok(vec![]) }
}

pub fn absolute_url(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.is_empty() { return String::new(); }
    if url.starts_with("http://") || url.starts_with("https://") { return url.to_string(); }
    if url.starts_with("//") {
        let scheme = if base.starts_with("https") { "https" } else { "http" };
        return format!("{}:{}", scheme, url);
    }
    let base = base.trim_end_matches('/');
    if url.starts_with('/') {
        // 取 base 的 origin
        if let Some(pos) = base.find("//") {
            let rest = &base[pos + 2..];
            let host = rest.split('/').next().unwrap_or("");
            let scheme = &base[..pos];
            return format!("{}//{}{}", scheme, host, url);
        }
    }
    format!("{}/{}", base, url.trim_start_matches("./"))
}

#[cfg(test)]
mod pagination_tests {
    use super::*;
    use crate::model::{Book, BookChapter};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn page(path: &str) -> Option<String> {
        Some(match path {
            "/toc1" => r#"<ul class="list"><li><a href="/c/1">第1章</a></li><li><a href="/c/2">第2章</a></li></ul>
                <a class="next" href="/toc2">下一页</a>"#.into(),
            // 第 2 页含与第 1 页重复的章节，用于验证去重
            "/toc2" => r#"<ul class="list"><li><a href="/c/2">第2章</a></li><li><a href="/c/3">第3章</a></li></ul>
                <a class="next" href="/toc3">下一页</a>"#.into(),
            // 第 3 页的“下一页”绕回第 1 页，用于验证防循环
            "/toc3" => r#"<ul class="list"><li><a href="/c/4">第4章</a></li></ul>
                <a class="next" href="/toc1">下一页</a>"#.into(),
            "/c/1" => r#"<div id="content">第一页内容</div><a id="nx" href="/c/1_2">下一页</a>"#.into(),
            "/c/1_2" => r#"<div id="content">第二页内容</div><a id="nx" href="/c/2">下一章</a>"#.into(),
            "/c/2" => r#"<div id="content">下一章内容</div>"#.into(),
            _ => return None,
        })
    }

    async fn spawn_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let (status, body) = match page(&path) {
                        Some(b) => ("200 OK", b),
                        None => ("404 Not Found", String::new()),
                    };
                    let resp = format!(
                        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status, body.as_bytes().len(), body
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        port
    }

    fn service() -> BookService {
        let dir = std::env::temp_dir().join(format!("reader-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.db")).unwrap());
        BookService::new(Arc::new(Fetcher::new()), db)
    }

    fn source(port: u16, list_rule: &str) -> BookSource {
        serde_json::from_value(serde_json::json!({
            "bookSourceUrl": format!("http://127.0.0.1:{}", port),
            "bookSourceName": "测试源",
            "ruleToc": {
                "chapterList": list_rule,
                "chapterName": "tag.a@text",
                "chapterUrl": "tag.a@href",
                "nextTocUrl": "class.next@href"
            },
            "ruleContent": {
                "content": "id.content@text",
                "nextContentUrl": "id.nx@href"
            }
        })).unwrap()
    }

    #[tokio::test]
    async fn toc_follows_next_pages_dedups_and_stops_on_cycle() {
        let port = spawn_server().await;
        let svc = service();
        let src = source(port, "class.list@tag.li");
        let book = Book { book_url: format!("http://127.0.0.1:{}/toc1", port), ..Default::default() };
        let chapters = svc.chapters(&src, &book).await.unwrap();
        let titles: Vec<_> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第1章", "第2章", "第3章", "第4章"]);
        assert!(chapters[0].url.ends_with("/c/1"));
        assert_eq!(chapters.iter().map(|c| c.index).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn toc_minus_prefix_reverses() {
        let port = spawn_server().await;
        let svc = service();
        let src = source(port, "-class.list@tag.li");
        let book = Book { book_url: format!("http://127.0.0.1:{}/toc1", port), ..Default::default() };
        let chapters = svc.chapters(&src, &book).await.unwrap();
        let titles: Vec<_> = chapters.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["第4章", "第3章", "第2章", "第1章"]);
    }

    #[tokio::test]
    async fn content_follows_next_page_and_stops_at_next_chapter() {
        let port = spawn_server().await;
        let svc = service();
        let src = source(port, "class.list@tag.li");
        let book = Book::default();
        let ch = BookChapter { url: format!("http://127.0.0.1:{}/c/1", port), ..Default::default() };
        let next_ch = format!("http://127.0.0.1:{}/c/2", port);
        let text = svc.content_with_next(&src, &book, &ch, Some(&next_ch)).await.unwrap();
        assert_eq!(text, "第一页内容\n第二页内容");
        // 不知道下一章时，会把“下一章”也并进来（与官方一致，所以 API/下载器要传 next_url）
        let text2 = svc.content_with_next(&src, &book, &ch, None).await.unwrap();
        assert_eq!(text2, "第一页内容\n第二页内容\n下一章内容");
    }
}
