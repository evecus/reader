use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

const DEFAULT_CHUNK: u64 = 2 * 1024 * 1024;
const MAX_SUFFIX_RANGE: u64 = 8 * 1024 * 1024;

pub struct AssetOpts {
    pub content_type: String,
    pub etag: Option<String>,
    pub last_modified: Option<i64>,
    pub filename: Option<String>,
    pub seekable: bool,
    /// If `If-Modified-Since`-equivalent timestamp is provided by the caller
    /// (mtime of a book file), answer 304 when the payload is not newer.
    pub not_modified_on: Option<i64>,
}

impl Default for AssetOpts {
    fn default() -> Self {
        Self {
            content_type: "application/octet-stream".to_string(),
            etag: None,
            last_modified: None,
            filename: None,
            seekable: false,
            not_modified_on: None,
        }
    }
}

pub enum AssetPayload {
    Memory(Vec<u8>),
    File { path: std::path::PathBuf, size: u64 },
}

struct Range {
    start: u64,
    end: u64,
    suffix: bool,
}

/// Mirrors the Node `parseRange`: single `bytes=start-end`, suffix `bytes=-N`,
/// a default 2MB window for an open-ended range, and 416 for a bad one.
fn parse_range(header: Option<&str>, total: u64) -> Option<Result<Range, ()>> {
    let raw = header?.trim();
    let spec = raw.strip_prefix("bytes=")?;
    let (start_s, end_s) = spec.split_once('-')?;
    if start_s.is_empty() && end_s.is_empty() {
        return Some(Err(()));
    }
    if start_s.is_empty() {
        let suffix: u64 = end_s.parse().ok()?;
        if suffix == 0 {
            return Some(Err(()));
        }
        let start = total.saturating_sub(suffix);
        if total == 0 {
            return Some(Err(()));
        }
        return Some(Ok(Range { start, end: total - 1, suffix: true }));
    }
    let start: u64 = start_s.parse().ok()?;
    let end = if end_s.is_empty() {
        (start + DEFAULT_CHUNK - 1).min(total.saturating_sub(1))
    } else {
        end_s.parse().ok()?
    };
    if start > end || start >= total {
        return Some(Err(()));
    }
    Some(Ok(Range { start, end: end.min(total - 1), suffix: false }))
}

fn http_date(ms: i64) -> String {
    // Format a UTC date from epoch millis without pulling in a date crate.
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        weekday(days),
        day,
        month_name(month),
        year,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn weekday(days: i64) -> &'static str {
    const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    NAMES[(days + 4).rem_euclid(7) as usize]
}

fn month_name(m: i64) -> &'static str {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    NAMES[(m - 1).clamp(0, 11) as usize]
}

fn base_headers(opts: &AssetOpts, builder: &mut axum::http::response::Builder) {
    if let Ok(v) = HeaderValue::from_str(&opts.content_type) {
        builder.headers_mut().map(|h| h.insert(header::CONTENT_TYPE, v));
    }
    builder.headers_mut().map(|h| {
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        )
    });
    if let Some(etag) = &opts.etag {
        if let Ok(v) = HeaderValue::from_str(&format!("\"{etag}\"")) {
            builder.headers_mut().map(|h| h.insert(header::ETAG, v));
        }
    }
    if let Some(ms) = opts.last_modified {
        if let Ok(v) = HeaderValue::from_str(&http_date(ms)) {
            builder.headers_mut().map(|h| h.insert(header::LAST_MODIFIED, v));
        }
    }
    if let Some(name) = &opts.filename {
        if let Ok(v) = HeaderValue::from_str(&format!(
            "inline; filename*=UTF-8''{}",
            percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
        )) {
            builder.headers_mut().map(|h| h.insert(header::CONTENT_DISPOSITION, v));
        }
    }
}

pub async fn send_asset(
    request_headers: &HeaderMap,
    payload: AssetPayload,
    opts: AssetOpts,
) -> Response {
    // 304 wins before any range handling.
    if let (Some(since), Some(lm)) = (opts.not_modified_on, opts.last_modified) {
        if lm <= since {
            let mut builder = Response::builder().status(StatusCode::NOT_MODIFIED);
            base_headers(&opts, &mut builder);
            return builder.body(Body::empty()).unwrap();
        }
    }

    let total = match &payload {
        AssetPayload::Memory(data) => data.len() as u64,
        AssetPayload::File { size, .. } => *size,
    };

    let range = if opts.seekable {
        parse_range(
            request_headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
            total,
        )
    } else {
        None
    };

    if let Some(Err(())) = range {
        let mut builder = Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE);
        if let Some(h) = builder.headers_mut() {
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{total}")) {
                h.insert(header::CONTENT_RANGE, v);
            }
        }
        return builder.body(Body::empty()).unwrap();
    }

    let (start, length, partial) = match range {
        Some(Ok(Range { start, end, suffix })) => {
            let length = end - start + 1;
            // A huge suffix range cannot be produced by skipping a forward-only
            // stream; answering `none` with the full body is the honest fallback.
            if suffix && length > MAX_SUFFIX_RANGE {
                (0u64, total, false)
            } else {
                (start, length, true)
            }
        }
        _ => (0u64, total, false),
    };

    let body = match build_body(payload, start, length).await {
        Ok(body) => body,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error":{"code":"ASSET_READ","message":e}})),
            )
                .into_response();
        }
    };

    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut builder = Response::builder().status(status);
    base_headers(&opts, &mut builder);
    if let Some(h) = builder.headers_mut() {
        h.insert(
            header::ACCEPT_RANGES,
            HeaderValue::from_static(if opts.seekable { "bytes" } else { "none" }),
        );
        if let Ok(v) = HeaderValue::from_str(&length.to_string()) {
            h.insert(header::CONTENT_LENGTH, v);
        }
        if partial {
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{total}", start + length - 1)) {
                h.insert(header::CONTENT_RANGE, v);
            }
        }
    }
    builder.body(body).unwrap()
}

async fn build_body(payload: AssetPayload, start: u64, length: u64) -> Result<Body, String> {
    match payload {
        AssetPayload::Memory(data) => {
            let s = (start as usize).min(data.len());
            let e = (s + length as usize).min(data.len());
            Ok(Body::from(data[s..e].to_vec()))
        }
        AssetPayload::File { path, .. } => {
            let mut file = tokio::fs::File::open(&path)
                .await
                .map_err(|e| e.to_string())?;
            if start > 0 {
                file.seek(std::io::SeekFrom::Start(start))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let limited = file.take(length);
            Ok(Body::from_stream(ReaderStream::new(limited)))
        }
    }
}
