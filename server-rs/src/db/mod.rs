use crate::config::Config;
use anyhow::Context;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::sync::Arc;

// ─── Archive rows ─────────────────────────────────────────────────────────────

pub struct SubjectRow {
    pub id: i64,
    pub role: i64,  // 1 = main, 2 = supporting
    pub stype: i64, // subject type (2=anime, 4=game, …)
    pub year: i32,
    pub score: f64,
    pub collects: i64,
    pub name: String,
    pub name_cn: String,
    /// Serialised tags array from subjects.tags_json
    pub tags_json: String,
    /// Serialised meta_tags array from subjects.meta_tags_json
    pub meta_tags_json: String,
}

pub struct CharacterDoc {
    pub id: i64,
    pub name: String,
    pub infobox: String,
    pub summary: String,
    pub collects: i64,
    pub comments: i64,
}

// ─── DbPools ──────────────────────────────────────────────────────────────────

pub struct DbPools {
    pub archive_db: Pool<SqliteConnectionManager>,
    pub app_db: Pool<SqliteConnectionManager>,
    /// Directory for locally cached/transcoded character images.
    pub image_cache_dir: String,
}

pub async fn init_pools(config: &Config) -> anyhow::Result<Arc<DbPools>> {
    // Ensure WAL mode is set before spinning up concurrent connections in the pool
    {
        let conn = Connection::open(&config.app_db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }

    // Connect to archive DB (read-only). Keep SQLite page cache modest; hot paths query
    // indexed rows directly instead of materialising archive.sqlite into process memory.
    let archive_manager = SqliteConnectionManager::file(&config.archive_db_path)
        .with_flags(rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_init(|c| {
            c.pragma_update(None, "busy_timeout", "10000")?;
            c.pragma_update(None, "cache_size", "-8192")?; // 8 MB page cache per connection
            c.pragma_update(None, "mmap_size", "0")?;
            c.pragma_update(None, "temp_store", "MEMORY")
        });
    let archive_db = Pool::builder()
        .max_size(8)
        .connection_timeout(std::time::Duration::from_secs(30))
        .build(archive_manager)?;

    // Connect to app DB (read/write)
    let app_manager = SqliteConnectionManager::file(&config.app_db_path).with_init(|c| {
        c.pragma_update(None, "busy_timeout", "5000")?;
        c.pragma_update(None, "synchronous", "NORMAL")
    });
    let app_db = Pool::builder().max_size(8).build(app_manager)?;

    // Create app DB tables if they don't exist
    {
        let app_conn = app_db.get()?;
        init_app_schema(&app_conn)?;
    }

    tracing::info!("Archive DB ready in low-memory on-demand mode");

    Ok(Arc::new(DbPools {
        archive_db,
        app_db,
        image_cache_dir: config.image_cache_dir.clone(),
    }))
}

pub async fn with_archive_db<T, F>(pools: Arc<DbPools>, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
{
    let archive_db = pools.archive_db.clone();
    tokio::task::spawn_blocking(move || {
        let mut conn = archive_db
            .get()
            .map_err(|e| anyhow::anyhow!("archive_db pool error: {}", e))?;
        f(&mut conn)
    })
    .await
    .context("archive_db spawn_blocking join failed")?
}

pub async fn with_app_db<T, F>(pools: Arc<DbPools>, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
{
    let app_db = pools.app_db.clone();
    tokio::task::spawn_blocking(move || {
        let mut conn = app_db
            .get()
            .map_err(|e| anyhow::anyhow!("app_db pool error: {}", e))?;
        f(&mut conn)
    })
    .await
    .context("app_db spawn_blocking join failed")?
}

fn init_app_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS image_cache (
            id TEXT PRIMARY KEY,
            local_path TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        -- Persisted mirror cache for character image source URLs (from legacy JSON or BGM API fallback).
        CREATE TABLE IF NOT EXISTS character_image_sources (
            character_id INTEGER PRIMARY KEY,
            image_medium TEXT NOT NULL DEFAULT '',
            image_grid TEXT NOT NULL DEFAULT '',
            fetched_at_ms INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT '' -- e.g. 'json' | 'bgm'
        );
        -- Persisted mirror cache for subject (anime/game) image source URLs from BGM API fallback.
        CREATE TABLE IF NOT EXISTS subject_image_sources (
            subject_id INTEGER PRIMARY KEY,
            image_medium TEXT NOT NULL DEFAULT '',
            image_grid TEXT NOT NULL DEFAULT '',
            fetched_at_ms INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT '' -- e.g. 'bgm_image' | 'bgm'
        );
        -- Persisted mirror cache for character voice actors (animeVAs), stored as JSON array of names.
        CREATE TABLE IF NOT EXISTS character_vas (
            character_id INTEGER PRIMARY KEY,
            va_names_json TEXT NOT NULL DEFAULT '[]',
            fetched_at_ms INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT '' -- e.g. 'dump' | 'bgm'
        );
        CREATE TABLE IF NOT EXISTS leaderboard (
            user_id TEXT PRIMARY KEY,
            username TEXT NOT NULL DEFAULT '',
            score INTEGER NOT NULL DEFAULT 0,
            games_played INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS answer_count (
            id INTEGER PRIMARY KEY,
            character_name TEXT NOT NULL DEFAULT '',
            count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS guess_count (
            id INTEGER PRIMARY KEY,
            character_name TEXT NOT NULL DEFAULT '',
            count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS weekly_count (
            id INTEGER PRIMARY KEY,
            character_name TEXT NOT NULL DEFAULT '',
            count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS redeem_codes (
            code TEXT PRIMARY KEY,
            avatar_id TEXT NOT NULL DEFAULT '',
            avatar_image TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS character_tags (
            id INTEGER PRIMARY KEY,
            tag_counts TEXT NOT NULL DEFAULT '{}'
        );
        CREATE TABLE IF NOT EXISTS game_character_tags (
            subject_id INTEGER PRIMARY KEY,
            tags_json TEXT NOT NULL DEFAULT '{}'
        );
        CREATE TABLE IF NOT EXISTS new_tags (
            id INTEGER PRIMARY KEY,
            tag_counts TEXT NOT NULL DEFAULT '{}'
        );
        CREATE TABLE IF NOT EXISTS bug_feedback (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            bug_type TEXT NOT NULL,
            description TEXT NOT NULL,
            logs TEXT,
            errors TEXT,
            diagnostic_data TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        "
    )?;
    Ok(())
}
