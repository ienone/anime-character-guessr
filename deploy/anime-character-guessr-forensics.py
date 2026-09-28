#!/usr/bin/env python3
"""Bounded local probes and source-linked evidence capture; never restarts the app."""
import collections
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request

SERVICE = os.environ.get("FORENSICS_SERVICE", "anime-character-guessr.service")
APP = Path(os.environ.get("FORENSICS_APP", "/opt/anime-character-guessr"))
STATE = Path(os.environ.get("FORENSICS_STATE", "/run/anime-character-guessr-forensics"))
OUTPUT = Path(os.environ.get("FORENSICS_OUTPUT", "/var/log/anime-character-guessr-forensics"))
BASE = os.environ.get("FORENSICS_BASE_URL", "http://127.0.0.1:3001").rstrip("/")
PORT = urllib.parse.urlparse(BASE).port or 80
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
PROBES = {
    "health": "/health",
    "rooms": "/room-count",
    "search": "/api/archive/search/characters?keyword=saber&limit=1",
}


def write_json(path, value):
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(value, ensure_ascii=False) + "\n")
    temp.replace(path)


def probe(name, path):
    started = time.monotonic()
    result = {"ok": False}
    try:
        with OPENER.open(BASE + path, timeout=2) as response:
            result["status"] = response.status
            body = response.read(65537)
        if len(body) > 65536:
            raise ValueError("oversized response")
        if name == "health":
            result["ok"] = body.strip() == b"OK"
        else:
            data = json.loads(body)
            if name == "search":
                result["ok"] = isinstance(data.get("data"), list)
                if result["ok"]:
                    result["results"] = len(data["data"])
            else:
                result["ok"] = isinstance(data.get("count"), int)
                result["stats"] = {k: v for k, v in data.items()
                                   if isinstance(v, (int, float))}
    except urllib.error.HTTPError as error:
        result.update(status=error.code, error="http_error")
    except (OSError, ValueError, urllib.error.URLError, AttributeError) as error:
        result["error"] = type(error).__name__
    result["elapsed_ms"] = round((time.monotonic() - started) * 1000, 1)
    return result


def transition(previous, identity, healthy):
    """Re-arm after recovery and after process replacement (including PID reuse)."""
    if previous.get("identity") != identity or healthy:
        previous = {"identity": identity, "failures": 0, "captured": False}
    current = dict(previous)
    if not healthy:
        current["failures"] += 1
    return current, current["failures"] >= 3 and not current["captured"]


def identity_for(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return f"{pid}:{fields[19]}"
    except (OSError, IndexError):
        return None


def process_sample(pid):
    sample = {}
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            key, value = line.split(":", 1)
            if key in {"VmRSS", "VmSwap", "Threads"}:
                sample[key] = value.strip()
        sample["open_fds"] = len(list(Path(f"/proc/{pid}/fd").iterdir()))
    except OSError:
        pass
    return sample


def command(args, timeout=5):
    try:
        completed = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   timeout=timeout, text=True, errors="replace", check=False)
        return f"exit_code={completed.returncode}\n" + completed.stdout[:524288]
    except subprocess.TimeoutExpired as error:
        output = error.stdout or b""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        return "timeout\n" + output[:524288]
    except OSError as error:
        return f"{type(error).__name__}: {error}\n"


def capture(pid, identity, sample):
    directory = OUTPUT / f"{time.strftime('%Y%m%d-%H%M%S')}-pid{pid}"
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    write_json(directory / "trigger.json", sample)
    for source in [STATE / "recent.jsonl", APP / "build-info.json"]:
        if source.is_file():
            (directory / source.name).write_bytes(source.read_bytes())
    for name in ["status", "limits", "maps", "wchan"]:
        try:
            (directory / f"proc-{name}.txt").write_bytes(Path(f"/proc/{pid}/{name}").read_bytes())
        except OSError:
            pass
    executable = Path(f"/proc/{pid}/exe")
    try:
        write_json(directory / "executable.json", {
            "path": os.readlink(executable),
            "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
            "identity": identity,
        })
    except OSError:
        pass
    commands = {
        "systemd-status.txt": ["systemctl", "status", SERVICE, "--no-pager"],
        "threads.txt": ["ps", "-L", "-p", str(pid), "-o", "pid,tid,psr,stat,wchan:32,pcpu,time,comm"],
        "file-descriptors.txt": ["ls", "-l", f"/proc/{pid}/fd"],
        "socket-listener.txt": ["ss", "-tanp", "sport", "=", f":{PORT}"],
        "journal.txt": ["journalctl", "-u", SERVICE, "--since", "10 minutes ago", "-n", "1000", "--no-pager", "-o", "short-iso"],
    }
    for name, args in commands.items():
        (directory / name).write_text(command(args))
    for name in ["access", "error"]:
        output = command(["tail", "-n", "500", f"/var/log/nginx/{name}.log"])
        # Queries can contain administration tokens; paths/statuses suffice here.
        (directory / f"nginx-{name}-tail.txt").write_text(re.sub(r'\?[^\s"]*', '?<redacted>', output))
    if identity_for(pid) == identity:
        args = ["timeout", "--signal=INT", "--kill-after=2s", "15s", "gdb", "-nx", "-q", "-batch",
                "-ex", "set pagination off", "-ex", "set debuginfod enabled off",
                "-ex", "set print frame-arguments none",
                "-ex", f"set substitute-path /build/anime-character-guessr {APP}/source",
                "-ex", f"directory {APP}/source/server-rs",
                "-ex", "info threads", "-ex", "thread apply all bt 40",
                "-ex", "detach", "-p", str(pid)]
        (directory / "gdb-all-threads.txt").write_text(command(args, timeout=20))
    if identity_for(pid) == identity:
        (directory / "eu-stack.txt").write_text(command([
            "timeout", "--signal=INT", "--kill-after=2s", "7s", "eu-stack", "-v", "-p", str(pid)], timeout=10))
    if identity_for(pid) == identity:
        (directory / "strace.txt").write_text(command([
            "timeout", "--signal=INT", "--kill-after=2s", "3s", "strace", "-f", "-tt", "-T", "-e",
            "trace=futex,epoll_wait,epoll_pwait,poll,ppoll,accept,accept4,connect",
            "-p", str(pid)], timeout=6))
    print(json.dumps({"event": "hang_evidence_captured", "pid": pid, "directory": str(directory)}), flush=True)


def main():
    os.umask(0o077)
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    OUTPUT.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (STATE / "lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        try:
            pid = int(subprocess.check_output(["systemctl", "show", "-p", "MainPID", "--value", SERVICE], timeout=3))
        except (OSError, ValueError, subprocess.SubprocessError):
            return
        identity = identity_for(pid) if pid else None
        if not identity:
            return
        probes = {"health": probe("health", PROBES["health"])}
        if probes["health"]["ok"]:
            probes.update({name: probe(name, PROBES[name]) for name in ("rooms", "search")})
        if identity_for(pid) != identity:
            return
        sample = {"time": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "identity": identity,
                  "probes": probes, "process": process_sample(pid)}
        recent_path = STATE / "recent.jsonl"
        recent = collections.deque(maxlen=120)
        if recent_path.exists():
            recent.extend(recent_path.read_text().splitlines())
        recent.append(json.dumps(sample))
        recent_path.write_text("\n".join(recent) + "\n")
        state_path = STATE / "state.json"
        try:
            previous = json.loads(state_path.read_text())
        except (OSError, ValueError):
            previous = {}
        healthy = all(result["ok"] for result in probes.values())
        current, should_capture = transition(previous, identity, healthy)
        if should_capture:
            capture(pid, identity, sample)
            current["captured"] = True
        write_json(state_path, current)


if __name__ == "__main__":
    main()
