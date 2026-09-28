use crate::db::{self, DbPools};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};

use super::write_guard::{enforce_public_write_limit, reject, truncate_chars, validate_chars};

const BUG_FEEDBACK_LIMIT_PER_MINUTE: usize = 6;
const MAX_BUG_DESCRIPTION_CHARS: usize = 4_000;
const MAX_BUG_BLOB_CHARS: usize = 60_000;

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
