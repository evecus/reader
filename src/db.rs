use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{types::ValueRef, Connection, Params};
use serde_json::{Map, Number, Value};

use crate::error::{api_err, ApiResult};

pub const SCHEMA_SQL: &str = include_str!("../schema.sql");
pub const MIGRATIONS_SQL: &str = include_str!("../migrations.sql");

pub type Row = Map<String, Value>;

/// The on-disk format is the Node server's: plain SQL, additive migrations.
/// Keeping it identical means an existing reader.db is opened unchanged and
/// the Node build can still be started against the same data directory.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> ApiResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.apply_migrations()?;
        Ok(db)
    }

    /// SQLite does not support `ADD COLUMN IF NOT EXISTS`. The Node build
    /// detects an already-applied migration by the duplicate-column error;
    /// checking table_info is equivalent but does not rely on an error string.
    fn apply_migrations(&self) -> ApiResult<()> {
        let conn = self.lock();
        for statement in MIGRATIONS_SQL.split(';') {
            let sql = statement.trim();
            if sql.is_empty() {
                continue;
            }
            if sql.to_ascii_uppercase().starts_with("ALTER TABLE") {
                let parts: Vec<&str> = sql.split_whitespace().collect();
                if parts.len() >= 6 {
                    let table = parts[2].trim_matches('"');
                    let column = parts[5].trim_matches('"');
                    // PRAGMA cannot take bind parameters; the table name comes
                    // from our own migrations file, sanitized to identifier chars.
                    let safe_table: String = table.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                    let sql_check = format!("PRAGMA table_info({safe_table})");
                    let mut stmt = conn.prepare(&sql_check)?;
                    let exists = stmt
                        .query_map([], |r| r.get::<_, String>(1))?
                        .filter_map(Result::ok)
                        .any(|name| name == column);
                    if exists {
                        continue;
                    }
                }
            }
            match conn.execute_batch(sql) {
                Ok(()) => {}
                Err(e) => {
                    let text = e.to_string();
                    if !text.contains("duplicate column name") && !text.contains("already exists") {
                        return Err(api_err(500, "MIGRATION", format!("{sql}: {text}")));
                    }
                }
            }
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Read query. Every row becomes a JSON object keyed by column name, which
    /// keeps DTO assembly (and therefore the HTTP contract) in one place.
    pub fn query<P: Params>(&self, sql: &str, p: P) -> ApiResult<Vec<Row>> {
        let conn = self.lock();
        query(&conn, sql, p)
    }

    pub fn one<P: Params>(&self, sql: &str, p: P) -> ApiResult<Option<Row>> {
        Ok(self.query(sql, p)?.into_iter().next())
    }

    pub fn exec<P: Params>(&self, sql: &str, p: P) -> ApiResult<usize> {
        let conn = self.lock();
        Ok(conn.execute(sql, p)?)
    }


    /// Runs `f` inside a transaction, rolling back on any error.
    pub fn transaction<T>(&self, f: impl FnOnce(&Connection) -> ApiResult<T>) -> ApiResult<T> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn count(&self, sql: &str) -> ApiResult<i64> {
        let conn = self.lock();
        Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
    }
}

pub fn query<P: Params>(conn: &Connection, sql: &str, p: P) -> ApiResult<Vec<Row>> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(p)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut map = Map::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => Value::Number(Number::from(n)),
                ValueRef::Real(f) => Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => Value::String(URL_SAFE_NO_PAD.encode(b)),
            };
            map.insert(name.clone(), value);
        }
        out.push(map);
    }
    Ok(out)
}

pub fn one<P: Params>(conn: &Connection, sql: &str, p: P) -> ApiResult<Option<Row>> {
    Ok(query(conn, sql, p)?.into_iter().next())
}

pub fn s(row: &Row, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub fn i(row: &Row, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub fn f(row: &Row, key: &str) -> f64 {
    row.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

