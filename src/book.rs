//! 四条业务链路：搜索 / 详情 / 目录 / 正文。
//! 对应 Legado WebBook + BookList + BookInfo + BookChapterList + BookContent。

use crate::db::Db;
use crate::engine::js::{self, new_var_store, JsContext, VarStore};
use crate::engine::RuleContext;
use crate::error::{AppError, AppResult};
use crate::fetcher::Fetcher;
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
        -> std::sync::Arc<dyn Fn(&str, &str) -> String + Send + Sync>
    {
        let fetcher = self.fetcher.clone();
        let header = source.header.clone().unwrap_or_default();
        Arc::new(move |url: &str, _h: &str| fetcher.request_blocking(url, Some(&header)))
    }

    fn make_context(&self, html: String, base_url: String, source: &BookSource, vars: &VarStore) -> RuleContext {
        let mut ctx = RuleContext::new(html, base_url);
        ctx.js_lib = source.js_lib.clone().unwrap_or_default();
        ctx.variables = vars.clone();
        ctx.cookies = self.fetcher.cookies.clone();
        ctx.fetch = Some(self.fetch_closure(source));
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

    /// 目录
    pub async fn chapters(&self, source: &BookSource, book: &Book) -> AppResult<Vec<BookChapter>> {
        let rule = source.rule_toc.as_ref().ok_or_else(|| AppError::bad_request("书源缺少目录规则"))?;
        let url = if book.toc_url.is_empty() { &book.book_url } else { &book.toc_url };
        let (body, final_url) = self.fetch_url(url, source).await?;
        let vars = self.load_vars(source);
        let mut ctx = self.make_context(body, final_url.clone(), source, &vars);

        // Legado 目录反转前缀（BookChapterList.kt:52-60,124-126）：
        //   "-xxx" → 书源已正序，抓取后不再反转；"+xxx" 或无前缀 → 抓取后反转
        let raw_list_rule = rule.chapter_list.as_deref().unwrap_or("");
        let (list_rule, should_reverse) = if let Some(rest) = raw_list_rule.strip_prefix('-') {
            (rest, false)
        } else if let Some(rest) = raw_list_rule.strip_prefix('+') {
            (rest, true)
        } else {
            (raw_list_rule, true)
        };
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

        // 反转（Legado 默认 chapterList 是倒序，最终正序；"-" 前缀书源已正序则不反转）
        if should_reverse { chapters.reverse(); }
        for (i, c) in chapters.iter_mut().enumerate() { c.index = i; }
        self.save_vars(source, &vars);
        Ok(chapters)
    }

    /// 正文
    pub async fn content(&self, source: &BookSource, _book: &Book, chapter: &BookChapter) -> AppResult<String> {
        let rule = source.rule_content.as_ref().ok_or_else(|| AppError::bad_request("书源缺少正文规则"))?;
        let content_rule = rule.content.as_deref().unwrap_or("");
        if content_rule.is_empty() { return Ok(chapter.url.clone()); }

        let (body, final_url) = self.fetch_url(&chapter.url, source).await?;
        let vars = self.load_vars(source);
        let mut ctx = self.make_context(body, final_url, source, &vars);
        ctx.chapter_title = chapter.title.clone();
        let mut text = ctx.get_string(content_rule)?;

        // 全文替换
        if let Some(replace) = rule.replace_regex.as_deref().filter(|s| !s.is_empty()) {
            text = ctx.get_string(replace).unwrap_or(text);
        }
        self.save_vars(source, &vars);
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
