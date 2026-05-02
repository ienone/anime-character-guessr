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


pub mod game;
pub mod leaderboard;
pub mod rooms;
pub mod roulette;
pub mod stats;
pub mod tags;

// ─── Route Builders ───────────────────────────────────────────────────────────

/// Room management routes mounted at "/"
pub use rooms::room_routes;

/// /api/* — game logic, leaderboard, stats
pub fn api_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        // Single-player game (archive.sqlite backed)
        .route("/game/random", post(get_random_character))
        .route("/game/character", post(get_character_by_id))
        // BGM proxy (for index mode / search — BGM calls go through server)
        .route("/bgm/index-info", get(bgm_proxy_index_info))
        .route("/bgm/index-subjects", get(bgm_proxy_index_subjects))
        .route("/bgm/search", post(bgm_proxy_search))
        .route("/bgm/character", get(bgm_proxy_character))
        // Roulette
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
        .route("/character-usage/{id}", get(stats::character_usage))
        .route("/subject-added", post(stats::subject_added))
        // Tags & Feedback
        .route("/character-tags", post(tags::update_character_tags))
        .route("/character-tags/{id}", get(tags::get_character_tags))
        .route("/game-character-tags", post(tags::update_game_character_tags))
        .route("/game-character-tags/{subject_id}", get(tags::get_game_character_tags))
        .route("/propose-tags", post(tags::propose_tags))
        .route("/feedback-tags", post(tags::feedback_tags))
        .route("/bug-feedback", post(tags::bug_feedback))
        .with_state(pools)
}

/// /img/* — character image proxy with local WebP cache
pub fn image_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        .route("/{id}", get(get_character_image))
        .with_state(pools)
}

/// POST /api/game/random
/// Pick a random character based on game settings — fully in-memory, zero DB queries.
async fn get_random_character(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let settings = game::GameSettings::from_json(&body);
    let pools_clone = Arc::clone(&pools);

    // spawn_blocking to avoid blocking the async executor during tag aggregation
    let result = tokio::task::spawn_blocking(move || {
        game::random_character(&pools_clone, &settings)
    }).await;

    match result {
        Ok(Ok((_, payload))) => Json(payload).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/game/character  { id: number, settings: {...} }
/// Return full gameplay payload for a specific character — fully in-memory.
async fn get_character_by_id(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let char_id = match body.get("id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "id required" }))).into_response(),
    };
    let settings = game::GameSettings::from_json(body.get("settings").unwrap_or(&json!({})));
    let pools_clone = Arc::clone(&pools);

    let result = tokio::task::spawn_blocking(move || {
        game::character_by_id(&pools_clone, char_id, &settings)
    }).await;

    match result {
        Ok(Ok(payload)) => Json(payload).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

// ─── BGM Proxy Routes (for Index/Search mode) ─────────────────────────────────
// These proxy calls to api.bgm.tv with server-side caching to mitigate
// connectivity issues from the client.

use axum::extract::Query as AxumQuery;
use std::collections::HashMap;

/// GET /api/bgm/index-info?indexId=xxx
async fn bgm_proxy_index_info(
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    let index_id = match q.get("indexId") {
        Some(id) if !id.is_empty() => id.clone(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "indexId required" }))).into_response(),
    };
    let url = format!("https://api.bgm.tv/v0/indices/{}", index_id);
    match bgm_get(&url).await {
        Ok(data) => Json(json!({ "title": data.get("title"), "total": data.get("total") })).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// GET /api/bgm/index-subjects?indexId=xxx&offset=0&limit=10
async fn bgm_proxy_index_subjects(
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    let index_id = match q.get("indexId") {
        Some(id) if !id.is_empty() => id.clone(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "indexId required" }))).into_response(),
    };
    let offset = q.get("offset").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let limit = q.get("limit").and_then(|s| s.parse::<u64>().ok()).unwrap_or(10).min(50);
    let url = format!(
        "https://api.bgm.tv/v0/indices/{}/subjects?limit={}&offset={}",
        index_id, limit, offset
    );
    match bgm_get(&url).await {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/bgm/search  — forward body to BGM search API
async fn bgm_proxy_search(Json(body): Json<Value>) -> impl IntoResponse {
    let offset = body.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(10).min(50);
    let url = format!(
        "https://api.bgm.tv/v0/search/subjects?limit={}&offset={}",
        limit, offset
    );
    let forward_body = body.get("filter").cloned().map(|filter| json!({ "sort": body.get("sort").cloned().unwrap_or(json!("heat")), "filter": filter })).unwrap_or(body.clone());
    match bgm_post(&url, &forward_body).await {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// GET /api/bgm/character?id=xxx — proxy single character details from BGM
async fn bgm_proxy_character(
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    let id = match q.get("id") {
        Some(id) if !id.is_empty() => id.clone(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "id required" }))).into_response(),
    };
    let url = format!("https://api.bgm.tv/v0/characters/{}", id);
    match bgm_get(&url).await {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn bgm_get(url: &str) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)")
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    Ok(resp.json().await?)
}

async fn bgm_post(url: &str, body: &Value) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)")
        .build()?;
    let resp = client.post(url).json(body).send().await?.error_for_status()?;
    Ok(resp.json().await?)
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
