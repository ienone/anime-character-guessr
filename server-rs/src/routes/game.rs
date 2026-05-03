//! Single-player game character assembly from archive.sqlite.
//!
//! Mirrors the logic in client/src/utils/bangumi.js, but runs server-side
//! against the pre-built archive.sqlite — no live Bangumi API calls needed.

use crate::db::{CharacterCache, DbPools};
use anyhow::Result;
use rand::prelude::IndexedRandom;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// ─── Public entry points ──────────────────────────────────────────────────────

/// Build a complete character payload for single-player mode.
/// Reads exclusively from the in-memory CharacterCache — zero DB round-trips.
pub fn assemble_character(
    cache: &CharacterCache,
    char_id: i64,
    settings: &GameSettings,
) -> Result<Value> {
    let char_val = cache
        .char_json
        .get(&char_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Character {} not found in cache", char_id))?;
    let subjects = cache
        .char_subjects
        .get(&char_id)
        .cloned()
        .unwrap_or_default();
    let subject_infos: Vec<SubjectInfo> = subjects
        .into_iter()
        .map(|r| SubjectInfo {
            id: r.id,
            role: r.role,
            stype: r.stype,
            year: r.year,
            score: r.score,
            collects: r.collects,
            name: r.name,
            name_cn: r.name_cn,
            tags: serde_json::from_str(&r.tags_json).unwrap_or_default(),
            meta_tags: serde_json::from_str(&r.meta_tags_json).unwrap_or_default(),
        })
        .collect();
    assemble_payload(char_id, char_val, subject_infos, settings)
}

/// Pick a random character consistent with `settings` using the pre-built
/// in-memory index — zero DB round-trips for candidate selection.
/// Returns `(char_id, payload)` after assembling from CharacterCache.
pub fn random_character(pools: &Arc<DbPools>, settings: &GameSettings) -> Result<(i64, Value)> {
    let char_id = pick_candidate(pools, settings)?;
    let payload = assemble_character(&pools.character_cache, char_id, settings)?;
    Ok((char_id, payload))
}

/// Build payload for a specific character by ID.
pub fn character_by_id(
    pools: &Arc<DbPools>,
    char_id: i64,
    settings: &GameSettings,
) -> Result<Value> {
    assemble_character(&pools.character_cache, char_id, settings)
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

/// Select a candidate character_id from the in-memory index in O(n_filtered) time.
fn pick_candidate(pools: &Arc<DbPools>, settings: &GameSettings) -> Result<i64> {
    let idx = &pools.candidate_index;
    let cache = &pools.character_cache;
    let types = settings.subject_types();

    // Collect entries matching type + year constraints, verified to exist in char_json
    let mut candidates: Vec<i64> = Vec::new();

    let sources: Vec<&Vec<(i32, i64, i64)>> = if types.len() == 4 {
        // "全部" mode — use pre-sorted all list
        vec![&idx.all]
    } else {
        types.iter().filter_map(|t| idx.by_type.get(t)).collect()
    };

    let top_n = settings.top_n_subjects.unwrap_or(1000).min(3000) as usize;

    for bucket in sources {
        for &(year, char_id, _collects) in bucket.iter().take(top_n) {
            // Year filter
            if let Some(sy) = settings.start_year {
                if year < sy {
                    continue;
                }
            }
            if let Some(ey) = settings.end_year {
                if year > ey {
                    continue;
                }
            }
            // Only include characters present in the cache
            if !cache.char_json.contains_key(&char_id) {
                continue;
            }
            candidates.push(char_id);
        }
    }

    // Deduplicate (a character may appear in multiple type buckets)
    candidates.sort_unstable();
    candidates.dedup();

    if candidates.is_empty() {
        // Fallback: pick from all cached character IDs
        candidates = cache.char_json.keys().copied().collect();
    }

    candidates
        .choose(&mut rand::rng())
        .copied()
        .ok_or_else(|| anyhow::anyhow!("No eligible characters found"))
}

// ─── Settings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // Some fields parsed from JSON are reserved for future filtering logic
pub struct GameSettings {
    pub start_year: Option<i32>,
    pub end_year: Option<i32>,
    /// e.g. ["动画", "游戏"], maps to subject type filter
    pub meta_tags: Vec<String>,
    /// top-N subjects by collects
    pub top_n_subjects: Option<i64>,
    /// Whether to output rawTags (commonTags mode)
    pub common_tags: bool,
    pub subject_tag_num: usize,
    pub character_tag_num: usize,
    pub main_character_only: bool,
    pub character_num: usize,
}

impl GameSettings {
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

    pub fn from_json(v: &Value) -> Self {
        Self {
            start_year: v
                .get("startYear")
                .and_then(|x| x.as_i64())
                .map(|x| x as i32),
            end_year: v.get("endYear").and_then(|x| x.as_i64()).map(|x| x as i32),
            meta_tags: v
                .get("metaTags")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            top_n_subjects: v.get("topNSubjects").and_then(|x| x.as_i64()),
            common_tags: v
                .get("commonTags")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            subject_tag_num: v.get("subjectTagNum").and_then(|x| x.as_u64()).unwrap_or(6) as usize,
            character_tag_num: v
                .get("characterTagNum")
                .and_then(|x| x.as_u64())
                .unwrap_or(6) as usize,
            main_character_only: v
                .get("mainCharacterOnly")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            character_num: v.get("characterNum").and_then(|x| x.as_u64()).unwrap_or(6) as usize,
        }
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Flat subject info built from CharacterCache — no DB access needed.
struct SubjectInfo {
    id: i64,
    role: i64,
    stype: i64,
    year: i32,
    score: f64,
    collects: i64,
    name: String,
    name_cn: String,
    tags: Value,
    meta_tags: Value,
}

/// Construct the full character payload from pre-loaded cache data.
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
    let collects = char_val
        .get("collects")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let comments = char_val
        .get("comments")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let popularity = collects + comments;

    // Parse infobox for nameCn, nameEn, gender
    let infobox_str = char_val
        .get("infobox")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name_cn = extract_infobox_field(infobox_str, "简体中文名").map(|s| s.to_string());
    let gender_raw = extract_infobox_field(infobox_str, "性别").map(|s| s.to_string());
    let gender = match gender_raw.as_deref() {
        Some("男") => "male",
        Some("女") => "female",
        _ => "?",
    };
    let name_en = extract_alias(infobox_str, "英文名")
        .or_else(|| extract_alias(infobox_str, "罗马字"))
        .map(|s| s.to_string());

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
            if s.year <= 0 || s.year > current_year {
                return false;
            }
            if let Some(sy) = settings.start_year {
                if s.year < sy {
                    return false;
                }
            }
            if let Some(ey) = settings.end_year {
                if s.year > ey {
                    return false;
                }
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

    // Sort by collects descending (already sorted in cache, but filtered set may differ)
    let mut sorted_subjects: Vec<&SubjectInfo> = appearance_subjects.clone();
    sorted_subjects.sort_by(|a, b| b.collects.cmp(&a.collects));

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
        sorted_source.sort_by(|a, b| b.1.cmp(&a.1));
        if let Some((top_src, top_cnt)) = sorted_source.first() {
            *raw_tags_map.entry(top_src.clone()).or_insert(0) += top_cnt;
        }

        let mut sorted_raw: Vec<(String, i64)> = raw_tags_map
            .into_iter()
            .filter(|(k, _)| !k.contains("20"))
            .collect();
        sorted_raw.sort_by(|a, b| b.1.cmp(&a.1));

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
        sorted_source.sort_by(|a, b| b.1.cmp(&a.1));

        let mut sorted_meta: Vec<(String, i64)> = meta_tag_counts.into_iter().collect();
        sorted_meta.sort_by(|a, b| b.1.cmp(&a.1));

        let mut sorted_tags: Vec<(String, i64)> = tag_counts.into_iter().collect();
        sorted_tags.sort_by(|a, b| b.1.cmp(&a.1));

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
    value
        .get("rawTags")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
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

// ─── Infobox parsers ──────────────────────────────────────────────────────────

fn extract_infobox_field<'a>(infobox: &'a str, key: &str) -> Option<&'a str> {
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

fn extract_alias<'a>(infobox: &'a str, alias_key: &str) -> Option<&'a str> {
    let search = format!("[{}|", alias_key);
    let start = infobox.find(&search)? + search.len();
    let rest = &infobox[start..];
    let end = rest.find(']').unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() { None } else { Some(value) }
}

pub fn parse_character_basic_fields(
    char_id: i64,
    char_val: &Value,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    i64,
) {
    let infobox_str = char_val
        .get("infobox")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name_cn = extract_infobox_field(infobox_str, "简体中文名").map(|s| s.to_string());
    let gender_raw = extract_infobox_field(infobox_str, "性别").map(|s| s.to_string());
    let gender = match gender_raw.as_deref() {
        Some("男") => "male",
        Some("女") => "female",
        _ => "?",
    }
    .to_string();
    let name_en = extract_alias(infobox_str, "英文名")
        .or_else(|| extract_alias(infobox_str, "罗马字"))
        .map(|s| s.to_string());

    let summary = char_val
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let collects = char_val
        .get("collects")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let comments = char_val
        .get("comments")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let popularity = collects + comments;

    // always use proxy urls
    let image = Some(format!("/img/{}.webp", char_id));
    let image_grid = Some(format!("/img/{}.webp", char_id));

    (
        name_cn, name_en, image, image_grid, gender, summary, popularity,
    )
}
