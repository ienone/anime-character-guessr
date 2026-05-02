use crate::config::Config;
use anyhow::Context;
use rusqlite::Connection;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;
use std::collections::HashMap;
use serde_json::Value;

// ─── Pre-built in-memory candidate index ─────────────────────────────────────

/// Maps subject_type -> sorted Vec<(year, character_id, subject_collects)>
/// Built once at startup; enables O(n) in-memory filtering of candidates.
pub struct CandidateIndex {
    /// type_id -> Vec<(year: i32, char_id: i64, collects: i64)> sorted by collects desc
    pub by_type: HashMap<i64, Vec<(i32, i64, i64)>>,
    /// All entries across all types (fallback)
    pub all: Vec<(i32, i64, i64)>,
}

// ─── Pre-loaded character data ────────────────────────────────────────────────

/// One subject appearance for a character — only the fields used in assembly.
/// Kept compact to minimise memory footprint (~300 B per row).
#[derive(Clone)]
pub struct SubjectRow {
    pub id: i64,
    pub role: i64,        // 1 = main, 2 = supporting
    pub stype: i64,       // subject type (2=anime, 4=game, …)
    pub year: i32,
    pub score: f64,
    pub collects: i64,
    pub name: String,
    pub name_cn: String,
    /// Serialised tags array from subjects.raw_json["tags"]
    pub tags_json: String,
    /// Serialised meta_tags array from subjects.raw_json["meta_tags"]
    pub meta_tags_json: String,
}

/// Full in-memory snapshot of archive.sqlite needed by assemble_character.
pub struct CharacterCache {
    /// char_id -> parsed raw_json Value
    pub char_json: HashMap<i64, Value>,
    /// char_id -> Vec of subject rows (sorted by collects desc at load time)
    pub char_subjects: HashMap<i64, Vec<SubjectRow>>,
}

// ─── DbPools ──────────────────────────────────────────────────────────────────

pub struct DbPools {
    pub archive_db: Pool<SqliteConnectionManager>,
    pub app_db: Pool<SqliteConnectionManager>,
    pub character_ids: Vec<i64>,
    /// Pre-built candidate index: enables sub-millisecond character filtering
    pub candidate_index: CandidateIndex,
    /// Full in-memory character + subject data: eliminates all DB queries on hot path
    pub character_cache: Arc<CharacterCache>,
    /// Directory for locally cached/transcoded character images.
    pub image_cache_dir: String,
}

pub async fn init_pools(config: &Config) -> anyhow::Result<Arc<DbPools>> {
    // Ensure WAL mode is set before spinning up concurrent connections in the pool
    {
        let conn = Connection::open(&config.app_db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }

    // Connect to archive DB (read-only)
    // mmap_size = 256 MB, cache_size = 128 MB per connection — keeps hot pages in RAM
    let archive_manager = SqliteConnectionManager::file(&config.archive_db_path)
        .with_flags(rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_init(|c| {
            c.pragma_update(None, "busy_timeout", "10000")?;
            c.pragma_update(None, "cache_size", "-131072")?;   // 128 MB page cache
            c.pragma_update(None, "mmap_size", "268435456")?;  // 256 MB mmap
            c.pragma_update(None, "temp_store", "MEMORY")
        });
    let archive_db = Pool::builder()
        .max_size(32)
        .connection_timeout(std::time::Duration::from_secs(30))
        .build(archive_manager)?;

    // Connect to app DB (read/write)
    let app_manager = SqliteConnectionManager::file(&config.app_db_path)
        .with_init(|c| {
            c.pragma_update(None, "busy_timeout", "5000")?;
            c.pragma_update(None, "synchronous", "NORMAL")
        });
    let app_db = Pool::builder().max_size(20).build(app_manager)?;

    // Create app DB tables if they don't exist
    {
        let app_conn = app_db.get()?;
        init_app_schema(&app_conn)?;
    }

    // ── Build all in-memory structures from archive.sqlite ───────────────────
    let archive_conn = archive_db.get()?;

    tracing::info!("Building in-memory character cache (this may take a few seconds)...");
    let t0 = std::time::Instant::now();

    let character_ids = load_character_ids(&archive_conn)?;
    let candidate_index = build_candidate_index(&archive_conn)?;
    let character_cache = Arc::new(build_character_cache(&archive_conn)?);

    tracing::info!(
        "Cache ready in {:.2}s — {} chars, {} candidate entries, {} subject rows cached",
        t0.elapsed().as_secs_f64(),
        character_cache.char_json.len(),
        candidate_index.all.len(),
        character_cache.char_subjects.values().map(|v| v.len()).sum::<usize>(),
    );

    Ok(Arc::new(DbPools {
        archive_db,
        app_db,
        character_ids,
        candidate_index,
        character_cache,
        image_cache_dir: config.image_cache_dir.clone(),
    }))
}

/// Build the in-memory candidate index from archive.sqlite.
/// Runs once at startup; takes ~100-500ms depending on disk speed.
fn build_candidate_index(conn: &Connection) -> anyhow::Result<CandidateIndex> {
    let mut stmt = conn.prepare(
        "SELECT sc.character_id, s.type, s.collects,
                CAST(SUBSTR(s.date, 1, 4) AS INTEGER) as year
         FROM subject_characters sc
         JOIN subjects s ON sc.subject_id = s.id
         WHERE s.nsfw = 0 AND s.date IS NOT NULL AND s.date != ''
         ORDER BY s.collects DESC",
    )?;

    // character_id -> best (year, type, collects) — keep highest-collects subject per char
    let mut char_best: HashMap<i64, (i32, i64, i64)> = HashMap::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,  // character_id
            row.get::<_, i64>(1)?,  // type
            row.get::<_, i64>(2)?,  // collects
            row.get::<_, i32>(3)?,  // year
        ))
    })?;

    for row in rows {
        let (char_id, stype, collects, year) = match row { Ok(v) => v, Err(_) => continue };
        if year <= 0 || year > 2099 { continue; }
        // For each character keep its primary (highest-collects) subject's type
        char_best.entry(char_id).or_insert((year, stype, collects));
    }

    let mut by_type: HashMap<i64, Vec<(i32, i64, i64)>> = HashMap::new();
    let mut all: Vec<(i32, i64, i64)> = Vec::new();

    for (char_id, (year, stype, collects)) in char_best {
        by_type.entry(stype).or_default().push((year, char_id, collects));
        all.push((year, char_id, collects));
    }

    // Sort each bucket by collects descending
    for entries in by_type.values_mut() {
        entries.sort_unstable_by(|a, b| b.2.cmp(&a.2));
    }
    all.sort_unstable_by(|a, b| b.2.cmp(&a.2));

    Ok(CandidateIndex { by_type, all })
}

fn load_character_ids(conn: &Connection) -> anyhow::Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM characters")?;
    let ids = stmt.query_map([], |row| row.get(0))?
        .filter_map(Result::ok)
        .collect::<Vec<i64>>();
    Ok(ids)
}

/// Pre-load ALL character JSON and subject data into memory.
/// Eliminates every DB query from the `assemble_character` hot path.
fn build_character_cache(conn: &Connection) -> anyhow::Result<CharacterCache> {
    // ── 1. Load character raw_json ────────────────────────────────────────────
    let mut char_json: HashMap<i64, Value> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, raw_json FROM characters")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, raw) = row?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                char_json.insert(id, v);
            }
        }
    }

    // ── 2. Load subject_characters + subjects join ────────────────────────────
    // We extract only the fields used in assemble_payload to keep memory compact.
    let mut char_subjects: HashMap<i64, Vec<SubjectRow>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT sc.character_id, sc.type, s.id, s.raw_json, s.collects
             FROM subject_characters sc
             JOIN subjects s ON sc.subject_id = s.id
             ORDER BY s.collects DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,  // character_id
                row.get::<_, i64>(1)?,  // role (sc.type)
                row.get::<_, i64>(2)?,  // subject id
                row.get::<_, String>(3)?,  // subject raw_json
                row.get::<_, i64>(4)?,  // collects
            ))
        })?;

        for row in rows {
            let (char_id, role, sid, raw, collects) = match row { Ok(v) => v, Err(_) => continue };
            let sval: Value = match serde_json::from_str(&raw) { Ok(v) => v, Err(_) => continue };

            let stype = sval.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
            let year = sval.get("date").and_then(|v| v.as_str())
                .and_then(|d| d.split('-').next())
                .and_then(|y| y.parse::<i32>().ok())
                .unwrap_or(-1);
            let score = sval.get("score").and_then(|v| v.as_f64()).unwrap_or(-1.0);
            let name = sval.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name_cn = sval.get("name_cn").and_then(|v| v.as_str()).unwrap_or("").to_string();

            // Serialise tags/meta_tags back to compact strings to avoid storing full Value trees
            let tags_json = sval.get("tags")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let meta_tags_json = sval.get("meta_tags")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());

            char_subjects.entry(char_id).or_default().push(SubjectRow {
                id: sid, role, stype, year, score, collects,
                name, name_cn, tags_json, meta_tags_json,
            });
        }
    }

    Ok(CharacterCache { char_json, char_subjects })
}

pub async fn with_archive_db<T, F>(pools: Arc<DbPools>, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
{
    let archive_db = pools.archive_db.clone();
    tokio::task::spawn_blocking(move || {
        let mut conn = archive_db.get().map_err(|e| anyhow::anyhow!("archive_db pool error: {}", e))?;
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
        let mut conn = app_db.get().map_err(|e| anyhow::anyhow!("app_db pool error: {}", e))?;
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
