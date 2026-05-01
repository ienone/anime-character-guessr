use axum::{routing::get, Router};
use socketioxide::SocketIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber;

mod config;
mod db;
mod routes;
mod socket;
mod utils;

use socket::state::ServerState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = config::load_config();
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
    socket::register_handlers(io.clone(), Arc::clone(&server_state));

    // Spawn room auto-cleanup background task (replaces autoClean.js)
    utils::start_room_cleanup(Arc::clone(&server_state), io.clone());

    // Build the Axum application
    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        // Room management routes
        .nest("/", routes::room_routes(Arc::clone(&server_state), io.clone()))
        // REST API routes (game + leaderboard + stats)
        .nest("/api", routes::api_routes(Arc::clone(&db_pools)))
        // Image API
        .nest("/img", routes::image_routes(Arc::clone(&db_pools)))
        // Attach socket.io layer
        .layer(layer);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Listening on {}", addr);

    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
