use crate::db::{self, DbPools};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

/// A single entry in the leaderboard response.
#[derive(Debug, Serialize)]
pub struct LeaderboardEntry {
    pub user_id: String,
    pub username: String,
    pub score: i64,
    pub games_played: i64,
    pub rank: i64,
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
