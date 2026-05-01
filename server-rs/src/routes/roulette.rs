use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::sync::Arc;
use rand::seq::SliceRandom;
use crate::db::{self, DbPools};

/// GET /api/roulette
/// Returns 10 random characters with image URLs from archive.sqlite.
/// Mirrors Node.js /roulette endpoint used for the character roulette animation.
pub async fn roulette(State(pools): State<Arc<DbPools>>) -> impl IntoResponse {
    if pools.character_ids.len() < 10 {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Not enough characters loaded" })),
        ).into_response();
    }

    // Sampling done synchronously before any .await — rng does not cross await boundary
    let sample: Vec<i64> = {
        let mut ids = pools.character_ids.clone();
        let mut rng = rand::rng();
        ids.shuffle(&mut rng);
        ids.into_iter().take(10).collect()
    }; // rng dropped here, before .await

    let result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let mut out: Vec<Value> = Vec::with_capacity(10);
        for id in &sample {
            let raw: Option<String> = conn.query_row(
                "SELECT raw_json FROM characters WHERE id = ?1",
                [id],
                |row| row.get(0),
            ).ok();

            let entry = raw
                .and_then(|r| serde_json::from_str::<Value>(&r).ok())
                .map(|char_val| {
                    let images = char_val.get("images");
                    let medium = images
                        .and_then(|i| i.get("medium").or_else(|| i.get("large")))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let grid = images
                        .and_then(|i| i.get("grid").or_else(|| i.get("small")))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    json!({
                        "id": id,
                        "image_medium": medium,
                        "image_grid": grid,
                    })
                })
                .unwrap_or_else(|| json!({ "id": id }));

            out.push(entry);
        }
        Ok(out)
    }).await;

    match result {
        Ok(chars) => Json(chars).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ).into_response(),
    }
}
