use serde_json::Value;
use socketioxide::extract::{Data, SocketRef, State};
use socketioxide::SocketIo;
use std::sync::Arc;
use tracing::info;
use crate::db::DbPools;

pub fn register_handlers(io: SocketIo, pools: Arc<DbPools>) {
    // We attach the DB pools as state for the socket layer
    io.ns("/", move |socket: SocketRef| {
        // Handle connection
        info!("Client connected: {}", socket.id);
        
        socket.on("join_room", |socket: SocketRef, Data::<Value>(data)| {
            info!("User {} joining room {:?}", socket.id, data);
            // Handle join room logic...
        });

        socket.on("submit_guess", |socket: SocketRef, Data::<Value>(data)| {
            info!("User {} submitted guess {:?}", socket.id, data);
            // Handle guess logic...
        });

        socket.on("chat", |socket: SocketRef, Data::<Value>(data)| {
            info!("User {} chat {:?}", socket.id, data);
            // Handle chat logic...
        });

        socket.on_disconnect(|socket: SocketRef| async move {
            info!("Client disconnected: {}", socket.id);
            // Cleanup player state
        });
    });
}
