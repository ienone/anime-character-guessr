use crate::db::{self, DbPools};
use axum::{
    Json,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::{Datelike, Duration, FixedOffset, Utc};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};

use super::write_guard::{enforce_public_write_limit, reject, truncate_chars};

const STATS_WRITE_LIMIT_PER_MINUTE: usize = 120;
const MAX_CHARACTER_NAME_CHARS: usize = 128;
const WEEKLY_RESET_HOUR: i64 = 4;
const WEEKLY_RESET_TZ_OFFSET_SECONDS: i32 = 8 * 60 * 60;

#[derive(Deserialize)]
pub struct LimitQuery {
    limit: Option<i64>,
}

fn normalize_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(30).clamp(1, 100)
}

fn current_week_key() -> String {
    let tz = FixedOffset::east_opt(WEEKLY_RESET_TZ_OFFSET_SECONDS)
        .expect("weekly reset timezone offset is valid");
    let shifted = Utc::now().with_timezone(&tz) - Duration::hours(WEEKLY_RESET_HOUR);
    let week = shifted.iso_week();
    format!("{:04}-W{:02}", week.year(), week.week())
}

fn reset_weekly_count_if_needed(conn: &mut rusqlite::Connection) -> anyhow::Result<()> {
    let current_period = current_week_key();
    let stored_period: Option<String> = conn
        .query_row(
            "SELECT value FROM app_metadata WHERE key = 'weekly_count_period'",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if stored_period.as_deref() == Some(current_period.as_str()) {
        return Ok(());
    }

    let tx = conn.transaction()?;
    tx.execute("DELETE FROM weekly_count", [])?;
    tx.execute(
        "INSERT INTO app_metadata (key, value) VALUES ('weekly_count_period', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [&current_period],
    )?;
    tx.commit()?;
    Ok(())
}

/// POST /api/answer-character-count
/// Increments the count of times a character has been used as an answer.
pub async fn answer_character_count(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Err(response) = enforce_public_write_limit(
        &headers,
        Some(peer_addr),
        "answer-character-count",
        STATS_WRITE_LIMIT_PER_MINUTE,
    ) {
        return response.into_response();
    }

    let char_id = match body.get("characterId").and_then(|v| v.as_i64()) {
        Some(id) if id > 0 => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "characterId must be a number" })),
            )
                .into_response();
        }
        _ => return reject(StatusCode::BAD_REQUEST, "characterId must be positive"),
    };
    let char_name = body
        .get("characterName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let char_name = truncate_chars(&char_name, MAX_CHARACTER_NAME_CHARS);

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT INTO answer_count (id, character_name, count) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET
                 character_name = excluded.character_name,
                 count = answer_count.count + 1",
            rusqlite::params![char_id, char_name],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "message": "Character answer count updated successfully", "characterId": char_id })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/guess-character-count
/// Increments guess_count and weekly_count for a character.
pub async fn guess_character_count(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Err(response) = enforce_public_write_limit(
        &headers,
        Some(peer_addr),
        "guess-character-count",
        STATS_WRITE_LIMIT_PER_MINUTE,
    ) {
        return response.into_response();
    }

    let char_id = match body.get("characterId").and_then(|v| v.as_i64()) {
        Some(id) if id > 0 => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "characterId must be a number" })),
            )
                .into_response();
        }
        _ => return reject(StatusCode::BAD_REQUEST, "characterId must be positive"),
    };
    let char_name = body
        .get("characterName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let char_name = truncate_chars(&char_name, MAX_CHARACTER_NAME_CHARS);

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        reset_weekly_count_if_needed(conn)?;
        conn.execute(
            "INSERT INTO guess_count (id, character_name, count) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET
                 character_name = excluded.character_name,
                 count = guess_count.count + 1",
            rusqlite::params![char_id, &char_name],
        )?;
        conn.execute(
            "INSERT INTO weekly_count (id, character_name, count) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET
                 character_name = excluded.character_name,
                 count = weekly_count.count + 1",
            rusqlite::params![char_id, &char_name],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "message": "Character guess count updated successfully", "characterId": char_id })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// GET /api/character-usage/:id
pub async fn character_usage(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let row = conn.query_row(
            "SELECT id, character_name, count FROM answer_count WHERE id = ?1",
            [id],
            |row| Ok(json!({ "_id": row.get::<_, i64>(0)?, "characterName": row.get::<_, String>(1)?, "count": row.get::<_, i64>(2)? })),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    }).await;

    match result {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Character usage not found" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/subject-added  — POST body: { addedSubjects: [{id, name, name_cn, type}] }
/// Simplified: just acknowledge (subject data lives in archive.sqlite, no write needed).
pub async fn subject_added(Json(_body): Json<Value>) -> impl IntoResponse {
    // In the Rust architecture, subject data is in the read-only archive.sqlite.
    // This endpoint is called by the client to report which subjects appeared in a game;
    // we acknowledge it without persisting (stats can be computed from archive.sqlite).
    Json(json!({ "message": "acknowledged" }))
}

/// GET /api/leaderboard/characters?limit=30
pub async fn leaderboard_characters(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = normalize_limit(q.limit);
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, character_name, count FROM answer_count WHERE count > 0 ORDER BY count DESC LIMIT ?1",
        )?;
        let rows: Vec<Value> = stmt.query_map([limit], |row| {
            let id = row.get::<_, i64>(0)?;
            Ok(json!({
                "_id": id,
                "characterName": row.get::<_, String>(1)?,
                "count": row.get::<_, i64>(2)?,
                "image": format!("/img/{}.webp", id),
            }))
        })?.filter_map(Result::ok).collect();
        Ok(rows)
    }).await;

    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/leaderboard/guesses?limit=30
pub async fn leaderboard_guesses(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = normalize_limit(q.limit);
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, character_name, count FROM guess_count WHERE count > 0 ORDER BY count DESC LIMIT ?1",
        )?;
        let rows: Vec<Value> = stmt.query_map([limit], |row| {
            let id = row.get::<_, i64>(0)?;
            Ok(json!({
                "_id": id,
                "characterName": row.get::<_, String>(1)?,
                "count": row.get::<_, i64>(2)?,
                "image": format!("/img/{}.webp", id),
            }))
        })?.filter_map(Result::ok).collect();
        Ok(rows)
    }).await;

    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/leaderboard/weekly?limit=30
/// Returns top characters ranked by the current weekly_count bucket.
pub async fn leaderboard_weekly(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = normalize_limit(q.limit);
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        reset_weekly_count_if_needed(conn)?;
        let mut stmt = conn.prepare(
            "SELECT id, character_name, count FROM weekly_count WHERE count > 0 ORDER BY count DESC LIMIT ?1",
        )?;
        let rows: Vec<Value> = stmt.query_map([limit], |row| {
            let id = row.get::<_, i64>(0)?;
            Ok(json!({
                "_id": id,
                "characterName": row.get::<_, String>(1)?,
                "count": row.get::<_, i64>(2)?,
                "image": format!("/img/{}.webp", id),
            }))
        })?.filter_map(Result::ok).collect();
        Ok(rows)
    }).await;

    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /redeem?code=XXX
pub async fn redeem(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = match q.get("code") {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Code is required" })),
            )
                .into_response();
        }
    };

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let row = conn.query_row(
            "SELECT avatar_id, avatar_image FROM redeem_codes WHERE code = ?1",
            [&code],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    })
    .await;

    match result {
        Ok(Some((avatar_id, avatar_image))) => {
            Json(json!({ "avatarId": avatar_id, "avatarImage": avatar_image })).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Invalid or expired code" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
