use crate::db::{self, DbPools};
use crate::utils;
use axum::http::{StatusCode, header};
use axum::response::Redirect;
use axum::{
    Json, Router,
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

pub mod archive;
pub mod game;
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
    cache.insert(
        key,
        CacheEntry {
            expires_at_ms: now_ms() + ttl_ms,
            value,
        },
    );
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
    // In-flight de-duplication for BGM image source resolution (per character_id).
    // Prevents bursty concurrent requests all hitting BGM for the same character.
    static ref PENDING_IMAGE_SOURCE: DashMap<i64, broadcast::Sender<bool>> = DashMap::new();
    // Same idea, but for subject images (independent namespace).
    static ref PENDING_SUBJECT_IMAGE_SOURCE: DashMap<i64, broadcast::Sender<bool>> = DashMap::new();
}

/// /api/* — game logic, leaderboard, stats
pub fn api_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        // Single-player game (archive.sqlite backed)
        .route("/game/random", post(get_random_character))
        .route("/game/character", post(get_character_by_id))
        // Image resolve helper (JSON): tells client whether /img is cached yet
        .route("/img/resolve/{id}", get(resolve_character_image))
        .route("/img/resolve/subject/{id}", get(resolve_subject_image))
        .route("/img/source/{id}", get(resolve_character_image_source))
        .route(
            "/img/source/subject/{id}",
            get(resolve_subject_image_source),
        )
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
        .route(
            "/leaderboard/characters",
            get(stats::leaderboard_characters),
        )
        .route("/leaderboard/guesses", get(stats::leaderboard_guesses))
        .route("/leaderboard/weekly", get(stats::leaderboard_weekly))
        // Stats write endpoints
        .route(
            "/answer-character-count",
            post(stats::answer_character_count),
        )
        .route("/guess-character-count", post(stats::guess_character_count))
        .route("/character-usage/{id}", get(stats::character_usage))
        .route("/subject-added", post(stats::subject_added))
        // Tags & Feedback
        .route("/character-tags", post(tags::update_character_tags))
        .route("/character-tags/{id}", get(tags::get_character_tags))
        .route(
            "/game-character-tags",
            post(tags::update_game_character_tags),
        )
        .route(
            "/game-character-tags/{subject_id}",
            get(tags::get_game_character_tags),
        )
        .route("/propose-tags", post(tags::propose_tags))
        .route("/feedback-tags", post(tags::feedback_tags))
        .route("/bug-feedback", post(tags::bug_feedback))
        .with_state(pools)
}

/// /img/* — character & subject image proxy with local WebP cache
pub fn image_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        // Subject route MUST be registered before the character catch-all so
        // `/img/subject/{id}` is not swallowed by `/img/{id}`.
        .route("/subject/{id}", get(get_subject_image))
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
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "id must be positive" })),
        )
            .into_response();
    }

    let wait_ms = q
        .get("waitMs")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1200)
        .min(5000);

    // 1) If cached already, answer immediately.
    let cached = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    {
        Ok(Some(path)) => tokio::fs::metadata(path).await.is_ok(),
        _ => false,
    };

    let img_url = format!("/img/{}.webp", id);
    if cached {
        return Json(json!({
            "cached": true,
            "imgUrl": img_url,
        }))
        .into_response();
    }

    // 2) Resolve a usable source URL (app.sqlite mirror first; then BGM fallback).
    let Some((image_medium, image_grid)) = ensure_image_source_cached(&pools, id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "cached": false,
                "code": "NO_SOURCE",
                "imgUrl": img_url,
                "sourceUrl": "https://lain.bgm.tv/pic/user/l/icon.jpg",
            })),
        )
            .into_response();
    };

    let source_url = if !image_grid.trim().is_empty() {
        image_grid
    } else if !image_medium.trim().is_empty() {
        image_medium
    } else {
        "https://lain.bgm.tv/pic/user/l/icon.jpg".to_string()
    };

    // 3) Kick off caching; wait briefly for UX, otherwise tell client to try direct.
    let pools_clone = Arc::clone(&pools);
    let source_clone = source_url.clone();
    let cache_key = id.to_string();
    let handle = tokio::spawn(async move {
        utils::download_and_cache_image(cache_key, source_clone, pools_clone).await;
    });

    let _ = tokio::time::timeout(Duration::from_millis(wait_ms), handle).await;

    // Check again after the wait.
    let cached = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    {
        Ok(Some(path)) => tokio::fs::metadata(path).await.is_ok(),
        _ => false,
    };

    if cached {
        return Json(json!({
            "cached": true,
            "imgUrl": img_url,
        }))
        .into_response();
    }

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "cached": false,
            "code": "TIMEOUT",
            "imgUrl": img_url,
            "sourceUrl": source_url,
        })),
    )
        .into_response()
}

async fn resolve_character_image_source(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "id must be positive" })),
        )
            .into_response();
    }

    let Some((image_medium, image_grid)) = ensure_image_source_cached(&pools, id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": "NO_SOURCE",
                "sourceUrl": "https://lain.bgm.tv/pic/user/l/icon.jpg",
            })),
        )
            .into_response();
    };

    let source_url = if !image_medium.trim().is_empty() {
        image_medium
    } else {
        image_grid
    };

    Json(json!({ "sourceUrl": source_url })).into_response()
}

/// GET /api/img/resolve/subject/:id?waitMs=1200
/// Same helper as character image resolution, but backed by the subject image
/// redirect API and stored under the `s:{id}` image-cache namespace.
async fn resolve_subject_image(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "id must be positive" })),
        )
            .into_response();
    }

    let wait_ms = q
        .get("waitMs")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1200)
        .min(5000);

    let cache_key = format!("s:{}", id);
    let cached_key = cache_key.clone();
    let cached = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [cached_key],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    {
        Ok(Some(path)) => tokio::fs::metadata(path).await.is_ok(),
        _ => false,
    };

    let img_url = format!("/img/subject/{}.webp", id);
    if cached {
        return Json(json!({
            "cached": true,
            "imgUrl": img_url,
        }))
        .into_response();
    }

    let Some((image_medium, image_grid)) = ensure_subject_image_source_cached(&pools, id).await
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "cached": false,
                "code": "NO_SOURCE",
                "imgUrl": img_url,
                "sourceUrl": "",
            })),
        )
            .into_response();
    };

    let source_url = if !image_grid.trim().is_empty() {
        image_grid
    } else {
        image_medium
    };

    let pools_clone = Arc::clone(&pools);
    let source_clone = source_url.clone();
    let cache_key_for_dl = cache_key.clone();
    let handle = tokio::spawn(async move {
        utils::download_and_cache_image(cache_key_for_dl, source_clone, pools_clone).await;
    });

    let _ = tokio::time::timeout(Duration::from_millis(wait_ms), handle).await;

    let cached_key = cache_key.clone();
    let cached = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [cached_key],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    {
        Ok(Some(path)) => tokio::fs::metadata(path).await.is_ok(),
        _ => false,
    };

    if cached {
        return Json(json!({
            "cached": true,
            "imgUrl": img_url,
        }))
        .into_response();
    }

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "cached": false,
            "code": "TIMEOUT",
            "imgUrl": img_url,
            "sourceUrl": source_url,
        })),
    )
        .into_response()
}

async fn resolve_subject_image_source(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "id must be positive" })),
        )
            .into_response();
    }

    let Some((image_medium, image_grid)) = ensure_subject_image_source_cached(&pools, id).await
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": "NO_SOURCE",
                "sourceUrl": "",
            })),
        )
            .into_response();
    };

    let source_url = if !image_medium.trim().is_empty() {
        image_medium
    } else {
        image_grid
    };

    Json(json!({ "sourceUrl": source_url })).into_response()
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
    let result =
        tokio::task::spawn_blocking(move || game::random_character(&pools_clone, &settings)).await;

    match result {
        Ok(Ok((char_id, mut payload))) => {
            // Fill animeVAs via persisted mirror cache; BGM is used only as fallback and then stored in app.sqlite.
            if let Some(vas) = ensure_vas_cached(&pools, char_id).await {
                if let Value::Object(ref mut obj) = payload {
                    obj.insert(
                        "animeVAs".to_string(),
                        Value::Array(vas.into_iter().map(Value::String).collect()),
                    );
                }
            }
            Json(payload).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
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
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "id required" })),
            )
                .into_response();
        }
    };
    let settings = game::GameSettings::from_json(body.get("settings").unwrap_or(&json!({})));
    let pools_clone = Arc::clone(&pools);

    let result = tokio::task::spawn_blocking(move || {
        game::character_by_id(&pools_clone, char_id, &settings)
    })
    .await;

    match result {
        Ok(Ok(mut payload)) => {
            if let Some(vas) = ensure_vas_cached(&pools, char_id).await {
                if let Value::Object(ref mut obj) = payload {
                    obj.insert(
                        "animeVAs".to_string(),
                        Value::Array(vas.into_iter().map(Value::String).collect()),
                    );
                }
            }
            Json(payload).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
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
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "indexId required" })),
            )
                .into_response();
        }
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
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/bgm/index-subjects?indexId=xxx&offset=0&limit=10
async fn bgm_proxy_index_subjects(
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    let index_id = match q.get("indexId") {
        Some(id) if !id.is_empty() => id.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "indexId required" })),
            )
                .into_response();
        }
    };
    let offset = q
        .get("offset")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .min(50);
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
            cache_put_ttl(
                &INDEX_SUBJECTS_CACHE,
                cache_key,
                10 * 60 * 1000,
                data.clone(),
            );
            Json(data).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/bgm/character?id=xxx — proxy single character details from BGM
async fn bgm_proxy_character(
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    let id = match q.get("id") {
        Some(id) if !id.is_empty() => id.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "id required" })),
            )
                .into_response();
        }
    };
    let url = format!("https://api.bgm.tv/v0/characters/{}", id);
    match bgm_get(&url).await {
        Ok(data) => Json(data).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn bgm_get(url: &str) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent(
            "anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)",
        )
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
                    warn!(
                        "bgm_get retrying after error: {}",
                        last_err.as_ref().unwrap()
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("bgm_get failed")))
}

async fn bgm_get_character(id: i64) -> anyhow::Result<Value> {
    let url = format!("https://api.bgm.tv/v0/characters/{}", id);
    bgm_get(&url).await
}

async fn bgm_get_character_image_location(
    id: i64,
    image_type: &str,
) -> anyhow::Result<Option<String>> {
    let url = format!(
        "https://api.bgm.tv/v0/characters/{}/image?type={}",
        id, image_type
    );

    // This endpoint responds with 302 and the image URL in the Location header.
    // We explicitly disable redirects to capture Location without fetching the image.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(
            "anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)",
        )
        .build()?;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..=1 {
        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_redirection() {
                    if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                        let mut s = loc.to_str().unwrap_or("").trim().to_string();
                        if s.is_empty() {
                            return Ok(None);
                        }
                        // Some servers may return a relative location.
                        if s.starts_with('/') {
                            s = format!("https://lain.bgm.tv{}", s);
                        }
                        // Spec note: "no image" returns a default placeholder URL.
                        if s.contains("/img/no_icon_subject.png") {
                            return Ok(None);
                        }
                        return Ok(Some(s));
                    }
                    return Ok(None);
                }

                // If server ever changes to return 200 + body or something else, treat it as no image.
                if status.is_success() {
                    return Ok(None);
                }

                last_err = Some(anyhow::anyhow!("bgm image endpoint returned {}", status));
            }
            Err(e) => last_err = Some(e.into()),
        }

        if attempt == 0 {
            warn!(
                "bgm_get_character_image_location retrying after error: {}",
                last_err.as_ref().unwrap()
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("bgm image endpoint failed")))
}

async fn load_cached_image_source(pools: &Arc<DbPools>, id: i64) -> Option<(String, String)> {
    let id2 = id;
    let row = db::with_app_db(Arc::clone(pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT image_medium, image_grid FROM character_image_sources WHERE character_id = ?1",
        )?;
        let r = stmt
            .query_row([id2], |row| {
                Ok((
                    row.get::<_, String>(0).unwrap_or_default(),
                    row.get::<_, String>(1).unwrap_or_default(),
                ))
            })
            .ok();
        Ok(r)
    })
    .await
    .ok()
    .flatten();

    row.and_then(|(m, g)| {
        let m2 = m.trim().to_string();
        let g2 = g.trim().to_string();
        if m2.is_empty() && g2.is_empty() {
            None
        } else {
            Some((m2, g2))
        }
    })
}

async fn save_cached_image_source(
    pools: &Arc<DbPools>,
    id: i64,
    image_medium: String,
    image_grid: String,
    source: &str,
) {
    let fetched_at_ms = chrono::Utc::now().timestamp_millis();
    let src = source.to_string();
    let m = image_medium;
    let g = image_grid;
    let _ = db::with_app_db(Arc::clone(pools), move |conn| {
        conn.execute(
            "INSERT INTO character_image_sources (character_id, image_medium, image_grid, fetched_at_ms, source)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(character_id) DO UPDATE SET
               image_medium=excluded.image_medium,
               image_grid=excluded.image_grid,
               fetched_at_ms=excluded.fetched_at_ms,
               source=excluded.source",
            rusqlite::params![id, m, g, fetched_at_ms, src],
        )?;
        Ok(())
    })
    .await;
}

async fn load_cached_vas(pools: &Arc<DbPools>, id: i64) -> Option<Vec<String>> {
    let id2 = id;
    let json_opt = db::with_app_db(Arc::clone(pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT va_names_json FROM character_vas WHERE character_id = ?1",
                [id2],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    .ok()
    .flatten();

    let json = json_opt?;
    serde_json::from_str::<Vec<String>>(&json)
        .ok()
        .filter(|v| !v.is_empty())
}

fn extract_images_from_bgm_character(raw: &Value) -> (String, String) {
    let imgs = raw.get("images").and_then(|v| v.as_object());
    let get = |k: &str| {
        imgs.and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let medium = {
        let m = get("medium");
        if !m.is_empty() { m } else { get("large") }
    };
    let grid = {
        let g = get("grid");
        if !g.is_empty() { g } else { medium.clone() }
    };
    (medium, grid)
}

async fn ensure_image_source_cached(pools: &Arc<DbPools>, id: i64) -> Option<(String, String)> {
    // 1) app.sqlite cache
    if let Some((m, g)) = load_cached_image_source(pools, id).await {
        return Some((m, g));
    }

    // 2) In-flight de-duplication: if another request is already resolving this id,
    // wait briefly and then re-check app.sqlite.
    if let Some(entry) = PENDING_IMAGE_SOURCE.get(&id) {
        let mut rx = entry.value().subscribe();
        let _ = tokio::time::timeout(Duration::from_millis(1200), rx.recv()).await;
        if let Some((m, g)) = load_cached_image_source(pools, id).await {
            return Some((m, g));
        }
        // still not available; fall through to do our own resolution best-effort
    }

    // Become the leader for this id (best-effort). If another leader races us, we just join it.
    let (tx, _rx) = broadcast::channel(1);
    let we_are_leader = PENDING_IMAGE_SOURCE.insert(id, tx.clone()).is_none();
    struct CleanupPendingImageSource(i64);
    impl Drop for CleanupPendingImageSource {
        fn drop(&mut self) {
            PENDING_IMAGE_SOURCE.remove(&self.0);
        }
    }

    if !we_are_leader {
        // Someone else inserted concurrently. Wait and re-check.
        if let Some(entry) = PENDING_IMAGE_SOURCE.get(&id) {
            let mut rx = entry.value().subscribe();
            let _ = tokio::time::timeout(Duration::from_millis(1200), rx.recv()).await;
        }
        return load_cached_image_source(pools, id).await;
    }
    let _cleanup = CleanupPendingImageSource(id);

    // 3) BGM image endpoint fallback (preferred): capture Location without downloading the image.
    let (medium, grid) = tokio::join!(
        bgm_get_character_image_location(id, "medium"),
        bgm_get_character_image_location(id, "grid"),
    );
    let medium = medium.ok().flatten().unwrap_or_default();
    let grid = grid.ok().flatten().unwrap_or_default();
    if !medium.trim().is_empty() || !grid.trim().is_empty() {
        save_cached_image_source(pools, id, medium.clone(), grid.clone(), "bgm_image").await;
        let _ = tx.send(true);
        return Some((medium, grid));
    }

    // 4) BGM character detail fallback (legacy): parse `images` field.
    if let Ok(raw) = bgm_get_character(id).await {
        let (m, g) = extract_images_from_bgm_character(&raw);
        if !m.trim().is_empty() || !g.trim().is_empty() {
            save_cached_image_source(pools, id, m.clone(), g.clone(), "bgm").await;
            let _ = tx.send(true);
            return Some((m, g));
        }
    }

    let _ = tx.send(true);
    None
}

async fn ensure_vas_cached(pools: &Arc<DbPools>, id: i64) -> Option<Vec<String>> {
    if let Some(v) = load_cached_vas(pools, id).await {
        return Some(v);
    }

    None
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
            headers.insert(
                header::CACHE_CONTROL,
                "public, max-age=31536000".parse().unwrap(),
            );
            return (headers, content).into_response();
        }
    }

    // 2. Cache miss — resolve source URL (app.sqlite mirror -> legacy json -> BGM API fallback).
    let Some((image_medium, image_grid)) = ensure_image_source_cached(&pools, id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let target_url = if !image_grid.trim().is_empty() {
        image_grid
    } else {
        image_medium
    };

    // 3. Download+transcode in background, but try to serve within a short time window
    let pools_clone = Arc::clone(&pools);
    let url_clone = target_url.clone();
    let cache_key = id.to_string();
    let handle = tokio::spawn(async move {
        utils::download_and_cache_image(cache_key, url_clone, pools_clone).await;
    });

    // Give the background job a small chance to finish for better UX.
    // If it doesn't finish quickly, redirect to the resolved origin URL so the browser
    // can still display something, while caching continues in background.
    match tokio::time::timeout(std::time::Duration::from_millis(1200), handle).await {
        Ok(_) => {
            let local_path: Option<String> =
                match db::with_app_db(Arc::clone(&pools), move |conn| {
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
                    headers.insert(
                        header::CACHE_CONTROL,
                        "public, max-age=31536000".parse().unwrap(),
                    );
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

// ─── Subject image proxy ─────────────────────────────────────────────────────
//
// BGM offline dump does NOT include subject images. We resolve them on demand
// via `GET https://api.bgm.tv/v0/subjects/{id}/image?type=...` (302), then
// transcode + cache like character images. Cached entries are namespaced as
// `s:{id}` in `image_cache` and stored as `subject_{id}.webp` on disk.

async fn bgm_get_subject_image_location(
    id: i64,
    image_type: &str,
) -> anyhow::Result<Option<String>> {
    let url = format!(
        "https://api.bgm.tv/v0/subjects/{}/image?type={}",
        id, image_type
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(
            "anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)",
        )
        .build()?;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..=1 {
        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_redirection() {
                    if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                        let mut s = loc.to_str().unwrap_or("").trim().to_string();
                        if s.is_empty() {
                            return Ok(None);
                        }
                        if s.starts_with('/') {
                            s = format!("https://lain.bgm.tv{}", s);
                        }
                        if s.contains("/img/no_icon_subject.png") {
                            return Ok(None);
                        }
                        return Ok(Some(s));
                    }
                    return Ok(None);
                }

                if status.is_success() {
                    return Ok(None);
                }

                last_err = Some(anyhow::anyhow!(
                    "bgm subject image endpoint returned {}",
                    status
                ));
            }
            Err(e) => last_err = Some(e.into()),
        }

        if attempt == 0 {
            warn!(
                "bgm_get_subject_image_location retrying after error: {}",
                last_err.as_ref().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("bgm subject image endpoint failed")))
}

fn extract_images_from_bgm_subject(raw: &Value) -> (String, String) {
    let imgs = raw.get("images").and_then(|v| v.as_object());
    let get = |k: &str| {
        imgs.and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let medium = {
        let m = get("medium");
        if !m.is_empty() { m } else { get("common") }
    };
    let grid = {
        let g = get("grid");
        if !g.is_empty() { g } else { medium.clone() }
    };
    (medium, grid)
}

async fn load_cached_subject_image_source(
    pools: &Arc<DbPools>,
    id: i64,
) -> Option<(String, String)> {
    let id2 = id;
    let row = db::with_app_db(Arc::clone(pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT image_medium, image_grid FROM subject_image_sources WHERE subject_id = ?1",
        )?;
        let r = stmt
            .query_row([id2], |row| {
                Ok((
                    row.get::<_, String>(0).unwrap_or_default(),
                    row.get::<_, String>(1).unwrap_or_default(),
                ))
            })
            .ok();
        Ok(r)
    })
    .await
    .ok()
    .flatten();

    row.and_then(|(m, g)| {
        let m2 = m.trim().to_string();
        let g2 = g.trim().to_string();
        if m2.is_empty() && g2.is_empty() {
            None
        } else {
            Some((m2, g2))
        }
    })
}

async fn save_cached_subject_image_source(
    pools: &Arc<DbPools>,
    id: i64,
    image_medium: String,
    image_grid: String,
    source: &str,
) {
    let fetched_at_ms = chrono::Utc::now().timestamp_millis();
    let src = source.to_string();
    let _ = db::with_app_db(Arc::clone(pools), move |conn| {
        conn.execute(
            "INSERT INTO subject_image_sources (subject_id, image_medium, image_grid, fetched_at_ms, source)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(subject_id) DO UPDATE SET
               image_medium=excluded.image_medium,
               image_grid=excluded.image_grid,
               fetched_at_ms=excluded.fetched_at_ms,
               source=excluded.source",
            rusqlite::params![id, image_medium, image_grid, fetched_at_ms, src],
        )?;
        Ok(())
    })
    .await;
}

async fn ensure_subject_image_source_cached(
    pools: &Arc<DbPools>,
    id: i64,
) -> Option<(String, String)> {
    if let Some((m, g)) = load_cached_subject_image_source(pools, id).await {
        return Some((m, g));
    }

    if let Some(entry) = PENDING_SUBJECT_IMAGE_SOURCE.get(&id) {
        let mut rx = entry.value().subscribe();
        let _ = tokio::time::timeout(Duration::from_millis(1200), rx.recv()).await;
        if let Some((m, g)) = load_cached_subject_image_source(pools, id).await {
            return Some((m, g));
        }
    }

    let (tx, _rx) = broadcast::channel(1);
    let we_are_leader = PENDING_SUBJECT_IMAGE_SOURCE
        .insert(id, tx.clone())
        .is_none();
    struct CleanupPendingSubjectImageSource(i64);
    impl Drop for CleanupPendingSubjectImageSource {
        fn drop(&mut self) {
            PENDING_SUBJECT_IMAGE_SOURCE.remove(&self.0);
        }
    }

    if !we_are_leader {
        if let Some(entry) = PENDING_SUBJECT_IMAGE_SOURCE.get(&id) {
            let mut rx = entry.value().subscribe();
            let _ = tokio::time::timeout(Duration::from_millis(1200), rx.recv()).await;
        }
        return load_cached_subject_image_source(pools, id).await;
    }
    let _cleanup = CleanupPendingSubjectImageSource(id);

    let (medium, grid) = tokio::join!(
        bgm_get_subject_image_location(id, "common"),
        bgm_get_subject_image_location(id, "grid"),
    );
    let medium = medium.ok().flatten().unwrap_or_default();
    let grid = grid.ok().flatten().unwrap_or_default();
    if !medium.trim().is_empty() || !grid.trim().is_empty() {
        save_cached_subject_image_source(pools, id, medium.clone(), grid.clone(), "bgm_image")
            .await;
        let _ = tx.send(true);
        return Some((medium, grid));
    }

    if let Ok(raw) = bgm_get(&format!("https://api.bgm.tv/v0/subjects/{}", id)).await {
        let (m, g) = extract_images_from_bgm_subject(&raw);
        if !m.trim().is_empty() || !g.trim().is_empty() {
            save_cached_subject_image_source(pools, id, m.clone(), g.clone(), "bgm").await;
            let _ = tx.send(true);
            return Some((m, g));
        }
    }

    let _ = tx.send(true);
    None
}

async fn get_subject_image(
    State(pools): State<Arc<DbPools>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let id: i64 = id_str.split('.').next().unwrap_or("0").parse().unwrap_or(0);

    if id == 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let cache_key = format!("s:{}", id);

    // 1. Check local cache
    let key_lookup = cache_key.clone();
    let local_path: Option<String> = match db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [key_lookup],
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
            headers.insert(
                header::CACHE_CONTROL,
                "public, max-age=31536000".parse().unwrap(),
            );
            return (headers, content).into_response();
        }
    }

    // 2. Cache miss — resolve source URL via BGM image redirect endpoint.
    let Some((image_medium, image_grid)) = ensure_subject_image_source_cached(&pools, id).await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let target_url = if !image_grid.trim().is_empty() {
        image_grid
    } else {
        image_medium
    };

    // 3. Download+transcode in background, but try to serve within a short window.
    let pools_clone = Arc::clone(&pools);
    let url_clone = target_url.clone();
    let cache_key_for_dl = cache_key.clone();
    let handle = tokio::spawn(async move {
        utils::download_and_cache_image(cache_key_for_dl, url_clone, pools_clone).await;
    });

    if tokio::time::timeout(std::time::Duration::from_millis(1200), handle)
        .await
        .is_ok()
    {
        let key_lookup = cache_key.clone();
        let local_path: Option<String> = match db::with_app_db(Arc::clone(&pools), move |conn| {
            Ok(conn
                .query_row(
                    "SELECT local_path FROM image_cache WHERE id = ?1",
                    [key_lookup],
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
                headers.insert(
                    header::CACHE_CONTROL,
                    "public, max-age=31536000".parse().unwrap(),
                );
                return (headers, content).into_response();
            }
        }
    }

    Redirect::temporary(&target_url).into_response()
}
