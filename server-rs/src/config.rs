use anyhow::Context;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub archive_db_path: String,
    pub app_db_path: String,
    pub tantivy_index_dir: String,
    /// Directory for locally cached / transcoded character images.
    pub image_cache_dir: String,
    /// Comma-separated list of allowed CORS origins. Empty/`*` allows any.
    pub client_url: String,
}

pub fn load_config() -> anyhow::Result<Config> {
    Ok(Config {
        port: env::var("PORT")
            .unwrap_or_else(|_| "3001".to_string())
            .parse()
            .context("PORT must be a valid u16")?,
        archive_db_path: env::var("ARCHIVE_DB_PATH")
            .unwrap_or_else(|_| "../archive.sqlite".to_string()),
        app_db_path: env::var("APP_DB_PATH").unwrap_or_else(|_| "data/app.sqlite".to_string()),
        tantivy_index_dir: env::var("TANTIVY_INDEX_DIR")
            .unwrap_or_else(|_| "data/tantivy".to_string()),
        image_cache_dir: env::var("IMAGE_CACHE_DIR").unwrap_or_else(|_| "data/images".to_string()),
        client_url: env::var("CLIENT_URL").unwrap_or_else(|_| {
            "http://localhost:5173,http://localhost:3000,http://localhost".to_string()
        }),
    })
}
