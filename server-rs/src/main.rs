use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::{Router, routing::get};
use socketioxide::SocketIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;

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
    tracing_subscriber::fmt::init();
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

    // CORS — must allow the dev/prod client origin(s) for both REST and Socket.IO.
    let cors = build_cors_layer(&config.client_url);

    // Build the Axum application
    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/metrics", get(middleware::metrics::metrics_handler))
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
