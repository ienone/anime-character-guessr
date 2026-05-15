use axum::{
    Json,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PUBLIC_WRITE_WINDOW: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_KEYS: usize = 4096;

lazy_static::lazy_static! {
    static ref PUBLIC_WRITE_LIMITS: Mutex<HashMap<String, VecDeque<Instant>>> =
        Mutex::new(HashMap::new());
}

pub fn reject(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

pub struct GuardRejection {
    status: StatusCode,
    message: String,
}

impl IntoResponse for GuardRejection {
    fn into_response(self) -> Response {
        reject(self.status, self.message)
    }
}

fn guard_reject(status: StatusCode, message: impl Into<String>) -> GuardRejection {
    GuardRejection {
        status,
        message: message.into(),
    }
}

pub fn enforce_public_write_limit(
    headers: &HeaderMap,
    scope: &str,
    max_per_minute: usize,
) -> Result<(), GuardRejection> {
    let client = client_key(headers);
    let key = format!("{scope}:{client}");
    let now = Instant::now();
    let mut limits = PUBLIC_WRITE_LIMITS.lock().map_err(|_| {
        guard_reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rate limiter unavailable",
        )
    })?;

    if limits.len() > MAX_RATE_LIMIT_KEYS {
        limits.retain(|_, hits| {
            hits.back()
                .map(|last_seen| now.duration_since(*last_seen) <= PUBLIC_WRITE_WINDOW)
                .unwrap_or(false)
        });
    }

    let hits = limits.entry(key).or_default();
    while hits
        .front()
        .map(|timestamp| now.duration_since(*timestamp) > PUBLIC_WRITE_WINDOW)
        .unwrap_or(false)
    {
        hits.pop_front();
    }

    if hits.len() >= max_per_minute {
        return Err(guard_reject(
            StatusCode::TOO_MANY_REQUESTS,
            "too many write requests, please retry later",
        ));
    }

    hits.push_back(now);
    Ok(())
}

pub fn validate_chars(value: &str, field: &str, max_chars: usize) -> Result<(), GuardRejection> {
    if value.chars().count() > max_chars {
        return Err(guard_reject(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{field} is too long"),
        ));
    }
    Ok(())
}

pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            return out;
        }
        out.push(ch);
    }
    value.to_string()
}

pub fn validate_json_object(
    value: &Value,
    field: &str,
    max_keys: usize,
    max_key_chars: usize,
) -> Result<(), GuardRejection> {
    let Some(map) = value.as_object() else {
        return Err(guard_reject(
            StatusCode::BAD_REQUEST,
            format!("{field} must be an object"),
        ));
    };

    if map.len() > max_keys {
        return Err(guard_reject(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{field} contains too many keys"),
        ));
    }

    for key in map.keys() {
        validate_chars(key, field, max_key_chars)?;
    }

    Ok(())
}

fn client_key(headers: &HeaderMap) -> String {
    let raw = headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-real-ip"))
        .or_else(|| headers.get("x-forwarded-for"))
        .or_else(|| headers.get(header::USER_AGENT))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");

    let normalized: String = raw
        .split(',')
        .next()
        .unwrap_or(raw)
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | ':' | '-' | '_'))
        .take(80)
        .collect();

    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized
    }
}
