//! Request metrics tower middleware.
//!
//! Logs every HTTP request with: method, path, status, duration_ms.
//! Slow requests (>500ms) are emitted at WARN level.
//! Exposes Prometheus-format metrics at `GET /metrics`.

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
use sysinfo::{Pid, System};
use tracing::{info, warn};

// ─── Global atomic counters ───────────────────────────────────────────────────

static REQUESTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static REQUESTS_SLOW: AtomicU64 = AtomicU64::new(0); // >500ms
static REQUESTS_ERROR: AtomicU64 = AtomicU64::new(0); // 5xx

// Histogram buckets: cumulative counts of requests completing within N ms
static BUCKET_10MS: AtomicU64 = AtomicU64::new(0);
static BUCKET_50MS: AtomicU64 = AtomicU64::new(0);
static BUCKET_100MS: AtomicU64 = AtomicU64::new(0);
static BUCKET_500MS: AtomicU64 = AtomicU64::new(0);
static BUCKET_1000MS: AtomicU64 = AtomicU64::new(0);
static BUCKET_INF: AtomicU64 = AtomicU64::new(0);
static LATENCY_SUM_MS: AtomicU64 = AtomicU64::new(0);

// ─── Process metrics (sampled on /metrics) ───────────────────────────────────

static PROCESS_RSS_KB: AtomicU64 = AtomicU64::new(0);
static CPU_USAGE_PERCENT_X100: AtomicU64 = AtomicU64::new(0);

// ─── Middleware function ──────────────────────────────────────────────────────

/// Axum middleware: records latency and status for every request.
pub async fn track_metrics(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();

    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;
    let status = response.status();

    REQUESTS_TOTAL.fetch_add(1, Ordering::Relaxed);
    LATENCY_SUM_MS.fetch_add(elapsed_ms, Ordering::Relaxed);

    // Histogram buckets
    match elapsed_ms {
        0..=10 => BUCKET_10MS.fetch_add(1, Ordering::Relaxed),
        11..=50 => BUCKET_50MS.fetch_add(1, Ordering::Relaxed),
        51..=100 => BUCKET_100MS.fetch_add(1, Ordering::Relaxed),
        101..=500 => BUCKET_500MS.fetch_add(1, Ordering::Relaxed),
        501..=1000 => BUCKET_1000MS.fetch_add(1, Ordering::Relaxed),
        _ => BUCKET_INF.fetch_add(1, Ordering::Relaxed),
    };

    if status.is_server_error() {
        REQUESTS_ERROR.fetch_add(1, Ordering::Relaxed);
    }

    if elapsed_ms > 500 {
        REQUESTS_SLOW.fetch_add(1, Ordering::Relaxed);
        warn!(
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = elapsed_ms,
            "SLOW request"
        );
    } else {
        info!(
            method = %method,
            path = %path,
            status = status.as_u16(),
            duration_ms = elapsed_ms,
        );
    }

    response
}

// ─── /metrics endpoint ────────────────────────────────────────────────────────

/// GET /metrics — Prometheus text format
pub async fn metrics_handler() -> impl IntoResponse {
    let total = REQUESTS_TOTAL.load(Ordering::Relaxed);
    let slow = REQUESTS_SLOW.load(Ordering::Relaxed);
    let errors = REQUESTS_ERROR.load(Ordering::Relaxed);
    let sum_ms = LATENCY_SUM_MS.load(Ordering::Relaxed);
    let avg_ms = if total > 0 { sum_ms / total } else { 0 };

    let b10 = BUCKET_10MS.load(Ordering::Relaxed);
    let b50 = BUCKET_50MS.load(Ordering::Relaxed);
    let b100 = BUCKET_100MS.load(Ordering::Relaxed);
    let b500 = BUCKET_500MS.load(Ordering::Relaxed);
    let b1000 = BUCKET_1000MS.load(Ordering::Relaxed);
    let binf = BUCKET_INF.load(Ordering::Relaxed);

    // Update process metrics snapshot (best effort).
    sample_process_metrics();
    let rss_kb = PROCESS_RSS_KB.load(Ordering::Relaxed);
    let cpu_x100 = CPU_USAGE_PERCENT_X100.load(Ordering::Relaxed);
    let cpu_percent = (cpu_x100 as f64) / 100.0;

    let body = format!(
        "# HELP http_requests_total Total HTTP requests\n\
         # TYPE http_requests_total counter\n\
         http_requests_total {total}\n\
         \n\
         # HELP http_requests_slow Requests taking >500ms\n\
         # TYPE http_requests_slow counter\n\
         http_requests_slow {slow}\n\
         \n\
         # HELP http_requests_errors 5xx responses\n\
         # TYPE http_requests_errors counter\n\
         http_requests_errors {errors}\n\
         \n\
         # HELP http_latency_avg_ms Average response latency (ms)\n\
         # TYPE http_latency_avg_ms gauge\n\
         http_latency_avg_ms {avg_ms}\n\
         \n\
         # HELP http_latency_sum_ms Cumulative latency sum (ms)\n\
         # TYPE http_latency_sum_ms counter\n\
         http_latency_sum_ms {sum_ms}\n\
         \n\
         # HELP http_request_duration_ms_bucket Latency histogram buckets\n\
         # TYPE http_request_duration_ms_bucket histogram\n\
         http_request_duration_ms_bucket{{le=\"10\"}} {b10}\n\
         http_request_duration_ms_bucket{{le=\"50\"}} {b50}\n\
         http_request_duration_ms_bucket{{le=\"100\"}} {b100}\n\
         http_request_duration_ms_bucket{{le=\"500\"}} {b500}\n\
         http_request_duration_ms_bucket{{le=\"1000\"}} {b1000}\n\
         http_request_duration_ms_bucket{{le=\"+Inf\"}} {binf}\n\
         \n\
         # HELP process_rss_kb Resident set size (KB)\n\
         # TYPE process_rss_kb gauge\n\
         process_rss_kb {rss_kb}\n\
         \n\
         # HELP cpu_usage_percent Process CPU usage percent (0-100)\n\
         # TYPE cpu_usage_percent gauge\n\
         cpu_usage_percent {cpu_percent}\n",
        total = total,
        slow = slow,
        errors = errors,
        avg_ms = avg_ms,
        sum_ms = sum_ms,
        b10 = b10 + b50 + b100 + b500 + b1000 + binf,
        b50 = b50 + b100 + b500 + b1000 + binf,
        b100 = b100 + b500 + b1000 + binf,
        b500 = b500 + b1000 + binf,
        b1000 = b1000 + binf,
        binf = binf,
        rss_kb = rss_kb,
        cpu_percent = cpu_percent,
    );

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
}

fn sample_process_metrics() {
    // sysinfo CPU usage is a delta between refreshes; keep a small static System
    // to preserve previous ticks across /metrics calls.
    static SYS: std::sync::OnceLock<std::sync::Mutex<System>> = std::sync::OnceLock::new();
    let sys = SYS.get_or_init(|| std::sync::Mutex::new(System::new()));
    let mut system = sys.lock().unwrap();

    let pid = Pid::from_u32(std::process::id());

    system.refresh_cpu_all();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    if let Some(p) = system.process(pid) {
        let rss_kb = p.memory(); // KB
        PROCESS_RSS_KB.store(rss_kb, Ordering::Relaxed);

        // On some platforms sysinfo may report >100 for multi-core aggregate usage.
        let cpu = p.cpu_usage() as f64;
        let cpu_clamped = cpu.max(0.0).min(100.0);
        let cpu_x100 = (cpu_clamped * 100.0).round() as u64;
        CPU_USAGE_PERCENT_X100.store(cpu_x100, Ordering::Relaxed);
    }
}
