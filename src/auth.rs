use axum::http::{header::AUTHORIZATION, HeaderMap};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use rand::RngCore;
use scrypt::{scrypt, Params};
use sha2::Sha256;
use uuid::Uuid;

use crate::{
    config::Config,
    error::{unauthorized, ApiResult},
    state::AppState,
};

const SCRYPT_LOG_N: u8 = 14; // 2^14 = 16384
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const SCRYPT_LEN: usize = 64;
const REFRESH_PEPPER: &str = "refresh-token-pepper";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: String,
    pub role: String,
    pub ver: i64,
}

pub fn hash_password(password: &str) -> ApiResult<String> {
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let mut derived = [0u8; SCRYPT_LEN];
    scrypt(
        password.as_bytes(),
        &salt,
        &Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_LEN)?,
        &mut derived,
    )?;
    Ok(format!(
        "scrypt${}${}${}${}${}",
        1u32 << SCRYPT_LOG_N,
        SCRYPT_R,
        SCRYPT_P,
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(derived)
    ))
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 6 || parts[0] != "scrypt" {
        return false;
    }
    let (Ok(n), Ok(r), Ok(p)) = (
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
        parts[3].parse::<u32>(),
    ) else {
        return false;
    };
    let log_n = n.trailing_zeros() as u8;
    if !n.is_power_of_two() || (1u32 << log_n) != n {
        return false;
    }
    let (Ok(salt), Ok(expected)) = (
        URL_SAFE_NO_PAD.decode(parts[4]),
        URL_SAFE_NO_PAD.decode(parts[5]),
    ) else {
        return false;
    };
    let Ok(params) = Params::new(log_n, r, p, expected.len()) else {
        return false;
    };
    let mut derived = vec![0u8; expected.len()];
    if scrypt(password.as_bytes(), &salt, &params, &mut derived).is_err() {
        return false;
    }
    constant_time_eq(&derived, &expected)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hmac_b64(secret: &str, data: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(data.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// HS256-compatible shape, hand built so the field order matches the Node
/// server byte-for-byte (a Node build can verify tokens issued here).
pub fn sign_access_token(
    config: &Config,
    user: &AuthUser,
) -> ApiResult<(String, i64)> {
    let iat = Config::now_s();
    let exp = iat + config.access_ttl;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = format!(
        r#"{{"sub":"{}","role":"{}","type":"access","iat":{},"exp":{},"ver":{}}}"#,
        user.id, user.role, iat, exp, user.ver
    );
    let body = URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let signing_input = format!("{header}.{body}");
    let signature = hmac_b64(&config.jwt_secret, &signing_input);
    Ok((format!("{signing_input}.{signature}"), exp * 1000))
}

pub fn verify_access_token(config: &Config, token: &str) -> ApiResult<AuthUser> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(unauthorized("malformed token", "TOKEN_INVALID"));
    }
    let expected = hmac_b64(&config.jwt_secret, &format!("{}.{}", parts[0], parts[1]));
    if !constant_time_eq(expected.as_bytes(), parts[2].as_bytes()) {
        return Err(unauthorized("bad token signature", "TOKEN_INVALID"));
    }
    let Ok(body) = URL_SAFE_NO_PAD.decode(parts[1]) else {
        return Err(unauthorized("bad token payload", "TOKEN_INVALID"));
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Err(unauthorized("bad token payload", "TOKEN_INVALID"));
    };
    if payload.get("type").and_then(|v| v.as_str()) != Some("access") {
        return Err(unauthorized("wrong token type", "TOKEN_INVALID"));
    }
    let exp = payload.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
    if exp * 1000 <= Config::now_ms() {
        return Err(unauthorized("token expired", "TOKEN_EXPIRED"));
    }
    Ok(AuthUser {
        id: payload.get("sub").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        role: payload
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("member")
            .to_string(),
        ver: payload.get("ver").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

pub struct RefreshToken {
    pub token: String,
    pub hash: String,
    pub id: String,
}

pub fn new_refresh_token() -> RefreshToken {
    let token = format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    let hash = hmac_b64(REFRESH_PEPPER, &token);
    RefreshToken {
        token,
        hash,
        id: Uuid::new_v4().to_string(),
    }
}

pub fn hash_refresh_token(token: &str) -> String {
    hmac_b64(REFRESH_PEPPER, token)
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let rest = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then(|| token.to_string())
}

pub fn auth(headers: &HeaderMap, state: &AppState) -> ApiResult<AuthUser> {
    let token = bearer(headers)
        .ok_or_else(|| unauthorized("missing bearer token", "NO_TOKEN"))?;
    let user = verify_access_token(&state.config, &token)?;
    let row = state
        .db
        .one(
            "SELECT id, role, disabled, auth_version FROM users WHERE id = ?1",
            [&user.id],
        )?
        .ok_or_else(|| unauthorized("account no longer exists", "TOKEN_INVALID"))?;
    if crate::db::i(&row, "disabled") != 0 {
        return Err(unauthorized("account disabled", "ACCOUNT_DISABLED"));
    }
    let ver = crate::db::i(&row, "auth_version");
    if ver != user.ver {
        return Err(unauthorized("session revoked", "TOKEN_INVALID"));
    }
    Ok(AuthUser {
        id: user.id,
        role: crate::db::s(&row, "role"),
        ver,
    })
}


pub fn require_admin(user: &AuthUser) -> ApiResult<()> {
    if user.role != "admin" {
        return Err(crate::error::forbidden("admin required", "ADMIN_REQUIRED"));
    }
    Ok(())
}
