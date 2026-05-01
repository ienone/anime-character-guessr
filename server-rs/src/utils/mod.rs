use std::sync::Arc;
use crate::db::DbPools;
use tracing::{error, info};

pub fn ensure_directories() -> anyhow::Result<()> {
    // Ensure data/images exist for local image cache
    std::fs::create_dir_all("data/images")?;
    Ok(())
}

pub async fn download_and_cache_image(id: i64, url: String, pools: Arc<DbPools>) {
    // Check if another task already cached it to avoid race conditions
    {
        let app_db = pools.app_db.lock().unwrap();
        let mut stmt = app_db.prepare("SELECT 1 FROM image_cache WHERE id = ?").unwrap();
        if stmt.exists([id.to_string()]).unwrap_or(false) {
            return;
        }
    }

    info!("Downloading image for character {}: {}", id, url);

    // 1. Download image
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

    // 2. Transcode to WebP in a blocking task
    let transcode_result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let img = image::load_from_memory(&bytes)?;
        let path = format!("data/images/{}.webp", id);
        
        // Write the actual file
        img.save_with_format(&path, image::ImageFormat::WebP)
           .or_else(|_| img.save_with_format(&path, image::ImageFormat::Jpeg))?;

        Ok(path)
    }).await;

    let path = match transcode_result {
        Ok(Ok(p)) => p,
        _ => {
            error!("Failed to transcode image for {}", id);
            return;
        }
    };

    // 3. Save to database
    let app_db = pools.app_db.lock().unwrap();
    if let Err(e) = app_db.execute(
        "INSERT OR IGNORE INTO image_cache (id, local_path) VALUES (?1, ?2)",
        [id.to_string(), path.clone()]
    ) {
        error!("Failed to save cache record for {}: {}", id, e);
    } else {
        info!("Successfully cached image for character {} at {}", id, path);
    }
}
