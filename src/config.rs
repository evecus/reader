use std::{
    env, fs,
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;

use crate::error::{api_err, ApiResult};

/// Runtime configuration. The environment contract is kept byte-for-byte with
/// the Node server so an existing docker-compose / systemd unit needs no edit.
#[derive(Clone, Debug)]
pub struct Config {
    /// User's book directory; may be mounted read-only or writable.
    pub books_dir: PathBuf,
    /// Writable directory owned by this server. Never inside books_dir.
    pub data_dir: PathBuf,
    pub host: String,
    pub port: u16,
    /// Persistent HMAC secret, auto generated on first boot if absent.
    pub jwt_secret: String,
    pub access_ttl: i64,
    pub refresh_ttl: i64,
    pub scan_interval: u64,
    /// Empty means "reflect any origin" (LAN self-hosting, no cookies in play).
    pub cors_origins: Vec<String>,
}

fn env_int(name: &str, fallback: i64) -> i64 {
    env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .unwrap_or(fallback)
}

fn absolute(path: PathBuf) -> ApiResult<PathBuf> {
    std::path::absolute(&path)
        .map_err(|e| api_err(500, "CONFIG", format!("invalid path {}: {e}", path.display())))
}

/// Regenerating this on every boot would invalidate every client session.
fn load_or_create_secret(data_dir: &Path) -> ApiResult<String> {
    for name in ["READER_TOKEN_SECRET", "JWT_SECRET"] {
        if let Ok(value) = env::var(name) {
            if value.len() >= 16 {
                return Ok(value);
            }
        }
    }
    let path = data_dir.join("token.secret");
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if trimmed.len() >= 16 {
            return Ok(trimmed.to_string());
        }
    }
    let mut bytes = [0u8; 48];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let generated = URL_SAFE_NO_PAD.encode(bytes);
    fs::write(&path, &generated).map_err(|e| {
        api_err(
            500,
            "DATA_DIR_READONLY",
            format!(
                "cannot write {} ({e}); DATA_DIR must be writable, or set READER_TOKEN_SECRET (>=16 chars)",
                path.display()
            ),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(generated)
}

impl Config {
    pub fn load() -> ApiResult<Self> {
        let books_dir = absolute(PathBuf::from(env::var("BOOKS_DIR").unwrap_or_else(|_| "/books".into())))?;
        let data_dir = absolute(PathBuf::from(
            env::var("DATA_DIR").unwrap_or_else(|_| "data".into()),
        ))?;

        if data_dir.join(".reader-backup-incomplete").exists()
            || data_dir.join("reader-backup.json").exists()
        {
            return Err(api_err(
                500,
                "BACKUP_PENDING",
                "DATA_DIR contains an incomplete backup; restore a verified backup first",
            ));
        }
        if data_dir.starts_with(&books_dir) {
            return Err(api_err(
                500,
                "CONFIG",
                format!(
                    "DATA_DIR ({}) must not live inside BOOKS_DIR ({})",
                    data_dir.display(),
                    books_dir.display()
                ),
            ));
        }
        fs::create_dir_all(&data_dir)
            .map_err(|e| api_err(500, "DATA_DIR", format!("cannot create DATA_DIR: {e}")))?;

        let port = env_int("PORT", 5888);
        let port = u16::try_from(port).unwrap_or(5888);
        let scan_interval = env_int("SCAN_INTERVAL", 1800).max(0) as u64;

        Ok(Self {
            jwt_secret: load_or_create_secret(&data_dir)?,
            books_dir,
            data_dir,
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port,
            access_ttl: env_int("ACCESS_TOKEN_TTL", 60 * 60 * 24 * 7),
            refresh_ttl: env_int("REFRESH_TOKEN_TTL", 60 * 60 * 24 * 365),
            scan_interval,
            cors_origins: env::var("CORS_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        })
    }

    pub fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    pub fn now_s() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}
