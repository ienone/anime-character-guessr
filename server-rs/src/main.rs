use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::{Router, routing::get};
use socketioxide::SocketIo;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;
use tracing_subscriber::fmt::MakeWriter;

mod config;
mod db;
mod middleware;
mod routes;
mod search_index;
mod socket;
mod utils;

use socket::state::ServerState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_non_blocking_tracing();
    std::panic::set_hook(Box::new(|panic_info| {
        tracing::error!(%panic_info, "panic");
    }));

    let config = config::load_config()?;
    info!("Starting Anime Character Guessr Server...");

    utils::ensure_directories(&config.image_cache_dir)?;

    let db_pools = db::init_pools(&config).await?;

    // Shared room state — accessible by both socket handlers and HTTP routes
    let server_state = Arc::new(ServerState::default());

    // Initialize Socket.IO
    let (layer, io) = SocketIo::builder()
        .with_state(Arc::clone(&db_pools))
        .build_layer();

    // Register socket.io event handlers (pass shared state)
    socket::register_handlers(io.clone(), Arc::clone(&server_state), Arc::clone(&db_pools));

    // Graceful shutdown: notify clients before the process exits.
    // Best-effort: emit on Ctrl+C (portable across platforms).
    {
        let io_shutdown = io.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = io_shutdown
                .emit(
                    "serverShutdown",
                    &serde_json::json!({
                        "message": "服务器已关闭，这可能是更新导致的重启或出现了Bug"
                    }),
                )
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });
    }

    // Spawn room auto-cleanup background task (replaces autoClean.js)
    utils::start_room_cleanup(Arc::clone(&server_state), io.clone());

    // Runtime heartbeat and event-loop lag detector for production freezes.
    utils::start_runtime_watchdog(Arc::clone(&server_state));

    // Spawn image cache cleanup background task
    utils::start_image_cache_cleanup(Arc::clone(&db_pools));
    routes::start_image_diagnostics_logger();

    // CORS — must allow the dev/prod client origin(s) for both REST and Socket.IO.
    let cors = build_cors_layer(&config.client_url);

    // Build the Axum application
    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        // Room management routes
        .merge(routes::room_routes(Arc::clone(&server_state), io.clone()))
        // REST API routes (game + leaderboard + stats)
        .nest("/api", routes::api_routes(Arc::clone(&db_pools)))
        // Image API
        .nest("/img", routes::image_routes(Arc::clone(&db_pools)))
        // Attach socket.io layer
        .layer(layer)
        // CORS (must wrap Socket.IO too)
        .layer(cors)
        // Request metrics (outermost — measures full pipeline)
        .layer(axum::middleware::from_fn(
            middleware::metrics::track_metrics,
        ));

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Listening on {}", addr);

    let listener = TcpListener::bind(&addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

#[derive(Clone)]
struct NonBlockingLogWriter {
    sender: SyncSender<Vec<u8>>,
}

struct NonBlockingLogLineWriter {
    sender: SyncSender<Vec<u8>>,
    buffer: Vec<u8>,
}

impl Write for NonBlockingLogLineWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let bytes = std::mem::take(&mut self.buffer);
            let _ = self.sender.try_send(bytes);
        }
        Ok(())
    }
}

impl Drop for NonBlockingLogLineWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl<'a> MakeWriter<'a> for NonBlockingLogWriter {
    type Writer = NonBlockingLogLineWriter;

    fn make_writer(&'a self) -> Self::Writer {
        NonBlockingLogLineWriter {
            sender: self.sender.clone(),
            buffer: Vec::with_capacity(512),
        }
    }
}

fn init_non_blocking_tracing() {
    let (sender, receiver) = sync_channel::<Vec<u8>>(8192);
    let _ = thread::Builder::new()
        .name("nonblocking-log-writer".to_string())
        .spawn(move || {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            while let Ok(bytes) = receiver.recv() {
                let _ = stdout.write_all(&bytes);
                let _ = stdout.flush();
            }
        });

    tracing_subscriber::fmt()
        .with_writer(NonBlockingLogWriter { sender })
        .init();
}

/// Build a CORS layer from a comma-separated origins list.
/// `*` or empty means "allow any" for development.
fn build_cors_layer(client_url: &str) -> CorsLayer {
    let trimmed = client_url.trim();

    let base = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::PATCH,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            header::ORIGIN,
            HeaderName::from_static("x-admin-token"),
        ])
        .allow_credentials(true);

    if trimmed.is_empty() || trimmed == "*" {
        // `allow_credentials(true)` is incompatible with `Any`; mirror the
        // request origin instead so browser dev usage still works.
        return base.allow_origin(AllowOrigin::mirror_request());
    }

    let origins: Vec<HeaderValue> = trimmed
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| HeaderValue::from_str(s).ok())
        .collect();

    if origins.is_empty() {
        base.allow_origin(AllowOrigin::mirror_request())
    } else {
        info!("CORS allowed origins: {:?}", origins);
        base.allow_origin(AllowOrigin::list(origins))
    }
}
