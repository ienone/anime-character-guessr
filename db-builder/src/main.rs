use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use chrono;

const DEFAULT_DUMP_DIR: &str = "../dump-2026-04-28.210420Z";
const DEFAULT_DB_PATH: &str = "../archive.sqlite";
const DEFAULT_APP_DB_PATH: &str = "../server-rs/data/app.sqlite";
const DEFAULT_IMAGES_JSON_PATH: &str = "../dump-2026-04-28.210420Z/character-images.jsonlines";

#[derive(Debug, Clone)]
struct Args {
    dump_dir: PathBuf,
    out_db: PathBuf,
    app_db: PathBuf,
    images_path: PathBuf,
    mode: String, // build-archive | migrate-app
}

fn parse_args() -> Result<Args> {
    let mut dump_dir: Option<PathBuf> = None;
    let mut out_db: Option<PathBuf> = None;
    let mut app_db: Option<PathBuf> = None;
    let mut images_path: Option<PathBuf> = None;
    let mut mode: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "db-builder\n\nUSAGE:\n  db-builder [--mode <build-archive|migrate-app>] [--dump-dir <path>] [--out <path>] [--app-db <path>] [--images-path <path>]\n\nMODES:\n  build-archive  Build trimmed archive.sqlite from dump (default)\n  migrate-app    Populate app.sqlite caches (image sources + VAs) from dump files\n\nOPTIONS:\n  -m, --mode <mode>         build-archive | migrate-app (default: build-archive)\n  -d, --dump-dir <path>     Dump folder containing *.jsonlines (default: {DEFAULT_DUMP_DIR})\n  -o, --out <path>          Output archive sqlite path (default: {DEFAULT_DB_PATH})\n  --app-db <path>           app.sqlite path (default: {DEFAULT_APP_DB_PATH})\n  --images-path <path>      character image source JSON/JSONL path (default: {DEFAULT_IMAGES_JSON_PATH})\n  -h, --help                Print help\n"
                );
                std::process::exit(0);
            }
            "-m" | "--mode" => {
                let v = it.next().context("--mode requires a value")?;
                mode = Some(v);
            }
            "-d" | "--dump-dir" => {
                let v = it
                    .next()
                    .context("--dump-dir requires a value")?;
                dump_dir = Some(PathBuf::from(v));
            }
            "-o" | "--out" => {
                let v = it.next().context("--out requires a value")?;
                out_db = Some(PathBuf::from(v));
            }
            "--app-db" => {
                let v = it.next().context("--app-db requires a value")?;
                app_db = Some(PathBuf::from(v));
            }
            "--images-path" => {
                let v = it.next().context("--images-path requires a value")?;
                images_path = Some(PathBuf::from(v));
            }
            // Backwards-compat
            "--images-json" => {
                let v = it.next().context("--images-json requires a value")?;
                images_path = Some(PathBuf::from(v));
            }
            _ if arg.starts_with('-') => {
                anyhow::bail!("Unknown option: {arg}")
            }
            _ => {
                // Positional fallback: first is dump_dir, second is out_db
                if dump_dir.is_none() {
                    dump_dir = Some(PathBuf::from(arg));
                } else if out_db.is_none() {
                    out_db = Some(PathBuf::from(arg));
                } else {
                    anyhow::bail!("Unexpected extra argument: {arg}")
                }
            }
        }
    }

    Ok(Args {
        dump_dir: dump_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DUMP_DIR)),
        out_db: out_db.unwrap_or_else(|| PathBuf::from(DEFAULT_DB_PATH)),
        app_db: app_db.unwrap_or_else(|| PathBuf::from(DEFAULT_APP_DB_PATH)),
        images_path: images_path.unwrap_or_else(|| PathBuf::from(DEFAULT_IMAGES_JSON_PATH)),
        mode: mode.unwrap_or_else(|| "build-archive".to_string()),
    })
}

fn main() -> Result<()> {
    let start_time = Instant::now();
    let args = parse_args()?;

    match args.mode.as_str() {
        "build-archive" => {
            println!("开始离线构建精简版 archive.sqlite...");
            println!("dump_dir: {}", args.dump_dir.display());
            println!("out_db: {}", args.out_db.display());

            let mut db = Connection::open(&args.out_db)?;
            init_db(&mut db)?;

            // ==========================================
            // 第一层漏斗：过滤作品 (Subject)
            // ==========================================
            println!("Step 1: 扫描并过滤 subject.jsonlines...");
            let valid_subjects = process_subjects(&mut db, &args.dump_dir)?;
            println!("已保留热门动漫/游戏作品数: {}", valid_subjects.len());

            // ==========================================
            // 第二层漏斗：过滤关联映射 (Subject-Characters)
            // ==========================================
            println!("\nStep 2: 扫描 subject-characters.jsonlines...");
            let valid_chars = process_relations(&mut db, &args.dump_dir, &valid_subjects)?;
            println!("符合条件的核心角色 (主角/配角) 且属于热门作品的集合数: {}", valid_chars.len());

            // ==========================================
            // 第三层漏斗：过滤角色详细信息 (Character)
            // ==========================================
            println!("\nStep 3: 扫描并过滤 character.jsonlines...");
            process_characters(&mut db, &args.dump_dir, &valid_chars)?;

            // ==========================================
            // 清理与优化
            // ==========================================
            println!("\n执行 SQLite 空间优化与索引构建...");
            db.execute_batch("VACUUM; OPTIMIZE;")?;

            println!("构建完成！耗时: {:.2?}", start_time.elapsed());
        }
        "migrate-app" => {
            println!("开始迁移 app.sqlite 缓存数据（图片源 + 声优）...");
            println!("dump_dir: {}", args.dump_dir.display());
            println!("archive_db: {}", args.out_db.display());
            println!("app_db: {}", args.app_db.display());
            println!("images_path: {}", args.images_path.display());

            migrate_app_db(&args.dump_dir, &args.out_db, &args.app_db, &args.images_path)?;
            println!("迁移完成！耗时: {:.2?}", start_time.elapsed());
        }
        other => anyhow::bail!("Unknown mode: {other}"),
    }

    Ok(())
}

fn ensure_app_schema(app: &Connection) -> Result<()> {
    app.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS character_image_sources (
            character_id INTEGER PRIMARY KEY,
            image_medium TEXT NOT NULL DEFAULT '',
            image_grid TEXT NOT NULL DEFAULT '',
            fetched_at_ms INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS character_vas (
            character_id INTEGER PRIMARY KEY,
            va_names_json TEXT NOT NULL DEFAULT '[]',
            fetched_at_ms INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT ''
        );
        "
    )?;
    Ok(())
}

fn migrate_app_db(dump_dir: &Path, archive_db_path: &Path, app_db_path: &Path, images_json_path: &Path) -> Result<()> {
    // Load valid subject/character sets from archive.sqlite so we only cache within game scope.
    let archive = Connection::open(archive_db_path)?;
    let mut stmt = archive.prepare("SELECT id FROM subjects")?;
    let valid_subjects: HashSet<i64> = stmt.query_map([], |row| row.get::<_, i64>(0))?.filter_map(Result::ok).collect();
    let mut stmt2 = archive.prepare("SELECT id FROM characters")?;
    let valid_chars: HashSet<i64> = stmt2.query_map([], |row| row.get::<_, i64>(0))?.filter_map(Result::ok).collect();
    println!("valid subjects: {}, valid chars: {}", valid_subjects.len(), valid_chars.len());

    let mut app = Connection::open(app_db_path)?;
    ensure_app_schema(&app)?;

    // ── 1) Import character image mapping into character_image_sources ──
    // Supports either:
    //  - JSON array:    [{ id, image_medium: [..], image_grid: [..], ... }, ...]
    //  - JSONL:         one JSON object per line (same schema)
    let now_ms = chrono::Utc::now().timestamp_millis();
    let imported_imgs = import_character_image_sources(&mut app, images_json_path, &valid_chars, now_ms)?;
    println!("imported image sources: {}", imported_imgs);

    // ── 2) Build character_vas from dump (person + person-characters) ──
    // Load seiyu person id -> name
    let mut seiyu_names: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    {
        let file = File::open(dump_dir.join("person.jsonlines"))
            .with_context(|| format!("failed to open {}", dump_dir.join("person.jsonlines").display()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            let v: Value = serde_json::from_str(&line)?;
            let id = v["id"].as_i64().unwrap_or(0);
            if id == 0 { continue; }
            let careers = v.get("career").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            let is_seiyu = careers.iter().any(|c| c.as_str() == Some("seiyu"));
            if !is_seiyu { continue; }
            let name = v["name"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() { continue; }
            seiyu_names.insert(id, name);
        }
    }
    println!("seiyu persons: {}", seiyu_names.len());

    let mut va_by_char: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    {
        let file = File::open(dump_dir.join("person-characters.jsonlines"))
            .with_context(|| format!("failed to open {}", dump_dir.join("person-characters.jsonlines").display()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            let v: Value = serde_json::from_str(&line)?;
            // type==0 is the dominant mapping (voice actor)
            if v["type"].as_i64().unwrap_or(-1) != 0 { continue; }
            let subject_id = v["subject_id"].as_i64().unwrap_or(0);
            let character_id = v["character_id"].as_i64().unwrap_or(0);
            let person_id = v["person_id"].as_i64().unwrap_or(0);
            if subject_id == 0 || character_id == 0 || person_id == 0 { continue; }
            if !valid_subjects.contains(&subject_id) { continue; }
            if !valid_chars.contains(&character_id) { continue; }
            let Some(name) = seiyu_names.get(&person_id) else { continue; };

            let entry = va_by_char.entry(character_id).or_insert_with(Vec::new);
            if entry.len() >= 8 { continue; }
            if !entry.iter().any(|n| n == name) {
                entry.push(name.clone());
            }
        }
    }
    println!("chars with va: {}", va_by_char.len());

    {
        let tx2 = app.transaction()?;
        let mut va_stmt = tx2.prepare(
            "INSERT INTO character_vas (character_id, va_names_json, fetched_at_ms, source)\n\
             VALUES (?1, ?2, ?3, 'dump')\n\
             ON CONFLICT(character_id) DO UPDATE SET\n\
               va_names_json=excluded.va_names_json,\n\
               fetched_at_ms=excluded.fetched_at_ms,\n\
               source=excluded.source"
        )?;
        let mut written_vas = 0;
        for (cid, names) in va_by_char {
            let json = serde_json::to_string(&names).unwrap_or("[]".to_string());
            va_stmt.execute((cid, json, now_ms))?;
            written_vas += 1;
        }
        drop(va_stmt);
        tx2.commit()?;
        println!("written character_vas rows: {}", written_vas);
    }

    // Final coverage summary
    let img_cnt: i64 = app.query_row("SELECT COUNT(1) FROM character_image_sources", [], |r| r.get(0))?;
    let va_cnt: i64 = app.query_row("SELECT COUNT(1) FROM character_vas", [], |r| r.get(0))?;
    println!("app.sqlite totals: image_sources={} vas={}", img_cnt, va_cnt);

    Ok(())
}

fn import_character_image_sources(
    app: &mut Connection,
    images_path: &Path,
    valid_chars: &HashSet<i64>,
    now_ms: i64,
) -> Result<i64> {
    let ext = images_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let tx = app.transaction()?;
    let mut img_stmt = tx.prepare(
        "INSERT INTO character_image_sources (character_id, image_medium, image_grid, fetched_at_ms, source)\n\
         VALUES (?1, ?2, ?3, ?4, ?5)\n\
         ON CONFLICT(character_id) DO UPDATE SET\n\
           image_medium=excluded.image_medium,\n\
           image_grid=excluded.image_grid,\n\
           fetched_at_ms=excluded.fetched_at_ms,\n\
           source=excluded.source",
    )?;

    let mut imported: i64 = 0;

    // Helper: write one entry
    let mut write_item = |item: Value, source: &str| -> Result<()> {
        let Some(id) = item.get("id").and_then(|x| x.as_i64()) else { return Ok(()); };
        if !valid_chars.contains(&id) {
            return Ok(());
        }

        let first_str = |v: Option<&Value>| -> String {
            v.and_then(|x| x.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };

        let medium = first_str(item.get("image_medium"));
        let grid = first_str(item.get("image_grid"));

        if medium.is_empty() && grid.is_empty() {
            return Ok(());
        }

        img_stmt.execute((id, medium, grid, now_ms, source))?;
        imported += 1;
        Ok(())
    };

    if ext == "jsonl" {
        let file = File::open(images_path)
            .with_context(|| format!("failed to open images jsonl at {}", images_path.display()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(t)
                .with_context(|| format!("failed to parse images jsonl line for {}", images_path.display()))?;
            write_item(v, "jsonl")?;
        }
    } else {
        let text = std::fs::read_to_string(images_path)
            .with_context(|| format!("failed to read images json at {}", images_path.display()))?;
        let v: Value = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse images json at {}", images_path.display()))?;
        let arr = v.as_array().cloned().unwrap_or_default();
        for item in arr {
            write_item(item, "json")?;
        }
    }

    drop(img_stmt);
    tx.commit()?;
    Ok(imported)
}

fn init_db(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "
        DROP TABLE IF EXISTS subjects;
        CREATE TABLE subjects (
            id INTEGER PRIMARY KEY,
            type INTEGER,
            name TEXT,
            name_cn TEXT,
            date TEXT,
            nsfw INTEGER,
            rank INTEGER,
            collects INTEGER,
            raw_json TEXT
        );

        DROP TABLE IF EXISTS characters;
        CREATE TABLE characters (
            id INTEGER PRIMARY KEY,
            name TEXT,
            role INTEGER,
            collects INTEGER,
            comments INTEGER,
            raw_json TEXT
        );

        DROP TABLE IF EXISTS subject_characters;
        CREATE TABLE subject_characters (
            subject_id INTEGER,
            character_id INTEGER,
            type INTEGER,
            order_num INTEGER
        );
        CREATE INDEX idx_sub_char ON subject_characters(subject_id, character_id);
        "
    )?;
    Ok(())
}

fn process_subjects(db: &mut Connection, dump_dir: &Path) -> Result<HashSet<i64>> {
    let file = File::open(dump_dir.join("subject.jsonlines"))
        .with_context(|| format!("failed to open {}", dump_dir.join("subject.jsonlines").display()))?;
    let reader = BufReader::new(file);
    let mut valid_ids = HashSet::new();

    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO subjects (id, type, name, name_cn, date, nsfw, rank, collects, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        )?;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            let v: Value = serde_json::from_str(&line)?;

            let id = v["id"].as_i64().unwrap_or(0);
            let type_id = v["type"].as_i64().unwrap_or(0);
            let nsfw = v["nsfw"].as_bool().unwrap_or(false);

            // 规则 1：只保留动漫(2)和游戏(4)
            if type_id != 2 && type_id != 4 {
                continue;
            }

            // 规则 2：不可控内容安全过滤
            if nsfw {
                continue;
            }

            let rank = v["rank"].as_i64().unwrap_or(0);
            let fav = &v["favorite"];
            let collects = fav["wish"].as_i64().unwrap_or(0)
                         + fav["done"].as_i64().unwrap_or(0)
                         + fav["doing"].as_i64().unwrap_or(0)
                         + fav["on_hold"].as_i64().unwrap_or(0)
                         + fav["dropped"].as_i64().unwrap_or(0);

            // 规则 3：过滤零热度作品 (动漫>=100或有排名，游戏>=50或有排名)
            let is_anime_valid = type_id == 2 && (collects >= 100 || rank > 0);
            let is_game_valid = type_id == 4 && (collects >= 50 || rank > 0);

            if is_anime_valid || is_game_valid {
                valid_ids.insert(id);
                stmt.execute((
                    id,
                    type_id,
                    v["name"].as_str().unwrap_or(""),
                    v["name_cn"].as_str().unwrap_or(""),
                    v["date"].as_str().unwrap_or(""),
                    nsfw as i32,
                    rank,
                    collects,
                    line.as_str() // 存入完整 raw_json 备用
                ))?;
            }
        }
    }
    tx.commit()?;
    Ok(valid_ids)
}

fn process_relations(db: &mut Connection, dump_dir: &Path, valid_subjects: &HashSet<i64>) -> Result<HashSet<i64>> {
    let file = File::open(dump_dir.join("subject-characters.jsonlines"))
        .with_context(|| format!("failed to open {}", dump_dir.join("subject-characters.jsonlines").display()))?;
    let reader = BufReader::new(file);
    let mut valid_chars = HashSet::new();

    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO subject_characters (subject_id, character_id, type, order_num)
             VALUES (?1, ?2, ?3, ?4)"
        )?;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            let v: Value = serde_json::from_str(&line)?;

            let sub_id = v["subject_id"].as_i64().unwrap_or(0);
            let char_id = v["character_id"].as_i64().unwrap_or(0);
            let type_id = v["type"].as_i64().unwrap_or(0); // 1:主角, 2:配角

            // 必须在热门作品池中，且必须是主角或配角
            if valid_subjects.contains(&sub_id) && (type_id == 1 || type_id == 2) {
                valid_chars.insert(char_id);
                stmt.execute((sub_id, char_id, type_id, v["order"].as_i64().unwrap_or(0)))?;
            }
        }
    }
    tx.commit()?;
    Ok(valid_chars)
}

fn process_characters(db: &mut Connection, dump_dir: &Path, valid_chars: &HashSet<i64>) -> Result<()> {
    let file = File::open(dump_dir.join("character.jsonlines"))
        .with_context(|| format!("failed to open {}", dump_dir.join("character.jsonlines").display()))?;
    let reader = BufReader::new(file);
    
    let mut total_kept = 0;

    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO characters (id, name, role, collects, comments, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            let v: Value = serde_json::from_str(&line)?;

            let id = v["id"].as_i64().unwrap_or(0);
            let collects = v["collects"].as_i64().unwrap_or(0);

            // 必须是前置步骤保留下的角色，并且过滤掉 0 收藏量的僵尸角色
            if valid_chars.contains(&id) && collects > 0 {
                total_kept += 1;
                stmt.execute((
                    id,
                    v["name"].as_str().unwrap_or(""),
                    v["role"].as_i64().unwrap_or(0),
                    collects,
                    v["comments"].as_i64().unwrap_or(0),
                    line.as_str()
                ))?;
            }
        }
    }
    tx.commit()?;
    println!("实际最终写入库的优质角色数量: {}", total_kept);
    Ok(())
}
