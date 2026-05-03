use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::db::{self, DbPools};
use crate::routes::game;

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
        let raw: Option<String> = conn
            .query_row("SELECT raw_json FROM subjects WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .ok();
        Ok(raw)
    })
    .await;

    match result {
        Ok(Some(raw)) => {
            let mut v: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            // BGM API sometimes returns locked; archive data may not contain it.
            if v.get("locked").is_none() {
                if let Value::Object(ref mut m) = v {
                    m.insert("locked".to_string(), Value::Bool(false));
                }
            }
            Json(v).into_response()
        }
        Ok(None) => {
            // Fallback to live BGM API for subjects not present in archive.sqlite.
            // This still goes through the server (and is cached in-memory) so the client
            // never directly depends on BGM reachability.
            let cache_key = id.to_string();
            if let Some(v) = super::cache_get_ttl(&super::SUBJECT_DETAILS_CACHE, &cache_key) {
                return Json(v).into_response();
            }

            let url = format!("https://api.bgm.tv/v0/subjects/{}", id);
            match super::bgm_get(&url).await {
                Ok(v) => {
                    super::cache_put_ttl(
                        &super::SUBJECT_DETAILS_CACHE,
                        cache_key,
                        10 * 60 * 1000,
                        v.clone(),
                    );
                    Json(v).into_response()
                }
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
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
            "SELECT sc.character_id, sc.type, c.raw_json
             FROM subject_characters sc
             JOIN characters c ON sc.character_id = c.id
             WHERE sc.subject_id = ?1
             ORDER BY sc.order_num ASC, c.collects DESC",
        )?;
        let rows = stmt.query_map([subject_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,    // character_id
                row.get::<_, i64>(1)?,    // sc.type (1 main, 2 supporting)
                row.get::<_, String>(2)?, // character raw_json
            ))
        })?;

        let mut out: Vec<Value> = Vec::new();
        for row in rows {
            let (cid, role, raw) = match row {
                Ok(v) => v,
                Err(_) => continue,
            };

            let v: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let name = v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let relation = if role == 1 { "主角" } else { "配角" };
            // Always serve images through our proxy (/img/:id.webp).
            // Frontend reads `character.images?.grid` for the dropdown thumbnail.
            let img_url = format!("/img/{}.webp", cid);
            out.push(json!({
                "id": cid,
                "relation": relation,
                "name": name,
                "images": { "grid": img_url, "medium": img_url },
            }));
        }

        Ok(out)
    })
    .await;

    match result {
        Ok(v) => {
            if !v.is_empty() {
                return Json(v).into_response();
            }

            // Fallback to BGM for subjects not present in archive.sqlite (or empty joins).
            let cache_key = subject_id.to_string();
            if let Some(v) = super::cache_get_ttl(&super::SUBJECT_CHARACTERS_CACHE, &cache_key) {
                return Json(v).into_response();
            }

            let url = format!("https://api.bgm.tv/v0/subjects/{}/characters", subject_id);
            match super::bgm_get(&url).await {
                Ok(raw) => {
                    let out = Value::Array(raw.as_array().cloned().unwrap_or_default());
                    super::cache_put_ttl(
                        &super::SUBJECT_CHARACTERS_CACHE,
                        cache_key,
                        10 * 60 * 1000,
                        out.clone(),
                    );
                    Json(out).into_response()
                }
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
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

    let char_val_opt = pools.character_cache.char_json.get(&id);
    if char_val_opt.is_none() {
        // Fallback to BGM for characters not present in archive.sqlite
        let cache_key = id.to_string();
        if let Some(v) = super::cache_get_ttl(&super::CHARACTER_DETAILS_CACHE, &cache_key) {
            return Json(v).into_response();
        }

        let url = format!("https://api.bgm.tv/v0/characters/{}", id);
        match super::bgm_get(&url).await {
            Ok(raw) => {
                let name = raw
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let gender = raw.get("gender").and_then(|v| v.as_str()).unwrap_or("?");
                let gender = match gender {
                    "male" | "female" => gender,
                    _ => "?",
                };
                let image = raw
                    .get("images")
                    .and_then(|imgs| {
                        imgs.get("medium")
                            .or_else(|| imgs.get("large"))
                            .or_else(|| imgs.get("grid"))
                    })
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "https://lain.bgm.tv/pic/user/l/icon.jpg".to_string());
                let image_grid = raw
                    .get("images")
                    .and_then(|imgs| {
                        imgs.get("grid")
                            .or_else(|| imgs.get("medium"))
                            .or_else(|| imgs.get("large"))
                    })
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| image.clone());

                let stat_collects = raw
                    .get("stat")
                    .and_then(|s| s.get("collects"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let stat_comments = raw
                    .get("stat")
                    .and_then(|s| s.get("comments"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let popularity = stat_collects + stat_comments;
                let summary = raw
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Try to extract nameCn/nameEn similarly to frontend fallback logic.
                let name_cn = raw
                    .get("name_cn")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        raw.get("infobox")
                            .and_then(|v| v.as_array())
                            .and_then(|arr| {
                                arr.iter().find(|it| {
                                    it.get("key").and_then(|k| k.as_str()) == Some("简体中文名")
                                })
                            })
                            .and_then(|it| it.get("value"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });

                let name_en = raw
                    .get("infobox")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| {
                        arr.iter()
                            .find(|it| it.get("key").and_then(|k| k.as_str()) == Some("别名"))
                    })
                    .and_then(|it| it.get("value"))
                    .and_then(|v| v.as_array())
                    .and_then(|aliases| {
                        let find_alias = |k: &str| {
                            aliases
                                .iter()
                                .find(|a| a.get("k").and_then(|v| v.as_str()) == Some(k))
                                .and_then(|a| a.get("v").and_then(|v| v.as_str()))
                                .map(|s| s.to_string())
                        };
                        find_alias("英文名").or_else(|| find_alias("罗马字"))
                    });

                let out = json!({
                    "id": id,
                    "name": name,
                    "nameCn": name_cn,
                    "nameEn": name_en,
                    "gender": gender,
                    "image": image,
                    "imageGrid": image_grid,
                    "summary": summary,
                    "popularity": popularity,
                });
                super::cache_put_ttl(
                    &super::CHARACTER_DETAILS_CACHE,
                    cache_key,
                    10 * 60 * 1000,
                    out.clone(),
                );
                return Json(out).into_response();
            }
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
        }
    }

    let char_val = char_val_opt.expect("checked above");

    let name = char_val
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (name_cn, name_en, image, image_grid, gender, summary, popularity) =
        game::parse_character_basic_fields(id, char_val);

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
    let types: Vec<i64> = q
        .get("type")
        .map(|s| {
            s.split(',')
                .filter_map(|p| p.trim().parse::<i64>().ok())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![2, 4]);

    let list = pools
        .subject_search_index
        .search(&keyword, &types, limit)
        .into_iter()
        .map(|doc| {
            // Always serve through our subject image proxy. BGM offline dump has
            // no image URLs for subjects; the proxy resolves + caches lazily.
            let img_url = format!("/img/subject/{}.webp", doc.id);
            json!({
                "id": doc.id,
                "type": doc.stype,
                "date": doc.date,
                "name": doc.name,
                "name_cn": doc.name_cn,
                "images": { "grid": img_url, "medium": img_url, "common": img_url },
            })
        })
        .collect::<Vec<_>>();

    Json(json!({ "data": list })).into_response()
}

/// GET /api/archive/search/characters?keyword=xxx&limit=10&offset=0
///
/// Local replacement for `POST /v0/search/characters` used by the character search UI.
/// Runs entirely against archive.sqlite via prebuilt in-memory index (no BGM dependency).
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

    let offset = q
        .get("offset")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0) as usize;

    let list = pools
        .character_search_index
        .search(&keyword, offset, limit)
        .into_iter()
        .map(|doc| {
            // Parse common display fields from archive infobox text.
            let (name_cn, name_en_any, image, _image_grid, gender, _summary, popularity) =
                game::parse_character_basic_fields(
                    doc.id,
                    &json!({
                        "infobox": doc.infobox,
                        "summary": doc.summary,
                        "collects": doc.collects,
                        "comments": doc.comments,
                    }),
                );

            // Best-effort split: provide both "英文名" and "罗马字" if present.
            let en = doc
                .infobox
                .split("[英文名|")
                .nth(1)
                .and_then(|rest| rest.split(']').next())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let romaji = doc
                .infobox
                .split("[罗马字|")
                .nth(1)
                .and_then(|rest| rest.split(']').next())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let _name_en = en.clone().or_else(|| romaji.clone()).or(name_en_any);

            json!({
                "id": doc.id,
                "name": doc.name,
                "gender": gender,
                "images": { "grid": image.clone().unwrap_or_default() },
                "infobox": [
                    { "key": "简体中文名", "value": name_cn.unwrap_or_else(|| doc.name.clone()) },
                    { "key": "别名", "value": [
                        { "k": "英文名", "v": en.unwrap_or_default() },
                        { "k": "罗马字", "v": romaji.unwrap_or_default() }
                    ]}
                ],
                "stat": { "collects": doc.collects, "comments": doc.comments },
                "popularity": popularity,
            })
        })
        .collect::<Vec<_>>();

    Json(json!({ "data": list })).into_response()
}
