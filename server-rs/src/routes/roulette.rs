use axum::{Json, extract::State, response::IntoResponse};
use serde_json::json;
use std::sync::Arc;

use crate::db::{self, DbPools};

/// GET /api/roulette
///
/// Returns 10 random characters for the avatar roulette.
/// We intentionally serve images through our `/img/:id.webp` proxy.
pub async fn roulette(State(pools): State<Arc<DbPools>>) -> impl IntoResponse {
    // Sample from existing image sources in app.sqlite so we don't return dead avatars.
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT character_id, source
             FROM character_image_sources
             WHERE (image_medium != '' OR image_grid != '')
             ORDER BY RANDOM()
             LIMIT 10",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
            ))
        })?;
        let mut out: Vec<(i64, String)> = Vec::new();
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
        .map(|(id, source)| {
            json!({
                "id": id,
                // No more JSON tiers; keep client styling stable with a neutral tier.
                "tier": if source == "json" || source == "jsonl" { "A" } else { "B" },
                "image_medium": format!("/img/{}.webp", id),
                "image_grid": format!("/img/{}.webp", id),
            })
        })
        .collect::<Vec<_>>();

    Json(selected).into_response()
}
