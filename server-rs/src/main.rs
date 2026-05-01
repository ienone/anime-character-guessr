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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing for logging
    tracing_subscriber::fmt::init();

    // Ensure local data directories exist
    utils::ensure_directories()?;

    // Load configuration
    let config = config::load_config();
    info!("Starting Anime Character Guessr Server...");

    // Initialize database connections
    let db_pools = db::init_pools(&config).await?;

    // Initialize Socket.IO
    let (layer, io) = SocketIo::new_layer();
    
    // Register socket.io event handlers
    socket::register_handlers(io.clone(), Arc::clone(&db_pools));

    // Build the Axum application
    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        // Mount REST API routes
        .nest("/api", routes::api_routes(Arc::clone(&db_pools)))
        // Mount Image API
        .nest("/img", routes::image_routes(Arc::clone(&db_pools)))
        // Attach socket.io layer
        .layer(layer);

    // Start the server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!("Listening on {}", addr);
    
    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
