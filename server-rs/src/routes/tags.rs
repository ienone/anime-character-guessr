use crate::db::{self, DbPools};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::sync::Arc;

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

/// POST /api/character-tags
pub async fn update_character_tags(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let id = match body.get("_id").and_then(|v| v.as_i64()) {
        Some(i) => i,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing _id" })),
            )
                .into_response();
        }
    };
    let tags = body.get("tagCounts").cloned().unwrap_or(json!({}));
    let tags_str = tags.to_string();

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT INTO character_tags (id, tag_counts) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET tag_counts = excluded.tag_counts",
            rusqlite::params![id, tags_str],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "message": "success" })).into_response(),
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

/// POST /api/game-character-tags
pub async fn update_game_character_tags(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let subject_id = match body.get("subjectId").and_then(|v| v.as_i64()) {
        Some(i) => i,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing subjectId" })),
            )
                .into_response();
        }
    };
    let characters = body.get("characters").cloned().unwrap_or(json!({}));
    let characters_str = characters.to_string();

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT INTO game_character_tags (subject_id, tags_json) VALUES (?1, ?2)
             ON CONFLICT(subject_id) DO UPDATE SET tags_json = excluded.tags_json",
            rusqlite::params![subject_id, characters_str],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "message": "success" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/propose-tags
pub async fn propose_tags(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let id = match body.get("_id").and_then(|v| v.as_i64()) {
        Some(i) => i,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing _id" })),
            )
                .into_response();
        }
    };
    let tags = body.get("tagCounts").cloned().unwrap_or(json!({}));
    let tags_str = tags.to_string();

    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        // Simple JSON update/merge logic would be better, but standard SQLite json functions
        // require newer versions. Assuming replace or complete set for now to match interface.
        conn.execute(
            "INSERT INTO new_tags (id, tag_counts) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET tag_counts = excluded.tag_counts",
            rusqlite::params![id, tags_str],
        )?;
        Ok(())
    })
    .await;

    match result {
        Ok(()) => Json(json!({ "message": "success" })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// POST /api/feedback-tags
pub async fn feedback_tags(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let id = match body.get("characterId").and_then(|v| v.as_i64()) {
        Some(i) => i,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing characterId" })),
            )
                .into_response();
        }
    };
    let update_action = body.get("update").cloned().unwrap_or(json!({}));
    // We would need to update specific tag counts in JSON here.
    // For MVP, we fetch existing, modify in Rust, and save back.
    let result = db::with_app_db(Arc::clone(&pools), move |conn| {
        let tx = conn.transaction()?;
        let current: Option<String> = tx
            .query_row(
                "SELECT tag_counts FROM character_tags WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;

        let mut tags: Value = if let Some(c) = current {
            serde_json::from_str(&c).unwrap_or(json!({}))
        } else {
            json!({})
        };

        // Apply update object (e.g. { "tag1": 1, "tag2": -1 })
        if let Value::Object(ref mut map) = tags {
            if let Value::Object(updates) = update_action {
                for (k, v) in updates {
                    if let Some(delta) = v.as_i64() {
                        let current_val = map.get(&k).and_then(|x| x.as_i64()).unwrap_or(0);
                        map.insert(k, json!(current_val + delta));
                    }
                }
            }
        }

        tx.execute(
            "INSERT INTO character_tags (id, tag_counts) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET tag_counts = excluded.tag_counts",
            rusqlite::params![id, tags.to_string()],
        )?;
        tx.commit()?;
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

/// POST /api/bug-feedback
pub async fn bug_feedback(
    State(pools): State<Arc<DbPools>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let bug_type = body
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let logs = body.get("logs").map(|v| v.to_string()).unwrap_or_default();
    let errors = body
        .get("errors")
        .map(|v| v.to_string())
        .unwrap_or_default();
    let diagnostic_data = body
        .get("diagnosticData")
        .map(|v| v.to_string())
        .unwrap_or_default();

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
