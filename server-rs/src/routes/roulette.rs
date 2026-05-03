use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;
use std::sync::Arc;
use rand::prelude::IndexedRandom;
use crate::db::DbPools;

/// GET /api/roulette
/// Returns 10 random characters with image URLs from archive.sqlite.
/// Mirrors Node.js /roulette endpoint used for the character roulette animation.
pub async fn roulette(State(pools): State<Arc<DbPools>>) -> impl IntoResponse {
    if pools.character_image_index.list.len() < 10 {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Not enough character images" })),
        ).into_response();
    }

    // Same behavior as Node: sample 10 unique entries; pick one random URL from each list.
    let mut rng = rand::rng();
    let selected = pools
        .character_image_index
        .list
        .sample(&mut rng, 10)
        .map(|c| {
            json!({
                "id": c.id,
                "tier": c.tier,
                // Serve through our image proxy so the client never needs to hit external URLs.
                "image_medium": format!("/img/{}.webp", c.id),
                "image_grid": format!("/img/{}.webp", c.id),
            })
        })
        .collect::<Vec<_>>();

    Json(selected).into_response()
}
