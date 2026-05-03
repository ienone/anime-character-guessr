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
    static ref PENDING_DOWNLOADS: DashMap<String, broadcast::Sender<bool>> = DashMap::new();
}

struct CleanupPending(String, broadcast::Sender<bool>);
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
/// Runs every 5 minutes.
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
///
/// `cache_key` is the row id used in `image_cache` (e.g. `"123"` for a
/// character, `"s:123"` for a subject). It is also used to derive the
/// on-disk filename (colons replaced with underscores).
pub async fn download_and_cache_image(cache_key: String, url: String, pools: Arc<DbPools>) {
    // Coalesce duplicate requests
    let rx_opt = {
        if let Some(entry) = PENDING_DOWNLOADS.get(&cache_key) {
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
    PENDING_DOWNLOADS.insert(cache_key.clone(), tx.clone());
    let _cleanup = CleanupPending(cache_key.clone(), tx);

    // Check if another task already cached it to avoid duplicate work
    let key_lookup = cache_key.clone();
    let already_cached = db::with_app_db(Arc::clone(&pools), move |conn| {
        let mut stmt = conn.prepare("SELECT 1 FROM image_cache WHERE id = ?1")?;
        Ok(stmt.exists([key_lookup]).unwrap_or(false))
    })
    .await
    .unwrap_or(false);

    if already_cached {
        return;
    }

    info!("Downloading image for {}: {}", cache_key, url);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to build reqwest client for image {}: {}", cache_key, e);
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
            warn!("Retrying image download for {} after error: {}", cache_key, last_err.as_ref().unwrap());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    let resp = match resp_opt {
        Some(r) => r,
        None => {
            error!(
                "Failed to fetch image for {}: {}",
                cache_key,
                last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown error".to_string())
            );
            return;
        }
    };

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to read image bytes for {}: {}", cache_key, e);
            return;
        }
    };

    // Filename: replace ':' with '_' for safe paths (e.g. "s:123" → "s_123.webp").
    let file_basename = cache_key.replace(':', "_");

    // Transcode in a blocking task — image crate is CPU-bound
    let image_cache_dir = pools.image_cache_dir.clone();
    let transcode_result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let img = image::load_from_memory(&bytes)?;
        let path = format!("{}/{}.webp", image_cache_dir, file_basename);
        img.save_with_format(&path, image::ImageFormat::WebP)?;
        Ok(path)
    })
    .await;

    let path = match transcode_result {
        Ok(Ok(p)) => p,
        _ => {
            error!("Failed to transcode image for {}", cache_key);
            return;
        }
    };

    let key_for_write = cache_key.clone();
    let path_clone = path.clone();
    let write_result = db::with_app_db(Arc::clone(&pools), move |conn| {
        conn.execute(
            "INSERT OR IGNORE INTO image_cache (id, local_path) VALUES (?1, ?2)",
            [key_for_write, path_clone],
        )?;
        Ok(())
    })
    .await;

    match write_result {
        Err(e) => error!("Failed to save cache record for {}: {}", cache_key, e),
        Ok(()) => info!("Cached image for {} at {}", cache_key, path),
    }
}

/// Periodically checks the image cache size and removes the oldest entries.
///
/// Disabled for now: cached files are intentionally tiny 50x50 WebP thumbnails,
/// so keeping all generated thumbnails is cheaper than repeatedly refetching
/// them from BGM.
pub fn start_image_cache_cleanup(pools: Arc<DbPools>) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(60 * 60); // every 1 hour
        loop {
            tokio::time::sleep(interval).await;
            let count = db::with_app_db(Arc::clone(&pools), move |conn| {
                let mut stmt = conn.prepare("SELECT count(*) FROM image_cache")?;
                let count: i64 = stmt.query_row([], |row| row.get(0))?;
                Ok(count)
            }).await.unwrap_or_default();
            info!("Image thumbnail cache cleanup skipped; {} thumbnails retained", count);
        }
    });
}
