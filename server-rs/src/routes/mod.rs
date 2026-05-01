use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use axum::http::{StatusCode, header};
use serde_json::{json, Value};
use std::sync::Arc;
use crate::db::{self, DbPools};
use crate::utils;
use rand::seq::IndexedRandom;

pub mod leaderboard;
pub mod rooms;
pub mod roulette;
pub mod stats;

// ─── Route Builders ───────────────────────────────────────────────────────────

/// Room management routes mounted at "/"
pub use rooms::room_routes;

/// /api/* — game logic, leaderboard, stats
pub fn api_routes(pools: Arc<DbPools>) -> Router {
    // State for stats handlers needs both DbPools and ServerState
    Router::new()
        // Core game
        .route("/game/random", post(get_random_character))
        // Roulette (character picker animation)
        .route("/roulette", get(roulette::roulette))
        // Redeem codes
        .route("/redeem", get(stats::redeem))
        // Player leaderboard
        .route("/leaderboard", get(leaderboard::get_leaderboard))
        .route("/leaderboard/submit", post(leaderboard::submit_score))
        // Character leaderboards
        .route("/leaderboard/characters", get(stats::leaderboard_characters))
        .route("/leaderboard/guesses", get(stats::leaderboard_guesses))
        .route("/leaderboard/weekly", get(stats::leaderboard_weekly))
        // Stats write endpoints
        .route("/answer-character-count", post(stats::answer_character_count))
        .route("/guess-character-count", post(stats::guess_character_count))
        .route("/character-usage/:id", get(stats::character_usage))
        .route("/subject-added", post(stats::subject_added))
        .with_state(pools)
}

/// /img/* — character image proxy with local WebP cache
pub fn image_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        .route("/{id}", get(get_character_image))
        .with_state(pools)
}

// ─── Route Handlers ───────────────────────────────────────────────────────────

async fn get_random_character(
    State(pools): State<Arc<DbPools>>,
    Json(_settings): Json<Value>,
) -> impl IntoResponse {
    let char_id = {
        let mut rng = rand::rng();
        match pools.character_ids.choose(&mut rng) {
            Some(&id) => id,
            None => return Json(json!({ "error": "No characters loaded" })).into_response(),
        }
    };

    let db_result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let char_raw: String = conn
            .query_row(
                "SELECT raw_json FROM characters WHERE id = ?1",
                [char_id],
                |row| row.get(0),
            )
            .unwrap_or_default();

        let mut sub_stmt = conn.prepare(
            "SELECT s.raw_json
             FROM subject_characters sc
             JOIN subjects s ON sc.subject_id = s.id
             WHERE sc.character_id = ?1
             ORDER BY s.collects DESC",
        )?;

        let mut subjects_raw = Vec::new();
        let mut rows = sub_stmt.query([char_id])?;
        while let Some(row) = rows.next()? {
            let sub_raw: String = row.get(0)?;
            subjects_raw.push(sub_raw);
        }
        Ok((char_raw, subjects_raw))
    })
    .await;

    let (char_raw, subjects_raw) = match db_result {
        Ok(v) => v,
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "db_error", "message": e.to_string() })),
        ).into_response(),
    };

    let mut character: Value = serde_json::from_str(&char_raw).unwrap_or(json!({}));
    let subjects = subjects_raw
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .collect::<Vec<_>>();

    character["id"] = json!(char_id);
    character["appearances"] = json!(subjects);

    Json(character).into_response()
}

async fn get_character_image(
    State(pools): State<Arc<DbPools>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let id: i64 = id_str.split('.').next().unwrap_or("0").parse().unwrap_or(0);

    if id == 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // 1. Check local cache
    let local_path: Option<String> = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .ok())
    })
    .await
    {
        Ok(v) => v,
        Err(_) => None,
    };

    if let Some(path) = local_path {
        if let Ok(content) = tokio::fs::read(&path).await {
            let mut headers = header::HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "image/webp".parse().unwrap());
            headers.insert(header::CACHE_CONTROL, "public, max-age=31536000".parse().unwrap());
            return (headers, content).into_response();
        }
    }

    // 2. Cache miss — fetch original URL from archive.sqlite
    let original_url: Option<String> = match db::with_archive_db(Arc::clone(&pools), move |conn| {
        let raw: Option<String> = conn
            .query_row(
                "SELECT raw_json FROM characters WHERE id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .ok();

        let url = raw.and_then(|raw| {
            serde_json::from_str::<Value>(&raw).ok().and_then(|val| {
                val.get("images").and_then(|imgs| {
                    imgs.get("large")
                        .or_else(|| imgs.get("medium"))
                        .or_else(|| imgs.get("grid"))
                        .and_then(|u| u.as_str().map(|s| s.to_string()))
                })
            })
        });
        Ok(url)
    })
    .await
    {
        Ok(v) => v,
        Err(_) => None,
    };

    let target_url = original_url.unwrap_or_else(|| "https://lain.bgm.tv/pic/user/l/icon.jpg".to_string());

    // 3. Kick off background download+transcode
    let pools_clone = Arc::clone(&pools);
    let target_url_clone = target_url.clone();
    tokio::spawn(async move {
        utils::download_and_cache_image(id, target_url_clone, pools_clone).await;
    });

    // 4. Redirect immediately
    Redirect::temporary(&target_url).into_response()
}
