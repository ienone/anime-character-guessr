use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use crate::db::DbPools;

pub fn api_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        .route("/game/random", get(get_random_character))
        .with_state(pools)
}

pub fn image_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        .route("/{id}", get(get_character_image))
        .with_state(pools)
}

// --- Route Handlers ---

async fn get_random_character(State(_pools): State<Arc<DbPools>>) -> impl IntoResponse {
    // Phase 2 implementation: Fetch a random character
    // 1. Read preset_pools from memory (to be implemented)
    // 2. Select a random ID
    // 3. Query archive.sqlite
    
    // Stub implementation
    Json(json!({
        "status": "success",
        "message": "Not implemented yet",
        "data": {}
    }))
}

async fn get_character_image(
    State(_pools): State<Arc<DbPools>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Phase 2 implementation: Image caching and serving
    // 1. Check app.sqlite image_cache table for local path
    // 2. If hit: Serve local file using tower-http fs
    // 3. If miss: Fetch from BGM, save to cache (async task), redirect or return proxy response

    // Stub implementation
    format!("Image for {} not implemented yet", id)
}
