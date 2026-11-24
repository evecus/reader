//! SQLite 持久层：书源、书架、缓存、配置。

use crate::error::{AppError, AppResult};
use crate::model::{BookSource, CachedChapter, ShelfItem};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
  url TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  group_name TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1,
  custom_order INTEGER NOT NULL DEFAULT 0,
  last_update_time INTEGER NOT NULL DEFAULT 0,
  respond_time INTEGER NOT NULL DEFAULT 180000,
  raw_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS shelf (
  id TEXT PRIMARY KEY,
  book_url TEXT NOT NULL,
  origin TEXT NOT NULL,
  name TEXT NOT NULL,
  author TEXT NOT NULL,
  cover_url TEXT NOT NULL DEFAULT '',
  dur_chapter_index INTEGER NOT NULL DEFAULT 0,
  dur_chapter_title TEXT NOT NULL DEFAULT '',
  added_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  raw_json TEXT NOT NULL,
  UNIQUE(book_url, origin)
);

CREATE TABLE IF NOT EXISTS cached_chapters (
  book_url TEXT NOT NULL,
  chapter_url TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',
  content TEXT NOT NULL,
  cached_at INTEGER NOT NULL,
  PRIMARY KEY (book_url, chapter_url)
);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- 书源级持久化变量（对应 Legado source.getVariable/setVariable、登录态）
CREATE TABLE IF NOT EXISTS source_variables (
  source_url TEXT PRIMARY KEY,
  variables TEXT NOT NULL DEFAULT '{}',
  updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_shelf_book ON shelf(book_url);
CREATE INDEX IF NOT EXISTS idx_cache_book ON cached_chapters(book_url);
"#;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| AppError::internal(format!("open db: {e}")))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| AppError::internal(format!("pragma: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| AppError::internal(format!("schema: {e}")))?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> AppResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|e| AppError::internal(format!("db lock: {e}")))
    }

    // ---- 书源 ----
    pub fn upsert_source(&self, s: &BookSource, raw: &str) -> AppResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO sources(url,name,group_name,enabled,custom_order,last_update_time,respond_time,raw_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(url) DO UPDATE SET name=excluded.name, group_name=excluded.group_name,
               enabled=excluded.enabled, custom_order=excluded.custom_order,
               last_update_time=excluded.last_update_time, respond_time=excluded.respond_time,
               raw_json=excluded.raw_json",
            params![s.book_source_url, s.book_source_name, s.book_source_group,
                s.enabled as i32, s.custom_order, s.last_update_time, s.respond_time, raw],
        )?;
        Ok(())
    }

    pub fn list_sources(&self, enabled_only: bool) -> AppResult<Vec<BookSource>> {
        let conn = self.conn()?;
        let sql = if enabled_only {
            "SELECT raw_json FROM sources WHERE enabled=1 ORDER BY custom_order, name"
        } else {
            "SELECT raw_json FROM sources ORDER BY custom_order, name"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row?;
            if let Ok(s) = serde_json::from_str::<BookSource>(&raw) { out.push(s); }
        }
        Ok(out)
    }

    pub fn delete_source(&self, url: &str) -> AppResult<()> {
        self.conn()?.execute("DELETE FROM sources WHERE url=?1", params![url])?;
        Ok(())
    }

    pub fn set_source_enabled(&self, url: &str, enabled: bool) -> AppResult<()> {
        self.conn()?.execute(
            "UPDATE sources SET enabled=?1 WHERE url=?2",
            params![enabled as i32, url],
        )?;
        Ok(())
    }

    // ---- 书架 ----
    pub fn upsert_shelf(&self, item: &ShelfItem) -> AppResult<()> {
        let b = &item.book;
        let raw = serde_json::to_string(item)?;
        self.conn()?.execute(
            "INSERT INTO shelf(id,book_url,origin,name,author,cover_url,dur_chapter_index,dur_chapter_title,added_at,updated_at,raw_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(book_url,origin) DO UPDATE SET name=excluded.name, author=excluded.author,
               cover_url=excluded.cover_url, dur_chapter_index=excluded.dur_chapter_index,
               dur_chapter_title=excluded.dur_chapter_title, updated_at=excluded.updated_at,
               raw_json=excluded.raw_json",
            params![item.id, b.book_url, b.origin, b.name, b.author, b.cover_url,
                item.dur_chapter_index, item.dur_chapter_title, item.added_at, item.updated_at, raw],
        )?;
        Ok(())
    }

    pub fn list_shelf(&self) -> AppResult<Vec<ShelfItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT raw_json FROM shelf ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            if let Ok(it) = serde_json::from_str::<ShelfItem>(&row?) { out.push(it); }
        }
        Ok(out)
    }

    pub fn get_shelf(&self, book_url: &str, origin: &str) -> AppResult<Option<ShelfItem>> {
        let conn = self.conn()?;
        let raw: Option<String> = conn.query_row(
            "SELECT raw_json FROM shelf WHERE book_url=?1 AND origin=?2",
            params![book_url, origin], |r| r.get(0),
        ).optional()?;
        raw.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
    }

    pub fn remove_shelf(&self, book_url: &str, origin: &str) -> AppResult<()> {
        self.conn()?.execute(
            "DELETE FROM shelf WHERE book_url=?1 AND origin=?2", params![book_url, origin],
        )?;
        Ok(())
    }

    // ---- 缓存 ----
    pub fn cache_chapter(&self, c: &CachedChapter) -> AppResult<()> {
        self.conn()?.execute(
            "INSERT INTO cached_chapters(book_url,chapter_url,title,content,cached_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(book_url,chapter_url) DO UPDATE SET title=excluded.title,
               content=excluded.content, cached_at=excluded.cached_at",
            params![c.book_url, c.chapter_url, c.title, c.content, c.cached_at],
        )?;
        Ok(())
    }

    pub fn get_cached(&self, book_url: &str, chapter_url: &str) -> AppResult<Option<CachedChapter>> {
        let conn = self.conn()?;
        let raw: Option<(String, String, i64)> = conn.query_row(
            "SELECT title,content,cached_at FROM cached_chapters WHERE book_url=?1 AND chapter_url=?2",
            params![book_url, chapter_url], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        Ok(raw.map(|(title, content, cached_at)| CachedChapter {
            book_url: book_url.into(), chapter_url: chapter_url.into(), title, content, cached_at,
        }))
    }

    pub fn list_cached(&self) -> AppResult<Vec<CachedChapter>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT book_url,chapter_url,title,length(content),cached_at FROM cached_chapters ORDER BY cached_at DESC"
        )?;
        let rows = stmt.query_map([], |r| Ok(CachedChapter {
            book_url: r.get(0)?, chapter_url: r.get(1)?, title: r.get(2)?,
            content: String::new(), cached_at: r.get(4)?,
            // 复用字段：用一个伪字段存字节数前端展示（实际 content 为空）
        }))?;
        let mut out = Vec::new();
        for row in rows { out.push(row?); }
        Ok(out)
    }

    pub fn delete_cached(&self, book_url: &str, chapter_url: &str) -> AppResult<()> {
        self.conn()?.execute(
            "DELETE FROM cached_chapters WHERE book_url=?1 AND chapter_url=?2",
            params![book_url, chapter_url],
        )?;
        Ok(())
    }

    pub fn clear_cached(&self) -> AppResult<()> {
        self.conn()?.execute("DELETE FROM cached_chapters", [])?;
        Ok(())
    }

    // ---- 设置 ----
    pub fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.conn()?.query_row(
            "SELECT value FROM settings WHERE key=?1", params![key], |r| r.get(0),
        ).optional().map_err(Into::into)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.conn()?.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn all_settings(&self) -> AppResult<serde_json::Map<String, Value>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT key,value FROM settings")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut map = serde_json::Map::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, Value::String(v));
        }
        Ok(map)
    }

    // ---- 书源持久化变量（登录态 / sourceVariables）----
    /// 读取书源变量 JSON 对象；不存在返回 `{}`
    pub fn get_source_variables(&self, source_url: &str) -> AppResult<Value> {
        let raw: Option<String> = self.conn()?.query_row(
            "SELECT variables FROM source_variables WHERE source_url=?1",
            params![source_url], |r| r.get(0),
        ).optional()?;
        match raw {
            Some(s) => serde_json::from_str(&s).map_err(Into::into),
            None => Ok(Value::Object(Default::default())),
        }
    }

    /// 覆写书源变量 JSON（同一书源串行更新，避免并发覆盖）
    pub fn set_source_variables(&self, source_url: &str, variables: &Value) -> AppResult<()> {
        let raw = serde_json::to_string(variables)?;
        let now = chrono::Utc::now().timestamp_millis();
        self.conn()?.execute(
            "INSERT INTO source_variables(source_url,variables,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(source_url) DO UPDATE SET variables=excluded.variables, updated_at=excluded.updated_at",
            params![source_url, raw, now],
        )?;
        Ok(())
    }
}

// rusqlite 的 optional() 扩展需要 trait
use rusqlite::OptionalExtension;
