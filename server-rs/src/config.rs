use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub archive_db_path: String,
    pub app_db_path: String,
    /// Directory for locally cached / transcoded character images.
    pub image_cache_dir: String,
    /// Offline character image URL mapping JSON (server/data/character_images.json).
    pub character_images_path: String,
}

pub fn load_config() -> Config {
    Config {
        port: env::var("PORT")
            .unwrap_or_else(|_| "3001".to_string())
            .parse()
            .expect("PORT must be a valid u16"),
        archive_db_path: env::var("ARCHIVE_DB_PATH")
            .unwrap_or_else(|_| "../archive.sqlite".to_string()),
        app_db_path: env::var("APP_DB_PATH")
            .unwrap_or_else(|_| "data/app.sqlite".to_string()),
        image_cache_dir: env::var("IMAGE_CACHE_DIR")
            .unwrap_or_else(|_| "data/images".to_string()),
        character_images_path: env::var("CHARACTER_IMAGES_PATH")
            .unwrap_or_else(|_| "assets/character_images.json".to_string()),
    }
}
