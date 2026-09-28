#!/usr/bin/env python3
"""Build a Bangumi snapshot; install it only while the game server has no rooms."""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import time
import urllib.request
import zipfile

LATEST = 'https://raw.githubusercontent.com/bangumi/Archive/master/aux/latest.json'
DUMP_FILES = ('subject', 'character', 'subject-characters', 'person', 'person-characters')


def download(url, path):
    subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error',
                    '--retry', '2', '--max-time', '1200', url, '-o', str(path)], check=True)


def build_snapshot(builder, metadata, archive_zip, output, scratch):
    digest = hashlib.sha256()
    with archive_zip.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    if 'sha256:' + digest.hexdigest() != metadata['digest']:
        raise RuntimeError('Bangumi archive checksum mismatch')
    dump = scratch / 'dump'
    dump.mkdir()
    with zipfile.ZipFile(archive_zip) as archive:
        for name in DUMP_FILES:
            filename = name + '.jsonlines'
            member = next(item for item in archive.namelist() if Path(item).name == filename)
            with archive.open(member) as src, (dump / filename).open('wb') as dst:
                shutil.copyfileobj(src, dst)
    output.mkdir(parents=True)
    args = [str(builder), '--dump-dir', str(dump), '--out', str(output / 'archive.sqlite'),
            '--tantivy-dir', str(output / 'tantivy')]
    subprocess.run(args + ['--mode', 'build-archive'], check=True)
    # Image URLs are filled on demand; the official dump does not include image files.
    images = scratch / 'images.json'
    images.write_text('[]')
    subprocess.run(args + ['--mode', 'migrate-app', '--app-db', str(output / 'cache.sqlite'),
                           '--images-path', str(images)], check=True)
    with sqlite3.connect(output / 'archive.sqlite') as db:
        if db.execute('PRAGMA quick_check').fetchone()[0] != 'ok':
            raise RuntimeError('Archive integrity check failed')
        metadata = dict(metadata, subjects=db.execute('SELECT count(*) FROM subjects').fetchone()[0],
                        characters=db.execute('SELECT count(*) FROM characters').fetchone()[0])
        if not metadata['subjects'] or not metadata['characters']:
            raise RuntimeError('Refusing to publish an empty archive')
        db.execute('CREATE TABLE archive_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL)')
        db.execute('INSERT INTO archive_metadata VALUES (?, ?)', ('snapshot', json.dumps(metadata)))
    with sqlite3.connect(output / 'cache.sqlite') as db:
        if not db.execute('SELECT count(*) FROM character_vas').fetchone()[0]:
            raise RuntimeError('Refusing to publish an empty voice actor cache')
    (output / 'data-version.json').write_text(json.dumps(metadata, indent=2) + '\n')
    return metadata


def has_rooms(base_url):
    with urllib.request.urlopen(base_url + '/room-count', timeout=5) as response:
        return json.load(response)['count'] > 0


def copy_database(source, target):
    with sqlite3.connect(source) as src, sqlite3.connect(target) as dst:
        src.backup(dst)


def install_snapshot(bundle, app, service, base_url):
    if has_rooms(base_url):
        print('Rooms are active; data update deferred to the next run.', flush=True)
        return
    backup = app / 'releases-backup' / (datetime.datetime.now().strftime('%Y%m%d-%H%M%S') + '-data')
    backup.mkdir(parents=True)
    live_db = app / 'data/app.sqlite'
    moved = []
    subprocess.run(['systemctl', 'stop', service], check=True)
    try:
        copy_database(live_db, backup / 'app.sqlite')
        for relative in ('archive.sqlite', 'data/tantivy', 'data-version.json'):
            live = app / relative
            staged = bundle / ('tantivy' if relative == 'data/tantivy' else relative)
            old = backup / Path(relative).name
            if live.exists():
                live.rename(old)
            moved.append((live, old))
            staged.rename(live)
        # Tantivy creates private metadata and lock files as the builder user.
        owner = live_db.stat()
        index = app / 'data/tantivy'
        for path in (index, *index.rglob('*')):
            shutil.chown(path, user=owner.st_uid, group=owner.st_gid)
        with sqlite3.connect(live_db) as db:
            db.execute('ATTACH DATABASE ? AS fresh', (str(bundle / 'cache.sqlite'),))
            db.execute('DELETE FROM character_vas')
            db.execute('INSERT INTO character_vas SELECT * FROM fresh.character_vas')
        subprocess.run(['systemctl', 'start', service], check=True)
        for _ in range(20):
            try:
                with urllib.request.urlopen(base_url + '/health', timeout=2) as response:
                    if response.read() == b'OK':
                        for old_backup in sorted(backup.parent.glob('*-data'))[:-2]:
                            shutil.rmtree(old_backup)
                        print('Snapshot installed. Backup: ' + str(backup), flush=True)
                        return
            except OSError:
                pass
            time.sleep(0.5)
        raise RuntimeError('Server did not become healthy after data update')
    except Exception:
        subprocess.run(['systemctl', 'stop', service], check=True)
        for live, old in reversed(moved):
            if live.is_dir():
                shutil.rmtree(live)
            elif live.exists():
                live.unlink()
            if old.exists():
                old.rename(live)
        if (backup / 'app.sqlite').exists():
            copy_database(backup / 'app.sqlite', live_db)
        subprocess.run(['systemctl', 'start', service], check=True)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--builder', type=Path, default=Path('/opt/anime-character-guessr/db-builder'))
    parser.add_argument('--app-dir', type=Path, default=Path('/opt/anime-character-guessr'))
    parser.add_argument('--output', type=Path, help='Build only; leave a portable snapshot in this new directory')
    parser.add_argument('--archive-zip', type=Path, help='Use an already downloaded official dump')
    parser.add_argument('--metadata', type=Path, help='Use its matching official latest.json')
    parser.add_argument('--service', default='anime-character-guessr.service')
    parser.add_argument('--base-url', default='http://127.0.0.1:3001')
    args = parser.parse_args()
    if not args.output and has_rooms(args.base_url):
        print('Rooms are active; data update deferred.', flush=True)
        return
    # Keep staging on the installation filesystem so final renames are atomic.
    with tempfile.TemporaryDirectory(prefix='anime-data-', dir=None if args.output else args.app_dir) as tmp:
        scratch = Path(tmp)
        meta_path = args.metadata or scratch / 'latest.json'
        if not args.metadata:
            download(LATEST, meta_path)
        metadata = json.loads(meta_path.read_text())
        version = args.app_dir / 'data-version.json'
        if not args.output and version.exists() and json.loads(version.read_text())['digest'] == metadata['digest']:
            print('Data is already current.', flush=True)
            return
        archive_zip = args.archive_zip or scratch / 'dump.zip'
        if not args.archive_zip:
            download(metadata['browser_download_url'], archive_zip)
        output = args.output.resolve() if args.output else scratch / 'snapshot'
        result = build_snapshot(args.builder.resolve(), metadata, archive_zip.resolve(), output, scratch)
        print(json.dumps(result, ensure_ascii=False), flush=True)
        if not args.output:
            install_snapshot(output, args.app_dir, args.service, args.base_url)


if __name__ == '__main__':
    main()
