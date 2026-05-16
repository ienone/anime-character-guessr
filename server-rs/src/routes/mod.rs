use crate::db::{self, DbPools};
use crate::utils;
use axum::http::{HeaderMap, StatusCode, header};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::fmt;
use std::time::{Duration, Instant};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::{Semaphore, broadcast};
use tracing::warn;

pub mod archive;
pub mod game;
pub mod rooms;
pub mod roulette;
pub mod stats;
pub mod tags;
pub mod write_guard;

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
    if let Some(entry) = cache.get(key)
        && entry.expires_at_ms > now
    {
        return Some(entry.value.clone());
    }
    cache.remove(key);
    None
}

fn cache_put_ttl(cache: &DashMap<String, CacheEntry>, key: String, ttl_ms: i64, value: Value) {
    cache.insert(
        key.clone(),
        CacheEntry {
            expires_at_ms: now_ms() + ttl_ms,
            value,
        },
    );
    prune_ttl_cache(cache, &key);
}

fn prune_ttl_cache(cache: &DashMap<String, CacheEntry>, protected_key: &str) {
    let now = now_ms();
    let expired_keys = cache
        .iter()
        .filter(|entry| entry.value().expires_at_ms <= now)
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    for key in expired_keys {
        if key != protected_key {
            cache.remove(&key);
        }
    }

    if cache.len() <= TTL_CACHE_MAX_ENTRIES {
        return;
    }

    let remove_count = cache.len().saturating_sub(TTL_CACHE_MAX_ENTRIES);
    let keys = cache
        .iter()
        .filter(|entry| entry.key().as_str() != protected_key)
        .map(|entry| entry.key().clone())
        .take(remove_count)
        .collect::<Vec<_>>();
    for key in keys {
        cache.remove(&key);
    }
}

// stable_hash_json was previously used for BGM search caching; removed after
// migrating search fully to offline archive routes.

lazy_static::lazy_static! {
    static ref INDEX_INFO_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = indexId
    static ref INDEX_SUBJECTS_CACHE: DashMap<String, CacheEntry> = DashMap::new(); // key = indexId:offset:limit
    // In-flight de-duplication for BGM image source resolution (per character_id).
    // Prevents bursty concurrent requests all hitting BGM for the same character.
    static ref PENDING_IMAGE_SOURCE: DashMap<i64, broadcast::Sender<bool>> = DashMap::new();
    // Same idea, but for subject images (independent namespace).
    static ref PENDING_SUBJECT_IMAGE_SOURCE: DashMap<i64, broadcast::Sender<bool>> = DashMap::new();
    // Shared cap for external image-source resolution across characters and subjects.
    static ref IMAGE_SOURCE_RESOLVE_SEMAPHORE: Semaphore = Semaphore::new(4);
}

const IMAGE_SOURCE_PERMIT_TIMEOUT: Duration = Duration::from_millis(500);
const IMAGE_RESOLVE_DEFAULT_WAIT_MS: u64 = 900;
const IMAGE_RESOLVE_MAX_WAIT_MS: u64 = 5_000;
const IMAGE_SOURCE_FALLBACK_WAIT_MS: u64 = 500;
const BGM_PROXY_LIMIT_PER_MINUTE: usize = 90;
const IMAGE_RESOLVE_LIMIT_PER_MINUTE: usize = 180;
const GAME_RANDOM_LIMIT_PER_MINUTE: usize = 60;
const GAME_CHARACTER_LIMIT_PER_MINUTE: usize = 120;
const TTL_CACHE_MAX_ENTRIES: usize = 2048;

/// /api/* — game logic, leaderboard, stats
pub fn api_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        // Single-player game (archive.sqlite backed)
        .route("/game/random", post(get_random_character))
        .route("/game/character", post(get_character_by_id))
        // Image resolve helper (JSON): tells client whether /img is cached yet
        .route("/img/resolve/{id}", get(resolve_character_image))
        .route("/img/resolve/subject/{id}", get(resolve_subject_image))
        // Archive-backed local endpoints (reduce BGM API usage)
        .nest("/archive", archive::archive_routes(Arc::clone(&pools)))
        // BGM proxy (for index mode / search — BGM calls go through server)
        .route("/bgm/index-info", get(bgm_proxy_index_info))
        .route("/bgm/index-subjects", get(bgm_proxy_index_subjects))
        // Roulette
        .route("/roulette", get(roulette::roulette))
        // Redeem codes
        .route("/redeem", get(stats::redeem))
        // Character leaderboards
        .route(
            "/leaderboard/characters",
            get(stats::leaderboard_characters),
        )
        .route("/leaderboard/weekly", get(stats::leaderboard_weekly))
        // Bug feedback.
        .route("/bug-feedback", post(tags::bug_feedback))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(pools)
}

/// /img/* — character & subject image proxy with local WebP cache
pub fn image_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        // Subject route MUST be registered before the character catch-all so
        // `/img/subject/{id}` is not swallowed by `/img/{id}`.
        .route("/subject/medium/{id}", get(get_subject_medium_image))
        .route("/subject/{id}", get(get_subject_image))
        .route("/medium/{id}", get(get_character_medium_image))
        .route("/{id}", get(get_character_image))
        .with_state(pools)
}

fn image_resolve_limit_rejection(
    headers: &HeaderMap,
    peer_addr: SocketAddr,
) -> Option<write_guard::GuardRejection> {
    write_guard::enforce_public_write_limit(
        headers,
        Some(peer_addr),
        "image-resolve",
        IMAGE_RESOLVE_LIMIT_PER_MINUTE,
    )
    .err()
}

async fn archive_character_exists(pools: &Arc<DbPools>, id: i64) -> bool {
    db::with_archive_db_timed(
        Arc::clone(pools),
        "image_character_exists",
        Duration::from_millis(500),
        move |conn| {
            let mut stmt = conn.prepare("SELECT 1 FROM characters WHERE id = ?1 LIMIT 1")?;
            Ok(stmt.exists([id])?)
        },
    )
    .await
    .unwrap_or(false)
}

async fn archive_subject_exists(pools: &Arc<DbPools>, id: i64) -> bool {
    db::with_archive_db_timed(
        Arc::clone(pools),
        "image_subject_exists",
        Duration::from_millis(500),
        move |conn| {
            let mut stmt = conn.prepare("SELECT 1 FROM subjects WHERE id = ?1 LIMIT 1")?;
            Ok(stmt.exists([id])?)
        },
    )
    .await
    .unwrap_or(false)
}

#[derive(Clone, Copy)]
enum ImageKind {
    Character,
    Subject,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageVariant {
    Grid,
    Medium,
}

impl ImageVariant {
    fn from_query(q: &HashMap<String, String>) -> Self {
        match q.get("variant").map(|v| v.as_str()) {
            Some("medium") => Self::Medium,
            _ => Self::Grid,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::Medium => "medium",
        }
    }
}

fn image_cache_key(kind: ImageKind, variant: ImageVariant, id: i64) -> String {
    match (kind, variant) {
        (ImageKind::Character, ImageVariant::Grid) => id.to_string(),
        (ImageKind::Character, ImageVariant::Medium) => format!("m:{id}"),
        (ImageKind::Subject, ImageVariant::Grid) => format!("s:{id}"),
        (ImageKind::Subject, ImageVariant::Medium) => format!("sm:{id}"),
    }
}

fn image_proxy_url(kind: ImageKind, variant: ImageVariant, id: i64) -> String {
    match (kind, variant) {
        (ImageKind::Character, ImageVariant::Grid) => format!("/img/{id}.webp"),
        (ImageKind::Character, ImageVariant::Medium) => format!("/img/medium/{id}.webp"),
        (ImageKind::Subject, ImageVariant::Grid) => format!("/img/subject/{id}.webp"),
        (ImageKind::Subject, ImageVariant::Medium) => format!("/img/subject/medium/{id}.webp"),
    }
}

fn choose_image_source(
    image_medium: String,
    image_grid: String,
    variant: ImageVariant,
) -> Option<String> {
    let medium = image_medium.trim();
    let grid = image_grid.trim();
    match variant {
        ImageVariant::Grid => {
            if !grid.is_empty() {
                Some(grid.to_string())
            } else if !medium.is_empty() {
                Some(medium.to_string())
            } else {
                None
            }
        }
        ImageVariant::Medium => {
            if !medium.is_empty() {
                Some(medium.to_string())
            } else if !grid.is_empty() {
                Some(grid.to_string())
            } else {
                None
            }
        }
    }
}

async fn load_cached_image_path(pools: &Arc<DbPools>, cache_key: String) -> Option<String> {
    db::with_app_db(Arc::clone(pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [cache_key],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    .ok()
    .flatten()
}

async fn cached_image_exists(pools: &Arc<DbPools>, cache_key: &str) -> bool {
    match load_cached_image_path(pools, cache_key.to_string()).await {
        Some(path) => tokio::fs::metadata(path).await.is_ok(),
        None => false,
    }
}

async fn wait_for_cached_image(pools: &Arc<DbPools>, cache_key: &str, wait_ms: u64) -> bool {
    if wait_ms == 0 {
        return cached_image_exists(pools, cache_key).await;
    }

    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        if cached_image_exists(pools, cache_key).await {
            return true;
        }

        let now = Instant::now();
        if now >= deadline {
            return false;
        }

        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
    }
}

async fn load_cached_image_source_for_kind(
    pools: &Arc<DbPools>,
    kind: ImageKind,
    id: i64,
) -> Option<(String, String)> {
    match kind {
        ImageKind::Character => load_cached_image_source(pools, id).await,
        ImageKind::Subject => load_cached_subject_image_source(pools, id).await,
    }
}

async fn ensure_image_source_cached_for_kind(
    pools: &Arc<DbPools>,
    kind: ImageKind,
    id: i64,
) -> Option<(String, String)> {
    match kind {
        ImageKind::Character => ensure_image_source_cached(pools, id).await,
        ImageKind::Subject => ensure_subject_image_source_cached(pools, id).await,
    }
}

async fn load_cached_source_url(
    pools: &Arc<DbPools>,
    kind: ImageKind,
    variant: ImageVariant,
    id: i64,
) -> Option<String> {
    let (medium, grid) = load_cached_image_source_for_kind(pools, kind, id).await?;
    choose_image_source(medium, grid, variant)
}

async fn wait_for_cached_source_url(
    pools: &Arc<DbPools>,
    kind: ImageKind,
    variant: ImageVariant,
    id: i64,
    wait_ms: u64,
) -> Option<String> {
    if wait_ms == 0 {
        return load_cached_source_url(pools, kind, variant, id).await;
    }

    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        if let Some(url) = load_cached_source_url(pools, kind, variant, id).await {
            return Some(url);
        }

        let now = Instant::now();
        if now >= deadline {
            return None;
        }

        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
    }
}

fn spawn_image_cache_fill(
    pools: Arc<DbPools>,
    kind: ImageKind,
    variant: ImageVariant,
    id: i64,
    cache_key: String,
) {
    tokio::spawn(async move {
        let Some((image_medium, image_grid)) =
            ensure_image_source_cached_for_kind(&pools, kind, id).await
        else {
            return;
        };

        let Some(source_url) = choose_image_source(image_medium, image_grid, variant) else {
            return;
        };

        utils::download_and_cache_image(cache_key, source_url, pools).await;
    });
}

async fn serve_cached_image(
    pools: Arc<DbPools>,
    id_str: String,
    kind: ImageKind,
    variant: ImageVariant,
) -> Response {
    let id: i64 = id_str.split('.').next().unwrap_or("0").parse().unwrap_or(0);

    if id == 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let cache_key = image_cache_key(kind, variant, id);
    let local_path = load_cached_image_path(&pools, cache_key).await;

    if let Some(path) = local_path
        && let Ok(content) = tokio::fs::read(&path).await
    {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "image/webp".parse().unwrap());
        headers.insert(
            header::CACHE_CONTROL,
            "public, max-age=31536000".parse().unwrap(),
        );
        return (headers, content).into_response();
    }

    StatusCode::NOT_FOUND.into_response()
}

async fn resolve_image(
    pools: Arc<DbPools>,
    peer_addr: SocketAddr,
    headers: HeaderMap,
    id: i64,
    q: HashMap<String, String>,
    kind: ImageKind,
) -> Response {
    if id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "id must be positive" })),
        )
            .into_response();
    }

    let variant = ImageVariant::from_query(&q);
    let cache_key = image_cache_key(kind, variant, id);
    let img_url = image_proxy_url(kind, variant, id);

    if cached_image_exists(&pools, &cache_key).await {
        return Json(json!({
            "cached": true,
            "variant": variant.as_str(),
            "imgUrl": img_url,
        }))
        .into_response();
    }

    let cached_only = q
        .get("cachedOnly")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    if cached_only {
        return (
            StatusCode::ACCEPTED,
            Json(json!({
                "cached": false,
                "code": "CACHE_MISS",
                "variant": variant.as_str(),
                "imgUrl": img_url,
            })),
        )
            .into_response();
    }

    if let Some(rejection) = image_resolve_limit_rejection(&headers, peer_addr) {
        return rejection.into_response();
    }

    let wait_ms = q
        .get("waitMs")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(IMAGE_RESOLVE_DEFAULT_WAIT_MS)
        .min(IMAGE_RESOLVE_MAX_WAIT_MS);
    let allow_source_fallback = q
        .get("fallback")
        .map(|v| v != "none" && v != "local")
        .unwrap_or(true);

    spawn_image_cache_fill(Arc::clone(&pools), kind, variant, id, cache_key.clone());

    if wait_for_cached_image(&pools, &cache_key, wait_ms).await {
        return Json(json!({
            "cached": true,
            "variant": variant.as_str(),
            "imgUrl": img_url,
        }))
        .into_response();
    }

    let source_url = if allow_source_fallback {
        wait_for_cached_source_url(&pools, kind, variant, id, IMAGE_SOURCE_FALLBACK_WAIT_MS).await
    } else {
        None
    };

    let mut body = json!({
        "cached": false,
        "code": if source_url.is_some() { "TIMEOUT" } else { "SOURCE_PENDING" },
        "variant": variant.as_str(),
        "imgUrl": img_url,
    });
    if let Some(url) = source_url
        && let Value::Object(ref mut obj) = body
    {
        obj.insert("sourceUrl".to_string(), Value::String(url));
    }

    (StatusCode::ACCEPTED, Json(body)).into_response()
}

/// GET /api/img/resolve/:id
///
/// JSON helper used by the client Image component:
/// - If cached: returns `{ cached: true, imgUrl: "/img/{id}.webp" }`
/// - If not cached: starts/joins source resolution and local WebP fill, waits
///   briefly, then returns either the warmed proxy URL or an upstream fallback.
async fn resolve_character_image(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    resolve_image(pools, peer_addr, headers, id, q, ImageKind::Character).await
}

/// GET /api/img/resolve/subject/:id
/// Same helper as character image resolution, but backed by the subject image
/// redirect API and stored under the `s:{id}` image-cache namespace.
async fn resolve_subject_image(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> impl IntoResponse {
    resolve_image(pools, peer_addr, headers, id, q, ImageKind::Subject).await
}

fn character_name_for_stats(payload: &Value) -> String {
    let Some(obj) = payload.as_object() else {
        return String::new();
    };

    ["nameCn", "name_cn", "name"]
        .iter()
        .find_map(|key| {
            obj.get(*key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// POST /api/game/random
/// Pick a random character based on game settings from archive.sqlite.
async fn get_random_character(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Err(response) = write_guard::enforce_public_write_limit(
        &headers,
        Some(peer_addr),
        "game-random",
        GAME_RANDOM_LIMIT_PER_MINUTE,
    ) {
        return response.into_response();
    }

    let settings = game::GameSettings::from_json(&body);
    let pools_for_query = Arc::clone(&pools);
    let result = db::with_archive_db_timed(
        pools_for_query,
        "game_random_character",
        Duration::from_secs(2),
        move |conn| game::random_character_with_conn(conn, &settings),
    )
    .await;

    match result {
        Ok((char_id, mut payload)) => {
            if let Some(vas) = load_cached_vas(&pools, char_id).await
                && let Value::Object(ref mut obj) = payload
            {
                obj.insert(
                    "animeVAs".to_string(),
                    Value::Array(vas.into_iter().map(Value::String).collect()),
                );
            }
            let stats_name = character_name_for_stats(&payload);
            let stats_pools = Arc::clone(&pools);
            tokio::spawn(async move {
                stats::record_answer_character_count(stats_pools, char_id, stats_name).await;
            });
            Json(payload).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/game/character  { id: number, settings: {...} }
/// Return full gameplay payload for a specific character from archive.sqlite.
async fn get_character_by_id(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Err(response) = write_guard::enforce_public_write_limit(
        &headers,
        Some(peer_addr),
        "game-character",
        GAME_CHARACTER_LIMIT_PER_MINUTE,
    ) {
        return response.into_response();
    }

    let char_id = match body.get("id").and_then(|v| v.as_i64()) {
        Some(id) if id > 0 => id,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "id must be positive" })),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "id required" })),
            )
                .into_response();
        }
    };
    let stats_purpose = body
        .get("purpose")
        .and_then(|v| v.as_str())
        .unwrap_or("guess")
        .trim()
        .to_ascii_lowercase();
    let settings = game::GameSettings::from_json(body.get("settings").unwrap_or(&json!({})));
    let pools_for_query = Arc::clone(&pools);
    let result = db::with_archive_db_timed(
        pools_for_query,
        "game_character_by_id",
        Duration::from_secs(2),
        move |conn| game::character_by_id_with_conn(conn, char_id, &settings),
    )
    .await;

    match result {
        Ok(mut payload) => {
            if let Some(vas) = load_cached_vas(&pools, char_id).await
                && let Value::Object(ref mut obj) = payload
            {
                obj.insert(
                    "animeVAs".to_string(),
                    Value::Array(vas.into_iter().map(Value::String).collect()),
                );
            }
            let stats_name = character_name_for_stats(&payload);
            let stats_pools = Arc::clone(&pools);
            match stats_purpose.as_str() {
                "guess" => {
                    tokio::spawn(async move {
                        stats::record_guess_character_count(stats_pools, char_id, stats_name).await;
                    });
                }
                "answer" => {
                    tokio::spawn(async move {
                        stats::record_answer_character_count(stats_pools, char_id, stats_name)
                            .await;
                    });
                }
                _ => {}
            }
            Json(payload).into_response()
        }
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

fn bgm_proxy_limit_rejection(
    headers: &HeaderMap,
    peer_addr: SocketAddr,
) -> Option<write_guard::GuardRejection> {
    write_guard::enforce_public_write_limit(
        headers,
        Some(peer_addr),
        "bgm-proxy",
        BGM_PROXY_LIMIT_PER_MINUTE,
    )
    .err()
}

fn positive_query_id(q: &HashMap<String, String>, key: &'static str) -> Result<i64, String> {
    match q.get(key).and_then(|id| id.parse::<i64>().ok()) {
        Some(id) if id > 0 => Ok(id),
        _ => Err(format!("{key} must be a positive number")),
    }
}

#[derive(Debug)]
struct BgmHttpStatusError {
    status: StatusCode,
}

impl fmt::Display for BgmHttpStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BGM API returned {}", self.status)
    }
}

impl std::error::Error for BgmHttpStatusError {}

fn bgm_proxy_error_response(error: anyhow::Error) -> Response {
    let status = error
        .downcast_ref::<BgmHttpStatusError>()
        .map(|inner| inner.status)
        .filter(|status| *status == StatusCode::NOT_FOUND)
        .unwrap_or(StatusCode::BAD_GATEWAY);

    (status, Json(json!({ "error": error.to_string() }))).into_response()
}

/// GET /api/bgm/index-info?indexId=xxx
async fn bgm_proxy_index_info(
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> Response {
    if let Some(rejection) = bgm_proxy_limit_rejection(&headers, peer_addr) {
        return rejection.into_response();
    }
    let index_id = match positive_query_id(&q, "indexId") {
        Ok(id) => id.to_string(),
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
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
        Err(e) => bgm_proxy_error_response(e),
    }
}

/// GET /api/bgm/index-subjects?indexId=xxx&offset=0&limit=10
async fn bgm_proxy_index_subjects(
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxumQuery(q): AxumQuery<HashMap<String, String>>,
) -> Response {
    if let Some(rejection) = bgm_proxy_limit_rejection(&headers, peer_addr) {
        return rejection.into_response();
    }
    let index_id = match positive_query_id(&q, "indexId") {
        Ok(id) => id.to_string(),
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
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
        Err(e) => bgm_proxy_error_response(e),
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
                let status = r.status();
                if !status.is_success() {
                    let status =
                        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                    return Err(BgmHttpStatusError { status }.into());
                }
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

    if !archive_character_exists(pools, id).await {
        warn!(
            id,
            "character image source resolution skipped: id not in archive"
        );
        let _ = tx.send(true);
        return None;
    }

    let _permit = match tokio::time::timeout(
        IMAGE_SOURCE_PERMIT_TIMEOUT,
        IMAGE_SOURCE_RESOLVE_SEMAPHORE.acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => {
            warn!(
                id,
                "character image source resolution skipped: concurrency limit reached"
            );
            let _ = tx.send(true);
            return load_cached_image_source(pools, id).await;
        }
    };

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

async fn get_character_image(
    State(pools): State<Arc<DbPools>>,
    Path(id_str): Path<String>,
) -> Response {
    serve_cached_image(pools, id_str, ImageKind::Character, ImageVariant::Grid).await
}

async fn get_character_medium_image(
    State(pools): State<Arc<DbPools>>,
    Path(id_str): Path<String>,
) -> Response {
    serve_cached_image(pools, id_str, ImageKind::Character, ImageVariant::Medium).await
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

    if !archive_subject_exists(pools, id).await {
        warn!(
            id,
            "subject image source resolution skipped: id not in archive"
        );
        let _ = tx.send(true);
        return None;
    }

    let _permit = match tokio::time::timeout(
        IMAGE_SOURCE_PERMIT_TIMEOUT,
        IMAGE_SOURCE_RESOLVE_SEMAPHORE.acquire(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => {
            warn!(
                id,
                "subject image source resolution skipped: concurrency limit reached"
            );
            let _ = tx.send(true);
            return load_cached_subject_image_source(pools, id).await;
        }
    };

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
) -> Response {
    serve_cached_image(pools, id_str, ImageKind::Subject, ImageVariant::Grid).await
}

async fn get_subject_medium_image(
    State(pools): State<Arc<DbPools>>,
    Path(id_str): Path<String>,
) -> Response {
    serve_cached_image(pools, id_str, ImageKind::Subject, ImageVariant::Medium).await
}
