use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use rusqlite::{params, params_from_iter};
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
        let subject: Option<Value> = conn
            .query_row(
                "SELECT id, type, name, name_cn, date, nsfw, rank, collects, score, tags_json, meta_tags_json
                 FROM subjects WHERE id = ?1",
                [id],
                |row| {
                    let tags_json = row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string());
                    let meta_tags_json =
                        row.get::<_, String>(10).unwrap_or_else(|_| "[]".to_string());
                    let tags: Value = serde_json::from_str(&tags_json).unwrap_or_else(|_| json!([]));
                    let meta_tags: Value =
                        serde_json::from_str(&meta_tags_json).unwrap_or_else(|_| json!([]));
                    Ok(json!({
                        "id": row.get::<_, i64>(0)?,
                        "type": row.get::<_, i64>(1).unwrap_or(0),
                        "name": row.get::<_, String>(2).unwrap_or_default(),
                        "name_cn": row.get::<_, String>(3).unwrap_or_default(),
                        "date": row.get::<_, String>(4).unwrap_or_default(),
                        "nsfw": row.get::<_, i64>(5).unwrap_or(0) != 0,
                        "rank": row.get::<_, i64>(6).unwrap_or(0),
                        "collection": { "collect": row.get::<_, i64>(7).unwrap_or(0) },
                        "score": row.get::<_, f64>(8).unwrap_or(-1.0),
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
            "SELECT sc.character_id, sc.type, c.name
             FROM subject_characters sc
             JOIN characters c ON sc.character_id = c.id
             WHERE sc.subject_id = ?1
             ORDER BY sc.order_num ASC, c.collects DESC",
        )?;
        let rows = stmt.query_map([subject_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,    // character_id
                row.get::<_, i64>(1)?,    // sc.type (1 main, 2 supporting)
                row.get::<_, String>(2)?, // character name
            ))
        })?;

        let mut out: Vec<Value> = Vec::new();
        for row in rows {
            let (cid, role, name) = match row {
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

    let local_result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let local: Option<Value> = conn
            .query_row(
                "SELECT id, name, infobox, summary, collects, comments FROM characters WHERE id = ?1",
                [id],
                |row| {
                    Ok(json!({
                        "id": row.get::<_, i64>(0)?,
                        "name": row.get::<_, String>(1).unwrap_or_default(),
                        "infobox": row.get::<_, String>(2).unwrap_or_default(),
                        "summary": row.get::<_, String>(3).unwrap_or_default(),
                        "collects": row.get::<_, i64>(4).unwrap_or(0),
                        "comments": row.get::<_, i64>(5).unwrap_or(0),
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
    let (name_cn, name_en, image, image_grid, gender, summary, popularity) =
        game::parse_character_basic_fields(id, &char_val);

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

    let keyword_for_db = keyword.clone();
    let result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let type_placeholders = std::iter::repeat_n("?", types.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT s.id, s.type, s.date, s.collects, s.name, s.name_cn
             FROM subject_fts f
             JOIN subjects s ON s.id = f.rowid
             WHERE subject_fts MATCH ?
               AND s.nsfw = 0
               AND s.type IN ({type_placeholders})
               AND (s.name LIKE ? OR s.name_cn LIKE ?)
             ORDER BY s.collects DESC, bm25(subject_fts)
             LIMIT ?"
        );
        let fts_query = subject_fts_query(&keyword_for_db);
        let like = format!("%{}%", keyword_for_db);
        let mut values: Vec<String> = types.into_iter().map(|v| v.to_string()).collect();
        values.insert(0, fts_query);
        values.push(like.clone());
        values.push(like);
        values.push(limit.to_string());
        let mut stmt = conn.prepare(&sql)?;
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
            let (id, stype, date, name, name_cn) = row?;
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
        Ok(out)
    })
    .await;

    match result {
        Ok(list) => Json(json!({ "data": list })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
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

    let offset = q
        .get("offset")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0) as usize;

    let result = db::with_archive_db(Arc::clone(&pools), move |conn| {
        let fts_query = character_fts_query(&keyword);
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, c.collects, c.comments, c.infobox, c.summary
             FROM character_fts f
             JOIN characters c ON c.id = f.rowid
             WHERE character_fts MATCH ?1
               AND c.role = 1
               AND (c.name LIKE ?2 OR c.infobox LIKE ?2)
             ORDER BY c.collects + c.comments DESC, bm25(character_fts)
             LIMIT ?3 OFFSET ?4",
        )?;
        let like = format!("%{}%", keyword);
        let rows = stmt.query_map(
            params![fts_query, like, limit as i64, offset as i64],
            |row| {
                Ok(crate::db::CharacterDoc {
                    id: row.get::<_, i64>(0)?,
                    name: row.get::<_, String>(1).unwrap_or_default(),
                    collects: row.get::<_, i64>(2).unwrap_or(0),
                    comments: row.get::<_, i64>(3).unwrap_or(0),
                    infobox: row.get::<_, String>(4).unwrap_or_default(),
                    summary: row.get::<_, String>(5).unwrap_or_default(),
                })
            },
        )?;

        let mut out = Vec::new();
        for row in rows {
            let doc = row?;
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

            out.push(json!({
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
            }));
        }
        Ok(out)
    })
    .await;

    match result {
        Ok(list) => Json(json!({ "data": list })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn subject_fts_query(keyword: &str) -> String {
    fts_query_with_char_fallback(keyword)
}

fn character_fts_query(keyword: &str) -> String {
    fts_query_with_char_fallback(keyword)
}

fn fts_query_with_char_fallback(keyword: &str) -> String {
    keyword
        .split_whitespace()
        .map(|part| part.replace('"', ""))
        .filter(|part| !part.is_empty())
        .map(|part| format!("{}*", part))
        .collect::<Vec<_>>()
        .join(" AND ")
}
