use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use tantivy::schema::{Schema, TantivyDocument, FAST, INDEXED, STORED, STRING, TEXT};
use tantivy::{doc, Index};

const DEFAULT_DUMP_DIR: &str = "../dump-2026-04-28.210420Z";
const DEFAULT_DB_PATH: &str = "../archive.sqlite";
const DEFAULT_APP_DB_PATH: &str = "../server-rs/data/app.sqlite";
const DEFAULT_IMAGES_JSON_PATH: &str = "../dump-2026-04-28.210420Z/character-images.jsonlines";
const DEFAULT_TANTIVY_INDEX_DIR: &str = "../server-rs/data/tantivy";

#[derive(Debug, Clone)]
struct Args {
    dump_dir: PathBuf,
    out_db: PathBuf,
    app_db: PathBuf,
    images_path: PathBuf,
    tantivy_index_dir: PathBuf,
    mode: String, // build-archive | rebuild-fts | migrate-app
}

fn parse_args() -> Result<Args> {
    let mut dump_dir: Option<PathBuf> = None;
    let mut out_db: Option<PathBuf> = None;
    let mut app_db: Option<PathBuf> = None;
    let mut images_path: Option<PathBuf> = None;
    let mut tantivy_index_dir: Option<PathBuf> = None;
    let mut mode: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "db-builder\n\nUSAGE:\n  db-builder [--mode <build-archive|rebuild-fts|build-tantivy|migrate-app>] [--dump-dir <path>] [--out <path>] [--app-db <path>] [--images-path <path>] [--tantivy-dir <path>]\n\nMODES:\n  build-archive  Build trimmed archive.sqlite from dump (default)\n  rebuild-fts    Rebuild FTS search indexes on an existing archive.sqlite\n  build-tantivy  Build Tantivy search indexes from archive.sqlite\n  migrate-app    Populate app.sqlite caches (image sources + VAs) from dump files\n\nOPTIONS:\n  -m, --mode <mode>         build-archive | rebuild-fts | build-tantivy | migrate-app (default: build-archive)\n  -d, --dump-dir <path>     Dump folder containing *.jsonlines (default: {DEFAULT_DUMP_DIR})\n  -o, --out <path>          Output archive sqlite path (default: {DEFAULT_DB_PATH})\n  --app-db <path>           app.sqlite path (default: {DEFAULT_APP_DB_PATH})\n  --images-path <path>      character image source JSON/JSONL path (default: {DEFAULT_IMAGES_JSON_PATH})\n  --tantivy-dir <path>      Tantivy index output dir (default: {DEFAULT_TANTIVY_INDEX_DIR})\n  -h, --help                Print help\n"
                );
                std::process::exit(0);
            }
            "-m" | "--mode" => {
                let v = it.next().context("--mode requires a value")?;
                mode = Some(v);
            }
            "-d" | "--dump-dir" => {
                let v = it.next().context("--dump-dir requires a value")?;
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
            "--tantivy-dir" => {
                let v = it.next().context("--tantivy-dir requires a value")?;
                tantivy_index_dir = Some(PathBuf::from(v));
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
        tantivy_index_dir: tantivy_index_dir
            .unwrap_or_else(|| PathBuf::from(DEFAULT_TANTIVY_INDEX_DIR)),
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
            println!(
                "符合条件的核心角色 (主角/配角) 且属于热门作品的集合数: {}",
                valid_chars.len()
            );

            // ==========================================
            // 第三层漏斗：过滤角色详细信息 (Character)
            // ==========================================
            println!("\nStep 3: 扫描并过滤 character.jsonlines...");
            process_characters(&mut db, &args.dump_dir, &valid_chars)?;

            println!("\nStep 4: 裁剪没有可用角色关联的作品...");
            prune_subjects_without_characters(&db)?;

            println!("\nStep 5: 构建 SQLite FTS5 搜索索引...");
            build_search_indexes(&mut db, &args.dump_dir)?;

            println!("\nStep 6: 构建 Tantivy 搜索索引...");
            build_tantivy_indexes(&args.out_db, &args.dump_dir, &args.tantivy_index_dir)?;

            // ==========================================
            // 清理与优化
            // ==========================================
            println!("\n执行 SQLite 空间优化与索引构建...");
            db.execute_batch("VACUUM; PRAGMA optimize;")?;

            println!("构建完成！耗时: {:.2?}", start_time.elapsed());
        }
        "rebuild-fts" => {
            println!("开始重建 archive.sqlite FTS 搜索索引...");
            println!("dump_dir: {}", args.dump_dir.display());
            println!("archive_db: {}", args.out_db.display());
            let mut db = Connection::open(&args.out_db)?;
            ensure_archive_search_schema(&db)?;
            prune_subjects_without_characters(&db)?;
            rebuild_search_tables(&mut db)?;
            build_search_indexes(&mut db, &args.dump_dir)?;
            db.execute_batch("VACUUM; PRAGMA optimize;")?;
            println!("FTS 重建完成！耗时: {:.2?}", start_time.elapsed());
        }
        "build-tantivy" => {
            println!("开始构建 Tantivy 搜索索引...");
            println!("dump_dir: {}", args.dump_dir.display());
            println!("archive_db: {}", args.out_db.display());
            println!("tantivy_dir: {}", args.tantivy_index_dir.display());
            build_tantivy_indexes(&args.out_db, &args.dump_dir, &args.tantivy_index_dir)?;
            println!("Tantivy 索引构建完成！耗时: {:.2?}", start_time.elapsed());
        }
        "migrate-app" => {
            println!("开始迁移 app.sqlite 缓存数据（图片源 + 声优）...");
            println!("dump_dir: {}", args.dump_dir.display());
            println!("archive_db: {}", args.out_db.display());
            println!("app_db: {}", args.app_db.display());
            println!("images_path: {}", args.images_path.display());

            migrate_app_db(
                &args.dump_dir,
                &args.out_db,
                &args.app_db,
                &args.images_path,
            )?;
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
        ",
    )?;
    Ok(())
}

fn ensure_archive_search_schema(db: &Connection) -> Result<()> {
    for table in [
        "subjects",
        "subject_details",
        "characters",
        "character_profile",
        "character_aliases",
        "character_search_docs",
        "subject_characters",
    ] {
        let exists: i64 = db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
            [table],
            |row| row.get(0),
        )?;
        if exists == 0 {
            anyhow::bail!(
                "archive schema is outdated: missing table {table}; run --mode build-archive instead of rebuild-fts"
            );
        }
    }
    Ok(())
}

fn migrate_app_db(
    dump_dir: &Path,
    archive_db_path: &Path,
    app_db_path: &Path,
    images_json_path: &Path,
) -> Result<()> {
    // Load valid subject/character sets from archive.sqlite so we only cache within game scope.
    let archive = Connection::open(archive_db_path)?;
    let mut stmt = archive.prepare("SELECT id FROM subjects")?;
    let valid_subjects: HashSet<i64> = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .filter_map(Result::ok)
        .collect();
    let mut stmt2 = archive.prepare("SELECT id FROM characters")?;
    let valid_chars: HashSet<i64> = stmt2
        .query_map([], |row| row.get::<_, i64>(0))?
        .filter_map(Result::ok)
        .collect();
    println!(
        "valid subjects: {}, valid chars: {}",
        valid_subjects.len(),
        valid_chars.len()
    );

    let mut app = Connection::open(app_db_path)?;
    ensure_app_schema(&app)?;

    // ── 1) Import character image mapping into character_image_sources ──
    // Supports either:
    //  - JSON array:    [{ id, image_medium: [..], image_grid: [..], ... }, ...]
    //  - JSONL:         one JSON object per line (same schema)
    let now_ms = chrono::Utc::now().timestamp_millis();
    let imported_imgs =
        import_character_image_sources(&mut app, images_json_path, &valid_chars, now_ms)?;
    println!("imported image sources: {}", imported_imgs);

    // ── 2) Build character_vas from dump (person + person-characters) ──
    // Load seiyu person id -> name
    let mut seiyu_names: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    {
        let file = File::open(dump_dir.join("person.jsonlines")).with_context(|| {
            format!(
                "failed to open {}",
                dump_dir.join("person.jsonlines").display()
            )
        })?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line)?;
            let id = v["id"].as_i64().unwrap_or(0);
            if id == 0 {
                continue;
            }
            let careers = v
                .get("career")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            let is_seiyu = careers.iter().any(|c| c.as_str() == Some("seiyu"));
            if !is_seiyu {
                continue;
            }
            let name = v["name"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() {
                continue;
            }
            seiyu_names.insert(id, name);
        }
    }
    println!("seiyu persons: {}", seiyu_names.len());

    let mut va_by_char: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::new();
    {
        let file = File::open(dump_dir.join("person-characters.jsonlines")).with_context(|| {
            format!(
                "failed to open {}",
                dump_dir.join("person-characters.jsonlines").display()
            )
        })?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line)?;
            // type==0 is the dominant mapping (voice actor)
            if v["type"].as_i64().unwrap_or(-1) != 0 {
                continue;
            }
            let subject_id = v["subject_id"].as_i64().unwrap_or(0);
            let character_id = v["character_id"].as_i64().unwrap_or(0);
            let person_id = v["person_id"].as_i64().unwrap_or(0);
            if subject_id == 0 || character_id == 0 || person_id == 0 {
                continue;
            }
            if !valid_subjects.contains(&subject_id) {
                continue;
            }
            if !valid_chars.contains(&character_id) {
                continue;
            }
            let Some(name) = seiyu_names.get(&person_id) else {
                continue;
            };

            let entry = va_by_char.entry(character_id).or_default();
            if entry.len() >= 8 {
                continue;
            }
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
               source=excluded.source",
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
    let img_cnt: i64 = app.query_row("SELECT COUNT(1) FROM character_image_sources", [], |r| {
        r.get(0)
    })?;
    let va_cnt: i64 = app.query_row("SELECT COUNT(1) FROM character_vas", [], |r| r.get(0))?;
    println!(
        "app.sqlite totals: image_sources={} vas={}",
        img_cnt, va_cnt
    );

    Ok(())
}

fn build_tantivy_indexes(archive_db_path: &Path, dump_dir: &Path, index_dir: &Path) -> Result<()> {
    let archive = Connection::open(archive_db_path)?;
    ensure_archive_search_schema(&archive)?;
    fs::create_dir_all(index_dir)?;
    build_tantivy_character_index(&archive, &index_dir.join("characters"))?;
    build_tantivy_subject_index(&archive, dump_dir, &index_dir.join("subjects"))?;
    Ok(())
}

fn build_tantivy_character_index(db: &Connection, index_dir: &Path) -> Result<()> {
    recreate_dir(index_dir)?;

    let mut schema_builder = Schema::builder();
    let id = schema_builder.add_u64_field("id", STORED | FAST);
    let name = schema_builder.add_text_field("name", TEXT | STORED);
    let name_cn = schema_builder.add_text_field("name_cn", TEXT | STORED);
    let name_en = schema_builder.add_text_field("name_en", TEXT | STORED);
    let romaji = schema_builder.add_text_field("romaji", TEXT | STORED);
    let gender = schema_builder.add_text_field("gender", STRING | STORED);
    let popularity = schema_builder.add_u64_field("popularity", STORED | FAST);
    let aliases = schema_builder.add_text_field("aliases", TEXT | STORED);
    let search_terms = schema_builder.add_text_field("search_terms", TEXT);
    let default_subject_id = schema_builder.add_u64_field("default_subject_id", STORED | FAST);
    let default_subject_name = schema_builder.add_text_field("default_subject_name", STORED);
    let default_subject_name_cn = schema_builder.add_text_field("default_subject_name_cn", STORED);
    let schema = schema_builder.build();

    let index = Index::create_in_dir(index_dir, schema)?;
    let mut writer = index.writer_with_num_threads::<TantivyDocument>(1, 32_000_000)?;
    let mut stmt = db.prepare(
        "SELECT d.character_id, d.name, d.name_cn, d.name_en, d.romaji, d.gender, d.popularity,
                d.aliases, ds.id, ds.name, ds.name_cn
         FROM character_search_docs d
         LEFT JOIN subjects ds ON ds.id = (
             SELECT sc.subject_id
             FROM subject_characters sc
             JOIN subjects s2 ON s2.id = sc.subject_id
             WHERE sc.character_id = d.character_id
             ORDER BY CASE WHEN sc.type = 1 THEN 0 ELSE 1 END,
                      s2.popularity DESC,
                      sc.order_num ASC
             LIMIT 1
         )
         ORDER BY d.character_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, String>(3).unwrap_or_default(),
            row.get::<_, String>(4).unwrap_or_default(),
            row.get::<_, String>(5).unwrap_or_else(|_| "?".to_string()),
            row.get::<_, i64>(6).unwrap_or(0),
            row.get::<_, String>(7).unwrap_or_default(),
            row.get::<_, Option<i64>>(8).unwrap_or(None),
            row.get::<_, String>(9).unwrap_or_default(),
            row.get::<_, String>(10).unwrap_or_default(),
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (
            cid,
            cname,
            cname_cn,
            cname_en,
            cromaji,
            cgender,
            cpopularity,
            caliases,
            subject_id,
            subject_name,
            subject_name_cn,
        ) = row?;
        let terms = expanded_tantivy_search_terms(&[
            cname.as_str(),
            cname_cn.as_str(),
            cname_en.as_str(),
            cromaji.as_str(),
            caliases.as_str(),
        ]);
        writer.add_document(doc!(
            id => cid as u64,
            name => cname,
            name_cn => cname_cn,
            name_en => cname_en,
            romaji => cromaji,
            gender => cgender,
            popularity => cpopularity.max(0) as u64,
            aliases => caliases,
            search_terms => terms,
            default_subject_id => subject_id.unwrap_or(0).max(0) as u64,
            default_subject_name => subject_name,
            default_subject_name_cn => subject_name_cn,
        ))?;
        count += 1;
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    println!("Tantivy characters indexed: {count}");
    Ok(())
}

fn build_tantivy_subject_index(db: &Connection, dump_dir: &Path, index_dir: &Path) -> Result<()> {
    recreate_dir(index_dir)?;
    let subject_aliases = load_subject_aliases(dump_dir)?;

    let mut schema_builder = Schema::builder();
    let id = schema_builder.add_u64_field("id", STORED | FAST);
    let stype = schema_builder.add_u64_field("type", INDEXED | STORED | FAST);
    let date = schema_builder.add_text_field("date", STORED);
    let popularity = schema_builder.add_u64_field("popularity", STORED | FAST);
    let name = schema_builder.add_text_field("name", TEXT | STORED);
    let name_cn = schema_builder.add_text_field("name_cn", TEXT | STORED);
    let aliases = schema_builder.add_text_field("aliases", TEXT | STORED);
    let search_terms = schema_builder.add_text_field("search_terms", TEXT);
    let schema = schema_builder.build();

    let index = Index::create_in_dir(index_dir, schema)?;
    let mut writer = index.writer_with_num_threads::<TantivyDocument>(1, 16_000_000)?;
    let mut stmt = db.prepare(
        "SELECT id, type, date, popularity, name, name_cn
         FROM subjects
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1).unwrap_or(0),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, i64>(3).unwrap_or(0),
            row.get::<_, String>(4).unwrap_or_default(),
            row.get::<_, String>(5).unwrap_or_default(),
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (sid, subject_type, sdate, spopularity, sname, sname_cn) = row?;
        let subject_aliases = subject_aliases.get(&sid).map(String::as_str).unwrap_or("");
        let terms =
            expanded_tantivy_search_terms(&[sname.as_str(), sname_cn.as_str(), subject_aliases]);
        writer.add_document(doc!(
            id => sid as u64,
            stype => subject_type.max(0) as u64,
            date => sdate,
            popularity => spopularity.max(0) as u64,
            name => sname,
            name_cn => sname_cn,
            aliases => subject_aliases,
            search_terms => terms,
        ))?;
        count += 1;
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    println!("Tantivy subjects indexed: {count}");
    Ok(())
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
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
        let Some(id) = item.get("id").and_then(|x| x.as_i64()) else {
            return Ok(());
        };
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
            let v: Value = serde_json::from_str(t).with_context(|| {
                format!(
                    "failed to parse images jsonl line for {}",
                    images_path.display()
                )
            })?;
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
        DROP TABLE IF EXISTS subject_details;
        DROP TABLE IF EXISTS character_profile;
        DROP TABLE IF EXISTS character_aliases;
        DROP TABLE IF EXISTS character_search_docs;
        DROP TABLE IF EXISTS subjects;
        CREATE TABLE subjects (
            id INTEGER PRIMARY KEY,
            type INTEGER NOT NULL,
            name TEXT NOT NULL,
            name_cn TEXT NOT NULL DEFAULT '',
            date TEXT NOT NULL DEFAULT '',
            year INTEGER NOT NULL DEFAULT -1,
            popularity INTEGER NOT NULL DEFAULT 0,
            score REAL NOT NULL DEFAULT -1
        );

        CREATE TABLE subject_details (
            subject_id INTEGER PRIMARY KEY,
            tags_json TEXT NOT NULL DEFAULT '[]',
            meta_tags_json TEXT NOT NULL DEFAULT '[]',
            FOREIGN KEY(subject_id) REFERENCES subjects(id)
        );

        DROP TABLE IF EXISTS characters;
        CREATE TABLE characters (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            role INTEGER NOT NULL DEFAULT 0,
            popularity INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE character_profile (
            character_id INTEGER PRIMARY KEY,
            name_cn TEXT NOT NULL DEFAULT '',
            name_en TEXT NOT NULL DEFAULT '',
            romaji TEXT NOT NULL DEFAULT '',
            gender TEXT NOT NULL DEFAULT '?',
            summary TEXT NOT NULL DEFAULT '',
            FOREIGN KEY(character_id) REFERENCES characters(id)
        );

        CREATE TABLE character_aliases (
            character_id INTEGER NOT NULL,
            alias TEXT NOT NULL,
            alias_type TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 100,
            PRIMARY KEY (character_id, alias, alias_type),
            FOREIGN KEY(character_id) REFERENCES characters(id)
        );

        CREATE TABLE character_search_docs (
            character_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            name_cn TEXT NOT NULL DEFAULT '',
            name_en TEXT NOT NULL DEFAULT '',
            romaji TEXT NOT NULL DEFAULT '',
            aliases TEXT NOT NULL DEFAULT '',
            gender TEXT NOT NULL DEFAULT '?',
            popularity INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(character_id) REFERENCES characters(id)
        );

        DROP TABLE IF EXISTS subject_characters;
        CREATE TABLE subject_characters (
            subject_id INTEGER NOT NULL,
            character_id INTEGER NOT NULL,
            type INTEGER NOT NULL,
            order_num INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX idx_sub_char ON subject_characters(subject_id, character_id);
        CREATE INDEX idx_char_sub ON subject_characters(character_id, subject_id);
        CREATE INDEX idx_subjects_picker ON subjects(type, year, popularity DESC);
        CREATE INDEX idx_subjects_popularity ON subjects(popularity DESC);
        CREATE INDEX idx_characters_role_pop ON characters(role, popularity DESC);

        DROP TABLE IF EXISTS subject_fts;
        CREATE VIRTUAL TABLE subject_fts USING fts5(
            name,
            name_cn,
            aliases,
            content='',
            contentless_delete=1,
            tokenize='unicode61'
        );

        DROP TABLE IF EXISTS character_fts;
        CREATE VIRTUAL TABLE character_fts USING fts5(
            name,
            name_cn,
            name_en,
            romaji,
            aliases,
            content='',
            contentless_delete=1,
            tokenize='unicode61'
        );
        ",
    )?;
    Ok(())
}

fn rebuild_search_tables(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "
        DROP TABLE IF EXISTS subject_fts;
        CREATE VIRTUAL TABLE subject_fts USING fts5(
            name,
            name_cn,
            aliases,
            content='',
            contentless_delete=1,
            tokenize='unicode61'
        );

        DROP TABLE IF EXISTS character_fts;
        CREATE VIRTUAL TABLE character_fts USING fts5(
            name,
            name_cn,
            name_en,
            romaji,
            aliases,
            content='',
            contentless_delete=1,
            tokenize='unicode61'
        );
        ",
    )?;
    Ok(())
}

fn process_subjects(db: &mut Connection, dump_dir: &Path) -> Result<HashSet<i64>> {
    let file = File::open(dump_dir.join("subject.jsonlines")).with_context(|| {
        format!(
            "failed to open {}",
            dump_dir.join("subject.jsonlines").display()
        )
    })?;
    let reader = BufReader::new(file);
    let mut valid_ids = HashSet::new();

    let tx = db.transaction()?;
    {
        let mut subject_stmt = tx.prepare(
            "INSERT INTO subjects (id, type, name, name_cn, date, year, popularity, score)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut detail_stmt = tx.prepare(
            "INSERT INTO subject_details (subject_id, tags_json, meta_tags_json)
             VALUES (?1, ?2, ?3)",
        )?;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
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
                let date = v["date"].as_str().unwrap_or("");
                let year = parse_year(date);
                let tags_json = v
                    .get("tags")
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "[]".to_string());
                let meta_tags_json = v
                    .get("meta_tags")
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "[]".to_string());
                subject_stmt.execute((
                    id,
                    type_id,
                    v["name"].as_str().unwrap_or(""),
                    v["name_cn"].as_str().unwrap_or(""),
                    date,
                    year,
                    collects,
                    v["score"].as_f64().unwrap_or(-1.0),
                ))?;
                detail_stmt.execute((id, tags_json, meta_tags_json))?;
            }
        }
    }
    tx.commit()?;
    Ok(valid_ids)
}

fn process_relations(
    db: &mut Connection,
    dump_dir: &Path,
    valid_subjects: &HashSet<i64>,
) -> Result<HashSet<i64>> {
    let file = File::open(dump_dir.join("subject-characters.jsonlines")).with_context(|| {
        format!(
            "failed to open {}",
            dump_dir.join("subject-characters.jsonlines").display()
        )
    })?;
    let reader = BufReader::new(file);
    let mut valid_chars = HashSet::new();

    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO subject_characters (subject_id, character_id, type, order_num)
             VALUES (?1, ?2, ?3, ?4)",
        )?;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
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

fn process_characters(
    db: &mut Connection,
    dump_dir: &Path,
    valid_chars: &HashSet<i64>,
) -> Result<()> {
    let file = File::open(dump_dir.join("character.jsonlines")).with_context(|| {
        format!(
            "failed to open {}",
            dump_dir.join("character.jsonlines").display()
        )
    })?;
    let reader = BufReader::new(file);

    let mut total_kept = 0;

    let tx = db.transaction()?;
    {
        let mut char_stmt = tx.prepare(
            "INSERT INTO characters (id, name, role, popularity)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut profile_stmt = tx.prepare(
            "INSERT INTO character_profile (character_id, name_cn, name_en, romaji, gender, summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut alias_stmt = tx.prepare(
            "INSERT OR IGNORE INTO character_aliases (character_id, alias, alias_type, priority)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut search_stmt = tx.prepare(
            "INSERT INTO character_search_docs (character_id, name, name_cn, name_en, romaji, aliases, gender, popularity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line)?;

            let id = v["id"].as_i64().unwrap_or(0);
            let collects = v["collects"].as_i64().unwrap_or(0);
            let comments = v["comments"].as_i64().unwrap_or(0);
            let popularity = collects + comments;

            // 必须是前置步骤保留下的角色，并且过滤掉 0 收藏量的僵尸角色
            if valid_chars.contains(&id) && collects > 0 {
                total_kept += 1;
                let name = v["name"].as_str().unwrap_or("");
                let infobox = v["infobox"].as_str().unwrap_or("");
                let summary = v["summary"].as_str().unwrap_or("");
                let parsed = parse_character_infobox(infobox);
                let aliases_text = parsed
                    .aliases
                    .iter()
                    .map(|a| a.value.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");

                char_stmt.execute((id, name, v["role"].as_i64().unwrap_or(0), popularity))?;
                profile_stmt.execute((
                    id,
                    parsed.name_cn.as_deref().unwrap_or(""),
                    parsed.name_en.as_deref().unwrap_or(""),
                    parsed.romaji.as_deref().unwrap_or(""),
                    parsed.gender.as_str(),
                    summary,
                ))?;
                for alias in &parsed.aliases {
                    alias_stmt.execute((
                        id,
                        alias.value.as_str(),
                        alias.alias_type.as_str(),
                        alias.priority,
                    ))?;
                }
                search_stmt.execute((
                    id,
                    name,
                    parsed.name_cn.as_deref().unwrap_or(""),
                    parsed.name_en.as_deref().unwrap_or(""),
                    parsed.romaji.as_deref().unwrap_or(""),
                    aliases_text,
                    parsed.gender.as_str(),
                    popularity,
                ))?;
            }
        }
    }
    tx.commit()?;
    db.execute(
        "DELETE FROM subject_characters WHERE character_id NOT IN (SELECT id FROM characters)",
        [],
    )?;
    println!("实际最终写入库的优质角色数量: {}", total_kept);
    Ok(())
}

fn prune_subjects_without_characters(db: &Connection) -> Result<()> {
    let before: i64 = db.query_row("SELECT COUNT(*) FROM subjects", [], |row| row.get(0))?;
    let orphan_subjects: i64 = db.query_row(
        "SELECT COUNT(*)
         FROM subjects s
         WHERE NOT EXISTS (
             SELECT 1 FROM subject_characters sc WHERE sc.subject_id = s.id
         )",
        [],
        |row| row.get(0),
    )?;

    db.execute_batch(
        "
        DELETE FROM subject_details
        WHERE subject_id IN (
            SELECT s.id
            FROM subjects s
            WHERE NOT EXISTS (
                SELECT 1 FROM subject_characters sc WHERE sc.subject_id = s.id
            )
        );
        DELETE FROM subjects
        WHERE NOT EXISTS (
            SELECT 1 FROM subject_characters sc WHERE sc.subject_id = subjects.id
        );
        DELETE FROM subject_characters
        WHERE subject_id NOT IN (SELECT id FROM subjects);
        ",
    )?;

    let after: i64 = db.query_row("SELECT COUNT(*) FROM subjects", [], |row| row.get(0))?;
    println!(
        "已删除无可用角色关联作品: {}，作品数 {} -> {}",
        orphan_subjects, before, after
    );
    Ok(())
}

fn build_search_indexes(db: &mut Connection, dump_dir: &Path) -> Result<()> {
    let subject_aliases = load_subject_aliases(dump_dir)?;
    let subject_rows = {
        let mut stmt = db.prepare("SELECT id, name, name_cn FROM subjects ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO subject_fts(rowid, name, name_cn, aliases) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (id, name, name_cn) in subject_rows {
            let aliases = subject_aliases.get(&id).map(String::as_str).unwrap_or("");
            stmt.execute((id, name, name_cn, aliases))?;
        }
    }
    tx.commit()?;

    let character_rows = {
        let mut stmt = db.prepare(
            "SELECT character_id, name, name_cn, name_en, romaji, aliases
             FROM character_search_docs
             ORDER BY character_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
                row.get::<_, String>(4).unwrap_or_default(),
                row.get::<_, String>(5).unwrap_or_default(),
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let tx = db.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO character_fts(rowid, name, name_cn, name_en, romaji, aliases)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (id, name, name_cn, name_en, romaji, aliases) in character_rows {
            stmt.execute((id, name, name_cn, name_en, romaji, aliases))?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn load_subject_aliases(dump_dir: &Path) -> Result<std::collections::HashMap<i64, String>> {
    let file = File::open(dump_dir.join("subject.jsonlines")).with_context(|| {
        format!(
            "failed to open {}",
            dump_dir.join("subject.jsonlines").display()
        )
    })?;
    let reader = BufReader::new(file);
    let mut aliases = std::collections::HashMap::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let id = v["id"].as_i64().unwrap_or(0);
        let value = infobox_aliases(v["infobox"].as_str().unwrap_or(""));
        if id > 0 && !value.is_empty() {
            aliases.insert(id, value);
        }
    }
    Ok(aliases)
}

#[derive(Debug, Clone)]
struct AliasEntry {
    value: String,
    alias_type: String,
    priority: i64,
}

#[derive(Debug, Default)]
struct ParsedCharacterInfobox {
    name_cn: Option<String>,
    name_en: Option<String>,
    romaji: Option<String>,
    gender: String,
    aliases: Vec<AliasEntry>,
}

fn parse_year(date: &str) -> i64 {
    date.split('-')
        .next()
        .and_then(|y| y.parse::<i64>().ok())
        .unwrap_or(-1)
}

fn parse_character_infobox(infobox: &str) -> ParsedCharacterInfobox {
    let name_cn = extract_infobox_field(infobox, "简体中文名");
    let gender = match extract_infobox_field(infobox, "性别").as_deref() {
        Some("男") => "male",
        Some("女") => "female",
        _ => "?",
    }
    .to_string();

    let mut aliases = Vec::new();
    collect_alias_block(infobox, &mut aliases);

    let name_en = first_alias_value(&aliases, &["en"]);
    let romaji = first_alias_value(&aliases, &["romaji"]);

    if let Some(value) = &name_cn {
        push_alias(&mut aliases, value, "cn", 10);
    }
    if let Some(value) = &name_en {
        push_alias(&mut aliases, value, "en", 30);
    }
    if let Some(value) = &romaji {
        push_alias(&mut aliases, value, "romaji", 35);
    }

    ParsedCharacterInfobox {
        name_cn,
        name_en,
        romaji,
        gender,
        aliases,
    }
}

fn collect_alias_block(infobox: &str, aliases: &mut Vec<AliasEntry>) {
    let Some(start) = infobox.find("|别名={") else {
        return;
    };
    let rest = &infobox[start + "|别名={".len()..];
    let end = rest
        .find("\n}")
        .or_else(|| rest.find("\r\n}"))
        .unwrap_or(rest.len());

    for part in rest[..end].split('[') {
        let Some(end) = part.find(']') else {
            continue;
        };
        let raw = part[..end].trim();
        if raw.is_empty() {
            continue;
        }
        let (key, value) = raw
            .split_once('|')
            .map(|(k, v)| (k.trim(), v.trim()))
            .unwrap_or(("", raw));
        if value.is_empty() {
            continue;
        }
        let (alias_type, priority) = normalize_alias_type(key);
        push_alias(aliases, value, alias_type, priority);
    }
}

fn normalize_alias_type(key: &str) -> (&'static str, i64) {
    match key {
        "简体中文名" | "中文名" | "第二中文名" | "第三中文名" | "繁体中文名" => {
            ("cn_secondary", 20)
        }
        "英文名" | "英文名二" | "第二英文名" => ("en", 30),
        "罗马字" => ("romaji", 35),
        "日文名" | "第二日文名" | "原名" => ("jp", 40),
        "纯假名" => ("kana", 45),
        "昵称" | "昵称2" | "外号" | "称号" | "重要绰号" => ("nickname", 50),
        "代号" => ("code_name", 55),
        "本名" | "真名" | "全名" => ("real_name", 55),
        "" => ("unkeyed", 60),
        _ => ("other", 90),
    }
}

fn push_alias(aliases: &mut Vec<AliasEntry>, value: &str, alias_type: &str, priority: i64) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if aliases
        .iter()
        .any(|existing| existing.value == value && existing.alias_type == alias_type)
    {
        return;
    }
    aliases.push(AliasEntry {
        value: value.to_string(),
        alias_type: alias_type.to_string(),
        priority,
    });
}

fn first_alias_value(aliases: &[AliasEntry], alias_types: &[&str]) -> Option<String> {
    aliases
        .iter()
        .filter(|a| alias_types.contains(&a.alias_type.as_str()))
        .min_by_key(|a| a.priority)
        .map(|a| a.value.clone())
}

fn infobox_aliases(infobox: &str) -> String {
    let Some(start) = infobox.find("|别名={") else {
        return String::new();
    };
    let rest = &infobox[start + "|别名={".len()..];
    let end = rest
        .find("\n}")
        .or_else(|| rest.find("\r\n}"))
        .unwrap_or(rest.len());
    rest[..end]
        .split('[')
        .filter_map(|part| {
            let end = part.find(']')?;
            let value = part[..end].split('|').next_back()?.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn expanded_tantivy_search_terms(values: &[&str]) -> String {
    let mut terms = HashSet::new();
    for value in values {
        for token in search_tokens(value) {
            terms.insert(token.clone());
            for term in cjk_sub_terms(&token) {
                terms.insert(term);
            }
        }
    }
    let mut out = terms.into_iter().collect::<Vec<_>>();
    out.sort_unstable();
    out.join(" ")
}

fn search_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ',' | '，'
                        | '、'
                        | '/'
                        | '\\'
                        | '／'
                        | '|'
                        | ';'
                        | '；'
                        | ':'
                        | '：'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                        | '['
                        | ']'
                        | '【'
                        | '】'
                        | '・'
                        | '·'
                        | '「'
                        | '」'
                        | '"'
                        | '\''
                )
        })
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn cjk_sub_terms(token: &str) -> Vec<String> {
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() < 2 || !chars.iter().any(|ch| is_cjk(*ch)) {
        return Vec::new();
    }

    let mut terms = Vec::new();
    for start in 0..chars.len() {
        for len in 2..=3 {
            if start + len <= chars.len() {
                terms.push(chars[start..start + len].iter().collect());
            }
        }
    }
    if chars.len() > 2 {
        for start in 1..chars.len() - 1 {
            terms.push(chars[start..].iter().collect());
        }
    }
    terms
}

fn is_cjk(ch: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&ch)
        || ('\u{f900}'..='\u{faff}').contains(&ch)
        || ('\u{3040}'..='\u{30ff}').contains(&ch)
}

fn extract_infobox_field(infobox: &str, key: &str) -> Option<String> {
    let pattern = format!("|{}=", key);
    let start = infobox.find(&pattern)? + pattern.len();
    let rest = &infobox[start..];
    let end = rest
        .find('\n')
        .or_else(|| rest.find('\r'))
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
