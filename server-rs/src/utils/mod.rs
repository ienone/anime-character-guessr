use crate::db;
use crate::db::DbPools;
use crate::socket::state::ServerState;
use crate::socket::{broadcast_lobby_rooms_updated, emit_to_room};
use dashmap::DashMap;
use lazy_static::lazy_static;
use serde_json::json;
use socketioxide::SocketIo;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

lazy_static! {
    static ref PENDING_DOWNLOADS: DashMap<String, broadcast::Sender<bool>> = DashMap::new();
    static ref IMAGE_DOWNLOAD_SEMAPHORE: Semaphore = Semaphore::new(8);
}

const MEDIUM_IMAGE_CACHE_TTL_DAYS: i64 = 7;

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

            for (room_id, room) in state.room_snapshots().await {
                let idle = now - room.last_active;
                if room.current_game.is_none() && idle > stale_threshold_ms {
                    to_remove.push(room_id);
                }
            }

            let cleaned = to_remove.len();
            for room_id in to_remove {
                state.remove_room(&room_id).await;
                emit_to_room(
                    &io,
                    room_id.clone(),
                    "roomClosed",
                    json!({ "message": "房间因长时间无活动已关闭" }),
                );
                warn!("Auto-cleaned inactive room: {}", room_id);
            }

            if cleaned > 0 {
                broadcast_lobby_rooms_updated(&io);
                info!("Auto-cleanup: removed {} stale rooms", cleaned);
            }
        }
    });
}

/// Periodically proves the Tokio runtime is making progress and records room
/// cardinality. If the async runtime is blocked, the next tick logs the lag
/// after it resumes.
pub fn start_runtime_watchdog(state: Arc<ServerState>) {
    tokio::spawn(async move {
        let mut last_tick = Instant::now();
        let mut ticks: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let now = Instant::now();
            let elapsed = now.duration_since(last_tick);
            last_tick = now;
            ticks += 1;

            if elapsed > Duration::from_secs(3) {
                warn!(
                    lag_ms = elapsed.as_millis() as u64,
                    rooms = state.room_count(),
                    "runtime watchdog observed event-loop lag"
                );
            }

            if ticks.is_multiple_of(60) {
                let mut players = 0usize;
                let mut active_games = 0usize;
                for (_, room) in state.room_snapshots().await {
                    players += room.players.len();
                    if room.current_game.is_some() {
                        active_games += 1;
                    }
                }
                info!(
                    rooms = state.room_count(),
                    players, active_games, "runtime watchdog heartbeat"
                );
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
    if let Err(e) = tokio::time::timeout(
        Duration::from_secs(12),
        download_and_cache_image_inner(cache_key.clone(), url, pools),
    )
    .await
    {
        warn!("Image download hard-timeout for {}: {}", cache_key, e);
    }
}

async fn download_and_cache_image_inner(cache_key: String, url: String, pools: Arc<DbPools>) {
    // Coalesce duplicate requests
    let rx_opt = {
        PENDING_DOWNLOADS
            .get(&cache_key)
            .map(|entry| entry.value().subscribe())
    };

    if let Some(mut rx) = rx_opt {
        // A task is already downloading this image, wait for it
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        return;
    }

    let (tx, _rx) = broadcast::channel(1);
    PENDING_DOWNLOADS.insert(cache_key.clone(), tx.clone());
    let _cleanup = CleanupPending(cache_key.clone(), tx);

    // Check if another task already cached it to avoid duplicate work. A stale
    // DB row without the file must not block a fresh download.
    let key_lookup = cache_key.clone();
    let cached_path = db::with_app_db(Arc::clone(&pools), move |conn| {
        Ok(conn
            .query_row(
                "SELECT local_path FROM image_cache WHERE id = ?1",
                [key_lookup],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })
    .await
    .ok()
    .flatten();

    if let Some(path) = cached_path
        && tokio::fs::metadata(path).await.is_ok()
    {
        return;
    }

    let Ok(_permit) = IMAGE_DOWNLOAD_SEMAPHORE.acquire().await else {
        warn!("Image download semaphore closed for {}", cache_key);
        return;
    };

    info!("Downloading image for {}: {}", cache_key, url);

    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(6))
        .user_agent(
            "anime-character-guessr/2.0 (https://github.com/hammerlink/anime-character-guessr)",
        )
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!(
                "Failed to build reqwest client for image {}: {}",
                cache_key, e
            );
            return;
        }
    };

    let resp = match client.get(&url).send().await {
        Ok(r) => match r.error_for_status() {
            Ok(ok) => ok,
            Err(e) => {
                error!("Failed to fetch image for {}: {}", cache_key, e);
                return;
            }
        },
        Err(e) => {
            error!("Failed to fetch image for {}: {}", cache_key, e);
            return;
        }
    };

    let bytes = match tokio::time::timeout(Duration::from_secs(4), resp.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            error!("Failed to read image bytes for {}: {}", cache_key, e);
            return;
        }
        Err(_) => {
            error!("Timed out reading image bytes for {}", cache_key);
            return;
        }
    };
    if bytes.len() > 5 * 1024 * 1024 {
        error!(
            "Image response too large for {}: {} bytes",
            cache_key,
            bytes.len()
        );
        return;
    }

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
            "INSERT INTO image_cache (id, local_path)
             VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
                local_path = excluded.local_path,
                created_at = CURRENT_TIMESTAMP",
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

/// Periodically removes short-lived medium image cache entries.
///
/// Grid thumbnails are intentionally retained: they are tiny and used across
/// most views. Medium images are only opportunistic warmups for larger display
/// contexts, so stale `m:*` / `sm:*` rows and files are allowed to expire.
pub fn start_image_cache_cleanup(pools: Arc<DbPools>) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(60 * 60); // every 1 hour
        loop {
            tokio::time::sleep(interval).await;

            let expired_paths = db::with_app_db(Arc::clone(&pools), move |conn| {
                let ttl = format!("-{} days", MEDIUM_IMAGE_CACHE_TTL_DAYS);
                let mut stmt = conn.prepare(
                    "SELECT local_path
                     FROM image_cache
                     WHERE (id LIKE 'm:%' OR id LIKE 'sm:%')
                       AND created_at < datetime('now', ?1)",
                )?;
                let rows = stmt.query_map([ttl.as_str()], |row| row.get::<_, String>(0))?;
                let mut paths = Vec::new();
                for path in rows.flatten() {
                    paths.push(path);
                }

                conn.execute(
                    "DELETE FROM image_cache
                     WHERE (id LIKE 'm:%' OR id LIKE 'sm:%')
                       AND created_at < datetime('now', ?1)",
                    [ttl.as_str()],
                )?;
                Ok(paths)
            })
            .await;

            match expired_paths {
                Ok(paths) => {
                    let count = paths.len();
                    for path in paths {
                        match tokio::fs::remove_file(&path).await {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => {
                                warn!("Failed to remove expired medium image {}: {}", path, e)
                            }
                        }
                    }
                    if count > 0 {
                        info!("Removed {} expired medium image cache entries", count);
                    }
                }
                Err(e) => warn!("Image cache cleanup failed: {}", e),
            }
        }
    });
}
