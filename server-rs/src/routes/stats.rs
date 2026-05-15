use crate::db::{self, DbPools};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{Datelike, Duration, FixedOffset, Utc};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use super::write_guard::truncate_chars;

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

fn normalize_character_name(name: impl Into<String>) -> String {
    let name = name.into();
    truncate_chars(name.trim(), MAX_CHARACTER_NAME_CHARS)
}

async fn increment_answer_character_count(
    pools: Arc<DbPools>,
    char_id: i64,
    char_name: String,
) -> anyhow::Result<()> {
    db::with_app_db(pools, move |conn| {
        conn.execute(
            "INSERT INTO answer_count (id, character_name, count) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET
                 character_name = excluded.character_name,
                 count = answer_count.count + 1",
            rusqlite::params![char_id, char_name],
        )?;
        Ok(())
    })
    .await
}

async fn increment_guess_character_count(
    pools: Arc<DbPools>,
    char_id: i64,
    char_name: String,
) -> anyhow::Result<()> {
    db::with_app_db(pools, move |conn| {
        reset_weekly_count_if_needed(conn)?;
        conn.execute(
            "INSERT INTO weekly_count (id, character_name, count) VALUES (?1, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET
                 character_name = excluded.character_name,
                 count = weekly_count.count + 1",
            rusqlite::params![char_id, &char_name],
        )?;
        Ok(())
    })
    .await
}

pub async fn record_answer_character_count(
    pools: Arc<DbPools>,
    char_id: i64,
    char_name: impl Into<String>,
) {
    if char_id <= 0 {
        return;
    }
    let char_name = normalize_character_name(char_name);
    if let Err(e) = increment_answer_character_count(pools, char_id, char_name).await {
        tracing::warn!(char_id, error = %e, "failed to record answer character count");
    }
}

pub async fn record_guess_character_count(
    pools: Arc<DbPools>,
    char_id: i64,
    char_name: impl Into<String>,
) {
    if char_id <= 0 {
        return;
    }
    let char_name = normalize_character_name(char_name);
    if let Err(e) = increment_guess_character_count(pools, char_id, char_name).await {
        tracing::warn!(char_id, error = %e, "failed to record weekly guess count");
    }
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
