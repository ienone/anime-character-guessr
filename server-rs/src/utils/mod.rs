use std::sync::Arc;
use crate::db::DbPools;
use crate::db;
use crate::socket::state::ServerState;
use socketioxide::SocketIo;
use serde_json::json;
use tracing::{error, info, warn};
use dashmap::DashMap;
use tokio::sync::broadcast;
use lazy_static::lazy_static;
use std::time::Duration;

lazy_static! {
    static ref PENDING_DOWNLOADS: DashMap<i64, broadcast::Sender<bool>> = DashMap::new();
}

struct CleanupPending(i64, broadcast::Sender<bool>);
impl Drop for CleanupPending {
    fn drop(&mut self) {
        PENDING_DOWNLOADS.remove(&self.0);
        let _ = self.1.send(true);
    }
}

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
    // Coalesce duplicate requests
    let rx_opt = {
        if let Some(entry) = PENDING_DOWNLOADS.get(&id) {
            Some(entry.value().subscribe())
        } else {
            None
        }
    };

    if let Some(mut rx) = rx_opt {
        // A task is already downloading this image, wait for it
        let _ = rx.recv().await;
        return;
    }

    let (tx, _rx) = broadcast::channel(1);
    PENDING_DOWNLOADS.insert(id, tx.clone());
    let _cleanup = CleanupPending(id, tx);

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

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to build reqwest client for image {}: {}", id, e);
            return;
        }
    };

    let mut last_err: Option<anyhow::Error> = None;
    let mut resp_opt: Option<reqwest::Response> = None;
    for attempt in 0..=1 {
        match client.get(&url).send().await {
            Ok(r) => {
                match r.error_for_status() {
                    Ok(ok) => {
                        resp_opt = Some(ok);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e.into());
                    }
                }
            }
            Err(e) => last_err = Some(e.into()),
        }
        if attempt == 0 {
            warn!("Retrying image download for {} after error: {}", id, last_err.as_ref().unwrap());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    let resp = match resp_opt {
        Some(r) => r,
        None => {
            error!(
                "Failed to fetch image for {}: {}",
                id,
                last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown error".to_string())
            );
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
        img.save_with_format(&path, image::ImageFormat::WebP)?;
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

/// Periodically checks the image cache size and removes the oldest entries
/// if it exceeds a threshold (e.g. 5000 images).
pub fn start_image_cache_cleanup(pools: Arc<DbPools>) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(60 * 60); // every 1 hour
        let max_cache_size = 5000;
        loop {
            tokio::time::sleep(interval).await;
            info!("Running image cache cleanup task...");

            let pools_clone = Arc::clone(&pools);
            let to_delete = db::with_app_db(pools_clone, move |conn| {
                let mut stmt = conn.prepare("SELECT count(*) FROM image_cache")?;
                let count: i64 = stmt.query_row([], |row| row.get(0))?;

                if count <= max_cache_size {
                    return Ok(vec![]);
                }

                let limit = count - max_cache_size;
                let mut stmt2 = conn.prepare("SELECT id, local_path FROM image_cache ORDER BY created_at ASC LIMIT ?1")?;
                let rows = stmt2.query_map([limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?.filter_map(Result::ok).collect::<Vec<_>>();
                Ok(rows)
            }).await.unwrap_or_default();

            let mut deleted_count = 0;
            for (_, path) in &to_delete {
                if tokio::fs::remove_file(path).await.is_ok() || !std::path::Path::new(path).exists() {
                    deleted_count += 1;
                }
            }

            if deleted_count > 0 {
                let pools_clone = Arc::clone(&pools);
                let ids: Vec<String> = to_delete.into_iter().map(|(id, _)| id).collect();
                let _ = db::with_app_db(pools_clone, move |conn| {
                    let tx = conn.transaction()?;
                    {
                        let mut stmt = tx.prepare("DELETE FROM image_cache WHERE id = ?1")?;
                        for id in ids {
                            let _ = stmt.execute([id]);
                        }
                    }
                    tx.commit()?;
                    Ok(())
                }).await;

                info!("Auto-cleanup: removed {} old cached images", deleted_count);
            }
        }
    });
}
