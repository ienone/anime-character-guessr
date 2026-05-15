use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use rusqlite::{params, params_from_iter};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::db::{self, DbPools};

const SEARCH_CACHE_TTL_MS: i64 = 60_000;
const SEARCH_CACHE_MAX_ENTRIES: usize = 256;

const DEFAULT_SUBJECT_SEARCH_SQL: &str = "WITH matched AS (
                    SELECT rowid, bm25(subject_fts) AS rank
                    FROM subject_fts
                    WHERE subject_fts MATCH ?
                    LIMIT 300
                 )
                 SELECT s.id, s.type, s.date, s.popularity, s.name, s.name_cn,
                        CASE
                          WHEN s.name = ? THEN 0
                          WHEN s.name_cn = ? THEN 0
                          WHEN s.name LIKE ? THEN 1
                          WHEN s.name_cn LIKE ? THEN 1
                          ELSE 3
                        END AS relevance_bucket
                 FROM matched m
                 JOIN subjects s ON s.id = m.rowid
                 WHERE s.type IN (?,?)
                 ORDER BY relevance_bucket ASC, m.rank ASC, s.popularity DESC
                 LIMIT ?";

struct SearchCache {
    entries: HashMap<String, (i64, Value)>,
    order: VecDeque<String>,
}

impl SearchCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<Value> {
        let now = now_ms();
        if let Some((expires_at, value)) = self.entries.get(key)
            && *expires_at > now
        {
            return Some(value.clone());
        }
        self.entries.remove(key);
        None
    }

    fn put(&mut self, key: String, value: Value) {
        let expires_at = now_ms() + SEARCH_CACHE_TTL_MS;
        if !self.entries.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.entries.insert(key, (expires_at, value));
        self.evict();
    }

    fn evict(&mut self) {
        let now = now_ms();
        while let Some(front) = self.order.front() {
            let expired_or_missing = self
                .entries
                .get(front)
                .map(|(expires_at, _)| *expires_at <= now)
                .unwrap_or(true);
            if !expired_or_missing && self.entries.len() <= SEARCH_CACHE_MAX_ENTRIES {
                break;
            }
            if let Some(key) = self.order.pop_front()
                && (expired_or_missing || self.entries.len() > SEARCH_CACHE_MAX_ENTRIES)
            {
                self.entries.remove(&key);
            }
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

lazy_static::lazy_static! {
    static ref ARCHIVE_SEARCH_CACHE: Mutex<SearchCache> = Mutex::new(SearchCache::new());
}

fn search_cache_get(key: &str) -> Option<Value> {
    ARCHIVE_SEARCH_CACHE.lock().ok()?.get(key)
}

fn search_cache_put(key: String, value: Value) {
    if let Ok(mut cache) = ARCHIVE_SEARCH_CACHE.lock() {
        cache.put(key, value);
    }
}

pub fn archive_routes(pools: Arc<DbPools>) -> Router<Arc<DbPools>> {
    Router::new()
        .route("/subjects/{id}", get(get_subject))
        .route("/subjects/{id}/characters", get(get_subject_characters))
        .route("/characters/{id}", get(get_character_basic))
        .route("/search/subjects", get(search_subjects))
        .route("/search/characters", get(search_characters))
        .with_state(pools)
}

/// GET /api/archive/subjects/:id
/// Returns a subset of BGM subject JSON from local archive.sqlite.
async fn get_subject(State(pools): State<Arc<DbPools>>, Path(id): Path<i64>) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid id" })),
        )
            .into_response();
    }

    let result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let subject: Option<Value> = conn
            .query_row(
                "SELECT s.id, s.type, s.name, s.name_cn, s.date, s.popularity, s.score,
                        d.tags_json, d.meta_tags_json
                 FROM subjects s
                 LEFT JOIN subject_details d ON d.subject_id = s.id
                 WHERE s.id = ?1",
                [id],
                |row| {
                    let tags_json = row.get::<_, String>(7).unwrap_or_else(|_| "[]".to_string());
                    let meta_tags_json =
                        row.get::<_, String>(8).unwrap_or_else(|_| "[]".to_string());
                    let tags: Value =
                        serde_json::from_str(&tags_json).unwrap_or_else(|_| json!([]));
                    let meta_tags: Value =
                        serde_json::from_str(&meta_tags_json).unwrap_or_else(|_| json!([]));
                    Ok(json!({
                        "id": row.get::<_, i64>(0)?,
                        "type": row.get::<_, i64>(1).unwrap_or(0),
                        "name": row.get::<_, String>(2).unwrap_or_default(),
                        "name_cn": row.get::<_, String>(3).unwrap_or_default(),
                        "date": row.get::<_, String>(4).unwrap_or_default(),
                        "nsfw": false,
                        "rank": 0,
                        "collection": { "collect": row.get::<_, i64>(5).unwrap_or(0) },
                        "score": row.get::<_, f64>(6).unwrap_or(-1.0),
                        "tags": tags,
                        "meta_tags": meta_tags,
                        "locked": false,
                    }))
                },
            )
            .ok();
        Ok(subject)
    })
    .await;

    match result {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "subject not found in archive" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/archive/subjects/:id/characters
/// Local replacement for `GET /v0/subjects/:id/characters`.
async fn get_subject_characters(
    State(pools): State<Arc<DbPools>>,
    Path(subject_id): Path<i64>,
) -> impl IntoResponse {
    if subject_id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid id" })),
        )
            .into_response();
    }

    let result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT sc.character_id, sc.type, c.name,
                    COALESCE(p.name_cn, '') AS name_cn,
                    COALESCE(p.gender, '?') AS gender,
                    c.popularity
             FROM subject_characters sc
             JOIN characters c ON sc.character_id = c.id
             LEFT JOIN character_profile p ON p.character_id = c.id
             WHERE sc.subject_id = ?1
             ORDER BY sc.order_num ASC, c.popularity DESC",
        )?;
        let rows = stmt.query_map([subject_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,    // character_id
                row.get::<_, i64>(1)?,    // sc.type (1 main, 2 supporting)
                row.get::<_, String>(2)?, // character name
                row.get::<_, String>(3).unwrap_or_default(),
                row.get::<_, String>(4).unwrap_or_else(|_| "?".to_string()),
                row.get::<_, i64>(5).unwrap_or(0),
            ))
        })?;

        let mut out: Vec<Value> = Vec::new();
        for row in rows {
            let (cid, role, name, name_cn, gender, popularity) = match row {
                Ok(v) => v,
                Err(_) => continue,
            };

            let relation = if role == 1 { "主角" } else { "配角" };
            // Always serve images through our proxy (/img/:id.webp).
            // Frontend reads `character.images?.grid` for the dropdown thumbnail.
            let img_url = format!("/img/{}.webp", cid);
            out.push(json!({
                "id": cid,
                "relation": relation,
                "name": name,
                "nameCn": if name_cn.is_empty() { Value::Null } else { json!(name_cn) },
                "gender": gender,
                "image": img_url.clone(),
                "imageGrid": img_url.clone(),
                "popularity": popularity,
                "images": { "grid": img_url, "medium": img_url },
            }));
        }

        Ok(out)
    })
    .await;

    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/archive/characters/:id
/// Local replacement for `GET /v0/characters/:id` (subset used by frontend).
async fn get_character_basic(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid id" })),
        )
            .into_response();
    }

    let local_result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let local: Option<Value> = conn
            .query_row(
                "SELECT c.id, c.name, p.name_cn, p.name_en, p.gender, p.summary, c.popularity
                 FROM characters c
                 LEFT JOIN character_profile p ON p.character_id = c.id
                 WHERE c.id = ?1",
                [id],
                |row| {
                    Ok(json!({
                        "id": row.get::<_, i64>(0)?,
                        "name": row.get::<_, String>(1).unwrap_or_default(),
                        "nameCn": row.get::<_, String>(2).unwrap_or_default(),
                        "nameEn": row.get::<_, String>(3).unwrap_or_default(),
                        "gender": row.get::<_, String>(4).unwrap_or_else(|_| "?".to_string()),
                        "summary": row.get::<_, String>(5).unwrap_or_default(),
                        "popularity": row.get::<_, i64>(6).unwrap_or(0),
                    }))
                },
            )
            .ok();
        Ok(local)
    })
    .await;

    let char_val = match local_result {
        Ok(Some(v)) => v,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "character not found in archive" })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let name = char_val
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name_cn = char_val
        .get("nameCn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let name_en = char_val
        .get("nameEn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let gender = char_val
        .get("gender")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let summary = char_val
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let popularity = char_val
        .get("popularity")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let image = Some(format!("/img/{}.webp", id));
    let image_grid = image.clone();

    Json(json!({
        "id": id,
        "name": name,
        "nameCn": name_cn,
        "nameEn": name_en,
        "gender": gender,
        "image": image,
        "imageGrid": image_grid,
        "summary": summary,
        "popularity": popularity,
    }))
    .into_response()
}

/// GET /api/archive/search/subjects?keyword=xxx&type=2&type=4&limit=10
///
/// Local replacement for `POST /v0/search/subjects` used by the frontend's subject search UI.
/// Runs entirely against archive.sqlite (no BGM dependency).
async fn search_subjects(
    State(pools): State<Arc<DbPools>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let keyword = q
        .get("keyword")
        .cloned()
        .unwrap_or_default()
        .trim()
        .to_string();
    if keyword.is_empty() {
        return Json(json!({ "data": [] })).into_response();
    }

    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .min(50) as usize;

    // Types: allow a comma-separated list in `type`, default to [2,4]
    let mut types: Vec<i64> = q
        .get("type")
        .map(|s| {
            s.split(',')
                .filter_map(|p| p.trim().parse::<i64>().ok())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![2, 4]);
    types.sort_unstable();
    types.dedup();

    let keyword = normalize_search_keyword(&keyword);
    if keyword.is_empty() {
        return Json(json!({ "data": [] })).into_response();
    }

    let keyword_log = keyword.clone();
    let keyword_for_db = keyword.clone();
    let cache_key = format!(
        "subjects|{}|{}|{}",
        keyword,
        types
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(","),
        limit
    );
    if let Some(cached) = search_cache_get(&cache_key) {
        return Json(json!({ "data": cached })).into_response();
    }
    if let Some(search) = pools.tantivy_search.clone() {
        let keyword_for_search = keyword.clone();
        let types_for_search = types.clone();
        let cache_key_for_search = cache_key.clone();
        match tokio::task::spawn_blocking(move || {
            search.search_subjects(&keyword_for_search, &types_for_search, limit)
        })
        .await
        {
            Ok(Ok(list)) => {
                search_cache_put(cache_key_for_search, Value::Array(list.clone()));
                return Json(json!({ "data": list })).into_response();
            }
            Ok(Err(e)) => {
                tracing::warn!(keyword = %keyword, error = %e, "Tantivy subject search failed; falling back to SQLite");
            }
            Err(e) => {
                tracing::warn!(keyword = %keyword, error = %e, "Tantivy subject search task failed; falling back to SQLite");
            }
        }
    }
    let cache_key_for_db = cache_key.clone();
    let result = db::with_archive_db_timed(
        Arc::clone(&pools),
        "search_subjects",
        Duration::from_millis(800),
        move |conn| {
            let started = Instant::now();
            let sql = if types == [2, 4] {
                DEFAULT_SUBJECT_SEARCH_SQL.to_string()
            } else {
                let type_placeholders = std::iter::repeat_n("?", types.len())
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "WITH matched AS (
                        SELECT rowid, bm25(subject_fts) AS rank
                        FROM subject_fts
                        WHERE subject_fts MATCH ?
                        LIMIT 300
                     )
                     SELECT s.id, s.type, s.date, s.popularity, s.name, s.name_cn,
                            CASE
                              WHEN s.name = ? THEN 0
                              WHEN s.name_cn = ? THEN 0
                              WHEN s.name LIKE ? THEN 1
                              WHEN s.name_cn LIKE ? THEN 1
                              ELSE 3
                            END AS relevance_bucket
                     FROM matched m
                     JOIN subjects s ON s.id = m.rowid
                     WHERE s.type IN ({type_placeholders})
                     ORDER BY relevance_bucket ASC, m.rank ASC, s.popularity DESC
                     LIMIT ?"
                )
            };
            let fts_query = subject_fts_query(&keyword_for_db);
            tracing::debug!(
                keyword = %keyword_for_db,
                fts_query = %fts_query,
                limit,
                types = ?types,
                "search subjects query"
            );
            let mut rows = if fts_query.is_empty() {
                Vec::new()
            } else {
                query_subject_search(conn, &sql, &types, &keyword_for_db, &fts_query, limit)?
            };
            if rows.len() < limit {
                merge_subject_rows(
                    &mut rows,
                    query_subject_like_search(conn, &types, &keyword_for_db, limit)?,
                    limit,
                );
            }
            let mut out = Vec::new();
            for row in rows {
                let (id, stype, date, name, name_cn) = row;
                let img_url = format!("/img/subject/{}.webp", id);
                out.push(json!({
                    "id": id,
                    "type": stype,
                    "date": date,
                    "name": name,
                    "name_cn": name_cn,
                    "images": { "grid": img_url, "medium": img_url, "common": img_url },
                }));
            }
            tracing::info!(
                keyword = %keyword_for_db,
                result_count = out.len(),
                duration_ms = started.elapsed().as_millis() as u64,
                "search subjects completed"
            );
            search_cache_put(cache_key_for_db, Value::Array(out.clone()));
            Ok(out)
        },
    )
    .await;

    match result {
        Ok(list) => Json(json!({ "data": list })).into_response(),
        Err(e) => search_error_response("subjects", &keyword_log, e),
    }
}

/// GET /api/archive/search/characters?keyword=xxx&limit=10&offset=0
///
/// Local replacement for `POST /v0/search/characters` used by the character search UI.
/// Runs entirely against archive.sqlite via indexed on-demand queries (no BGM dependency).
async fn search_characters(
    State(pools): State<Arc<DbPools>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let keyword = q
        .get("keyword")
        .cloned()
        .unwrap_or_default()
        .trim()
        .to_string();
    if keyword.is_empty() {
        return Json(json!({ "data": [] })).into_response();
    }

    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .min(50) as usize;

    let offset_raw = q
        .get("offset")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if offset_raw > 100 {
        return Json(json!({ "data": [] })).into_response();
    }
    let offset = offset_raw as usize;

    let keyword = normalize_search_keyword(&keyword);
    if keyword.is_empty() {
        return Json(json!({ "data": [] })).into_response();
    }

    let keyword_log = keyword.clone();
    let keyword_for_db = keyword.clone();
    let cache_key = format!("characters|{}|{}|{}", keyword, limit, offset);
    if let Some(cached) = search_cache_get(&cache_key) {
        return Json(json!({ "data": cached })).into_response();
    }
    if let Some(search) = pools.tantivy_search.clone() {
        let keyword_for_search = keyword.clone();
        let cache_key_for_search = cache_key.clone();
        match tokio::task::spawn_blocking(move || {
            search.search_characters(&keyword_for_search, limit, offset)
        })
        .await
        {
            Ok(Ok(list)) => {
                search_cache_put(cache_key_for_search, Value::Array(list.clone()));
                return Json(json!({ "data": list })).into_response();
            }
            Ok(Err(e)) => {
                tracing::warn!(keyword = %keyword, error = %e, "Tantivy character search failed; falling back to SQLite");
            }
            Err(e) => {
                tracing::warn!(keyword = %keyword, error = %e, "Tantivy character search task failed; falling back to SQLite");
            }
        }
    }
    let cache_key_for_db = cache_key.clone();
    let result = db::with_archive_db_timed(
        Arc::clone(&pools),
        "search_characters",
        Duration::from_millis(800),
        move |conn| {
            let started = Instant::now();
            let fts_query = character_fts_query(&keyword_for_db);
            if fts_query.is_empty() {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare_cached(
                "WITH matched AS (
                SELECT rowid, bm25(character_fts) AS rank
                FROM character_fts
                WHERE character_fts MATCH ?1
                LIMIT 500
             )
             SELECT d.character_id, d.name, d.name_cn, d.name_en, d.romaji, d.gender, d.popularity,
                    ds.id, ds.name, ds.name_cn,
                    CASE
                      WHEN d.name = ?4 OR d.name_cn = ?4 THEN 0
                      WHEN d.name LIKE ?5 OR d.name_cn LIKE ?5 THEN 1
                      WHEN d.name_en LIKE ?5 OR d.romaji LIKE ?5 THEN 2
                      ELSE 3
                    END AS relevance_bucket,
                    m.rank
             FROM matched m
             JOIN character_search_docs d ON d.character_id = m.rowid
             JOIN characters c ON c.id = m.rowid
             LEFT JOIN subjects ds ON ds.id = (
                SELECT sc.subject_id
                FROM subject_characters sc
                JOIN subjects s2 ON s2.id = sc.subject_id
                WHERE sc.character_id = d.character_id
                ORDER BY CASE WHEN sc.type = 1 THEN 0 ELSE 1 END,
                         s2.popularity DESC,
                         sc.order_num ASC
                LIMIT 1
             )
             ORDER BY relevance_bucket ASC, m.rank ASC, d.popularity DESC
             LIMIT ?2 OFFSET ?3",
            )?;
            tracing::debug!(
                keyword = %keyword_for_db,
                fts_query = %fts_query,
                limit,
                offset,
                "search characters query"
            );
            let prefix = format!("{}%", keyword_for_db);
            let rows = stmt.query_map(
                params![
                    fts_query,
                    limit as i64,
                    offset as i64,
                    keyword_for_db,
                    prefix
                ],
                |row| {
                    Ok(crate::db::CharacterSearchDoc {
                        id: row.get::<_, i64>(0)?,
                        name: row.get::<_, String>(1).unwrap_or_default(),
                        name_cn: row.get::<_, String>(2).unwrap_or_default(),
                        name_en: row.get::<_, String>(3).unwrap_or_default(),
                        romaji: row.get::<_, String>(4).unwrap_or_default(),
                        gender: row.get::<_, String>(5).unwrap_or_else(|_| "?".to_string()),
                        popularity: row.get::<_, i64>(6).unwrap_or(0),
                        default_subject_id: row.get::<_, Option<i64>>(7).unwrap_or(None),
                        default_subject_name: row.get::<_, String>(8).unwrap_or_default(),
                        default_subject_name_cn: row.get::<_, String>(9).unwrap_or_default(),
                    })
                },
            )?;

            let mut out = Vec::new();
            for row in rows {
                let doc = row?;
                let image = format!("/img/{}.webp", doc.id);

                out.push(json!({
                    "id": doc.id,
                    "name": doc.name,
                    "nameCn": if doc.name_cn.is_empty() { Value::Null } else { json!(doc.name_cn) },
                    "nameEn": if doc.name_en.is_empty() { Value::Null } else { json!(doc.name_en) },
                    "romaji": if doc.romaji.is_empty() { Value::Null } else { json!(doc.romaji) },
                    "gender": doc.gender,
                    "images": { "grid": image },
                    "popularity": doc.popularity,
                    "defaultSubject": doc.default_subject_id.map(|subject_id| json!({
                        "id": subject_id,
                        "name": doc.default_subject_name,
                        "nameCn": doc.default_subject_name_cn,
                    })),
                }));
            }
            tracing::info!(
                keyword = %keyword_for_db,
                result_count = out.len(),
                duration_ms = started.elapsed().as_millis() as u64,
                "search characters completed"
            );
            search_cache_put(cache_key_for_db, Value::Array(out.clone()));
            Ok(out)
        },
    )
    .await;

    match result {
        Ok(list) => Json(json!({ "data": list })).into_response(),
        Err(e) => search_error_response("characters", &keyword_log, e),
    }
}

fn subject_fts_query(keyword: &str) -> String {
    keyword
        .split_whitespace()
        .flat_map(|part| part.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')))
        .filter(|part| !part.is_empty())
        .map(|part| format!("{}*", part))
        .collect::<Vec<_>>()
        .join(" AND ")
}

type SubjectSearchRow = (i64, i64, String, String, String);

fn query_subject_search(
    conn: &rusqlite::Connection,
    sql: &str,
    types: &[i64],
    keyword: &str,
    fts_query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SubjectSearchRow>> {
    let mut values: Vec<String> = Vec::with_capacity(types.len() + 6);
    values.push(fts_query.to_string());
    values.push(keyword.to_string());
    values.push(keyword.to_string());
    values.push(format!("{}%", keyword));
    values.push(format!("{}%", keyword));
    values.extend(types.iter().map(|v| v.to_string()));
    values.push(limit.to_string());
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(params_from_iter(values), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1).unwrap_or(0),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, String>(4).unwrap_or_default(),
            row.get::<_, String>(5).unwrap_or_default(),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn query_subject_like_search(
    conn: &rusqlite::Connection,
    types: &[i64],
    keyword: &str,
    limit: usize,
) -> anyhow::Result<Vec<SubjectSearchRow>> {
    let type_placeholders = std::iter::repeat_n("?", types.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT s.id, s.type, s.date, s.popularity, s.name, s.name_cn,
                CASE
                  WHEN s.name = ? THEN 0
                  WHEN s.name_cn = ? THEN 0
                  WHEN s.name LIKE ? THEN 1
                  WHEN s.name_cn LIKE ? THEN 1
                  ELSE 2
                END AS relevance_bucket
         FROM subjects s
         WHERE s.type IN ({type_placeholders})
           AND (s.name LIKE ? OR s.name_cn LIKE ?)
         ORDER BY relevance_bucket ASC, s.popularity DESC
         LIMIT ?"
    );

    let mut values: Vec<String> = Vec::with_capacity(types.len() + 7);
    values.push(keyword.to_string());
    values.push(keyword.to_string());
    values.push(format!("{}%", keyword));
    values.push(format!("{}%", keyword));
    values.extend(types.iter().map(|v| v.to_string()));
    values.push(format!("%{}%", keyword));
    values.push(format!("%{}%", keyword));
    values.push(limit.to_string());

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1).unwrap_or(0),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, String>(4).unwrap_or_default(),
            row.get::<_, String>(5).unwrap_or_default(),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn merge_subject_rows(
    rows: &mut Vec<SubjectSearchRow>,
    fallback_rows: Vec<SubjectSearchRow>,
    limit: usize,
) {
    for row in fallback_rows {
        if rows.iter().any(|existing| existing.0 == row.0) {
            continue;
        }
        rows.push(row);
        if rows.len() >= limit {
            break;
        }
    }
}

fn search_error_response(kind: &'static str, keyword: &str, e: anyhow::Error) -> Response {
    let error = e.to_string();
    let timed_out = error.contains("interrupted") || error.contains("timeout");
    let (status, code) = if timed_out {
        (StatusCode::REQUEST_TIMEOUT, "SEARCH_TIMEOUT")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "SEARCH_FAILED")
    };
    tracing::error!(
        kind,
        keyword,
        status = status.as_u16(),
        error = %error,
        "archive search failed"
    );
    (status, Json(json!({ "code": code, "error": error }))).into_response()
}

fn normalize_search_keyword(keyword: &str) -> String {
    keyword
        .trim()
        .chars()
        .filter(|ch| {
            !matches!(
                ch,
                '"' | '\'' | ':' | '^' | '(' | ')' | '{' | '}' | '[' | ']'
            )
        })
        .take(64)
        .collect::<String>()
}

fn character_fts_query(keyword: &str) -> String {
    fts_query_with_char_fallback(keyword)
}

fn fts_query_with_char_fallback(keyword: &str) -> String {
    keyword
        .split_whitespace()
        .flat_map(|part| part.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')))
        .filter(|part| !part.is_empty())
        .map(|part| format!("{}*", part))
        .collect::<Vec<_>>()
        .join(" AND ")
}
