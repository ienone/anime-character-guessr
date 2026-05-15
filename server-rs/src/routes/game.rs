//! Single-player game character assembly from archive.sqlite.
//!
//! Mirrors the logic in client/src/utils/bangumi.js, but runs server-side
//! against the pre-built archive.sqlite — no live Bangumi API calls needed.

use crate::db::SubjectRow;
use anyhow::Result;
use rand::prelude::IndexedRandom;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Map;
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;

const CANDIDATE_CACHE_MAX_ENTRIES: usize = 128;
const CANDIDATE_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_META_TAGS: usize = 8;
const MAX_META_TAG_CHARS: usize = 48;
const MAX_ADDED_SUBJECTS: usize = 100;
const MAX_ADDED_SUBJECT_NAME_CHARS: usize = 120;
const MAX_USE_HINTS: usize = 20;

static CANDIDATE_CACHE: LazyLock<DashMap<CandidateCacheKey, CandidateCacheEntry>> =
    LazyLock::new(DashMap::new);
static CANDIDATE_BUILD_LOCKS: LazyLock<DashMap<CandidateCacheKey, Arc<Mutex<()>>>> =
    LazyLock::new(DashMap::new);

#[derive(Debug, Clone)]
struct CandidateCacheEntry {
    candidates: Arc<Vec<i64>>,
    built_at: Instant,
}

#[derive(Debug, Clone, Eq)]
struct CandidateCacheKey {
    start_year: Option<i32>,
    end_year: Option<i32>,
    meta_tags: Vec<String>,
    top_n_subjects: Option<i64>,
    main_character_only: bool,
    character_num: usize,
    use_subject_per_year: bool,
    added_subject_ids: Vec<i64>,
}

impl PartialEq for CandidateCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.start_year == other.start_year
            && self.end_year == other.end_year
            && self.meta_tags == other.meta_tags
            && self.top_n_subjects == other.top_n_subjects
            && self.main_character_only == other.main_character_only
            && self.character_num == other.character_num
            && self.use_subject_per_year == other.use_subject_per_year
            && self.added_subject_ids == other.added_subject_ids
    }
}

impl Hash for CandidateCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.start_year.hash(state);
        self.end_year.hash(state);
        self.meta_tags.hash(state);
        self.top_n_subjects.hash(state);
        self.main_character_only.hash(state);
        self.character_num.hash(state);
        self.use_subject_per_year.hash(state);
        self.added_subject_ids.hash(state);
    }
}

// ─── Public entry points ──────────────────────────────────────────────────────

pub fn assemble_character(
    conn: &Connection,
    char_id: i64,
    settings: &GameSettings,
) -> Result<Value> {
    let char_val = load_character_value(conn, char_id)?;
    let subjects = load_subject_rows(conn, char_id)?;
    let subject_infos: Vec<SubjectInfo> = subjects
        .into_iter()
        .map(|r| SubjectInfo {
            id: r.id,
            role: r.role,
            stype: r.stype,
            year: r.year,
            score: r.score,
            popularity: r.popularity,
            name: r.name,
            name_cn: r.name_cn,
            tags: serde_json::from_str(&r.tags_json).unwrap_or_default(),
            meta_tags: serde_json::from_str(&r.meta_tags_json).unwrap_or_default(),
        })
        .collect();
    assemble_payload(char_id, char_val, subject_infos, settings)
}

/// Pick a random character consistent with `settings` via indexed SQLite queries.
pub fn random_character_with_conn(
    conn: &Connection,
    settings: &GameSettings,
) -> Result<(i64, Value)> {
    let char_id = pick_candidate(conn, settings)?;
    let payload = assemble_character(conn, char_id, settings)?;
    Ok((char_id, payload))
}

/// Build payload for a specific character by ID.
pub fn character_by_id_with_conn(
    conn: &Connection,
    char_id: i64,
    settings: &GameSettings,
) -> Result<Value> {
    assemble_character(conn, char_id, settings)
}

pub fn build_feedback(guess: &Value, answer: &Value, settings: &GameSettings) -> Value {
    let guess_id = guess.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let answer_id = answer.get("id").and_then(|v| v.as_i64()).unwrap_or(0);

    let gender_feedback = if guess.get("gender") == answer.get("gender") {
        "yes"
    } else {
        "no"
    };
    let popularity_feedback = compare_popularity(
        value_i64(guess, "popularity"),
        value_i64(answer, "popularity"),
    );
    let rating_feedback = compare_rating(
        value_f64(guess, "highestRating", -1.0),
        value_f64(answer, "highestRating", -1.0),
    );

    let guess_appearances = string_vec(guess.get("appearances"));
    let answer_appearances = string_vec(answer.get("appearances"));
    let answer_appearance_set: HashSet<&str> =
        answer_appearances.iter().map(String::as_str).collect();
    let shared_appearances: Vec<String> = guess_appearances
        .iter()
        .filter(|name| answer_appearance_set.contains(name.as_str()))
        .cloned()
        .collect();
    let appearances_count_feedback = compare_count(
        guess_appearances.len() as i64,
        answer_appearances.len() as i64,
    );

    let (meta_tags_guess, shared_meta_tags) = if settings.common_tags {
        common_tag_feedback(guess_id, answer_id, guess, answer, settings)
    } else {
        simple_meta_tag_feedback(guess, answer)
    };

    json!({
        "isCorrect": guess_id != 0 && guess_id == answer_id,
        "isPartialCorrect": guess_id != answer_id && !shared_appearances.is_empty(),
        "gender": { "guess": guess.get("gender").cloned().unwrap_or(Value::String("?".to_string())), "feedback": gender_feedback },
        "popularity": { "guess": value_i64(guess, "popularity"), "feedback": popularity_feedback },
        "rating": { "guess": value_f64(guess, "highestRating", -1.0), "feedback": rating_feedback },
        "shared_appearances": {
            "first": shared_appearances.first().cloned().unwrap_or_default(),
            "count": shared_appearances.len(),
        },
        "appearancesCount": { "guess": guess_appearances.len(), "feedback": appearances_count_feedback },
        "metaTags": { "guess": meta_tags_guess, "shared": shared_meta_tags },
        "latestAppearance": {
            "guess": year_guess_value(value_i64(guess, "latestAppearance"), value_i64(answer, "latestAppearance")),
            "feedback": compare_year(value_i64(guess, "latestAppearance"), value_i64(answer, "latestAppearance")),
        },
        "earliestAppearance": {
            "guess": value_i64(guess, "earliestAppearance"),
            "feedback": compare_year(value_i64(guess, "earliestAppearance"), value_i64(answer, "earliestAppearance")),
        },
    })
}

fn pick_candidate(conn: &Connection, settings: &GameSettings) -> Result<i64> {
    let key = CandidateCacheKey::from_settings(settings);
    let candidates = get_or_build_candidate_pool(conn, settings, &key)?;
    candidates
        .choose(&mut rand::rng())
        .copied()
        .ok_or_else(|| anyhow::anyhow!("No eligible characters found"))
}

fn get_or_build_candidate_pool(
    conn: &Connection,
    settings: &GameSettings,
    key: &CandidateCacheKey,
) -> Result<Arc<Vec<i64>>> {
    if let Some(entry) = CANDIDATE_CACHE.get(key)
        && entry.built_at.elapsed() < CANDIDATE_CACHE_TTL
        && !entry.candidates.is_empty()
    {
        return Ok(Arc::clone(&entry.candidates));
    }

    let build_lock = CANDIDATE_BUILD_LOCKS
        .entry(key.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    let _guard = build_lock
        .lock()
        .map_err(|_| anyhow::anyhow!("Candidate cache build lock poisoned"))?;

    if let Some(entry) = CANDIDATE_CACHE.get(key)
        && entry.built_at.elapsed() < CANDIDATE_CACHE_TTL
        && !entry.candidates.is_empty()
    {
        return Ok(Arc::clone(&entry.candidates));
    }

    let candidates = Arc::new(query_candidate_pool(conn, settings)?);
    prune_candidate_cache();
    CANDIDATE_CACHE.insert(
        key.clone(),
        CandidateCacheEntry {
            candidates: Arc::clone(&candidates),
            built_at: Instant::now(),
        },
    );
    Ok(candidates)
}

fn prune_candidate_cache() {
    if CANDIDATE_CACHE.len() < CANDIDATE_CACHE_MAX_ENTRIES {
        return;
    }

    let mut entries = CANDIDATE_CACHE
        .iter()
        .map(|entry| (entry.key().clone(), entry.built_at))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(_, built_at)| *built_at);

    let remove_count = entries
        .len()
        .saturating_sub(CANDIDATE_CACHE_MAX_ENTRIES - 1);
    for (key, _) in entries.into_iter().take(remove_count) {
        CANDIDATE_CACHE.remove(&key);
        CANDIDATE_BUILD_LOCKS.remove(&key);
    }
}

fn query_candidate_pool(conn: &Connection, settings: &GameSettings) -> Result<Vec<i64>> {
    let types = settings.subject_types();
    let top_n = settings
        .top_n_subjects
        .filter(|n| *n > 0)
        .map(|n| n.min(3000) as usize);
    let characters_per_subject = settings.character_num.clamp(1, 50);
    let added_subject_ids = settings.added_subject_ids.clone();
    let type_placeholders = std::iter::repeat_n("?", types.len())
        .collect::<Vec<_>>()
        .join(",");
    let added_placeholders = std::iter::repeat_n("?", added_subject_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let subject_filter_sql = settings.subject_filter_sql();
    let mut sql = if settings.use_subject_per_year && top_n.is_some() {
        format!(
            "WITH ranked_subjects AS (
                SELECT
                    s.id,
                    s.popularity,
                    ROW_NUMBER() OVER (
                        PARTITION BY s.year
                        ORDER BY s.popularity DESC, s.id ASC
                    ) AS year_rank
                FROM subjects s
                LEFT JOIN subject_details d ON d.subject_id = s.id
                WHERE s.year > 0
                  AND s.type IN ({type_placeholders})"
        )
    } else if top_n.is_some() {
        format!(
            "WITH top_subjects AS (
                SELECT id, popularity
                FROM (
                    SELECT s.id, s.popularity
                    FROM subjects s
                    LEFT JOIN subject_details d ON d.subject_id = s.id
                    WHERE s.year > 0
                      AND s.type IN ({type_placeholders})"
        )
    } else {
        format!(
            "WITH top_subjects AS (
                SELECT s.id, s.popularity
                FROM subjects s
                LEFT JOIN subject_details d ON d.subject_id = s.id
                WHERE s.year > 0
                  AND s.type IN ({type_placeholders})"
        )
    };
    if settings.start_year.is_some() {
        sql.push_str(" AND s.year >= ?");
    }
    if settings.end_year.is_some() {
        sql.push_str(" AND s.year <= ?");
    }
    sql.push_str(&subject_filter_sql);
    if settings.use_subject_per_year && top_n.is_some() {
        sql.push_str(
            ")
            , top_subjects AS (
                SELECT id, popularity
                FROM ranked_subjects
                WHERE year_rank <= ?",
        );
    } else if top_n.is_some() {
        sql.push_str(
            " ORDER BY popularity DESC
              LIMIT ?
                )",
        );
    }
    if !added_subject_ids.is_empty() {
        sql.push_str(&format!(
            "
            UNION
            SELECT id, popularity
            FROM subjects
            WHERE id IN ({added_placeholders})"
        ));
    }
    sql.push_str(
        "
        ),
        ranked_characters AS (
            SELECT
                sc.character_id,
                ROW_NUMBER() OVER (
                    PARTITION BY sc.subject_id
                    ORDER BY
                        CASE WHEN sc.type = 1 THEN 0 ELSE 1 END,
                        c.popularity DESC,
                        sc.character_id ASC
                ) AS subject_rank
            FROM top_subjects ts
            JOIN subject_characters sc ON sc.subject_id = ts.id
            JOIN characters c ON c.id = sc.character_id",
    );
    if settings.main_character_only {
        sql.push_str(" WHERE sc.type = 1");
    }
    sql.push_str(
        ")
        SELECT DISTINCT character_id
        FROM ranked_characters
        WHERE subject_rank <= ?",
    );

    let mut values: Vec<SqlValue> = types.into_iter().map(SqlValue::Integer).collect();
    if let Some(start_year) = settings.start_year {
        values.push(SqlValue::Integer(start_year as i64));
    }
    if let Some(end_year) = settings.end_year {
        values.push(SqlValue::Integer(end_year as i64));
    }
    values.extend(settings.subject_filter_values());
    if let Some(top_n) = top_n {
        values.push(SqlValue::Integer(top_n as i64));
    }
    values.extend(added_subject_ids.into_iter().map(SqlValue::Integer));
    values.push(SqlValue::Integer(characters_per_subject as i64));

    let mut stmt = conn.prepare(&sql)?;
    let mut candidates = stmt
        .query_map(params_from_iter(values.iter()), |row| row.get::<_, i64>(0))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();

    if candidates.is_empty() {
        let mut stmt =
            conn.prepare("SELECT id FROM characters WHERE role = 1 ORDER BY random() LIMIT 50")?;
        candidates = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .filter_map(Result::ok)
            .collect();
    }

    Ok(candidates)
}

// ─── Settings ─────────────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn default_common_tags() -> bool {
    true
}

fn default_subject_tag_num() -> usize {
    6
}

fn default_character_tag_num() -> usize {
    6
}

fn default_character_num() -> usize {
    6
}

fn default_max_attempts() -> usize {
    10
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<i64>().ok()
            }
        }
        _ => None,
    }
}

fn value_as_usize(value: &Value) -> Option<usize> {
    value_as_i64(value).and_then(|n| usize::try_from(n).ok())
}

fn deserialize_optional_i32<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value_as_i64(&value).and_then(|n| i32::try_from(n).ok()))
}

fn deserialize_optional_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value_as_i64(&value))
}

fn deserialize_subject_tag_num<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value_as_usize(&value).unwrap_or_else(default_subject_tag_num))
}

fn deserialize_character_tag_num<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value_as_usize(&value).unwrap_or_else(default_character_tag_num))
}

fn deserialize_character_num<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value_as_usize(&value).unwrap_or_else(default_character_num))
}

fn deserialize_max_attempts<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value_as_usize(&value).unwrap_or_else(default_max_attempts))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedSubject {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "name_cn")]
    pub name_cn: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<Value>,
}

fn added_subject_from_value(value: &Value) -> Option<AddedSubject> {
    if let Some(id) = value.as_i64() {
        return Some(AddedSubject {
            id,
            name: String::new(),
            name_cn: String::new(),
            r#type: None,
        });
    }

    let id = value.get("id").and_then(|id| id.as_i64())?;
    Some(AddedSubject {
        id,
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name_cn: value
            .get("name_cn")
            .or_else(|| value.get("nameCn"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        r#type: value.get("type").cloned(),
    })
}

fn deserialize_added_subjects<'de, D>(deserializer: D) -> Result<Vec<AddedSubject>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Some(values) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(values.iter().filter_map(added_subject_from_value).collect())
}

fn deserialize_use_hints<'de, D>(deserializer: D) -> Result<Vec<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Some(values) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(values
        .iter()
        .filter_map(value_as_i64)
        .filter(|n| *n > 0)
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Some fields parsed from JSON are reserved for future filtering logic
pub struct GameSettings {
    #[serde(default, deserialize_with = "deserialize_optional_i32")]
    pub start_year: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_optional_i32")]
    pub end_year: Option<i32>,
    /// e.g. ["动画", "游戏"], maps to subject type filter
    #[serde(default)]
    pub meta_tags: Vec<String>,
    /// top-N subjects by popularity
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    pub top_n_subjects: Option<i64>,
    /// Whether to output rawTags (commonTags mode)
    #[serde(default = "default_common_tags")]
    pub common_tags: bool,
    #[serde(
        default = "default_subject_tag_num",
        deserialize_with = "deserialize_subject_tag_num"
    )]
    pub subject_tag_num: usize,
    #[serde(
        default = "default_character_tag_num",
        deserialize_with = "deserialize_character_tag_num"
    )]
    pub character_tag_num: usize,
    #[serde(default = "default_true")]
    pub main_character_only: bool,
    #[serde(
        default = "default_character_num",
        deserialize_with = "deserialize_character_num"
    )]
    pub character_num: usize,
    #[serde(default)]
    pub use_subject_per_year: bool,
    #[serde(
        default,
        rename = "addedSubjects",
        deserialize_with = "deserialize_added_subjects"
    )]
    pub added_subjects: Vec<AddedSubject>,
    #[serde(default, skip)]
    pub added_subject_ids: Vec<i64>,
    #[serde(
        default = "default_max_attempts",
        deserialize_with = "deserialize_max_attempts"
    )]
    pub max_attempts: usize,
    #[serde(default)]
    pub sync_mode: bool,
    #[serde(default)]
    pub nonstop_mode: bool,
    #[serde(default)]
    pub global_pick: bool,
    #[serde(default)]
    pub tag_ban: bool,
    #[serde(default, deserialize_with = "deserialize_use_hints")]
    pub use_hints: Vec<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    pub use_image_hint: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    pub time_limit: Option<i64>,
    #[serde(default)]
    pub subject_search: bool,
    #[serde(default)]
    pub use_index: bool,
    #[serde(default)]
    pub index_id: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            start_year: None,
            end_year: None,
            meta_tags: Vec::new(),
            top_n_subjects: None,
            common_tags: true,
            subject_tag_num: default_subject_tag_num(),
            character_tag_num: default_character_tag_num(),
            main_character_only: true,
            character_num: default_character_num(),
            use_subject_per_year: false,
            added_subjects: Vec::new(),
            added_subject_ids: Vec::new(),
            max_attempts: default_max_attempts(),
            sync_mode: false,
            nonstop_mode: false,
            global_pick: false,
            tag_ban: false,
            use_hints: Vec::new(),
            use_image_hint: None,
            time_limit: None,
            subject_search: false,
            use_index: false,
            index_id: None,
            extra: Map::new(),
        }
    }
}

impl GameSettings {
    fn normalize_public_bounds(&mut self) {
        if let (Some(start), Some(end)) = (self.start_year, self.end_year)
            && start > end
        {
            self.start_year = Some(end);
            self.end_year = Some(start);
        }
        self.start_year = self.start_year.map(|year| year.clamp(1800, 2038));
        self.end_year = self.end_year.map(|year| year.clamp(1800, 2038));

        self.meta_tags = self
            .meta_tags
            .iter()
            .take(MAX_META_TAGS)
            .map(|tag| truncate_chars(tag.trim(), MAX_META_TAG_CHARS))
            .collect();
        while self
            .meta_tags
            .last()
            .map(|tag| tag.is_empty())
            .unwrap_or(false)
        {
            self.meta_tags.pop();
        }

        self.top_n_subjects = self
            .top_n_subjects
            .filter(|value| *value > 0)
            .map(|value| value.min(3000));
        self.subject_tag_num = self.subject_tag_num.clamp(0, 10);
        self.character_tag_num = self.character_tag_num.clamp(0, 10);
        self.character_num = self.character_num.clamp(1, 50);
        self.max_attempts = self.max_attempts.clamp(1, 15);
        self.use_hints = self
            .use_hints
            .iter()
            .copied()
            .filter(|id| *id > 0)
            .take(MAX_USE_HINTS)
            .collect();
        self.use_image_hint = self.use_image_hint.filter(|id| *id > 0);
        self.time_limit = self.time_limit.and_then(|seconds| {
            if seconds <= 0 {
                None
            } else {
                Some(seconds.clamp(15, 120))
            }
        });

        let mut seen_subjects = HashSet::new();
        self.added_subjects = self
            .added_subjects
            .drain(..)
            .filter(|subject| subject.id > 0)
            .filter(|subject| seen_subjects.insert(subject.id))
            .take(MAX_ADDED_SUBJECTS)
            .map(|mut subject| {
                subject.name = truncate_chars(subject.name.trim(), MAX_ADDED_SUBJECT_NAME_CHARS);
                subject.name_cn =
                    truncate_chars(subject.name_cn.trim(), MAX_ADDED_SUBJECT_NAME_CHARS);
                subject
            })
            .collect();

        let mut added_ids = self
            .added_subjects
            .iter()
            .map(|subject| subject.id)
            .chain(self.added_subject_ids.iter().copied())
            .filter(|id| *id > 0)
            .collect::<Vec<_>>();
        added_ids.sort_unstable();
        added_ids.dedup();
        added_ids.truncate(MAX_ADDED_SUBJECTS);
        self.added_subject_ids = added_ids;
    }

    pub fn subject_types(&self) -> Vec<i64> {
        let primary = self.meta_tags.first().map(|s| s.as_str()).unwrap_or("");
        match primary {
            "书籍" => vec![1],
            "游戏" | "Galgame" => vec![4],
            "三次元" => vec![6],
            "全部" => vec![1, 2, 4, 6],
            _ => vec![2], // default: anime
        }
    }

    fn primary_meta_filter(&self) -> Option<&str> {
        let primary = self.meta_tags.first().map(|s| s.as_str()).unwrap_or("");
        match primary {
            "" | "全部" | "游戏" | "书籍" | "三次元" | "Galgame" => None,
            tag => Some(tag),
        }
    }

    fn source_filter(&self) -> Option<&str> {
        self.meta_tags
            .get(1)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    fn genre_filter(&self) -> Option<&str> {
        self.meta_tags
            .get(2)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    fn subject_filter_sql(&self) -> String {
        let mut sql = String::new();
        if self.primary_meta_filter().is_some() {
            sql.push_str(
                " AND EXISTS (
                    SELECT 1
                    FROM json_each(COALESCE(d.meta_tags_json, '[]')) mt
                    WHERE mt.value = ?
                )",
            );
        }
        if let Some(source) = self.source_filter() {
            let placeholders = std::iter::repeat_n("?", source_aliases(source).len())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(
                " AND EXISTS (
                    SELECT 1
                    FROM json_each(COALESCE(d.tags_json, '[]')) tg
                    WHERE json_extract(tg.value, '$.name') IN ({placeholders})
                )"
            ));
        }
        if self.genre_filter().is_some() {
            sql.push_str(
                " AND (
                    EXISTS (
                        SELECT 1
                        FROM json_each(COALESCE(d.meta_tags_json, '[]')) mt
                        WHERE mt.value = ?
                    )
                    OR EXISTS (
                        SELECT 1
                        FROM json_each(COALESCE(d.tags_json, '[]')) tg
                        WHERE json_extract(tg.value, '$.name') = ?
                    )
                )",
            );
        }
        sql
    }

    fn subject_filter_values(&self) -> Vec<SqlValue> {
        let mut values = Vec::new();
        if let Some(primary) = self.primary_meta_filter() {
            values.push(SqlValue::Text(primary.to_string()));
        }
        if let Some(source) = self.source_filter() {
            values.extend(source_aliases(source).into_iter().map(SqlValue::Text));
        }
        if let Some(genre) = self.genre_filter() {
            values.push(SqlValue::Text(genre.to_string()));
            values.push(SqlValue::Text(genre.to_string()));
        }
        values
    }

    fn matches_subject(&self, subject: &SubjectInfo) -> bool {
        if let Some(primary) = self.primary_meta_filter()
            && !json_string_array_contains(&subject.meta_tags, primary)
        {
            return false;
        }
        if let Some(source) = self.source_filter() {
            let aliases = source_aliases(source);
            if !json_tag_array_contains_any(&subject.tags, &aliases) {
                return false;
            }
        }
        if let Some(genre) = self.genre_filter()
            && !json_string_array_contains(&subject.meta_tags, genre)
            && !json_tag_array_contains_any(&subject.tags, &[genre.to_string()])
        {
            return false;
        }
        true
    }

    pub fn from_json(v: &Value) -> Self {
        let mut settings = serde_json::from_value::<Self>(v.clone()).unwrap_or_default();
        settings.added_subject_ids = settings
            .added_subjects
            .iter()
            .map(|subject| subject.id)
            .collect();
        if settings.added_subject_ids.is_empty() {
            settings.added_subject_ids = v
                .get("addedSubjects")
                .and_then(|x| x.as_array())
                .map(|subjects| {
                    subjects
                        .iter()
                        .filter_map(|subject| {
                            subject
                                .as_i64()
                                .or_else(|| subject.get("id").and_then(|id| id.as_i64()))
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        settings.normalize_public_bounds();
        settings
    }
}

impl CandidateCacheKey {
    fn from_settings(settings: &GameSettings) -> Self {
        let mut added_subject_ids = settings.added_subject_ids.clone();
        added_subject_ids.sort_unstable();
        added_subject_ids.dedup();

        Self {
            start_year: settings.start_year,
            end_year: settings.end_year,
            meta_tags: settings.meta_tags.clone(),
            top_n_subjects: settings
                .top_n_subjects
                .filter(|n| *n > 0)
                .map(|n| n.min(3000)),
            main_character_only: settings.main_character_only,
            character_num: settings.character_num.clamp(1, 50),
            use_subject_per_year: settings.use_subject_per_year,
            added_subject_ids,
        }
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

struct SubjectInfo {
    id: i64,
    role: i64,
    stype: i64,
    year: i32,
    score: f64,
    popularity: i64,
    name: String,
    name_cn: String,
    tags: Value,
    meta_tags: Value,
}

fn source_aliases(source: &str) -> Vec<String> {
    match source {
        "原创" => vec!["原创", "原创动画"],
        "漫画改" => vec!["漫画改", "漫改", "漫画改编"],
        "游戏改" => vec!["游戏改", "游戏改编", "GAL改"],
        "小说改" => vec!["小说改", "小说改编", "轻小说改", "轻改", "网文改"],
        other => vec![other],
    }
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn json_string_array_contains(value: &Value, needle: &str) -> bool {
    value
        .as_array()
        .map(|items| items.iter().any(|item| item.as_str() == Some(needle)))
        .unwrap_or(false)
}

fn json_tag_array_contains_any(value: &Value, needles: &[String]) -> bool {
    value
        .as_array()
        .map(|items| {
            items.iter().any(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .map(|name| needles.iter().any(|needle| needle == name))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn load_character_value(conn: &Connection, char_id: i64) -> Result<Value> {
    conn.query_row(
        "SELECT c.id, c.name, p.name_cn, p.name_en, p.gender, p.summary, c.popularity
         FROM characters c
         LEFT JOIN character_profile p ON p.character_id = c.id
         WHERE c.id = ?1",
        [char_id],
        |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "name": row.get::<_, String>(1).unwrap_or_default(),
                "nameCn": row.get::<_, String>(2).unwrap_or_default(),
                "nameEn": row.get::<_, String>(3).unwrap_or_default(),
                "gender": row.get::<_, String>(4).unwrap_or_else(|_| "?".to_string()),
                "summary": row.get::<_, String>(5).unwrap_or_default(),
                "popularity": row.get::<_, i64>(6).unwrap_or(0),
            }))
        },
    )
    .map_err(|_| anyhow::anyhow!("Character {} not found in archive", char_id))
}

fn load_subject_rows(conn: &Connection, char_id: i64) -> Result<Vec<SubjectRow>> {
    let mut stmt = conn.prepare(
        "SELECT sc.type, s.id, s.type, s.year, s.score,
                s.popularity, s.name, s.name_cn, d.tags_json, d.meta_tags_json
         FROM subject_characters sc
         JOIN subjects s ON sc.subject_id = s.id
         LEFT JOIN subject_details d ON d.subject_id = s.id
         WHERE sc.character_id = ?1
         ORDER BY s.popularity DESC",
    )?;
    let rows = stmt.query_map([char_id], |row| {
        let year = row.get::<_, i32>(3).unwrap_or(-1);
        Ok(SubjectRow {
            id: row.get::<_, i64>(1)?,
            role: row.get::<_, i64>(0).unwrap_or(0),
            stype: row.get::<_, i64>(2).unwrap_or(0),
            year,
            score: row.get::<_, f64>(4).unwrap_or(-1.0),
            popularity: row.get::<_, i64>(5).unwrap_or(0),
            name: row.get::<_, String>(6).unwrap_or_default(),
            name_cn: row.get::<_, String>(7).unwrap_or_default(),
            tags_json: row.get::<_, String>(8).unwrap_or_else(|_| "[]".to_string()),
            meta_tags_json: row.get::<_, String>(9).unwrap_or_else(|_| "[]".to_string()),
        })
    })?;

    Ok(rows.filter_map(Result::ok).collect())
}

/// Construct the full character payload from archive rows.
fn assemble_payload(
    char_id: i64,
    char_val: Value,
    subjects: Vec<SubjectInfo>,
    settings: &GameSettings,
) -> Result<Value> {
    // ── Character base fields ────────────────────────────────────────────────
    let name = char_val
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let summary = char_val
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let popularity = char_val
        .get("popularity")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let name_cn = char_val
        .get("nameCn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let name_en = char_val
        .get("nameEn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let gender = char_val
        .get("gender")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();

    // Always serve images via our proxy endpoint; backend will fetch+cache as WebP.
    // This avoids leaking external URLs to the client and works even if archive.sqlite
    // strips the original "images" field from character raw_json.
    let image = format!("/img/{}.webp", char_id);
    let image_grid = format!("/img/{}.webp", char_id);

    // ── Filter subjects to relevant types + years ────────────────────────────
    let allowed_types = settings.subject_types();
    let current_year = chrono::Utc::now()
        .format("%Y")
        .to_string()
        .parse::<i32>()
        .unwrap_or(2099);

    let filtered: Vec<&SubjectInfo> = subjects
        .iter()
        .filter(|s| {
            if !allowed_types.contains(&s.stype) {
                return false;
            }
            if !settings.matches_subject(s) {
                return false;
            }
            if s.year <= 0 || s.year > current_year {
                return false;
            }
            if let Some(sy) = settings.start_year
                && s.year < sy
            {
                return false;
            }
            if let Some(ey) = settings.end_year
                && s.year > ey
            {
                return false;
            }
            true
        })
        .collect();

    let appearance_subjects: Vec<&SubjectInfo> = if filtered.is_empty() {
        subjects.iter().collect()
    } else {
        filtered
    };

    // ── Build appearance lists ───────────────────────────────────────────────
    let mut latest_appearance: i32 = -1;
    let mut earliest_appearance: i32 = -1;
    let mut highest_rating: f64 = -1.0;
    let mut appearance_names: Vec<String> = Vec::new();
    let mut appearance_ids: Vec<i64> = Vec::new();

    // Tag aggregation structures
    let source_tag_set: HashSet<&str> = ["原创", "游戏改", "小说改", "漫画改"]
        .iter()
        .cloned()
        .collect();
    let source_tag_map: HashMap<&str, &str> = [
        ("GAL改", "游戏改"),
        ("轻小说改", "小说改"),
        ("轻改", "小说改"),
        ("原创动画", "原创"),
        ("网文改", "小说改"),
        ("漫改", "漫画改"),
        ("漫画改编", "漫画改"),
        ("游戏改编", "游戏改"),
        ("小说改编", "小说改"),
    ]
    .iter()
    .cloned()
    .collect();
    let region_tag_set: HashSet<&str> = [
        "日本",
        "欧美",
        "美国",
        "中国",
        "法国",
        "韩国",
        "英国",
        "俄罗斯",
        "中国香港",
        "苏联",
        "捷克",
        "中国台湾",
        "马来西亚",
    ]
    .iter()
    .cloned()
    .collect();

    let mut source_tag_counts: HashMap<String, i64> = HashMap::new();
    let mut region_tags: HashSet<String> = HashSet::new();
    let mut tag_counts: HashMap<String, i64> = HashMap::new();
    let mut meta_tag_counts: HashMap<String, i64> = HashMap::new();
    let mut raw_tags_map: HashMap<String, i64> = HashMap::new();
    let _all_meta_tags: Vec<String>;

    // Sort by popularity descending (already sorted in cache, but filtered set may differ)
    let mut sorted_subjects: Vec<&SubjectInfo> = appearance_subjects.clone();
    sorted_subjects.sort_by_key(|subject| Reverse(subject.popularity));

    for s in &sorted_subjects {
        let stuff_factor: i64 = if s.role == 1 { 3 } else { 1 }; // 主角 weight

        let year = s.year;
        if year > 0 {
            if latest_appearance == -1 || year > latest_appearance {
                latest_appearance = year;
            }
            if earliest_appearance == -1 || year < earliest_appearance {
                earliest_appearance = year;
            }
        }

        if s.score > highest_rating {
            highest_rating = s.score;
        }

        let sub_name = if !s.name_cn.is_empty() {
            s.name_cn.clone()
        } else {
            s.name.clone()
        };
        appearance_names.push(sub_name);
        appearance_ids.push(s.id);

        // Tag aggregation
        let subject_tags = s.tags.as_array().cloned().unwrap_or_default();
        let subject_meta_tags = s.meta_tags.as_array().cloned().unwrap_or_default();

        if settings.common_tags {
            for tag_obj in &subject_tags {
                let tag_name = tag_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let tag_count = tag_obj.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if tag_name.contains("20") {
                    continue;
                } // skip year tags
                if let Some(&mapped) = source_tag_map.get(tag_name) {
                    *source_tag_counts.entry(mapped.to_string()).or_insert(0) +=
                        stuff_factor * tag_count;
                } else if source_tag_set.contains(tag_name) {
                    *source_tag_counts.entry(tag_name.to_string()).or_insert(0) +=
                        stuff_factor * tag_count;
                } else {
                    *raw_tags_map.entry(tag_name.to_string()).or_insert(0) +=
                        stuff_factor * tag_count;
                }
            }
        } else {
            // meta_tags mode
            for meta in &subject_meta_tags {
                let tag = meta.as_str().unwrap_or("");
                if source_tag_set.contains(tag) {
                    continue;
                }
                if region_tag_set.contains(tag) {
                    region_tags.insert(tag.to_string());
                } else {
                    *meta_tag_counts.entry(tag.to_string()).or_insert(0) += stuff_factor;
                }
            }
            for tag_obj in &subject_tags {
                let tag_name = tag_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let tag_count = tag_obj.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                if tag_name.contains("20") {
                    continue;
                }
                if source_tag_set.contains(tag_name) {
                    *source_tag_counts.entry(tag_name.to_string()).or_insert(0) +=
                        tag_count * stuff_factor;
                } else if let Some(&mapped) = source_tag_map.get(tag_name) {
                    *source_tag_counts.entry(mapped.to_string()).or_insert(0) +=
                        tag_count * stuff_factor;
                } else if region_tag_set.contains(tag_name) {
                    region_tags.insert(tag_name.to_string());
                } else {
                    *tag_counts.entry(tag_name.to_string()).or_insert(0) +=
                        tag_count * stuff_factor;
                }
            }
        }
    }

    // ── Build final tag output ────────────────────────────────────────────────
    let raw_tags_out: Value;
    let meta_tags_out: Vec<String>;

    if settings.common_tags {
        // Merge top source tag into raw_tags
        let mut sorted_source: Vec<(String, i64)> = source_tag_counts.into_iter().collect();
        sorted_source.sort_by_key(|entry| Reverse(entry.1));
        if let Some((top_src, top_cnt)) = sorted_source.first() {
            *raw_tags_map.entry(top_src.clone()).or_insert(0) += top_cnt;
        }

        let mut sorted_raw: Vec<(String, i64)> = raw_tags_map
            .into_iter()
            .filter(|(k, _)| !k.contains("20"))
            .collect();
        sorted_raw.sort_by_key(|entry| Reverse(entry.1));

        let max_count = sorted_raw.first().map(|e| e.1).unwrap_or(0);
        let threshold = (max_count as f64 * 0.1) as i64;
        let cutoff = sorted_raw
            .iter()
            .position(|(_, c)| *c < threshold)
            .unwrap_or(sorted_raw.len());
        let take = cutoff.max(settings.subject_tag_num);

        raw_tags_out = Value::Object(
            sorted_raw
                .into_iter()
                .take(take)
                .map(|(k, v)| (k, Value::Number(v.into())))
                .collect(),
        );
        meta_tags_out = Vec::new(); // not used in commonTags mode on frontend
    } else {
        // Build allMetaTags
        let mut sorted_source: Vec<(String, i64)> = source_tag_counts.into_iter().collect();
        sorted_source.sort_by_key(|entry| Reverse(entry.1));

        let mut sorted_meta: Vec<(String, i64)> = meta_tag_counts.into_iter().collect();
        sorted_meta.sort_by_key(|entry| Reverse(entry.1));

        let mut sorted_tags: Vec<(String, i64)> = tag_counts.into_iter().collect();
        sorted_tags.sort_by_key(|entry| Reverse(entry.1));

        let mut meta_set: Vec<String> = Vec::new();

        // One source tag
        if let Some((src, _)) = sorted_source.first() {
            meta_set.push(src.clone());
        }
        for (tag, _) in &sorted_meta {
            if meta_set.len() >= settings.subject_tag_num {
                break;
            }
            if !meta_set.contains(tag) {
                meta_set.push(tag.clone());
            }
        }
        for (tag, _) in &sorted_tags {
            if meta_set.len() >= settings.subject_tag_num {
                break;
            }
            if !meta_set.contains(tag) {
                meta_set.push(tag.clone());
            }
        }
        for tag in &region_tags {
            if !meta_set.contains(tag) {
                meta_set.push(tag.clone());
            }
        }

        // VA (声优) from subject_characters persons — skip for now, not in archive
        // (VA data would need a separate persons table)

        raw_tags_out = json!({});
        meta_tags_out = meta_set;
    }

    // ── Build response ────────────────────────────────────────────────────────
    let response = json!({
        "id": char_id,
        "name": name,
        "nameCn": name_cn,
        "nameEn": name_en,
        "gender": gender,
        "summary": summary,
        "image": image,
        "imageGrid": image_grid,
        "popularity": popularity,
        "appearances": appearance_names,
        "appearanceIds": appearance_ids,
        "latestAppearance": if latest_appearance == -1 { json!(null) } else { json!(latest_appearance) },
        "earliestAppearance": if earliest_appearance == -1 { json!(null) } else { json!(earliest_appearance) },
        "highestRating": if highest_rating < 0.0 { json!(-1) } else { json!(highest_rating) },
        "metaTags": meta_tags_out,
        "rawTags": raw_tags_out,
        "animeVAs": [],  // Filled by server-side VA cache when available
    });

    Ok(response)
}

fn value_i64(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|n| n as i64)))
        .unwrap_or(-1)
}

fn value_f64(value: &Value, key: &str, default: f64) -> f64 {
    value
        .get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
        .unwrap_or(default)
}

fn string_vec(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn compare_popularity(guess: i64, answer: i64) -> &'static str {
    let diff = guess - answer;
    let five_percent = (answer as f64) * 0.05;
    let twenty_percent = (answer as f64) * 0.2;
    if (diff.abs() as f64) <= five_percent {
        "="
    } else if diff > 0 {
        if (diff as f64) <= twenty_percent {
            "+"
        } else {
            "++"
        }
    } else if (diff as f64) >= -twenty_percent {
        "-"
    } else {
        "--"
    }
}

fn compare_rating(guess: f64, answer: f64) -> &'static str {
    if guess == -1.0 || answer == -1.0 {
        "?"
    } else {
        let diff = guess - answer;
        if diff.abs() <= 0.3 {
            "="
        } else if diff > 0.0 {
            if diff <= 1.0 { "+" } else { "++" }
        } else if diff >= -1.0 {
            "-"
        } else {
            "--"
        }
    }
}

fn compare_count(guess: i64, answer: i64) -> &'static str {
    let diff = guess - answer;
    if diff == 0 {
        "="
    } else if diff > 0 {
        if diff <= 2 { "+" } else { "++" }
    } else if diff >= -2 {
        "-"
    } else {
        "--"
    }
}

fn compare_year(guess: i64, answer: i64) -> &'static str {
    if guess == -1 || answer == -1 {
        if guess == -1 && answer == -1 {
            "="
        } else {
            "?"
        }
    } else {
        compare_count(guess, answer)
    }
}

fn year_guess_value(guess: i64, _answer: i64) -> Value {
    if guess == -1 {
        Value::String("?".to_string())
    } else {
        json!(guess)
    }
}

fn raw_tag_keys(value: &Value) -> Vec<String> {
    match value.get("rawTags") {
        Some(Value::Object(obj)) => obj.keys().cloned().collect(),
        Some(Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| match entry {
                Value::String(tag) => Some(tag.clone()),
                Value::Array(pair) => pair.first().and_then(Value::as_str).map(str::to_string),
                Value::Object(obj) => obj
                    .get("tag")
                    .or_else(|| obj.get("name"))
                    .or_else(|| obj.get("key"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn common_tag_feedback(
    guess_id: i64,
    answer_id: i64,
    guess: &Value,
    answer: &Value,
    settings: &GameSettings,
) -> (Vec<String>, Vec<String>) {
    let guess_subject_tags = raw_tag_keys(guess);
    let answer_subject_tags = raw_tag_keys(answer);
    let answer_subject_set: HashSet<&str> =
        answer_subject_tags.iter().map(String::as_str).collect();
    let mut subject_tags: Vec<String> = guess_subject_tags
        .iter()
        .filter(|tag| answer_subject_set.contains(tag.as_str()))
        .take(settings.subject_tag_num)
        .cloned()
        .collect();
    let shared_subject_tags = subject_tags.clone();
    for tag in &guess_subject_tags {
        if subject_tags.len() >= settings.subject_tag_num {
            break;
        }
        if !answer_subject_set.contains(tag.as_str()) {
            subject_tags.push(tag.clone());
        }
    }

    let id_tags = id_tags_map();
    let guess_character_tags = id_tags.get(&guess_id).cloned().unwrap_or_default();
    let answer_character_tags = id_tags.get(&answer_id).cloned().unwrap_or_default();
    let answer_character_set: HashSet<&str> =
        answer_character_tags.iter().map(String::as_str).collect();
    let mut character_tags: Vec<String> = guess_character_tags
        .iter()
        .filter(|tag| answer_character_set.contains(tag.as_str()))
        .take(settings.character_tag_num)
        .cloned()
        .collect();
    let shared_character_tags = character_tags.clone();
    for tag in &guess_character_tags {
        if character_tags.len() >= settings.character_tag_num {
            break;
        }
        if !answer_character_set.contains(tag.as_str()) {
            character_tags.push(tag.clone());
        }
    }

    let guess_cv_tags = string_vec(guess.get("animeVAs"));
    let answer_cv_tags = string_vec(answer.get("animeVAs"));
    let answer_cv_set: HashSet<&str> = answer_cv_tags.iter().map(String::as_str).collect();
    let shared_cv_tags: Vec<String> = guess_cv_tags
        .iter()
        .filter(|tag| answer_cv_set.contains(tag.as_str()))
        .cloned()
        .collect();

    let mut final_guess = Vec::new();
    push_unique(&mut final_guess, subject_tags);
    push_unique(&mut final_guess, character_tags);
    push_unique(&mut final_guess, guess_cv_tags);

    let mut final_shared = Vec::new();
    push_unique(&mut final_shared, shared_subject_tags);
    push_unique(&mut final_shared, shared_character_tags);
    push_unique(&mut final_shared, shared_cv_tags);
    (final_guess, final_shared)
}

fn simple_meta_tag_feedback(guess: &Value, answer: &Value) -> (Vec<String>, Vec<String>) {
    let guess_tags = string_vec(guess.get("metaTags"));
    let answer_tags = string_vec(answer.get("metaTags"));
    let answer_set: HashSet<&str> = answer_tags.iter().map(String::as_str).collect();
    let shared = guess_tags
        .iter()
        .filter(|tag| answer_set.contains(tag.as_str()))
        .cloned()
        .collect();
    (guess_tags, shared)
}

fn push_unique(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        if !target.iter().any(|existing| existing == &value) {
            target.push(value);
        }
    }
}

fn id_tags_map() -> HashMap<i64, Vec<String>> {
    let raw = include_str!("../id_tags.json");
    serde_json::from_str(raw).unwrap_or_default()
}
