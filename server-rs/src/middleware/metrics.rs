//! Request metrics tower middleware.
//!
//! Logs every HTTP request with: method, path, status, duration_ms.
//! Slow requests (>500ms) are emitted at WARN level.

use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use std::time::Instant;
use tracing::{debug, info, warn};

/// Axum middleware: logs latency and status for every request.
pub async fn track_metrics(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    if path == "/health" {
        return next.run(req).await;
    }

    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;
    let status = response.status();

    if elapsed_ms > 500 || status.is_server_error() {
        warn!(
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = elapsed_ms,
            "SLOW request"
        );
    } else if status.is_client_error() {
        info!(
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = elapsed_ms,
            "client error request"
        );
    } else {
        debug!(
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = elapsed_ms,
        );
    }

    response
}
