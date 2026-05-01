use serde_json::Value;
use socketioxide::extract::{Data, SocketRef, State};
use socketioxide::SocketIo;
use std::sync::Arc;
use tracing::info;
use crate::db::DbPools;

pub fn register_handlers(io: SocketIo) {
    // We attach the DB pools as state for the socket layer
    io.ns("/", move |socket: SocketRef, State(pools): State<Arc<DbPools>>| async move {
        // Handle connection
        info!("Client connected: {} with pools", socket.id);
        let _ = pools; // Just to avoid unused warning for now
        
        socket.on("join_room", |socket: SocketRef, Data::<Value>(data), State(pools): State<Arc<DbPools>>| async move {
            info!("User {} joining room {:?} (pools available: {})", socket.id, data, Arc::strong_count(&pools));
            // Handle join room logic...
        });

        socket.on("submit_guess", |socket: SocketRef, Data::<Value>(data), State(_pools): State<Arc<DbPools>>| async move {
            info!("User {} submitted guess {:?}", socket.id, data);
            // Handle guess logic...
        });

        socket.on("chat", |socket: SocketRef, Data::<Value>(data), State(_pools): State<Arc<DbPools>>| async move {
            info!("User {} chat {:?}", socket.id, data);
            // Handle chat logic...
        });

        socket.on_disconnect(|socket: SocketRef| async move {
            info!("Client disconnected: {}", socket.id);
            // Cleanup player state
        });
    });
}
