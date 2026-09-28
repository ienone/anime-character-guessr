import sqlite3
import json
import os
import time

DUMP_DIR = "../dump-2026-04-28.210420Z"
DB_PATH = "../archive.sqlite"

def init_db(cursor):
    cursor.executescript("""
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
    """)

def process_subjects(cursor):
    valid_ids = set()
    file_path = os.path.join(DUMP_DIR, "subject.jsonlines")
    
    with open(file_path, "r", encoding="utf-8") as f:
        print("Step 1: Parsing subject.jsonlines...")
        count_kept = 0
        for line in f:
            line = line.strip()
            if not line: continue
            
            v = json.loads(line)
            obj_id = v.get("id", 0)
            type_id = v.get("type", 0)
            nsfw = v.get("nsfw", False)
            
            if type_id not in (2, 4): continue
            if nsfw: continue
            
            rank = v.get("rank", 0)
            fav = v.get("favorite", {})
            collects = (fav.get("wish", 0) + fav.get("done", 0) + 
                        fav.get("doing", 0) + fav.get("on_hold", 0) + 
                        fav.get("dropped", 0))
            
            is_anime_valid = (type_id == 2) and (collects >= 100 or rank > 0)
            is_game_valid = (type_id == 4) and (collects >= 50 or rank > 0)
            
            if is_anime_valid or is_game_valid:
                valid_ids.add(obj_id)
                cursor.execute("""
                    INSERT INTO subjects (id, type, name, name_cn, date, nsfw, rank, collects, raw_json)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """, (
                    obj_id, type_id,
                    v.get("name", ""), v.get("name_cn", ""),
                    v.get("date", ""),
                    1 if nsfw else 0,
                    rank, collects, line
                ))
                count_kept += 1
                
        print(f"Kept {count_kept} subjects")
    return valid_ids

def process_relations(cursor, valid_subjects):
    valid_chars = set()
    file_path = os.path.join(DUMP_DIR, "subject-characters.jsonlines")
    
    with open(file_path, "r", encoding="utf-8") as f:
        print("\nStep 2: Parsing subject-characters.jsonlines...")
        count_kept = 0
        for line in f:
            line = line.strip()
            if not line: continue
            
            v = json.loads(line)
            sub_id = v.get("subject_id", 0)
            char_id = v.get("character_id", 0)
            type_id = v.get("type", 0)
            
            if sub_id in valid_subjects and type_id in (1, 2):
                valid_chars.add(char_id)
                cursor.execute("""
                    INSERT INTO subject_characters (subject_id, character_id, type, order_num)
                    VALUES (?, ?, ?, ?)
                """, (
                    sub_id, char_id, type_id, v.get("order", 0)
                ))
                count_kept += 1
                
        print(f"Kept {count_kept} relations")
    return valid_chars

def process_characters(cursor, valid_chars):
    file_path = os.path.join(DUMP_DIR, "character.jsonlines")
    
    with open(file_path, "r", encoding="utf-8") as f:
        print("\nStep 3: Parsing character.jsonlines...")
        count_kept = 0
        for line in f:
            line = line.strip()
            if not line: continue
            
            v = json.loads(line)
            obj_id = v.get("id", 0)
            collects = v.get("collects", 0)
            
            if obj_id in valid_chars and collects > 0:
                cursor.execute("""
                    INSERT INTO characters (id, name, role, collects, comments, raw_json)
                    VALUES (?, ?, ?, ?, ?, ?)
                """, (
                    obj_id,
                    v.get("name", ""),
                    v.get("role", 0),
                    collects,
                    v.get("comments", 0),
                    line
                ))
                count_kept += 1
                
        print(f"Kept {count_kept} characters")

def main():
    start_time = time.time()
    print("Building offline archive.sqlite...")
    
    conn = sqlite3.connect(DB_PATH)
    cursor = conn.cursor()
    
    cursor.execute("PRAGMA synchronous = OFF;")
    cursor.execute("PRAGMA journal_mode = MEMORY;")
    
    init_db(cursor)
    
    valid_subjects = process_subjects(cursor)
    valid_chars = process_relations(cursor, valid_subjects)
    process_characters(cursor, valid_chars)
    
    print("\nVACUUMing and building indexes...")
    conn.commit()
    cursor.execute("VACUUM;")
    cursor.execute("PRAGMA optimize;")
    
    conn.close()
    
    elapsed = time.time() - start_time
    size_mb = os.path.getsize(DB_PATH) / (1024 * 1024)
    print(f"\nDone in {elapsed:.2f} seconds.")
    print(f"archive.sqlite size: {size_mb:.2f} MB")

if __name__ == "__main__":
    main()