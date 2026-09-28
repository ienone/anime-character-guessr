use axum::{
    Json,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
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
    peer_addr: Option<SocketAddr>,
    scope: &str,
    max_per_minute: usize,
) -> Result<(), GuardRejection> {
    let client = client_key(headers, peer_addr);
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
            "too many requests, please retry later",
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

fn trust_forwarded_headers() -> bool {
    std::env::var("TRUST_PROXY_HEADERS")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes")
        })
        .unwrap_or(false)
}

fn trust_cloudflare_headers() -> bool {
    std::env::var("TRUST_CLOUDFLARE_HEADERS")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes")
        })
        .unwrap_or(false)
}

fn trust_x_forwarded_for() -> bool {
    std::env::var("TRUST_X_FORWARDED_FOR")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes")
        })
        .unwrap_or(false)
}

fn header_value_client_key(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|raw| normalize_client_key(raw, "proxy"))
        .filter(|value| value != "proxy:unknown")
}

fn header_client_key(headers: &HeaderMap) -> Option<String> {
    if trust_cloudflare_headers()
        && let Some(client) = header_value_client_key(headers, "cf-connecting-ip")
    {
        return Some(client);
    }

    if let Some(client) = header_value_client_key(headers, "x-real-ip") {
        return Some(client);
    }

    if trust_x_forwarded_for() {
        return header_value_client_key(headers, "x-forwarded-for");
    }

    None
}

fn normalize_client_key(raw: &str, prefix: &str) -> String {
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
        format!("{prefix}:unknown")
    } else {
        format!("{prefix}:{normalized}")
    }
}

fn client_key(headers: &HeaderMap, peer_addr: Option<SocketAddr>) -> String {
    if trust_forwarded_headers()
        && let Some(forwarded) = header_client_key(headers)
    {
        return forwarded;
    }

    if let Some(peer) = peer_addr {
        return normalize_client_key(&peer.ip().to_string(), "peer");
    }

    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| normalize_client_key(value, "ua"))
        .unwrap_or_else(|| "unknown".to_string())
}
