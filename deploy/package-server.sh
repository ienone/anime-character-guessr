#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
target="${1:-x86_64-unknown-linux-musl}"
package=release/server-rs
mkdir -p "$package/source"
cp "server-rs/target/$target/release/server-rs" "$package/server-rs"
objcopy --only-keep-debug "$package/server-rs" "$package/server-rs.debug"
strip --strip-debug "$package/server-rs"
objcopy --add-gnu-debuglink="$package/server-rs.debug" "$package/server-rs"
git archive HEAD server-rs/src server-rs/Cargo.toml server-rs/Cargo.lock | tar -x -C "$package/source"
cp deploy/anime-character-guessr.env.example deploy/anime-character-guessr.service deploy/nginx-site.conf "$package/"
python3 - "$package" "$target" <<'PY'
import hashlib
import json
from pathlib import Path
import subprocess
import sys

package = Path(sys.argv[1])
manifest = {
    "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "target": sys.argv[2],
    "source_prefix": "/build/anime-character-guessr",
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "sha256": {name: hashlib.sha256((package / name).read_bytes()).hexdigest()
               for name in ("server-rs", "server-rs.debug")},
}
(package / "build-info.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY
tar -czf "server-rs-$target.tar.gz" -C "$package" .
