use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub archive_db_path: String,
    pub app_db_path: String,
}

pub fn load_config() -> Config {
    // Load from environment or use default development values
    Config {
        port: env::var("PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .expect("PORT must be a valid u16"),
        archive_db_path: env::var("ARCHIVE_DB_PATH")
            .unwrap_or_else(|_| "../archive.sqlite".to_string()),
        app_db_path: env::var("APP_DB_PATH")
            .unwrap_or_else(|_| "app.sqlite".to_string()),
    }
}
