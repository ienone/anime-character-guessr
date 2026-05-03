use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum::http::{StatusCode, header};
use serde_json::{json, Value};
use std::sync::Arc;
use crate::db::{self, DbPools};
use crate::utils;
use dashmap::DashMap;
use tracing::warn;
use std::time::Duration;
use axum::response::Redirect;


pub mod game;
pub mod archive;
pub mod leaderboard;
pub mod rooms;
pub mod roulette;
pub mod stats;
pub mod tags;

// ─── Route Builders ───────────────────────────────────────────────────────────

/// Room management routes mounted at "/"
pub use rooms::room_routes;

// ─── BGM proxy caches ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct CacheEntry {
    expires_at_ms: i64,
    value: Value,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn cache_get_ttl(cache: &DashMap<String, CacheEntry>, key: &str) -> Option<Value> {
    let now = now_ms();
    if let Some(entry) = cache.get(key) {
        if entry.expires_at_ms > now {
            return Some(entry.value.clone());
        }
    }
    cache.remove(key);
    None
}

fn cache_put_ttl(cache: &DashMap<String, CacheEntry>, key: String, ttl_ms: i64, value: Value) {
    cache.insert(key, CacheEntry { expires_at_ms: now_ms() + ttl_ms, value });
}

// stable_hash_json was previously used for BGM search caching; removed after
// migrating search fully to offline archive routes.

lazy_static::lazy_static! {
    static ref INDEX_INFO_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = indexId
    static ref INDEX_SUBJECTS_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = indexId:offset:limit
    // Cache for archive fallbacks (when archive.sqlite doesn't contain the requested item)
    static ref SUBJECT_DETAILS_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = subjectId
    static ref SUBJECT_CHARACTERS_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = subjectId
    static ref CHARACTER_DETAILS_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = characterId
}

/// /api/* — game logic, leaderboard, stats
pub fn api_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        // Single-player game (archive.sqlite backed)
        .route("/game/random", post(get_random_character))
        .route("/game/character", post(get_character_by_id))
        // Image resolve helper (JSON): tells client whether /img is cached yet
        .route("/img/resolve/{id}", get(resolve_character_image))
        // Archive-backed local endpoints (reduce BGM API usage)
        .nest("/archive", archive::archive_routes(Arc::clone(&pools)))
        // BGM proxy (for index mode / search — BGM calls go through server)
        .route("/bgm/index-info", get(bgm_proxy_index_info))
        .route("/bgm/index-subjects", get(bgm_proxy_index_subjects))
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

/// GET /api/img/resolve/:id?waitMs=1200
///
/// JSON helper used by the client Image component:
/// - If cached: returns `{ cached: true, imgUrl: "/img/{id}.webp" }`
/// - If not cached and cannot fetch within wait window: returns HTTP 202 with
///   `{ cached: false, imgUrl, sourceUrl }` so the client can show a placeholder
///   and try loading `sourceUrl` directly.
async fn resolve_character_image(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    if id <= 0 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "id must be positive" }))).into_response();
    }

    let wait_ms = q.get("waitMs")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1200)
        .min(5000);

    // 1) If cached already, answer immediately.
    let cached = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn.query_row(
            "SELECT local_path FROM image_cache WHERE id = ?1",
            [id.to_string()],
            |row| row.get::<_, String>(0),
        ).ok())
    }).await {
        Ok(Some(path)) => tokio::fs::metadata(path).await.is_ok(),
        _ => false,
    };

    let img_url = format!("/img/{}.webp", id);
    if cached {
        return Json(json!({
            "cached": true,
            "imgUrl": img_url,
        })).into_response();
    }

    // 2) Resolve source URL from offline mapping (preferred).
    let source_url = match pools.character_image_index.by_id.get(&id) {
        Some(&idx) => {
            let entry = &pools.character_image_index.list[idx];
            entry
                .image_medium
                .first()
                .or_else(|| entry.image_grid.first())
                .cloned()
                .unwrap_or_else(|| "https://lain.bgm.tv/pic/user/l/icon.jpg".to_string())
        }
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "cached": false,
                    "code": "NO_MAPPING",
                    "imgUrl": img_url,
                    "sourceUrl": "https://lain.bgm.tv/pic/user/l/icon.jpg",
                })),
            )
                .into_response();
        }
    };

    // 3) Kick off caching; wait briefly for UX, otherwise tell client to try direct.
    let pools_clone = Arc::clone(&pools);
    let source_clone = source_url.clone();
    let handle = tokio::spawn(async move {
        utils::download_and_cache_image(id, source_clone, pools_clone).await;
    });

    let _ = tokio::time::timeout(Duration::from_millis(wait_ms), handle).await;

    // Check again after the wait.
    let cached = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn.query_row(
            "SELECT local_path FROM image_cache WHERE id = ?1",
            [id.to_string()],
            |row| row.get::<_, String>(0),
        ).ok())
    }).await {
        Ok(Some(path)) => tokio::fs::metadata(path).await.is_ok(),
        _ => false,
    };

    if cached {
        return Json(json!({
            "cached": true,
            "imgUrl": img_url,
        })).into_response();
    }

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "cached": false,
            "code": "TIMEOUT",
            "imgUrl": img_url,
            "sourceUrl": source_url,
        })),
    ).into_response()
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
    if let Some(v) = cache_get_ttl(&INDEX_INFO_CACHE, &index_id) {
        return Json(v).into_response();
    }
    let url = format!("https://api.bgm.tv/v0/indices/{}", index_id);
    match bgm_get(&url).await {
        Ok(data) => {
            // Reduce payload to only what frontend needs
            let minimal = json!({ "title": data.get("title"), "total": data.get("total") });
            cache_put_ttl(&INDEX_INFO_CACHE, index_id, 10 * 60 * 1000, minimal.clone());
            Json(minimal).into_response()
        },
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
    let cache_key = format!("{}:{}:{}", index_id, offset, limit);
    if let Some(v) = cache_get_ttl(&INDEX_SUBJECTS_CACHE, &cache_key) {
        return Json(v).into_response();
    }
    let url = format!(
        "https://api.bgm.tv/v0/indices/{}/subjects?limit={}&offset={}",
        index_id, limit, offset
    );
    match bgm_get(&url).await {
        Ok(data) => {
            cache_put_ttl(&INDEX_SUBJECTS_CACHE, cache_key, 10 * 60 * 1000, data.clone());
            Json(data).into_response()
        },
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
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)")
        .build()?;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..=1 {
        let resp = client.get(url).send().await;
        match resp {
            Ok(r) => {
                let r = r.error_for_status()?;
                return Ok(r.json().await?);
            }
            Err(e) => {
                last_err = Some(e.into());
                if attempt == 0 {
                    warn!("bgm_get retrying after error: {}", last_err.as_ref().unwrap());
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("bgm_get failed")))
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

    // 2. Cache miss — fetch original URL from archive.sqlite (if present)
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

    // 2.1 Fallback: use offline character_images.json mapping (preferred).
    // If we still can't resolve any URL, return 404 (per requirement).
    let mut resolved_url: Option<String> = original_url.filter(|u| !u.trim().is_empty());
    if resolved_url.is_none() {
        if let Some(&idx) = pools.character_image_index.by_id.get(&id) {
            let entry = &pools.character_image_index.list[idx];
            resolved_url = entry
                .image_medium
                .first()
                .or_else(|| entry.image_grid.first())
                .cloned()
                .filter(|u| !u.trim().is_empty());
        }
    }

    let Some(target_url) = resolved_url else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // 3. Download+transcode in background, but try to serve within a short time window
    let pools_clone = Arc::clone(&pools);
    let url_clone = target_url.clone();
    let handle = tokio::spawn(async move {
        utils::download_and_cache_image(id, url_clone, pools_clone).await;
    });

    // Give the background job a small chance to finish for better UX.
    // If it doesn't finish quickly, redirect to the resolved origin URL so the browser
    // can still display something, while caching continues in background.
    match tokio::time::timeout(std::time::Duration::from_millis(1200), handle).await {
        Ok(_) => {
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
        }
        Err(_) => {
            // timeout: keep going to fallback
        }
    }

    // Cache not ready: redirect to origin URL (client can still use /api/img/resolve for UX).
    Redirect::temporary(&target_url).into_response()
}
