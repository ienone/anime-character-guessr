use std::sync::Arc;
use crate::db::DbPools;
use crate::db;
use crate::socket::state::ServerState;
use socketioxide::SocketIo;
use serde_json::json;
use tracing::{error, info, warn};

/// Creates runtime data directories on first start.
pub fn ensure_directories(image_cache_dir: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(image_cache_dir)?;
    Ok(())
}

/// Spawns a background Tokio task that periodically removes stale rooms.
/// Replaces Node.js autoClean.js — runs every 5 minutes.
/// A room is cleaned if:
///   - No game is in progress, AND
///   - No activity for > 5 minutes (300_000 ms)
pub fn start_room_cleanup(state: Arc<ServerState>, io: SocketIo) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(5 * 60); // every 5 min
        loop {
            tokio::time::sleep(interval).await;

            let now = chrono::Utc::now().timestamp_millis();
            let stale_threshold_ms: i64 = 5 * 60 * 1000; // 5 minutes
            let mut to_remove: Vec<String> = Vec::new();

            for entry in state.rooms.iter() {
                let room = entry.value();
                let idle = now - room.last_active;
                if room.current_game.is_none() && idle > stale_threshold_ms {
                    to_remove.push(entry.key().clone());
                }
            }

            let cleaned = to_remove.len();
            for room_id in to_remove {
                state.rooms.remove(&room_id);
                let _ = io.to(room_id.clone()).emit(
                    "roomClosed",
                    &json!({ "message": "房间因长时间无活动已关闭" }),
                );
                warn!("Auto-cleaned inactive room: {}", room_id);
            }

            if cleaned > 0 {
                info!("Auto-cleanup: removed {} stale rooms", cleaned);
            }
        }
    });
}

/// Downloads `url`, transcodes to WebP, saves locally, and records the
/// cache path in app.sqlite. Silently returns on any error (background task).
pub async fn download_and_cache_image(id: i64, url: String, pools: Arc<DbPools>) {
    // Check if another task already cached it to avoid duplicate work
    let already_cached = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare("SELECT 1 FROM image_cache WHERE id = ?1")?;
        Ok(stmt.exists([id.to_string()]).unwrap_or(false))
    })
    .await
    .unwrap_or(false);

    if already_cached {
        return;
    }

    info!("Downloading image for character {}: {}", id, url);

    let resp = match reqwest::get(&url).await {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to fetch image for {}: {}", id, e);
            return;
        }
    };

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to read image bytes for {}: {}", id, e);
            return;
        }
    };

    // Transcode in a blocking task — image crate is CPU-bound
    let image_cache_dir = pools.image_cache_dir.clone();
    let transcode_result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let img = image::load_from_memory(&bytes)?;
        let path = format!("{}/{}.webp", image_cache_dir, id);
        img.save_with_format(&path, image::ImageFormat::WebP)
           .or_else(|_| img.save_with_format(&path, image::ImageFormat::Jpeg))?;
        Ok(path)
    })
    .await;

    let path = match transcode_result {
        Ok(Ok(p)) => p,
        _ => {
            error!("Failed to transcode image for {}", id);
            return;
        }
    };

    let id_str = id.to_string();
    let path_clone = path.clone();
    let write_result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT OR IGNORE INTO image_cache (id, local_path) VALUES (?1, ?2)",
            [id_str, path_clone],
        )?;
        Ok(())
    })
    .await;

    match write_result {
        Err(e) => error!("Failed to save cache record for {}: {}", id, e),
        Ok(()) => info!("Cached image for character {} at {}", id, path),
    }
}
