use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use crate::db::{self, DbPools};

#[derive(Deserialize)]
pub struct LimitQuery {
    limit: Option<i64>,
}

/// POST /api/answer-character-count
/// Increments the count of times a character has been used as an answer.
pub async fn answer_character_count(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let char_id = match body.get("characterId").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "characterId must be a number" }))).into_response(),
    };
    let char_name = body.get("characterName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT INTO answer_count (id, character_name, count) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET
                 character_name = excluded.character_name,
                 count = answer_count.count + 1",
            rusqlite::params![char_id, char_name],
        )?;
        Ok(())
    }).await;

    match result {
        Ok(()) => Json(json!({ "message": "Character answer count updated successfully", "characterId": char_id })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/guess-character-count
/// Increments guess_count and weekly_count for a character.
pub async fn guess_character_count(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let char_id = match body.get("characterId").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "characterId must be a number" }))).into_response(),
    };
    let char_name = body.get("characterName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
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
    }).await;

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
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "Character usage not found" }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
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
    let limit = q.limit.unwrap_or(30).min(100);
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, character_name, count FROM answer_count WHERE count > 0 ORDER BY count DESC LIMIT ?1",
        )?;
        let rows: Vec<Value> = stmt.query_map([limit], |row| {
            Ok(json!({
                "_id": row.get::<_, i64>(0)?,
                "characterName": row.get::<_, String>(1)?,
                "count": row.get::<_, i64>(2)?,
            }))
        })?.filter_map(Result::ok).collect();
        Ok(rows)
    }).await;

    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// GET /api/leaderboard/guesses?limit=30
pub async fn leaderboard_guesses(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(30).min(100);
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, character_name, count FROM guess_count WHERE count > 0 ORDER BY count DESC LIMIT ?1",
        )?;
        let rows: Vec<Value> = stmt.query_map([limit], |row| {
            Ok(json!({
                "_id": row.get::<_, i64>(0)?,
                "characterName": row.get::<_, String>(1)?,
                "count": row.get::<_, i64>(2)?,
            }))
        })?.filter_map(Result::ok).collect();
        Ok(rows)
    }).await;

    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// GET /api/leaderboard/weekly?limit=30
/// Returns top characters from guess_count with their weekly_count overlay.
pub async fn leaderboard_weekly(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<LimitQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(30).min(100);
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        // Top characters by total guesses
        let mut stmt = conn.prepare(
            "SELECT id FROM guess_count WHERE count > 0 ORDER BY count DESC LIMIT ?1",
        )?;
        let top_ids: Vec<i64> = stmt.query_map([limit], |row| row.get(0))?.filter_map(Result::ok).collect();
        if top_ids.is_empty() { return Ok(vec![]); }

        // Weekly counts for those IDs
        let placeholders = top_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id, count FROM weekly_count WHERE id IN ({})", placeholders);
        let mut stmt2 = conn.prepare(&sql)?;
        let weekly_map: std::collections::HashMap<i64, i64> = stmt2
            .query_map(rusqlite::params_from_iter(top_ids.iter()), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .filter_map(Result::ok)
            .collect();

        let rows: Vec<Value> = top_ids.iter().map(|id| {
            json!({ "_id": id, "count": weekly_map.get(id).copied().unwrap_or(0) })
        }).collect();
        Ok(rows)
    }).await;

    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// GET /redeem?code=XXX
pub async fn redeem(
    State(pools): State<Arc<DbPools>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = match q.get("code") {
        Some(c) if !c.is_empty() => c.clone(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "Code is required" }))).into_response(),
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
    }).await;

    match result {
        Ok(Some((avatar_id, avatar_image))) => Json(json!({ "avatarId": avatar_id, "avatarImage": avatar_image })).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "Invalid or expired code" }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}
