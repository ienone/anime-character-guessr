use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use crate::config::Config;

#[derive(Clone)]
pub struct DbPools {
    pub archive_db: Arc<Mutex<Connection>>,
    pub app_db: Arc<Mutex<Connection>>,
}

pub async fn init_pools(config: &Config) -> anyhow::Result<Arc<DbPools>> {
    // Connect to archive DB (read-only)
    let archive_conn = Connection::open_with_flags(
        &config.archive_db_path, 
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
    )?;
    
    // Connect to app DB (read/write)
    let app_conn = Connection::open(&config.app_db_path)?;
    
    // Configure app DB with WAL mode for better concurrency
    app_conn.pragma_update(None, "journal_mode", "WAL")?;
    app_conn.pragma_update(None, "synchronous", "NORMAL")?;

    // Create app DB tables if they don't exist
    init_app_schema(&app_conn)?;

    Ok(Arc::new(DbPools {
        archive_db: Arc::new(Mutex::new(archive_conn)),
        app_db: Arc::new(Mutex::new(app_conn)),
    }))
}

fn init_app_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS image_cache (
            id TEXT PRIMARY KEY,
            local_path TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE IF NOT EXISTS leaderboard (
            user_id TEXT PRIMARY KEY,
            score INTEGER NOT NULL,
            games_played INTEGER DEFAULT 0
        );
        "
    )?;
    Ok(())
}
