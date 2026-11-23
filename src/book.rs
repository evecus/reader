//! 四条业务链路：搜索 / 详情 / 目录 / 正文。
//! 对应 Legado WebBook + BookList + BookInfo + BookChapterList + BookContent。

use crate::engine::RuleContext;
use crate::error::{AppError, AppResult};
use crate::fetcher::Fetcher;
use crate::model::{Book, BookChapter, BookSource};
use futures::future::join_all;
use serde_json::Value;
use std::sync::Arc;

/// 拆分 Legado jsUrl 返回值：`url,{"headers":{...}}` → (url, Some(headers_json))
/// 也兼容 `url,{"method":"POST",...}` 等其它选项（目前仅消费 headers）。
fn parse_url_with_options(raw: &str) -> (String, Option<String>) {
    let s = raw.trim();
    // 从后往前找第一个顶层 "," 后跟 { 的位置
    if let Some(idx) = s.rfind(",{") {
        let url = s[..idx].trim().to_string();
        let opts = &s[idx + 1..];
        if serde_json::from_str::<Value>(opts).is_ok() {
            return (url, Some(opts.to_string()));
        }
    }
    (s.to_string(), None)
}

pub struct BookService {
    pub fetcher: Arc<Fetcher>,
}

impl BookService {
    pub fn new() -> Self { Self { fetcher: Arc::new(Fetcher::new()) } }

    fn make_context(&self, html: String, base_url: String, source: &BookSource) -> RuleContext {
        let mut ctx = RuleContext::new(html, base_url);
        ctx.js_lib = source.js_lib.clone().unwrap_or_default();
        let fetcher = self.fetcher.clone();
        let header = source.header.clone().unwrap_or_default();
        ctx.fetch = Some(Arc::new(move |url: &str, _h: &str| {
            Fetcher::request_with_options_blocking(url, Some(&header))
        }));
        ctx
    }

    /// 构造带书源绑定的 JS 上下文（供 jsUrl / 规则求值共用）
    fn js_context(&self, source: &BookSource) -> crate::engine::js::JsContext {
        use crate::engine::js::JsContext;
        let fetcher = self.fetcher.clone();
        let header = source.header.clone().unwrap_or_default();
        JsContext {
            js_lib: source.js_lib.clone().unwrap_or_default(),
            source_url: source.book_source_url.clone(),
            source_name: source.book_source_name.clone(),
            fetch: Some(std::sync::Arc::new(move |url: &str, _h: &str| {
                Fetcher::request_with_options_blocking(url, Some(&header))
            })),
            ..Default::default()
        }
    }

    /// 解析入口 URL：支持普通 URL / {{key}}{{page}} / <js> 脚本
    fn resolve_url(&self, source: &BookSource, raw: &str, key: Option<&str>, page: i32) -> AppResult<String> {
        if raw.trim().to_ascii_lowercase().starts_with("<js>")
            || raw.trim().to_ascii_lowercase().starts_with("@js:") {
            let mut ctx = self.js_context(source);
            ctx.key = key.unwrap_or("").to_string();
            ctx.page = page;
            if let Some(u) = crate::engine::js::eval_js_url(raw, &ctx)? {
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

    /// 请求 URL，兼容 Legado 的 `url,{"headers":{...}}` 选项语法
    async fn fetch_url(&self, raw: &str, source: &BookSource) -> AppResult<(String, String)> {
        let (url, extra_headers) = parse_url_with_options(raw);
        let mut header = source.header.clone().unwrap_or_default();
        if let Some(extra_opts) = extra_headers {
            // 选项对象形如 {"headers":{...},"method":"GET"}，提取其中的 headers map
            let add = serde_json::from_str::<Value>(&extra_opts).ok()
                .and_then(|v| v.get("headers").cloned())
                .filter(|h| h.is_object());
            if let Some(add) = add {
                let base = serde_json::from_str::<Value>(&header).ok().unwrap_or(Value::Object(Default::default()));
                let mut base = base.as_object().cloned().unwrap_or_default();
                if let Some(m) = add.as_object() {
                    for (k, v) in m { base.insert(k.clone(), v.clone()); }
                }
                header = serde_json::to_string(&Value::Object(base)).unwrap_or(header);
            }
        }
        self.fetcher.get(&url, Some(&header)).await
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
        let mut ctx = self.make_context(body.clone(), base_url.clone(), source);
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
            if let Some(b) = self.parse_info(source, body, &base_url)? {
                books.push(b);
            }
        }
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
        let (body, final_url) = self.fetcher.get(book_url, source.header.as_deref()).await?;
        self.parse_info(source, body, &final_url)?
            .ok_or_else(|| AppError::internal("解析详情失败"))
    }

    fn parse_info(&self, source: &BookSource, body: String, base_url: &str) -> AppResult<Option<Book>> {
        let rule = source.rule_book_info.as_ref().ok_or_else(|| AppError::bad_request("书源缺少详情规则"))?;
        let mut ctx = self.make_context(body.clone(), base_url.to_string(), source);

        // init 规则
        if let Some(init) = rule.init.as_deref().filter(|s| !s.is_empty()) {
            let init_result = ctx.get_string(init)?;
            if !init_result.is_empty() {
                ctx = self.make_context(init_result, base_url.to_string(), source);
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

    /// 目录
    pub async fn chapters(&self, source: &BookSource, book: &Book) -> AppResult<Vec<BookChapter>> {
        let rule = source.rule_toc.as_ref().ok_or_else(|| AppError::bad_request("书源缺少目录规则"))?;
        let url = if book.toc_url.is_empty() { &book.book_url } else { &book.toc_url };
        let (body, final_url) = self.fetcher.get(url, source.header.as_deref()).await?;
        let mut ctx = self.make_context(body, final_url.clone(), source);

        let list_rule = rule.chapter_list.as_deref().unwrap_or("");
        let elements = ctx.get_elements(list_rule)?;
        let mut chapters = Vec::new();

        for el in &elements {
            let name_rule = rule.chapter_name.as_deref().unwrap_or("text");
            let url_rule = rule.chapter_url.as_deref().unwrap_or("href");
            let names = ctx.element_values(std::slice::from_ref(el), name_rule)?;
            let urls = ctx.element_values(std::slice::from_ref(el), url_rule)?;
            let title = names.first().cloned().unwrap_or_default();
            if title.is_empty() { continue; }
            let chapter_url = absolute_url(&final_url, urls.first().map(|s| s.as_str()).unwrap_or(""));
            chapters.push(BookChapter {
                index: 0, title, url: chapter_url,
                is_volume: false, is_vip: false,
            });
        }

        if chapters.is_empty() { return Err(AppError::internal("目录为空")); }

        // 反转（Legado 默认 chapterList 是倒序，最终正序）
        chapters.reverse();
        for (i, c) in chapters.iter_mut().enumerate() { c.index = i; }
        Ok(chapters)
    }

    /// 正文
    pub async fn content(&self, source: &BookSource, book: &Book, chapter: &BookChapter) -> AppResult<String> {
        let rule = source.rule_content.as_ref().ok_or_else(|| AppError::bad_request("书源缺少正文规则"))?;
        let content_rule = rule.content.as_deref().unwrap_or("");
        if content_rule.is_empty() { return Ok(chapter.url.clone()); }

        let (body, final_url) = self.fetcher.get(&chapter.url, source.header.as_deref()).await?;
        let mut ctx = self.make_context(body, final_url, source);
        ctx.chapter_title = chapter.title.clone();
        let mut text = ctx.get_string(content_rule)?;

        // 全文替换
        if let Some(replace) = rule.replace_regex.as_deref().filter(|s| !s.is_empty()) {
            text = ctx.get_string(replace).unwrap_or(text);
        }
        Ok(text)
    }
}

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
