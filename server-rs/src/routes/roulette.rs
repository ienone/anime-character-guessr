use axum::{Json, extract::State, response::IntoResponse};
use serde_json::json;
use std::sync::Arc;

use crate::db::{self, DbPools};

/// GET /api/roulette
///
/// Returns 10 random characters for the avatar roulette.
/// Roulette previews are speculative, so medium images can use the upstream
/// source URL directly while the selected avatar still persists the grid proxy.
pub async fn roulette(State(pools): State<Arc<DbPools>>) -> impl IntoResponse {
    // Sample from existing image sources in app.sqlite so we don't return dead avatars.
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT character_id, source, image_medium, image_grid
             FROM character_image_sources
             WHERE (image_medium != '' OR image_grid != '')
             ORDER BY RANDOM()
             LIMIT 10",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
            ))
        })?;
        let mut out: Vec<(i64, String, String, String)> = Vec::new();
        for v in rows.flatten() {
            out.push(v);
        }
        Ok(out)
    })
    .await;

    let list = match result {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({ "error": e.to_string() })).into_response();
        }
    };

    // Soft fallback: if app.sqlite doesn't have enough entries yet, we still return what we have.
    // Client can handle fewer, but ideally migrate-app should fill >= 10.
    let selected = list
        .into_iter()
        .map(|(id, source, image_medium, image_grid)| {
            let preview = if !image_medium.trim().is_empty() {
                image_medium
            } else if !image_grid.trim().is_empty() {
                image_grid
            } else {
                format!("/img/{}.webp", id)
            };
            json!({
                "id": id,
                // No more JSON tiers; keep client styling stable with a neutral tier.
                "tier": if source == "json" || source == "jsonl" { "A" } else { "B" },
                "image_medium": preview,
                "image_grid": format!("/img/{}.webp", id),
            })
        })
        .collect::<Vec<_>>();

    Json(selected).into_response()
}
