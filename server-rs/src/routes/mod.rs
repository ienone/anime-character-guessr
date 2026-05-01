use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use axum::http::{StatusCode, header};
use serde_json::{json, Value};
use std::sync::Arc;
use crate::db::DbPools;
use crate::utils;
use rand::seq::IndexedRandom; // for rand 0.9.x

pub fn api_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        .route("/game/random", post(get_random_character))
        .with_state(pools)
}

pub fn image_routes(pools: Arc<DbPools>) -> Router {
    Router::new()
        .route("/{id}", get(get_character_image))
        .with_state(pools)
}

// --- Route Handlers ---

async fn get_random_character(
    State(pools): State<Arc<DbPools>>,
    Json(_settings): Json<Value>,
) -> impl IntoResponse {
    // 1. Pick a random character ID
    let char_id = {
        let mut rng = rand::rng();
        match pools.character_ids.choose(&mut rng) {
            Some(&id) => id,
            None => return Json(json!({ "error": "No characters loaded" })).into_response(),
        }
    };

    let archive_db = pools.archive_db.lock().unwrap();

    // 2. Fetch character details
    let mut char_stmt = archive_db.prepare("SELECT raw_json FROM characters WHERE id = ?").unwrap();
    let char_raw: String = char_stmt.query_row([char_id], |row| row.get(0)).unwrap_or_default();
    
    let mut character: Value = serde_json::from_str(&char_raw).unwrap_or(json!({}));

    // 3. Fetch appearances (subjects)
    let mut sub_stmt = archive_db.prepare(
        "SELECT s.raw_json 
         FROM subject_characters sc 
         JOIN subjects s ON sc.subject_id = s.id 
         WHERE sc.character_id = ?
         ORDER BY s.collects DESC" // Most popular first
    ).unwrap();
    
    let mut subjects = Vec::new();
    let mut rows = sub_stmt.query([char_id]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let sub_raw: String = row.get(0).unwrap();
        if let Ok(sub_val) = serde_json::from_str::<Value>(&sub_raw) {
            subjects.push(sub_val);
        }
    }

    // Align with Bangumi frontend expected format
    character["id"] = json!(char_id);
    character["appearances"] = json!(subjects);
    
    Json(character).into_response()
}

async fn get_character_image(
    State(pools): State<Arc<DbPools>>,
    Path(id_str): Path<String>,
) -> impl IntoResponse {
    let id: i64 = id_str.split('.').next().unwrap_or("0").parse().unwrap_or(0);
    
    if id == 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // 1. Check local cache
    let local_path: Option<String> = {
        let app_db = pools.app_db.lock().unwrap();
        let stmt_res = app_db.prepare("SELECT local_path FROM image_cache WHERE id = ?");
        if let Ok(mut stmt) = stmt_res {
            stmt.query_row([id.to_string()], |row| row.get(0)).ok()
        } else {
            None
        }
    };

    if let Some(path) = local_path {
        if let Ok(content) = tokio::fs::read(&path).await {
            let mut headers = header::HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "image/webp".parse().unwrap());
            headers.insert(header::CACHE_CONTROL, "public, max-age=31536000".parse().unwrap());
            return (headers, content).into_response();
        }
    }

    // 2. Cache miss, query original URL
    let original_url: Option<String> = {
        let archive_db = pools.archive_db.lock().unwrap();
        if let Ok(mut stmt) = archive_db.prepare("SELECT raw_json FROM characters WHERE id = ?") {
            if let Ok(raw) = stmt.query_row([id], |row| row.get::<_, String>(0)) {
                if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                    val.get("images").and_then(|imgs| {
                        imgs.get("large")
                            .or_else(|| imgs.get("medium"))
                            .or_else(|| imgs.get("grid"))
                            .and_then(|u| u.as_str().map(|s| s.to_string()))
                    })
                } else { None }
            } else { None }
        } else { None }
    };

    let target_url = original_url.unwrap_or_else(|| "https://lain.bgm.tv/pic/user/l/icon.jpg".to_string());

    // 3. Start background download and transcode task
    let pools_clone = Arc::clone(&pools);
    let target_url_clone = target_url.clone();
    tokio::spawn(async move {
        utils::download_and_cache_image(id, target_url_clone, pools_clone).await;
    });

    // 4. Redirect to the original URL immediately
    Redirect::temporary(&target_url).into_response()
}
