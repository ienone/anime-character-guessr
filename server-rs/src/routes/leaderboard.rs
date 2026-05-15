use crate::db::{self, DbPools};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use super::write_guard::{enforce_public_write_limit, reject, validate_chars};

const LEADERBOARD_SUBMIT_LIMIT_PER_MINUTE: usize = 20;
const MAX_USER_ID_CHARS: usize = 128;
const MAX_USERNAME_CHARS: usize = 80;
const MAX_SCORE_DELTA_ABS: i64 = 10_000;

/// A single entry in the leaderboard response.
#[derive(Debug, Serialize)]
pub struct LeaderboardEntry {
    pub user_id: String,
    pub username: String,
    pub score: i64,
    pub games_played: i64,
    pub rank: i64,
}

/// Body for submitting / updating a player's score after a game ends.
#[derive(Debug, Deserialize)]
pub struct ScoreSubmission {
    pub user_id: String,
    pub username: String,
    /// Delta score earned this game (may be negative).
    pub score_delta: i64,
}

/// GET /api/leaderboard
/// Returns the top 50 players ordered by total score.
pub async fn get_leaderboard(State(pools): State<Arc<DbPools>>) -> impl IntoResponse {
    let result = db::with_app_db(Arc::clone(&pools), |conn| {
        let mut stmt = conn.prepare(
            "SELECT user_id, username, score, games_played,
                    ROW_NUMBER() OVER (ORDER BY score DESC) AS rank
             FROM leaderboard
             ORDER BY score DESC
             LIMIT 50",
        )?;
        let entries: Vec<LeaderboardEntry> = stmt
            .query_map([], |row| {
                Ok(LeaderboardEntry {
                    user_id: row.get(0)?,
                    username: row.get(1)?,
                    score: row.get(2)?,
                    games_played: row.get(3)?,
                    rank: row.get(4)?,
                })
            })?
            .filter_map(Result::ok)
            .collect();
        Ok(entries)
    })
    .await;

    match result {
        Ok(entries) => Json(json!({ "leaderboard": entries })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/leaderboard/submit
/// Upserts a player's score contribution from a finished game.
/// Called internally by the socket game-end handler (or from tests).
pub async fn submit_score(
    State(pools): State<Arc<DbPools>>,
    headers: HeaderMap,
    Json(body): Json<ScoreSubmission>,
) -> impl IntoResponse {
    if let Err(response) = enforce_public_write_limit(
        &headers,
        "leaderboard-submit",
        LEADERBOARD_SUBMIT_LIMIT_PER_MINUTE,
    ) {
        return response.into_response();
    }

    if body.user_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "user_id is required" })),
        )
            .into_response();
    }
    if let Err(response) = validate_chars(&body.user_id, "user_id", MAX_USER_ID_CHARS) {
        return response.into_response();
    }
    if let Err(response) = validate_chars(&body.username, "username", MAX_USERNAME_CHARS) {
        return response.into_response();
    }
    if body.score_delta < -MAX_SCORE_DELTA_ABS || body.score_delta > MAX_SCORE_DELTA_ABS {
        return reject(StatusCode::BAD_REQUEST, "score_delta is out of range");
    }

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        // Upsert: insert or update score + games_played
        conn.execute(
            "INSERT INTO leaderboard (user_id, username, score, games_played)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(user_id) DO UPDATE SET
                 username     = excluded.username,
                 score        = leaderboard.score + excluded.score,
                 games_played = leaderboard.games_played + 1",
            rusqlite::params![body.user_id, body.username, body.score_delta],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
