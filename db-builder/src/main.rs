use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;

const DEFAULT_DUMP_DIR: &str = "../dump-2026-04-28.210420Z";
const DEFAULT_DB_PATH: &str = "../archive.sqlite";

#[derive(Debug, Clone)]
struct Args {
    dump_dir: PathBuf,
    out_db: PathBuf,
}

fn parse_args() -> Result<Args> {
    let mut dump_dir: Option<PathBuf> = None;
    let mut out_db: Option<PathBuf> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "db-builder\n\nUSAGE:\n  db-builder [--dump-dir <path>] [--out <path>]\n\nOPTIONS:\n  -d, --dump-dir <path>   Dump folder containing *.jsonlines (default: {DEFAULT_DUMP_DIR})\n  -o, --out <path>        Output archive sqlite path (default: {DEFAULT_DB_PATH})\n  -h, --help              Print help\n"
                );
                std::process::exit(0);
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
    })
}

fn main() -> Result<()> {
    let start_time = Instant::now();
    println!("开始离线构建精简版 archive.sqlite...");

    let args = parse_args()?;
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
    Ok(())
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
