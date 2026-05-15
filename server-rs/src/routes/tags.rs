use crate::db::{self, DbPools};
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};

use super::write_guard::{enforce_public_write_limit, reject, truncate_chars, validate_chars};

const BUG_FEEDBACK_LIMIT_PER_MINUTE: usize = 6;
const MAX_BUG_DESCRIPTION_CHARS: usize = 4_000;
const MAX_BUG_BLOB_CHARS: usize = 60_000;

/// GET /api/character-tags/:id
pub async fn get_character_tags(
    State(pools): State<Arc<DbPools>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let row = conn.query_row(
            "SELECT tag_counts FROM character_tags WHERE id = ?1",
            [id],
            |row| row.get::<_, String>(0),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    })
    .await;

    match result {
        Ok(Some(v)) => {
            let tags: Value = serde_json::from_str(&v).unwrap_or(json!({}));
            Json(json!({ "_id": id, "tagCounts": tags })).into_response()
        }
        Ok(None) => Json(json!({ "_id": id, "tagCounts": {} })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/game-character-tags/:subjectId
pub async fn get_game_character_tags(
    State(pools): State<Arc<DbPools>>,
    Path(subject_id): Path<i64>,
) -> impl IntoResponse {
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let row = conn.query_row(
            "SELECT tags_json FROM game_character_tags WHERE subject_id = ?1",
            [subject_id],
            |row| row.get::<_, String>(0),
        );
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    })
    .await;

    match result {
        Ok(Some(v)) => {
            let tags: Value = serde_json::from_str(&v).unwrap_or(json!({}));
            Json(json!({ "subjectId": subject_id, "characters": tags })).into_response()
        }
        Ok(None) => Json(json!({ "subjectId": subject_id, "characters": {} })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/bug-feedback
pub async fn bug_feedback(
    State(pools): State<Arc<DbPools>>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Err(response) = enforce_public_write_limit(
        &headers,
        Some(peer_addr),
        "bug-feedback",
        BUG_FEEDBACK_LIMIT_PER_MINUTE,
    ) {
        return response.into_response();
    }

    let bug_type = body
        .get("type")
        .or_else(|| body.get("bugType"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .trim();
    if let Err(response) = validate_chars(bug_type, "bugType", 64) {
        return response.into_response();
    }
    let bug_type = bug_type.to_string();

    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if description.is_empty() {
        return reject(StatusCode::BAD_REQUEST, "description is required");
    }
    if let Err(response) = validate_chars(description, "description", MAX_BUG_DESCRIPTION_CHARS) {
        return response.into_response();
    }
    let description = description.to_string();

    let logs = truncate_chars(
        &body.get("logs").map(|v| v.to_string()).unwrap_or_default(),
        MAX_BUG_BLOB_CHARS,
    );
    let errors = truncate_chars(
        &body
            .get("errors")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        MAX_BUG_BLOB_CHARS,
    );
    let diagnostic_data = truncate_chars(
        &body
            .get("diagnosticData")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        MAX_BUG_BLOB_CHARS,
    );

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT INTO bug_feedback (bug_type, description, logs, errors, diagnostic_data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![bug_type, description, logs, errors, diagnostic_data],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "message": "Feedback submitted successfully" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
