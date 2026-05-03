use crate::config::Config;
use anyhow::Context;
use jieba_rs::Jieba;
use lazy_static::lazy_static;
use pinyin::ToPinyin;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

lazy_static! {
    static ref JIEBA: Jieba = Jieba::new();
}

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
    pub role: i64,  // 1 = main, 2 = supporting
    pub stype: i64, // subject type (2=anime, 4=game, …)
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

// ─── Offline subject search index (segmentation + pinyin) ───────────────────

#[derive(Clone)]
pub struct SubjectDoc {
    pub id: i64,
    pub stype: i64,
    pub date: String,
    pub collects: i64,
    pub name: String,
    pub name_cn: String,
}

pub struct SubjectSearchIndex {
    pub docs: Vec<SubjectDoc>,
    pub postings: HashMap<String, Vec<usize>>, // token -> doc indices
}

impl SubjectSearchIndex {
    pub fn search(&self, keyword: &str, types: &[i64], limit: usize) -> Vec<SubjectDoc> {
        let q = keyword.trim();
        if q.is_empty() || limit == 0 {
            return vec![];
        }
        let q_lc = q.to_lowercase();

        let mut query_tokens = tokenize_query(q);
        // also allow direct substring match on query itself
        query_tokens.insert(q_lc.clone());

        let type_filter: Option<HashSet<i64>> = if types.is_empty() {
            None
        } else {
            Some(types.iter().copied().collect())
        };

        let mut scores: HashMap<usize, i64> = HashMap::new();

        for tok in &query_tokens {
            if tok.len() < 2 {
                continue;
            }
            if let Some(list) = self.postings.get(tok) {
                let w = token_weight(tok, &q_lc);
                for &idx in list {
                    *scores.entry(idx).or_insert(0) += w;
                }
            }
        }

        // Extra boost for substring match (covers non-tokenized partial input)
        if !q_lc.is_empty() {
            for (idx, doc) in self.docs.iter().enumerate() {
                if let Some(ref tf) = type_filter {
                    if !tf.contains(&doc.stype) {
                        continue;
                    }
                }
                let hay_cn = doc.name_cn.to_lowercase();
                let hay = doc.name.to_lowercase();
                if hay_cn.contains(&q_lc) || hay.contains(&q_lc) {
                    *scores.entry(idx).or_insert(0) += 200;
                }
            }
        }

        let mut scored: Vec<(usize, i64)> = scores
            .into_iter()
            .filter(|(idx, _)| {
                if let Some(ref tf) = type_filter {
                    tf.contains(&self.docs[*idx].stype)
                } else {
                    true
                }
            })
            .collect();

        scored.sort_unstable_by(|(a_idx, a_s), (b_idx, b_s)| {
            let a_doc = &self.docs[*a_idx];
            let b_doc = &self.docs[*b_idx];
            b_s.cmp(a_s)
                .then_with(|| b_doc.collects.cmp(&a_doc.collects))
                .then_with(|| a_doc.id.cmp(&b_doc.id))
        });

        scored
            .into_iter()
            .take(limit.min(100))
            .map(|(idx, _)| self.docs[idx].clone())
            .collect()
    }
}

fn token_weight(tok: &str, q_lc: &str) -> i64 {
    if tok == q_lc {
        return 500;
    }
    if tok.len() >= 6 {
        60
    } else if tok.len() >= 3 {
        30
    } else {
        10
    }
}

fn normalize_token(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .replace([' ', '\t', '\n', '\r', '　'], "")
}

fn is_ascii_word(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn tokenize_query(q: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let q_norm = normalize_token(q);
    if q_norm.is_empty() {
        return out;
    }

    // basic splits (latin / symbols)
    for part in q
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .map(normalize_token)
        .filter(|s| !s.is_empty())
    {
        out.insert(part);
    }

    // pinyin expansions: allow searching by full pinyin or initials
    let (py_full, py_initials) = to_pinyin_full_and_initials(&q_norm);
    if !py_full.is_empty() {
        out.insert(py_full);
    }
    if !py_initials.is_empty() {
        out.insert(py_initials);
    }

    // For CJK input, jieba cut adds much better recall
    for w in JIEBA.cut(q, false) {
        let t = normalize_token(w);
        if t.len() >= 2 {
            out.insert(t);
        }
    }

    out
}

fn to_pinyin_full_and_initials(s: &str) -> (String, String) {
    let mut full = String::new();
    let mut initials = String::new();
    for ch in s.chars() {
        if let Some(py) = ch.to_pinyin() {
            let p = py.plain().to_string();
            if let Some(first) = p.chars().next() {
                initials.push(first);
            }
            full.push_str(&p);
        } else if ch.is_ascii_alphabetic() {
            let lower = ch.to_ascii_lowercase();
            full.push(lower);
            initials.push(lower);
        }
    }
    (full, initials)
}

// ─── Offline character search index (segmentation + pinyin) ─────────────────

#[derive(Clone)]
pub struct CharacterDoc {
    pub id: i64,
    pub name: String,
    pub infobox: String,
    pub summary: String,
    pub collects: i64,
    pub comments: i64,
}

pub struct CharacterSearchIndex {
    pub docs: Vec<CharacterDoc>,
    pub postings: HashMap<String, Vec<usize>>, // token -> doc indices
}

impl CharacterSearchIndex {
    pub fn search(&self, keyword: &str, offset: usize, limit: usize) -> Vec<CharacterDoc> {
        let q = keyword.trim();
        if q.is_empty() || limit == 0 {
            return vec![];
        }
        let q_lc = q.to_lowercase();

        let mut query_tokens = tokenize_query(q);
        query_tokens.insert(q_lc.clone());

        let mut scores: HashMap<usize, i64> = HashMap::new();
        for tok in &query_tokens {
            if tok.len() < 2 {
                continue;
            }
            if let Some(list) = self.postings.get(tok) {
                let w = token_weight(tok, &q_lc);
                for &idx in list {
                    *scores.entry(idx).or_insert(0) += w;
                }
            }
        }

        // Substring boost for name/infobox-derived fields.
        if !q_lc.is_empty() {
            for (idx, doc) in self.docs.iter().enumerate() {
                let hay = doc.name.to_lowercase();
                if hay.contains(&q_lc) {
                    *scores.entry(idx).or_insert(0) += 200;
                }
            }
        }

        let mut scored: Vec<(usize, i64)> = scores.into_iter().collect();
        scored.sort_unstable_by(|(a_idx, a_s), (b_idx, b_s)| {
            let a_doc = &self.docs[*a_idx];
            let b_doc = &self.docs[*b_idx];
            b_s.cmp(a_s)
                .then_with(|| {
                    (b_doc.collects + b_doc.comments).cmp(&(a_doc.collects + a_doc.comments))
                })
                .then_with(|| a_doc.id.cmp(&b_doc.id))
        });

        scored
            .into_iter()
            .skip(offset)
            .take(limit.min(50))
            .map(|(idx, _)| self.docs[idx].clone())
            .collect()
    }
}

// ─── DbPools ──────────────────────────────────────────────────────────────────

pub struct DbPools {
    pub archive_db: Pool<SqliteConnectionManager>,
    pub app_db: Pool<SqliteConnectionManager>,
    /// Pre-built candidate index: enables sub-millisecond character filtering
    pub candidate_index: CandidateIndex,
    /// Full in-memory character + subject data: eliminates all DB queries on hot path
    pub character_cache: Arc<CharacterCache>,
    /// Directory for locally cached/transcoded character images.
    pub image_cache_dir: String,
    /// Offline subject search index (segmentation + pinyin).
    pub subject_search_index: Arc<SubjectSearchIndex>,
    /// Offline character search index (segmentation + pinyin).
    pub character_search_index: Arc<CharacterSearchIndex>,
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
            c.pragma_update(None, "cache_size", "-131072")?; // 128 MB page cache
            c.pragma_update(None, "mmap_size", "268435456")?; // 256 MB mmap
            c.pragma_update(None, "temp_store", "MEMORY")
        });
    let archive_db = Pool::builder()
        .max_size(32)
        .connection_timeout(std::time::Duration::from_secs(30))
        .build(archive_manager)?;

    // Connect to app DB (read/write)
    let app_manager = SqliteConnectionManager::file(&config.app_db_path).with_init(|c| {
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

    let candidate_index = build_candidate_index(&archive_conn)?;
    let character_cache = Arc::new(build_character_cache(&archive_conn)?);
    let subject_search_index = Arc::new(build_subject_search_index(&archive_conn)?);
    let character_search_index = Arc::new(build_character_search_index(&archive_conn)?);

    tracing::info!(
        "Cache ready in {:.2}s — {} chars, {} candidate entries, {} subject rows cached",
        t0.elapsed().as_secs_f64(),
        character_cache.char_json.len(),
        candidate_index.all.len(),
        character_cache
            .char_subjects
            .values()
            .map(|v| v.len())
            .sum::<usize>(),
    );

    Ok(Arc::new(DbPools {
        archive_db,
        app_db,
        candidate_index,
        character_cache,
        image_cache_dir: config.image_cache_dir.clone(),
        subject_search_index,
        character_search_index,
    }))
}

fn build_subject_search_index(conn: &Connection) -> anyhow::Result<SubjectSearchIndex> {
    let mut stmt = conn.prepare(
        "SELECT id, type, date, collects, raw_json
         FROM subjects
         WHERE nsfw = 0
         ORDER BY collects DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut docs: Vec<SubjectDoc> = Vec::new();
    let mut by_id: HashMap<i64, usize> = HashMap::new();
    let mut postings: HashMap<String, Vec<usize>> = HashMap::new();

    for row in rows {
        let (id, stype, date, collects, raw) = match row {
            Ok(v) => v,
            Err(_) => continue,
        };
        let v: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let name_cn = v
            .get("name_cn")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();

        let idx = docs.len();
        docs.push(SubjectDoc {
            id,
            stype,
            date,
            collects,
            name: name.clone(),
            name_cn: name_cn.clone(),
        });
        by_id.insert(id, idx);

        let mut tokens = HashSet::<String>::new();

        // raw names
        let name_norm = normalize_token(&name);
        let name_cn_norm = normalize_token(&name_cn);
        if !name_norm.is_empty() {
            tokens.insert(name_norm.clone());
        }
        if !name_cn_norm.is_empty() {
            tokens.insert(name_cn_norm.clone());
        }

        // ascii word split (romaji / english)
        for part in name
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .map(normalize_token)
            .filter(|s| s.len() >= 2 && is_ascii_word(s))
        {
            tokens.insert(part);
        }

        // jieba segmentation for chinese name fields
        if !name_cn.is_empty() {
            for w in JIEBA.cut(&name_cn, false) {
                let t = normalize_token(w);
                if t.len() >= 2 {
                    tokens.insert(t);
                }
            }
        }

        // pinyin expansions (full + initials)
        if !name_cn_norm.is_empty() {
            let (py_full, py_initials) = to_pinyin_full_and_initials(&name_cn_norm);
            if py_full.len() >= 2 {
                tokens.insert(py_full);
            }
            if py_initials.len() >= 2 {
                tokens.insert(py_initials);
            }
        }

        // index all tokens
        for t in tokens {
            postings.entry(t).or_default().push(idx);
        }
    }

    Ok(SubjectSearchIndex { docs, postings })
}

fn build_character_search_index(conn: &Connection) -> anyhow::Result<CharacterSearchIndex> {
    let mut stmt = conn.prepare(
        "SELECT id, name, collects, comments, raw_json
         FROM characters
         WHERE role = 1
         ORDER BY collects DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut docs: Vec<CharacterDoc> = Vec::new();
    let mut postings: HashMap<String, Vec<usize>> = HashMap::new();

    for row in rows {
        let (id, name, collects, comments, raw) = match row {
            Ok(v) => v,
            Err(_) => continue,
        };

        let v: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let infobox = v
            .get("infobox")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let summary = v
            .get("summary")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();

        let idx = docs.len();
        docs.push(CharacterDoc {
            id,
            name: name.clone(),
            infobox: infobox.clone(),
            summary,
            collects,
            comments,
        });

        let mut tokens = HashSet::<String>::new();

        let name_norm = normalize_token(&name);
        if !name_norm.is_empty() {
            tokens.insert(name_norm.clone());
        }

        // tokenise by separators for latin/romaji names
        for part in name
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .map(normalize_token)
            .filter(|s| s.len() >= 2)
        {
            tokens.insert(part);
        }

        // jieba cut on the name (helps CN names stored in `name`)
        for w in JIEBA.cut(&name, false) {
            let t = normalize_token(w);
            if t.len() >= 2 {
                tokens.insert(t);
            }
        }

        // Extract CN name from infobox (if present) and index it.
        if let Some(cn) = extract_infobox_field_inline(&infobox, "简体中文名") {
            let cn_norm = normalize_token(cn);
            if cn_norm.len() >= 2 {
                tokens.insert(cn_norm.clone());
                for w in JIEBA.cut(cn, false) {
                    let t = normalize_token(w);
                    if t.len() >= 2 {
                        tokens.insert(t);
                    }
                }
                let (py_full, py_initials) = to_pinyin_full_and_initials(&cn_norm);
                if py_full.len() >= 2 {
                    tokens.insert(py_full);
                }
                if py_initials.len() >= 2 {
                    tokens.insert(py_initials);
                }
            }
        }

        // Extract english/romaji alias from infobox and index it (lowercased, no spaces)
        if let Some(en) = extract_alias_inline(&infobox, "英文名")
            .or_else(|| extract_alias_inline(&infobox, "罗马字"))
        {
            let en_norm = normalize_token(en);
            if en_norm.len() >= 2 {
                tokens.insert(en_norm);
            }
        }

        for t in tokens {
            postings.entry(t).or_default().push(idx);
        }
    }

    Ok(CharacterSearchIndex { docs, postings })
}

fn extract_infobox_field_inline<'a>(infobox: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("|{}=", key);
    let start = infobox.find(&pattern)? + pattern.len();
    let rest = &infobox[start..];
    let end = rest
        .find('\n')
        .or_else(|| rest.find('\r'))
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() { None } else { Some(value) }
}

fn extract_alias_inline<'a>(infobox: &'a str, alias_key: &str) -> Option<&'a str> {
    let search = format!("[{}|", alias_key);
    let start = infobox.find(&search)? + search.len();
    let rest = &infobox[start..];
    let end = rest.find(']').unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() { None } else { Some(value) }
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
            row.get::<_, i64>(0)?, // character_id
            row.get::<_, i64>(1)?, // type
            row.get::<_, i64>(2)?, // collects
            row.get::<_, i32>(3)?, // year
        ))
    })?;

    for row in rows {
        let (char_id, stype, collects, year) = match row {
            Ok(v) => v,
            Err(_) => continue,
        };
        if year <= 0 || year > 2099 {
            continue;
        }
        // For each character keep its primary (highest-collects) subject's type
        char_best.entry(char_id).or_insert((year, stype, collects));
    }

    let mut by_type: HashMap<i64, Vec<(i32, i64, i64)>> = HashMap::new();
    let mut all: Vec<(i32, i64, i64)> = Vec::new();

    for (char_id, (year, stype, collects)) in char_best {
        by_type
            .entry(stype)
            .or_default()
            .push((year, char_id, collects));
        all.push((year, char_id, collects));
    }

    // Sort each bucket by collects descending
    for entries in by_type.values_mut() {
        entries.sort_unstable_by(|a, b| b.2.cmp(&a.2));
    }
    all.sort_unstable_by(|a, b| b.2.cmp(&a.2));

    Ok(CandidateIndex { by_type, all })
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
                row.get::<_, i64>(0)?,    // character_id
                row.get::<_, i64>(1)?,    // role (sc.type)
                row.get::<_, i64>(2)?,    // subject id
                row.get::<_, String>(3)?, // subject raw_json
                row.get::<_, i64>(4)?,    // collects
            ))
        })?;

        for row in rows {
            let (char_id, role, sid, raw, collects) = match row {
                Ok(v) => v,
                Err(_) => continue,
            };
            let sval: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let stype = sval.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
            let year = sval
                .get("date")
                .and_then(|v| v.as_str())
                .and_then(|d| d.split('-').next())
                .and_then(|y| y.parse::<i32>().ok())
                .unwrap_or(-1);
            let score = sval.get("score").and_then(|v| v.as_f64()).unwrap_or(-1.0);
            let name = sval
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name_cn = sval
                .get("name_cn")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Serialise tags/meta_tags back to compact strings to avoid storing full Value trees
            let tags_json = sval
                .get("tags")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());
            let meta_tags_json = sval
                .get("meta_tags")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "[]".to_string());

            char_subjects.entry(char_id).or_default().push(SubjectRow {
                id: sid,
                role,
                stype,
                year,
                score,
                collects,
                name,
                name_cn,
                tags_json,
                meta_tags_json,
            });
        }
    }

    Ok(CharacterCache {
        char_json,
        char_subjects,
    })
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
