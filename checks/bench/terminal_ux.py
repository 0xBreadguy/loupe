#!/usr/bin/env python3
"""Interactive terminal UX benchmark on synthetic T3 data.

Runs the real terminal presenter on a private PTY (no tmux, emulator or desktop), answers
its terminal-capability queries, applies its output to a small virtual screen, and accepts
a latency only when a synchronized-output frame shows the expected marker. Python's
standard library is the only dependency. Recovered from d627b28a (the Storage-only
version) and adapted to 1.3.0 and the Insights screen. TERMINAL_UX.md has the method, the
replay commands and what is still pending validation.

Everything is explicit and fails closed. Synthetic data only: the product gets `--db` on a
copy of a database this script built (or `--demo`) and a new EMPTY T3CODE_HOME, its environment
is built from nothing (never from this process's), no symlinked or foreign fixture is opened, and
no path under the real ~/.t3 or /tmp is accepted. The Storage-only scenario is the one exception
and needs an explicit flag.

Commands:
  fixture   build a synthetic data folder (board threads, a history for the reports); only its database is used
  snap      run one build, press keys, print the virtual screen after each (choose markers)
  live      old and new on matched live-board events, alternating trials
  insights  the new build alone: open, read, switch, scroll, exit; profile-driven
  storage   the new build alone: Storage ready and scanning (inherited from d627b28a)
  floor     the measuring path's own latency and the cost of its /proc samples (stand-in)
  selftest  the harness's own negative controls, against a stand-in product

Input is configurable because the Insights design is not final: a profile (JSON) names
the keys and the loading and ready markers. See checks/bench/terminal_ux_profile.json (product,
design-derived until calibrated) and terminal_ux_standin_profile.json (the selftest's own).
"""
from __future__ import annotations

import argparse
import concurrent.futures
import copy
import datetime as dt
import errno
import fcntl
import hashlib
import io
import json
import math
import os
from pathlib import Path
import pty
import pwd
import random
import re
import select
import shutil
import signal
import sqlite3
import stat
import statistics
import struct
import subprocess
import sys
import termios
import time
import unicodedata

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
SYNC_BEGIN = b"\x1b[?2026h"
SYNC_END = b"\x1b[?2026l"
LEGACY_NOW_MS = 1795026000000  # a fixed instant for the selftest's own fixtures (byte-reproducible); products never get it by default
COLS = 120
ROWS = 40
DEFAULT_ESC_WAIT_MS = 25.0  # crates/term/src/keys.rs ESC_WAIT; --source-root re-reads it
P = "orchestration_v2_projection_"


class Fail(Exception):
    """The evidence a result needs is absent or contradicts it. Never swallowed."""


# ---------------------------------------------------------------------------- display clock and period labels

ABBR = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
FULL_MONTH = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
PERIOD_WORDS = {"tw": "this week", "lw": "last week", "tm": "this month", "lm": "last month"}


def utc(ms: float) -> dt.datetime:
    return dt.datetime.fromtimestamp(ms / 1000, dt.timezone.utc)


def period_bounds(now_ms: float) -> dict[str, tuple[dt.date, dt.date]]:
    """First and last day of the four Insights periods at a display clock. The harness runs the product
    with TZ=UTC, so these are UTC days. A week starts on Monday (crates/core/src/recap.rs Period)."""
    today = utc(now_ms).date()
    monday = today - dt.timedelta(days=today.weekday())
    first_of_month = today.replace(day=1)
    last_month_end = first_of_month - dt.timedelta(days=1)
    nxt = (first_of_month + dt.timedelta(days=32)).replace(day=1)
    return {"tw": (monday, monday + dt.timedelta(days=6)),
            "lw": (monday - dt.timedelta(days=7), monday - dt.timedelta(days=1)),
            "tm": (first_of_month, nxt - dt.timedelta(days=1)),
            "lm": (last_month_end.replace(day=1), last_month_end)}


def period_dates(now_ms: float) -> dict[str, dict[str, str]]:
    """What the period row says for each period: `short` while a period is only requested (a first read),
    `full` once it has been read, as crates/core/src/recap_view.rs period_text and period_with_year spell
    them. `dates-check` compares these with the real product at several pinned clocks, so a wrong label
    here fails closed instead of passing another period's read."""
    out = {}
    for key, (a, b) in period_bounds(now_ms).items():
        if key in ("tw", "lw"):
            short = f"{ABBR[a.month - 1]} {a.day} – {b.day}" if a.month == b.month else f"{ABBR[a.month - 1]} {a.day} – {ABBR[b.month - 1]} {b.day}"
            out[key] = {"short": short, "full": f"{short}, {b.year}"}
        else:
            label = f"{FULL_MONTH[a.month - 1]} {a.year}"
            out[key] = {"short": label, "full": label}
    return out


def period_id(now_ms: float) -> tuple[str, str]:
    """Which week and which month a clock is in: a run that crosses either boundary changes what the periods mean."""
    d = utc(now_ms).date()
    year, week, _ = d.isocalendar()
    return f"{year}-W{week:02d}", f"{d.year}-{d.month:02d}"


class DisplayClock:
    """The instant the product's screen shows: pinned to the fixture's own `now_ms` (T3_FIXED_NOW) or the real clock."""
    fixed_ms: float | None = None

    @classmethod
    def now_ms(cls) -> float:
        return cls.fixed_ms if cls.fixed_ms is not None else time.time() * 1000


def expand_tokens(spec: str) -> str:
    """`<<dates:tw>>`, `<<short:lm>>` and `<<word:lw>>` become the regex-escaped label of that period at the display
    clock, taken now (a real clock moves between steps)."""
    if "<<" not in spec:
        return spec
    labels = period_dates(DisplayClock.now_ms())
    spec = re.sub(r"<<(dates|short):(tw|lw|tm|lm)>>", lambda m: re.escape(labels[m.group(2)]["full" if m.group(1) == "dates" else "short"]), spec)
    return re.sub(r"<<word:(tw|lw|tm|lm)>>", lambda m: PERIOD_WORDS[m.group(1)], spec)


# ---------------------------------------------------------------------------- utilities

def iso(ms: float) -> str:
    return dt.datetime.fromtimestamp(ms / 1000, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def load() -> dict:
    with open("/proc/loadavg", encoding="ascii") as handle:
        fields = handle.read().split()
    return {"1m": float(fields[0]), "5m": float(fields[1]), "15m": float(fields[2])}


def sha(path: Path) -> str:
    h = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def check_scratch(path: Path) -> Path:
    """Refuse /tmp and the real T3 home, resolving symlinks first. A path whose own last
    component is a symlink is refused too, so a link cannot stand in for a new folder."""
    raw = Path(os.path.abspath(path))
    if raw.is_symlink():
        raise ValueError(f"must not be a symlink: {raw}")
    path = raw.resolve()
    if path == Path("/tmp") or Path("/tmp") in path.parents:
        raise ValueError(f"must not be under /tmp: {path}")
    real_t3 = Path(pwd.getpwuid(os.getuid()).pw_dir) / ".t3"
    if path == real_t3 or real_t3 in path.parents:
        raise ValueError(f"must not be under the real T3 home: {path}")
    return path


def fresh_dir(path: Path) -> Path:
    """Create a folder that did not exist (no exist_ok, so an existing link or folder is an error)."""
    path = check_scratch(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        os.mkdir(path, 0o700)
    except FileExistsError:
        raise Fail(f"{path} already exists; every folder the harness uses must be new")
    return path


def open_inside(root: Path, rel: str) -> int:
    """Open root/rel for reading and return the descriptor. No link is followed at any level
    below root (O_NOFOLLOW at each step, relative to the parent's descriptor), so a symlink,
    inside or escaping, is refused before the file is opened. The file must be a regular file
    with one link: a hard link to some other file is refused as well."""
    parts = Path(rel).parts
    if not parts or Path(rel).is_absolute() or ".." in parts:
        raise Fail(f"unsafe relative path {rel!r}")
    try:
        fd = os.open(check_scratch(root), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        for part in parts[:-1]:
            nxt = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = nxt
        last = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=fd)
        os.close(fd)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.ENOTDIR):
            raise Fail(f"refusing {root}/{rel}: a symlink or non-directory is in the way ({error.strerror})")
        raise Fail(f"cannot open {root}/{rel}: {error.strerror}")
    info = os.fstat(last)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        os.close(last)
        raise Fail(f"refusing {root}/{rel}: not a regular file with a single link")
    return last


def copy_inside(root: Path, rel: str, dest: Path) -> str:
    """Copy root/rel (opened by open_inside) to a new file and return its sha256, hashed while
    it is copied: the source is read once and the copy is never read back."""
    h = hashlib.sha256()
    fd = open_inside(root, rel)
    with os.fdopen(fd, "rb") as src, open(dest, "xb") as out:
        for chunk in iter(lambda: src.read(1024 * 1024), b""):
            h.update(chunk)
            out.write(chunk)
    return h.hexdigest()


def nearest_rank(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(q * len(ordered)) - 1)]


def summarize(values: list[float]) -> dict:
    values = sorted(values)
    if not values:
        return {"n": 0, "error": "no confirmed samples"}
    return {"n": len(values), "median_ms": statistics.median(values), "p95_ms": nearest_rank(values, 0.95),
            "min_ms": values[0], "max_ms": values[-1], "range_ms": [values[0], values[-1]]}


def source_esc_wait(root: Path | None) -> float:
    """ESC_WAIT from the checkout the binary was built from, else the 1.3.0 value."""
    if root is not None:
        text = (Path(root) / "crates/term/src/keys.rs").read_text()
        match = re.search(r"pub const ESC_WAIT: Duration = Duration::from_millis\((\d+)\)", text)
        if not match:
            raise Fail(f"ESC_WAIT not found in {root}/crates/term/src/keys.rs")
        return float(match.group(1))
    return DEFAULT_ESC_WAIT_MS


KEYS = {
    "escape": b"\x1b", "tab": b"\t", "shift-tab": b"\x1b[Z", "enter": b"\r", "space": b" ",
    "up": b"\x1b[A", "down": b"\x1b[B", "right": b"\x1b[C", "left": b"\x1b[D",
    "pageup": b"\x1b[5~", "pagedown": b"\x1b[6~", "home": b"\x1b[H", "end": b"\x1b[F",
}


def key_bytes(name: str) -> bytes:
    if name in KEYS:
        return KEYS[name]
    if len(name) == 1:
        return name.encode()
    if re.fullmatch(r"ctrl-[a-z]", name):
        return bytes([ord(name[-1]) & 0x1F])
    raise Fail(f"unknown key name {name!r}; names: {sorted(KEYS)}, one character, or ctrl-<letter>")


# ---------------------------------------------------------------------------- fixture

HISTORY_PER_DAY = 10.7  # threads a day of history: the density of the original 150 threads over 14 days
MIN_RUNS_PER_PERIOD = 3


def auto_history_days(now_ms: float) -> int:
    """Days of history that reach back to the start of last month, so all four periods have data."""
    today = utc(now_ms).date()
    return (today - period_bounds(now_ms)["lm"][0]).days + 1


def make_fixture(root: Path, schema: Path, board: int, running: int, history_threads: int | None,
                 runs_per_thread: int, items_per_run: int, payload_bytes: int,
                 worktrees: int, files: int, now_ms: int | None = None, history_days: int | None = None) -> dict:
    """A new synthetic T3CODE_HOME.

    The clock: `now_ms` is the instant every timestamp is built relative to, by default the real clock
    when the fixture is built (the manifest records it; `T3_FIXED_NOW` is pinned to it and a real-clock
    run refuses a fixture that has gone stale). Build the fixture just before the run that uses it.

    The board: `board` threads titled "Bench task NNN", the first `running` of them with a
    running run and a running command item. The history: `history_threads` finished threads
    (default HISTORY_PER_DAY a day) with `runs_per_thread` runs of `items_per_run` padded items each,
    spread evenly over `history_days` before `now_ms` (default: back to the start of last month), so
    this week, last week, this month and last month each have records to read. The build refuses a
    clock at which one of them would hold fewer than MIN_RUNS_PER_PERIOD runs (early on a Monday or
    the first of a month): pin `now_ms` to another instant. Thread payloads carry `createdAt`, which
    is how the product knows when the records begin. Events
    are written as T3 writes them: rows in orchestration_events (application version 2,
    aggregate thread) and the thread-projections cursor, with the v2 log left empty.
    Optional worktree trees are for Storage only.
    """
    NOW_MS = int(now_ms) if now_ms is not None else int(time.time() * 1000) // 1000 * 1000
    history_days = history_days or auto_history_days(NOW_MS)
    if history_threads is None:
        history_threads = max(1, round(history_days * HISTORY_PER_DAY))
    root = check_scratch(root)
    if root.exists() and any(root.iterdir()):
        raise ValueError(f"fixture root is not empty: {root}")
    for sub in ("userdata", "project", "home", "tmp", "sqlite-tmp"):
        (root / sub).mkdir(parents=True, exist_ok=True)
    entries = 0
    paths: list[Path] = []
    if worktrees:
        (root / "worktrees/synthetic").mkdir(parents=True, exist_ok=True)
        paths = [root / "worktrees/synthetic" / f"task-{i:03d}" for i in range(worktrees)]

        def populate(item: tuple[int, Path]) -> int:
            _, tree = item
            tree.mkdir(parents=True, exist_ok=True)
            (tree / ".git").write_text("synthetic worktree marker\n")
            count = 1
            for group in range(max(1, math.ceil(files / 300))):
                folder = tree / f"pkg-{group // 8:02d}" / f"dir-{group:03d}"
                folder.mkdir(parents=True, exist_ok=True)
                count += 1
                for j in range(min(300, files - group * 300)):
                    os.close(os.open(folder / f"entry-{j:03d}", os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644))
                    count += 1
            return count

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            entries = sum(pool.map(populate, enumerate(paths)))

    db_path = root / "userdata/statev2.sqlite"
    db = sqlite3.connect(db_path)
    db.executescript(Path(schema).read_text())
    db.execute("PRAGMA journal_mode=WAL")
    rng = random.Random(20261119)  # fixed: the fixture is byte-reproducible per argument set
    db.execute("BEGIN")
    projects = [("p-atlas", "Atlas"), ("p-harbor", "Harbor"), ("p-lumen", "Lumen"), ("p-quill", "Quill")]
    for pid, title in projects:
        db.execute("INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at) VALUES(?,?,?,?,?,?)",
                   (pid, title, str(root / "project" / pid), "[]", iso(NOW_MS - 30 * 86400000), iso(NOW_MS)))
    for i in range(1, 97):
        db.execute("INSERT INTO effect_sql_migrations(migration_id,name) VALUES(?,?)", (i, f"SyntheticMigration{i:03d}"))
    models = [("codex", "gpt-6.1-sol"), ("claude", "claude-sonnet-5-5"), ("codex", "gpt-6.1-terra"), ("grok", "grok-5")]
    commands = ["rg -n flaky src", "cargo test -p core", "git status", "sed -n 1,80p src/lib.rs", "python3 tools/run.py"]
    kinds = ["command_execution", "file_search", "file_change", "reasoning", "dynamic_tool", "assistant_message"]
    filler = ("made up " * max(1, payload_bytes // 8))[:payload_bytes]
    seq = {"n": 0}
    versions: dict[str, int] = {}

    def event(stream: str, kind: str, at: str) -> None:
        versions[stream] = versions.get(stream, 0) + 1
        seq["n"] += 1
        db.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) "
                   "VALUES(?,?,?,?,?,?,'system','{\"note\":\"synthetic\"}','{}',2)",
                   (f"e-{seq['n']}", "thread", stream, versions[stream], kind, at))

    def thread(tid: str, title: str, pid: str, provider: tuple[str, str], created_ms: float, updated_ms: float, wt: str | None) -> None:
        payload = {"id": tid, "projectId": pid, "title": title, "createdAt": iso(created_ms), "activeProviderThreadId": f"pt-{tid}",
                   "worktreePath": wt, "branch": f"t3/{tid}", "updatedAt": iso(updated_ms),
                   "lineage": {"relationshipToParent": None}, "modelSelection": {"model": provider[1], "options": []}}
        db.execute(f"INSERT INTO {P}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,active_provider_thread_id,created_at,updated_at,payload_json,provider_instance_id) "
                   "VALUES(?,?,?,?,'full-access','default',?,?,?,?,?)",
                   (tid, pid, title, provider[0], f"pt-{tid}", iso(created_ms), iso(updated_ms), json.dumps(payload), provider[0]))
        event(tid, "thread.created", iso(created_ms))

    def run(tid: str, n: int, provider: tuple[str, str], start_ms: float, end_ms: float | None, status: str) -> str:
        rid = f"{tid}-r{n}"
        body = {"id": rid, "threadId": tid, "status": status, "providerThreadId": f"pt-{tid}", "requestedAt": iso(start_ms),
                "startedAt": iso(start_ms + 100), "completedAt": iso(end_ms) if end_ms else None,
                "providerInstanceId": provider[0], "modelSelection": {"model": provider[1], "options": []}}
        db.execute(f"INSERT INTO {P}runs(run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,completed_at,payload_json,provider_instance_id) "
                   "VALUES(?,?,?,?,?,?,?,?,?,?)",
                   (rid, tid, n, provider[0], f"pt-{tid}", status, iso(start_ms), iso(end_ms) if end_ms else None, json.dumps(body), provider[0]))
        event(tid, f"run.{status}", iso(end_ms or start_ms))
        return rid

    def item(tid: str, rid: str, ordinal: int, kind: str, status: str, start_ms: float, end_ms: float | None, pad: bool) -> None:
        body = {"startedAt": iso(start_ms), "completedAt": iso(end_ms) if end_ms else None, "schema": 2,
                "input": commands[ordinal % len(commands)], "toolName": "mcp__t3-code__task_status"}
        if pad:
            body["output"] = filler
        db.execute(f"INSERT INTO {P}turn_items(turn_item_id,thread_id,run_id,node_id,provider_thread_id,provider_turn_id,parent_item_id,ordinal,type,status,updated_at,payload_json) "
                   "VALUES(?,?,?,NULL,?,NULL,NULL,?,?,?,?,?)",
                   (f"{rid}-i{ordinal}", tid, rid, f"pt-{tid}", ordinal, kind, status, iso(end_ms or start_ms), json.dumps(body)))

    for i in range(board):
        tid = f"bench-{i:03d}"
        provider = models[i % len(models)]
        started = NOW_MS - (30 + i) * 60000
        live = i < running
        thread(tid, f"Bench task {i:03d}", projects[i % len(projects)][0], provider, started - 3600000,
               NOW_MS - 5000 - i * 1000 if live else started + 600000, str(paths[i]) if i < len(paths) else None)
        rid = run(tid, 1, provider, started, None if live else started + 600000, "running" if live else "completed")
        for n in range(3):
            item(tid, rid, n + 1, kinds[n % len(kinds)], "completed", started + n * 20000 + 1000, started + n * 20000 + 9000, False)
        if live:
            item(tid, rid, 4, "command_execution", "running", NOW_MS - 8000 - i, None, False)
    span = history_days * 86400000
    started_at: list[int] = []
    for h in range(history_threads):
        tid = f"hist-{h:04d}"
        provider = models[h % len(models)]
        # evenly spread with jitter inside each slot, so every period gets its share of the threads
        first = NOW_MS - span + int((h + rng.random()) / history_threads * (span - 3 * 3600000))
        started_at.append(first)
        thread(tid, f"History {h:04d}", projects[h % len(projects)][0], provider, first - 600000, first + runs_per_thread * 900000, None)
        for n in range(1, runs_per_thread + 1):
            start = first + (n - 1) * 900000
            end = start + 600000 + rng.randrange(0, 240000)
            status = "failed" if rng.random() < 0.06 else "completed"
            rid = run(tid, n, provider, start, end, status)
            for k in range(items_per_run):
                s = start + 1000 + k * (500000 // max(1, items_per_run))
                e = s + 200 + rng.randrange(0, 4000)
                item(tid, rid, k + 1, kinds[k % len(kinds)], "failed" if (status == "failed" and k == items_per_run - 1) else "completed", s, e, True)
    bounds = period_bounds(NOW_MS)
    day = lambda d: int(dt.datetime(d.year, d.month, d.day, tzinfo=dt.timezone.utc).timestamp() * 1000)
    per_period = {k: sum(1 for t in started_at if day(a) <= t < day(b) + 86400000) * runs_per_thread for k, (a, b) in bounds.items()}
    thin = {k: n for k, n in per_period.items() if n < MIN_RUNS_PER_PERIOD}
    if thin:
        db.close()
        raise Fail(f"at {iso(NOW_MS)} these periods would hold fewer than {MIN_RUNS_PER_PERIOD} runs: {thin}; a period with no records makes the "
                   "product show another period's data under its own label. Wait, or pin --now-ms to a later instant in the week and month")
    db.execute("INSERT INTO orchestration_v2_projection_metadata(projection_name,schema_version,last_sequence,updated_at) VALUES('thread-projections',1,?,?)",
               (seq["n"], iso(NOW_MS)))
    db.commit()
    if db.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
        raise AssertionError("synthetic database failed integrity_check")
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.close()
    for suffix in ("-wal", "-shm"):
        Path(str(db_path) + suffix).unlink(missing_ok=True)
    (root / "userdata/environment-id").write_text("synthetic-terminal-ux\n")
    manifest = {"format": 3, "synthetic": True, "now_ms": NOW_MS, "now_utc": iso(NOW_MS), "period_id": list(period_id(NOW_MS)),
                "history_days": history_days, "runs_in_period": per_period, "columns": COLS, "rows": ROWS,
                "board_threads": board, "running_threads": running, "history_threads": history_threads,
                "runs_per_thread": runs_per_thread, "items_per_run": items_per_run, "payload_bytes": payload_bytes,
                "database": str(db_path), "database_bytes": db_path.stat().st_size, "database_sha256": sha(db_path),
                "worktrees": worktrees, "files_per_worktree": files,
                "worktree_entries_including_git_and_directories": entries,
                "schema_sha256": sha(Path(schema)), "events": seq["n"]}
    (root / "fixture.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def fixture_manifest(root: Path) -> dict:
    """The manifest of a fixture this script built. The fixture folder is never a symlink and
    the manifest is opened without following links; a foreign folder has no such manifest."""
    with os.fdopen(open_inside(root, "fixture.json"), "rb") as handle:
        manifest = json.loads(handle.read())
    if not manifest.get("synthetic"):
        raise Fail("fixture manifest is not marked synthetic")
    if manifest.get("format") != 3:
        raise Fail("fixture format is not 3 (the dated, clock-relative fixture); rebuild it with the fixture command")
    return manifest


def trial_db(fixture: Path, scratch: Path) -> tuple[Path, str]:
    """A private copy of the fixture's database alone, in a new folder, for one trial: a writer
    never touches the shared file and every trial starts from identical bytes. The source is
    opened without following links, read once, and its hash must equal the manifest's, so a
    swapped file is refused. Returns the copy's path (what `--db` is given) and its sha256."""
    manifest = fixture_manifest(fixture)
    folder = fresh_dir(scratch / "synthetic-db")
    dest = folder / "statev2.sqlite"
    digest = copy_inside(fixture, "userdata/statev2.sqlite", dest)
    if digest != manifest["database_sha256"]:
        dest.unlink()
        raise Fail("the fixture database differs from its manifest (not the file this script built); refusing it")
    return dest, digest


def binary_is_script(path: Path) -> bool:
    """A file that starts with #! is a script, which is what the stand-in is and the product never is."""
    with open(path, "rb") as handle:
        return handle.read(2) == b"#!"


def clock_record(manifest: dict, real_clock: bool) -> dict:
    return {"mode": "real" if real_clock else "pinned", "fixture_now_utc": manifest["now_utc"],
            "fixture_age_min_at_report": round((time.time() * 1000 - manifest["now_ms"]) / 60000, 1),
            "period_id": manifest["period_id"], "display_now_utc": iso(DisplayClock.now_ms())}


def set_display_clock(manifest: dict | None, real_clock: bool, max_age_min: float) -> None:
    """Fix the display clock for a run, from the fixture's own `now_ms` or the real clock.

    A pinned run uses the fixture's instant, so reports are reproducible. A real-clock run needs data
    that is current under that clock: the fixture must be younger than `max_age_min`, built before
    now, and in the same week and month, or "this week" would mean something else than what was built
    (the product would then show another period's data under its own label). The run also checks, at
    the end of every trial, that neither boundary was crossed (check_clock_unchanged)."""
    if manifest is None:  # the product's --demo: made-up data relative to the display clock
        DisplayClock.fixed_ms = None if real_clock else float(LEGACY_NOW_MS)
        return
    if not real_clock:
        DisplayClock.fixed_ms = float(manifest["now_ms"])
        return
    DisplayClock.fixed_ms = None
    age_min = (time.time() * 1000 - manifest["now_ms"]) / 60000
    if age_min < -1:
        raise Fail(f"the fixture was built {-age_min:.1f} min in the future of this clock (now_ms {manifest['now_utc']}); rebuild it")
    if age_min > max_age_min:
        raise Fail(f"the fixture is {age_min:.0f} min old (limit {max_age_min:.0f}; built at {manifest['now_utc']}): its data was made relative to that "
                   "instant and a real clock has moved on. Build it again just before the run, or drop --real-clock to pin the clock to it")
    if list(period_id(time.time() * 1000)) != manifest["period_id"]:
        raise Fail(f"the real clock is in another week or month than the fixture (built {manifest['now_utc']}); build it again")


def check_clock_unchanged(manifest: dict, real_clock: bool) -> None:
    """A real-clock trial that crossed a week or month boundary read a different period than it started with."""
    if real_clock and list(period_id(time.time() * 1000)) != manifest["period_id"]:
        raise Fail("the real clock crossed a week or month boundary during the trial; its period labels changed. Run it again")


def trial_discovery_home(fixture: Path, scratch: Path) -> Path:
    """For the Storage-only scenario: a copy of the fixture's data folder to use as T3CODE_HOME
    for automatic discovery. Only `storage --accept-discovery-outside-db-rule` reaches this."""
    manifest = fixture_manifest(fixture)
    home = fresh_dir(scratch / "t3home")
    (home / "userdata").mkdir()
    if copy_inside(fixture, "userdata/statev2.sqlite", home / "userdata/statev2.sqlite") != manifest["database_sha256"]:
        raise Fail("the fixture database differs from its manifest; refusing it")
    copy_inside(fixture, "userdata/environment-id", home / "userdata/environment-id")
    return home


# Product arguments the harness accepts. Anything else is refused, so a command-line override
# can never point the product at a database, a snapshot or a window of its own: --db and --demo
# are added by product_command alone, and values may not be paths.
PRODUCT_FLAGS = {"--calm"}
PRODUCT_VALUE_FLAGS = {"--theme", "--interval", "--mode", "--screen", "--read-ms", "--read-bytes", "--esc-wait-ms"}
_VALUE = re.compile(r"[A-Za-z0-9_.:-]+")


def check_product_args(args: list[str]) -> list[str]:
    out: list[str] = []
    i = 0
    while i < len(args):
        flag, eq, value = args[i].partition("=")
        i += 1
        if flag in PRODUCT_FLAGS and not eq:
            out.append(flag)
        elif flag in PRODUCT_VALUE_FLAGS:
            if not eq:
                if i >= len(args):
                    raise Fail(f"product argument {flag} needs a value")
                value, i = args[i], i + 1
            if not _VALUE.fullmatch(value):
                raise Fail(f"product argument {flag}: {value!r} is not a plain word (paths and quoting are refused)")
            out.append(f"{flag}={value}")
        else:
            raise Fail(f"product argument {args[i - 1]!r} is not allowed: the harness supplies --db (a synthetic file of its own) or "
                       f"--demo, and accepts only {sorted(PRODUCT_FLAGS | PRODUCT_VALUE_FLAGS)}")
    return out


# ---------------------------------------------------------------------------- virtual screen

def char_width(ch: str) -> int:
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class ReferenceScreen:
    """The small ANSI subset emitted by t3-agents-term, including OSC 66, applied one byte at a time.
    This is the reference: simple and slow (about 5 ms for a full Insights frame). `Screen` below is the
    one the harness uses, and the selftest checks it against this on recorded and random input."""
    def __init__(self, w: int = COLS, h: int = ROWS):
        self.w, self.h = w, h
        self.cells = [[" "] * w for _ in range(h)]
        self.x = self.y = 0
        self.esc = bytearray()
        self.utf = bytearray()
        self.last: tuple[int, int] | None = None

    def text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.cells)

    def apply(self, data: bytes) -> None:
        for b in data:
            self.feed(b)

    def feed(self, b: int) -> None:
        if self.esc or b == 0x1b:
            self.last = None
            self.esc.append(b)
            if len(self.esc) == 1:
                return
            if self.esc[1] == ord("O"):
                if len(self.esc) >= 3:
                    self.esc.clear()
                return
            if self.esc[1] == ord("]"):
                self.osc(b)
                return
            if self.esc[1] != ord("["):
                if self.esc[1] in (ord("P"), ord("X"), ord("^"), ord("_")):
                    if self.esc.endswith(b"\x1b\\"):
                        self.esc.clear()
                    return
                self.esc.clear()
                return
            if len(self.esc) < 3:
                return
            if 0x40 <= b <= 0x7e:
                seq = bytes(self.esc)
                self.esc.clear()
                self.csi(seq)
            elif not (0x20 <= b <= 0x3f) or len(self.esc) > 64:
                self.esc.clear()
            return
        self.push_utf(b)

    def osc(self, b: int) -> None:
        st = self.esc.endswith(b"\x1b\\")
        if b != 7 and not st:
            if len(self.esc) > 4096:
                self.esc.clear()
            return
        end = len(self.esc) - (2 if st else 1)
        body = bytes(self.esc[2:end])
        self.esc.clear()
        if not body.startswith(b"66;") or b";" not in body[3:]:
            return
        params, text = body[3:].split(b";", 1)
        match = re.search(rb"(?:^|:)w=(\d+)", params)
        width = int(match.group(1)) if match else 0
        value = text.decode("utf-8", "replace")
        self.put(value, width or sum(char_width(c) for c in value))

    def csi(self, seq: bytes) -> None:
        final = seq[-1]
        body = seq[2:-1]
        if final in (ord("H"), ord("f")):
            fields = body.split(b";")
            try:
                row = int(re.sub(rb"\D", b"", fields[0]) or b"1")
                col = int(re.sub(rb"\D", b"", fields[1]) or b"1") if len(fields) > 1 else 1
            except ValueError:
                return
            self.y = min(self.h - 1, max(0, row - 1))
            self.x = min(self.w - 1, max(0, col - 1))
        elif final == ord("J"):
            if int(re.sub(rb"\D", b"", body) or b"0") >= 2:
                self.cells = [[" "] * self.w for _ in range(self.h)]

    def push_utf(self, b: int) -> None:
        self.utf.append(b)
        first = self.utf[0]
        need = 1 if first < 0x80 else 2 if 0xC0 <= first <= 0xDF else 3 if 0xE0 <= first <= 0xEF else 4
        if len(self.utf) < need:
            return
        raw = bytes(self.utf)
        self.utf.clear()
        try:
            ch = raw.decode("utf-8")
        except UnicodeDecodeError:
            return
        if self.last is not None and unicodedata.combining(ch):
            x, y = self.last
            self.cells[y][x] += ch
            return
        width = char_width(ch)
        if width:
            self.put(ch, width)
            if 0 <= self.x < self.w and 0 <= self.y < self.h:
                self.last = (self.x, self.y)

    def put(self, text: str, width: int) -> None:
        if not (0 <= self.x < self.w and 0 <= self.y < self.h):
            return
        x, y = self.x, self.y
        self.cells[y][x] = text
        for k in range(1, max(1, width)):
            if x + k < self.w:
                self.cells[y][x + k] = ""
        self.x = min(self.w, self.x + max(1, width))


_TOKEN = re.compile(
    rb"\x1b\[[\x20-\x3f]{0,62}[\x40-\x7e]"        # CSI
    rb"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"       # OSC, ended by BEL or ST
    rb"|\x1b[PX^_].*?\x1b\\"                       # DCS, SOS, PM, APC, ended by ST
    rb"|\x1bO."                                      # SS3
    rb"|\x1b."                                       # any other two-byte escape
    rb"|[^\x1b]+", re.S)
_WIDTH: dict[str, int] = {}
_OSC: dict[bytes, tuple[str, int] | None] = {}  # OSC sequence -> the (text, width) an OSC 66 puts on the screen, or None
_RUNS: dict[str, list[str] | None] = {}   # text run -> its characters when every one is single-width and non-combining


class Screen(ReferenceScreen):
    """The same screen, applied a token at a time: escape sequences by regular expression, runs of text by
    character. A frame costs about a fifth of the reference's time, which matters because the harness's per-frame
    handling delays its next read. Anything it does not recognise as well formed (a truncated or aborted sequence,
    bytes that are not UTF-8, a state left open by an earlier call) goes through the reference byte machine for the
    whole call, so the two always agree; the selftest replays recorded and random input to prove it."""

    def apply(self, data: bytes) -> None:
        if self.esc or self.utf:
            return super().apply(data)
        tokens = []
        pos = 0
        for m in _TOKEN.finditer(data):
            tok = m.group(0)
            if m.start() != pos or (tok[0] == 0x1b and len(tok) == 2 and tok[1] in b"[]PX^_"):
                return super().apply(data)
            pos = m.end()
            tokens.append(tok)
        if pos != len(data):
            return super().apply(data)
        # text runs must be whole UTF-8 characters; decode them before touching the screen
        decoded = []
        for tok in tokens:
            if tok[0] == 0x1b:
                decoded.append(tok)
            else:
                try:
                    decoded.append(tok.decode("utf-8"))
                except UnicodeDecodeError:
                    return super().apply(data)
        w, h = self.w, self.h
        width = _WIDTH
        for tok in decoded:
            if type(tok) is str:
                cells = self.cells                 # a clear (ESC[2J) replaces the grid, so fetch it for every run
                x, y = self.x, self.y
                last = self.last
                cached = _RUNS.get(tok)
                if cached is None:
                    simple = all(char_width(c) == 1 and not unicodedata.combining(c) for c in tok)
                    cached = (list(tok) if simple else None)
                    if len(_RUNS) < 8192:
                        _RUNS[tok] = cached
                if cached is not None and 0 <= y < h and 0 <= x and x + len(cached) <= w:
                    # a run of single-width, non-combining characters that fits the row: one slice
                    n = len(cached)
                    cells[y][x:x + n] = cached
                    x += n
                    if x < w:
                        last = (x, y)
                    elif n > 1:
                        last = (w - 1, y)
                    self.x, self.y, self.last = x, y, last
                    continue
                for ch in tok:
                    if last is not None and unicodedata.combining(ch):
                        cx, cy = last
                        cells[cy][cx] += ch
                        continue
                    cw = width.get(ch)
                    if cw is None:
                        cw = width[ch] = char_width(ch)
                    if not cw:
                        continue
                    if 0 <= x < w and 0 <= y < h:      # ReferenceScreen.put, then push_utf's update of `last`
                        row = cells[y]
                        row[x] = ch
                        for k in range(1, cw):
                            if x + k < w:
                                row[x + k] = ""
                        x = min(w, x + cw)
                        if x < w:
                            last = (x, y)
                self.x, self.y, self.last = x, y, last
                continue
            self.last = None
            kind = tok[1]
            if kind == 0x5b:      # [
                final = tok[-1]
                if final in (0x48, 0x66, 0x4a):
                    self.csi(tok)
            elif kind == 0x5d:    # ]
                parsed = _OSC.get(tok, 0)
                if parsed == 0:
                    parsed = None
                    body = tok[2:-1] if tok[-1] == 7 else tok[2:-2]
                    if body.startswith(b"66;") and b";" in body[3:]:        # the same parse as ReferenceScreen.osc
                        params, text = body[3:].split(b";", 1)
                        match = re.search(rb"(?:^|:)w=(\d+)", params)
                        value = text.decode("utf-8", "replace")
                        parsed = (value, (int(match.group(1)) if match else 0) or sum(char_width(c) for c in value))
                    if len(_OSC) < 8192:
                        _OSC[tok] = parsed
                if parsed is not None:
                    self.put(*parsed)


def screen_state(sc: "ReferenceScreen") -> tuple:
    return ([list(r) for r in sc.cells], sc.x, sc.y, sc.last, bytes(sc.esc), bytes(sc.utf))


def screen_equivalence(frames: list[bytes], random_cases: int = 400, seed: int = 11) -> dict:
    """Replay `frames` (and random sequences made of the pieces the product emits plus malformed ones) through
    the reference byte machine and the fast Screen, and compare the whole state after every call: cells, cursor,
    the combining-mark anchor and the unfinished-sequence buffers."""
    rng = random.Random(seed)
    pieces = [b"\x1b[H", b"\x1b[2J", b"\x1b[12;40H", b"\x1b[7;1H", b"\x1b[38;2;1;2;3m", b"\x1b[0m", b"\x1b[?2026h", b"\x1b[?2026l",
              b"\x1b[3A", b"\x1b]66;w=2;\xe2\x96\x8c\x07", b"\x1b]66;s=2:w=3;ab\x1b\\", b"\x1b]0;title\x07", b"\x1bP+q524742\x1b\\", b"\x1bOA",
              b"\x1b7", b"abc", " ".join(["word"] * 30).encode(), "▌ ─── ╭─╮ ▲ 3 above".encode(), "日本語 ok".encode(), "e\u0301x".encode(),
              b"\r\n", b"\x07", b"\xff\xfe", b"\xe2\x96", b"\x1b", b"\x1b[", b"\x1b[12", b"\x1b[1;\x01", b"\x1b]66;w=1;unterminated", b"\x1b[" + b"1" * 70 + b"H"]
    cases = [[f] for f in frames]
    for _ in range(random_cases):
        cases.append([b"".join(rng.choice(pieces) for _ in range(rng.randrange(1, 14))) for _ in range(rng.randrange(1, 4))])
    mismatches = 0
    for case in cases:
        ref, fast = ReferenceScreen(), Screen()
        for data in case:
            ref.apply(data)
            fast.apply(data)
            if screen_state(ref) != screen_state(fast):
                mismatches += 1
                break
    return {"cases": len(cases), "recorded": len(frames), "mismatches": mismatches}


# ---------------------------------------------------------------------------- PTY driver

def proc_io_rchar(pid: int) -> int | None:
    """Bytes the process has asked read(2) for, all threads. None when unreadable: the caller
    treats that as absent evidence, never as zero."""
    try:
        for line in Path(f"/proc/{pid}/io").read_text().splitlines():
            if line.startswith("rchar:"):
                return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return None


def proc_cpu_ticks(pid: int) -> int | None:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])
    except (OSError, ValueError, IndexError):
        return None


QUERY_ANSWERS = [  # the answers a foot terminal gives; see crates/term/src/caps.rs PROBE
    (rb"\x1b\[>0q", b"\x1bP>|foot(1.28.0)\x1b\\"),
    (rb"\x1b\[\?2027\$p", b"\x1b[?2027;1$y"),
    (rb"\x1b\[6n", b"\x1b[1;2R"),
    (rb"\x1b\[(?:0)?c", b"\x1b[?62;4c"),
    (rb"\x1bP\+q524742\x1b\\", b"\x1bP1+r524742\x1b\\"),
    (rb"\x1bP\+q5463\x1b\\", b"\x1bP1+r5463=31\x1b\\"),
    (rb"\x1b\]10;\?\x07", b"\x1b]10;rgb:c0c0/caca/f5f5\x07"),
    (rb"\x1b\]11;\?\x07", b"\x1b]11;rgb:1a1a/1b1b/2626\x07"),
]


class Pty:
    """One product process on a private PTY, its frames, and a clock for each of them."""

    def __init__(self, command: list[str], scratch: Path, extra_env: dict | None = None,
                 *, now_ms: float | None, size: tuple[int, int] = (ROWS, COLS), proc_samples: bool = True,
                 discovery_home: Path | None = None):
        # now_ms: the display clock the product is pinned to (T3_FIXED_NOW), or None for the real clock.
        # Required, so no product run is pinned to a stale instant by default.
        # proc_samples: read /proc/PID/io and stat once per frame. The during-read evidence needs
        # it; the live comparison does not, so it runs without it and pays nothing.
        # T3CODE_HOME is a new EMPTY folder of this trial's scratch, always: the product reads the
        # synthetic file named by --db (see product_command) and has nothing to discover. Only the
        # Storage-only scenario passes `discovery_home`, a copy of a fixture, for automatic discovery.
        self.proc_samples = proc_samples
        self.handle_ns: list[int] = []  # per frame: arrival to stored, the harness's own cost
        self.scratch = fresh_dir(scratch)
        self.home = fresh_dir(self.scratch / "home")
        fresh_dir(self.scratch / "tmp")
        fresh_dir(self.scratch / "sqlite-tmp")
        fresh_dir(self.scratch / "run")
        self.discovery = discovery_home is not None
        if self.discovery:
            t3 = check_scratch(discovery_home)
        else:
            t3 = fresh_dir(self.scratch / "empty-t3home")
            if any(t3.iterdir()):
                raise Fail(f"T3CODE_HOME stand-in is not empty: {t3}")
        self.t3_home = t3
        self.argv = list(command)
        if not self.discovery and not ("--demo" in command or any(a == "--db" or a.startswith("--db=") for a in command)):
            raise Fail("a product run needs --demo or --db (see product_command); refusing to let it discover a database")
        self.env = {
            "HOME": str(self.home), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
            "COLORTERM": "truecolor", "LANG": "C.UTF-8", "TZ": "UTC",
            "T3CODE_HOME": str(t3),
            "TMPDIR": str(self.scratch / "tmp"), "SQLITE_TMPDIR": str(self.scratch / "sqlite-tmp"),
            "XDG_CONFIG_HOME": str(self.home / ".config"), "XDG_DATA_HOME": str(self.home / ".local/share"),
            "XDG_CACHE_HOME": str(self.home / ".cache"), "XDG_STATE_HOME": str(self.home / ".local/state"),
            "XDG_RUNTIME_DIR": str(self.scratch / "run"),
            "T3_AGENTS_NO_CACHE": "1",  # the persistent report cache is opt-in; measure without it
        }
        if now_ms is not None:
            self.env["T3_FIXED_NOW"] = str(int(now_ms))
        if extra_env:
            self.env.update(extra_env)
        self.rows, self.cols = size
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
        os.set_blocking(self.master, False)
        self.screen = Screen(self.cols, self.rows)
        self.pending = b""
        self.query_pending = b""
        self.frames: list[dict] = []
        self.chunks = 0  # read(2) calls that returned data; frames sharing one cannot be told apart in time
        self.raw = bytearray()
        self.replies = 0
        self.start_ns = time.perf_counter_ns()

        def controlling() -> None:
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.proc = subprocess.Popen(command, stdin=self.slave, stdout=self.slave, stderr=self.slave,
                                     env=self.env, cwd=self.scratch, preexec_fn=controlling, close_fds=True)
        self.pid = self.proc.pid
        os.close(self.slave)

    # -- input
    def write(self, data: bytes, delay_ms: float = 0.0) -> int:
        """Write to the product. The clock starts before the optional injected delay, which
        exists only for the harness's own negative control."""
        started = time.perf_counter_ns()
        if delay_ms:
            time.sleep(delay_ms / 1000)
        view = memoryview(data)
        while view:
            try:
                view = view[os.write(self.master, view):]
            except BlockingIOError:
                select.select([], [self.master], [], 1)
            except InterruptedError:
                continue
        return started

    def answer_queries(self, chunk: bytes) -> None:
        self.query_pending = (self.query_pending + chunk)[-8192:]
        answers: list[bytes] = []
        for pattern, answer in QUERY_ANSWERS:
            count = len(re.findall(pattern, self.query_pending))
            if count:
                answers.extend([answer] * count)
                self.replies += count
                self.query_pending = re.sub(pattern, b"", self.query_pending)
        for match in re.finditer(rb"\x1b\]4;(\d+);\?\x07", self.query_pending):
            answers.append(f"\x1b]4;{int(match.group(1))};rgb:1a1a/1b1b/2626\x07".encode())
            self.replies += 1
        self.query_pending = re.sub(rb"\x1b\]4;\d+;\?\x07", b"", self.query_pending)
        for answer in answers:
            self.write(answer)

    # -- output
    def pump(self, timeout: float = 0.0) -> bool:
        try:
            readable, _, _ = select.select([self.master], [], [], timeout)
        except InterruptedError:
            return False
        if not readable:
            return False
        try:
            chunk = os.read(self.master, 65536)
        except BlockingIOError:
            return False
        except OSError as error:
            if error.errno == errno.EIO:
                return False
            raise
        if not chunk:
            return False
        arrived = time.perf_counter_ns()
        self.chunks += 1
        self.raw.extend(chunk)
        self.answer_queries(chunk)
        self.pending += chunk
        changed = False
        while True:
            at = self.pending.find(SYNC_END)
            if at < 0:
                break
            end = at + len(SYNC_END)
            frame = self.pending[:end]
            self.pending = self.pending[end:]
            self.screen.apply(frame)
            # `arrived` is the read(2) that completed the frame, not the later parse.
            # Bytes before the frame's own ESC[?2026h were not drawn inside a synchronized frame: the
            # first frame carries start-up output, but a later one with a prefix means something was
            # drawn outside the frames this harness accepts. startswith keeps this O(1) per frame.
            outside = 0 if frame.startswith(SYNC_BEGIN) else (frame.find(SYNC_BEGIN) if SYNC_BEGIN in frame else len(frame))
            self.frames.append({"at_ns": arrived, "chunk": self.chunks, "outside_sync_bytes": outside,
                                "raw": frame, "text": self.screen.text(),
                                "rchar": proc_io_rchar(self.pid) if self.proc_samples else None,
                                "cpu": proc_cpu_ticks(self.pid) if self.proc_samples else None,
                                "index": len(self.frames) + 1})
            self.handle_ns.append(time.perf_counter_ns() - arrived)
            changed = True
        return changed

    def drain(self) -> None:
        while self.pump():
            pass

    def wait(self, predicate, timeout: float = 10.0, start: int = 0, what: str = "frame") -> dict:
        deadline = time.monotonic() + timeout
        index = start
        while time.monotonic() < deadline:
            while index < len(self.frames):
                frame = self.frames[index]
                index += 1
                if predicate(frame):
                    return frame
            if self.proc.poll() is not None:
                raise Fail(f"product exited {self.proc.returncode} while waiting for {what}; screen:\n{self.screen.text()}")
            self.pump(min(0.02, max(0.0, deadline - time.monotonic())))
        raise Fail(f"timed out after {timeout}s waiting for {what}; screen:\n{self.screen.text()}")

    def settle(self, quiet: float = 0.3, limit: float = 5.0) -> None:
        end = time.monotonic() + limit
        last = time.monotonic()
        while time.monotonic() < end and time.monotonic() - last < quiet:
            if self.pump(0.02):
                last = time.monotonic()

    def press(self, name: str, delay_ms: float = 0.0) -> tuple[int, int]:
        """Drain what is already queued, so an older frame cannot answer this key, then write.
        Returns the clock reading and the number of frames seen before it."""
        self.drain()
        before = len(self.frames)
        started = self.write(key_bytes(name), delay_ms)
        return started, before

    def cpu_seconds(self) -> float | None:
        ticks = proc_cpu_ticks(self.pid)
        return None if ticks is None else ticks / os.sysconf("SC_CLK_TCK")

    def quit(self, key: str = "q", strict: bool | None = None) -> None:
        """Leave by the product's own quit key and check it exited 0. Called from a finally
        block while an error is in flight, it only stops the child (our own PID) and says
        nothing, so the first error is the one reported."""
        if strict is None:
            strict = sys.exc_info()[0] is None
        if self.proc.poll() is None and strict:
            self.write(key_bytes(key))
            deadline = time.monotonic() + 10
            while self.proc.poll() is None and time.monotonic() < deadline:
                self.pump(0.05)
        if self.proc.poll() is None:
            self.proc.terminate()  # our own child, by PID
            try:
                self.proc.wait(2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(2)
        while self.pump():
            pass
        os.close(self.master)
        if strict and self.proc.returncode != 0:
            raise Fail(f"product {self.pid} exit {self.proc.returncode}")


def harness_cost(p: "Pty") -> dict:
    """What the harness itself spent per frame (parse, virtual screen, /proc samples) between
    the read that completed a frame and having it stored. Latency stops at the read, so this
    time is not inside any figure, but it delays the next read: a latency can be inflated by
    at most the previous frame's handling time when frames arrive back to back. Also what the
    frames themselves show about the evidence: frames that shared a read(2), and bytes drawn
    outside a synchronized frame after the first one."""
    us = sorted(x / 1000 for x in p.handle_ns)
    if not us:
        return {"frames": 0}
    frames = p.frames
    return {"frames": len(us), "handle_us_median": statistics.median(us), "handle_us_p95": nearest_rank(us, 0.95),
            "handle_us_max": us[-1], "proc_samples": p.proc_samples,
            "frames_sharing_a_read": len(frames) - len({f["chunk"] for f in frames}),
            "bytes_outside_sync_frames_after_first": sum(f["outside_sync_bytes"] for f in frames[1:])}


def product_command(binary: Path, args: list[str], *, db: Path | None = None, demo: bool = False,
                    discovery: bool = False) -> list[str]:
    """argv for the product, built here and nowhere else. A run is either `--db FILE` (a
    synthetic file in this trial's scratch) or `--demo`, never both and never neither, with
    T3CODE_HOME a new empty folder (Pty). Extra arguments pass check_product_args, which
    refuses --db, --demo and anything that could name another file. `discovery` is the
    Storage-only scenario's way out: no --db, a fixture copy as T3CODE_HOME, an explicit flag."""
    if discovery:
        if db is not None or demo:
            raise Fail("discovery mode takes neither --db nor --demo")
        extra: list[str] = []
    elif (db is None) == (not demo):
        raise Fail("a product run needs exactly one of a synthetic --db file or --demo")
    else:
        extra = ["--demo"] if demo else ["--db", str(check_scratch(db))]
    return [str(Path(binary).resolve()), *check_product_args(list(args)), *extra]


# ---------------------------------------------------------------------------- shared scoring

def selected_lines(frame: dict, mark: str = "▌") -> list[str]:
    return [line for line in frame["text"].splitlines() if line.startswith(mark)]


class Marker:
    """A marker that is several regular expressions at once: `{"all": [...]}` needs every one
    to match the screen, `{"any": [...]}` at least one. A plain string is one expression. Used to
    tie a ready marker to a report's own body as well as to the footer status, so another
    report's finished read cannot stand in for this one's."""

    def __init__(self, parts: list[re.Pattern], every: bool):
        self.parts, self.every = parts, every

    def search(self, text: str):
        found = (p.search(text) for p in self.parts)
        return all(found) if self.every else any(found)


def compile_marker(spec, what: str):
    if spec is None:
        return None
    try:
        if isinstance(spec, dict):
            if set(spec) not in ({"all"}, {"any"}) or not isinstance(next(iter(spec.values())), list) or not next(iter(spec.values())):
                raise Fail(f"{what}: a marker object is {{\"all\": [..]}} or {{\"any\": [..]}} with a non-empty list")
            (kind, items), = spec.items()
            return Marker([re.compile(x, re.M) for x in items], kind == "all")
        return re.compile(spec, re.M)
    except re.error as error:
        raise Fail(f"{what}: bad regular expression {spec!r}: {error}")


# ---------------------------------------------------------------------------- snap

def command_snap(args) -> None:
    """Run one build, press the keys, print what the virtual screen shows after each. This is
    how to choose markers for a profile from the real frames rather than from a guess."""
    scratch = check_scratch(Path(args.scratch))
    if (args.fixture is None) != bool(args.demo):
        raise Fail("snap needs exactly one of --fixture (a synthetic database this script built) or --demo")
    scratch.mkdir(parents=True, exist_ok=True)
    if args.fixture is not None:
        set_display_clock(fixture_manifest(Path(args.fixture)), args.real_clock, args.max_fixture_age_min)
        db, _ = trial_db(Path(args.fixture), scratch)
        cmd = product_command(Path(args.binary), args.product_arg, db=db)
    else:
        set_display_clock(None, args.real_clock, args.max_fixture_age_min)
        cmd = product_command(Path(args.binary), args.product_arg, demo=True)
    p = Pty(cmd, scratch / "run", now_ms=DisplayClock.fixed_ms)
    try:
        first = p.wait(lambda f: True, args.timeout, what="first frame")
        p.settle(0.5)
        print(f"--- first frame {(first['at_ns'] - p.start_ns) / 1e6:.0f} ms after start; {len(p.frames)} frames")
        print(p.screen.text())
        for name in args.keys:
            if name.startswith("wait:"):
                p.settle(float(name[5:]) or 0.3, 30)
            else:
                started, before = p.press(name)
                p.settle(args.quiet, 30)
                print(f"--- after {name!r}: {len(p.frames) - before} new frames, last at "
                      f"{((p.frames[-1]['at_ns'] - started) / 1e6) if len(p.frames) > before else float('nan'):.1f} ms")
            print(p.screen.text())
    finally:
        p.quit(args.quit_key)



# ---------------------------------------------------------------------------- statistics

def agg(per_trial: list[list[float]]) -> dict:
    """Pooled samples, and the figure CONTRIBUTING asks for: the median and range of the
    trials' own medians."""
    flat = [x for trial in per_trial for x in trial]
    medians = [statistics.median(t) for t in per_trial if t]
    out = {"trials": len(per_trial), "trial_n": [len(t) for t in per_trial], "pooled": summarize(flat)}
    if medians:
        out.update(trial_medians_ms=medians, median_of_trial_medians_ms=statistics.median(medians),
                   range_of_trial_medians_ms=[min(medians), max(medians)])
    return out


def paired(new: list[list[float]], old: list[list[float]]) -> dict:
    """new minus old, round by round, for rounds where both have samples."""
    diffs = [statistics.median(n) - statistics.median(o) for n, o in zip(new, old) if n and o]
    if not diffs:
        return {"n": 0, "error": "no round has samples for both arms"}
    return {"n": len(diffs), "round_diffs_ms": diffs, "median_diff_ms": statistics.median(diffs),
            "range_diff_ms": [min(diffs), max(diffs)]}


def claim_status(rounds: int, loads: list[dict], load_limit: float, hashes_stable: bool,
                 problems: list[str], clock: str, min_rounds: int = 5) -> dict:
    """Whether numbers from this run may be quoted. Not a judgment of the result."""
    reasons = list(problems)
    if rounds < min_rounds:
        reasons.append(f"{rounds} rounds; at least {min_rounds} alternating trials are required")
    worst = max((x["1m"] for x in loads), default=0.0)
    if worst > load_limit:
        reasons.append(f"1-minute load average reached {worst:.2f} (limit {load_limit}); another heavy job may have been running")
    if not hashes_stable:
        reasons.append("a binary changed during the run")
    return {"claimable": not reasons, "reasons": reasons, "load_limit_1m": load_limit, "worst_load_1m": worst, "display_clock": clock}


# ---------------------------------------------------------------------------- profile

_RX: dict[str, object] = {}


def expand_spec(spec):
    """Expand the period tokens in a marker (a string, or an {"all"|"any": [...]} object) at the display clock."""
    if isinstance(spec, dict):
        return {k: [expand_tokens(x) for x in v] for k, v in spec.items()}
    return expand_tokens(spec)


def rx(spec):
    spec = expand_spec(spec)
    key = json.dumps(spec, sort_keys=True)
    if key not in _RX:
        _RX[key] = compile_marker(spec, "marker")
    return _RX[key]


# ---- state-driven steps (profile "format": 2)
#
# A step names the screen it ends on (`to`: a report and a period, and whether the report may be scrolled) and
# whether it reads. The harness builds every marker from that, so the report and the period are pinned by the
# screen's own text (the report's body heading, the period row's dates and the footer status) rather than by a
# marker someone has to keep in step. It also tracks the state it believes the screen is in and checks the screen
# against it before every key, so a step that starts from the wrong report or period fails closed.

FOOTER_LOADING = r"reading since \d\d:\d\d:\d\d$"
FOOTER_READY = r"read at \d\d:\d\d:\d\d$"
SCROLLED = r"▲ \d+ above"


def state_marker(prof: dict, to: dict, phase: str) -> dict:
    """The marker for a screen: `ready` (footer says read at, the period's dates with year and coverage, the report's
    heading), `refreshing` (the read of a report already on screen: its old body and dates stay, the footer says
    reading since) or `loading` (a first read: the period only requested, the body the loading sentence)."""
    report, period = to["report"], to["period"]
    meta = prof["reports"][report]
    ident = f"(?:{meta['heading']}|{SCROLLED})" if to.get("scrolled") else meta["heading"]
    footer = prof["footer"]
    if phase == "ready":
        return {"all": [footer["ready"], f"<<dates:{period}>>", ident]}
    if phase == "refreshing":
        return {"all": [footer["loading"], f"<<dates:{period}>>", ident]}
    if phase == "loading":
        return {"all": [footer["loading"], f"<<short:{period}>>$", f"Reading {meta['name']} for <<word:{period}>> from T3"]}
    raise Fail(f"unknown state phase {phase!r}")


def normalize_states(prof: dict, path: Path) -> None:
    """Fill loading, ready, expect, absent and forbid on every step that has `to`."""
    for need in ("reports", "footer"):
        if need not in prof:
            raise Fail(f"profile {path}: format 2 needs {need}")
    for r, meta in prof["reports"].items():
        if not (meta.get("heading") and meta.get("name")):
            raise Fail(f"profile {path}: reports.{r} needs heading and name")
    for f in ("loading", "ready"):
        if not prof["footer"].get(f):
            raise Fail(f"profile {path}: footer.{f} is required")

    def one(step: dict, where: str) -> None:
        to = step.get("to")
        if to is None:
            return
        if to.get("report") not in prof["reports"] or to.get("period") not in PERIOD_WORDS:
            raise Fail(f"profile {path}: {where}.to needs report in {sorted(prof['reports'])} and period in {sorted(PERIOD_WORDS)}")
        if step.get("reads"):
            step["loading"] = state_marker(prof, to, "refreshing" if step.get("body") == "snapshot" else "loading")
            step["ready"] = state_marker(prof, to, "ready")
        else:
            step["expect"] = state_marker(prof, to, "ready")
            step["forbid"] = prof["footer"]["loading"]
            if step.get("absent_extra"):
                step["absent"] = step["absent_extra"]

    one(prof["open"], "open")
    for group in ("switch", "after_reading"):
        for s in prof.get(group, []):
            one(s, f"{group}[{s.get('name', '?')}]")


def sub_n(spec, i: int):
    """Fill <<i>> (0-based) and <<n>> (1-based) in a marker, in each part of a composite."""
    if isinstance(spec, dict):
        return {k: [sub_n(x, i) for x in v] for k, v in spec.items()}
    return spec.replace("<<i>>", str(i)).replace("<<n>>", str(i + 1))


def load_profile(path: Path, kind: str = "product") -> dict:
    """A profile names the keys and markers of the Insights screen. Missing keys or markers
    are errors, never defaults: a guessed marker would make a result look better than the
    evidence allows. `kind` says which program the profile is written for ("product" or
    "stand-in"); a profile of the other kind is refused, so the harness's own controls keep
    working when the product profile is replaced by calibrated markers, and a stand-in profile
    can never time a product."""
    prof = json.loads(Path(path).read_text())
    if prof.get("kind") != kind:
        raise Fail(f"profile {path} is for {prof.get('kind')!r}, but this run needs a {kind!r} profile")
    if kind == "product" and not isinstance(prof.get("calibrated"), bool):
        raise Fail(f"profile {path}: a product profile must say calibrated: true or false")
    if prof.get("format") == 2:
        normalize_states(prof, path)
    need = [("board", "ready"), ("open", "key"), ("open", "loading"), ("open", "ready"), ("exit", "key"), ("exit", "expect")]
    for a, b in need:
        if not isinstance(prof.get(a), dict) or not prof[a].get(b):
            raise Fail(f"profile {path}: {a}.{b} is required")

    def check(step: dict, where: str, read: bool) -> None:
        for field in ("key",):
            if not step.get(field):
                raise Fail(f"profile {path}: {where}.{field} is required")
            key_bytes(step[field])
        if read:
            if not (step.get("loading") and step.get("ready")):
                raise Fail(f"profile {path}: {where} needs both loading and ready")
        elif not step.get("expect"):
            raise Fail(f"profile {path}: {where}.expect is required")
        for field in ("expect", "absent", "forbid", "loading", "ready"):
            if step.get(field):
                spec = expand_spec(sub_n(step[field], 0))
                if "<<" in json.dumps(spec):
                    raise Fail(f"profile {path}: {where}.{field} has a token this harness does not know: {step[field]!r}")
                compile_marker(spec, f"{where}.{field}")
        for d in step.get("during", []):
            check(d, f"{where}.during[{d.get('name', '?')}]", False)

    check(prof["open"], "open", True)
    for s in prof.get("reading", []):
        check(s, f"reading[{s.get('name', '?')}]", False)
    for group in ("switch", "after_reading"):
        for s in prof.get(group, []):
            check(s, f"{group}[{s.get('name', '?')}]", bool(s.get("loading") or s.get("ready")))
    exit_step = prof["exit"]
    key_bytes(exit_step["key"])
    if exit_step.get("alt_key"):
        key_bytes(exit_step["alt_key"])
    return prof


# ---------------------------------------------------------------------------- key and read scoring

def score_key(p: Pty, key: str, expect: str, *, absent: str | None = None, changed: bool = False,
              forbid: str | None = None, timeout: float = 10.0, delay_ms: float = 0.0, what: str = "key") -> dict:
    """Latency from the write to the first frame that shows `expect` (and not `absent`; and,
    with `changed`, differs from the screen before). No match in `timeout` raises Fail.

    With `forbid`, the key must be answered without any frame that shows that marker on the way: an
    instant step (a key answered from what is already read) whose response came after a frame saying
    the product was reading is not instant, and what it timed is a read. That is the state being
    wrong, not a slow sample, so it fails closed."""
    p.drain()
    before_text = p.screen.text()
    started, before = p.press(key, delay_ms)
    want, unwanted = rx(expect), rx(absent) if absent else None

    def ok(f: dict) -> bool:
        return bool(want.search(f["text"])) and not (unwanted and unwanted.search(f["text"])) \
            and (not changed or f["text"] != before_text)

    if forbid:
        bad = rx(forbid)

        def ok_or_forbidden(f: dict) -> bool:
            return bool(bad.search(f["text"])) or ok(f)

        frame = p.wait(ok_or_forbidden, timeout, before, what=f"{what} {key!r} to show /{expect}/")
        if bad.search(frame["text"]):
            raise Fail(f"state not recognized: {what} ({key!r}) was expected to be answered at once from what is already read, but a frame "
                       f"showing the loading status came first (frame {frame['index']}); the product read, or the profile is out of date; screen:\n{frame['text']}")
    else:
        frame = p.wait(ok, timeout, before, what=f"{what} {key!r} to show /{expect}/")
    return {"latency_ms": (frame["at_ns"] - started) / 1e6, "frame": frame["index"], "frames_after_write": frame["index"] - before}


def assert_state(p: Pty, spec, what: str) -> None:
    """The screen, once everything queued has been applied, must show this state before a key is written."""
    p.drain()
    text = p.screen.text()
    if not rx(spec).search(text):
        raise Fail(f"state not recognized before {what}: the screen does not show {json.dumps(expand_spec(spec), ensure_ascii=False)}; screen:\n{text}")


def loading_only(f: dict, load_rx: re.Pattern, ready_rx: re.Pattern) -> bool:
    return bool(load_rx.search(f["text"]))


def ready_only(f: dict, load_rx: re.Pattern, ready_rx: re.Pattern) -> bool:
    """A frame completes a read only if it shows the ready marker and no loading marker."""
    return bool(ready_rx.search(f["text"])) and not load_rx.search(f["text"])


def reject(**detail) -> dict:
    return {"accepted": False, **detail}


def read_step(p: Pty, step: dict, during: list[dict], *, min_bytes: int, timeout: float, delay_ms: float = 0.0) -> dict:
    """One key that starts a read, scored three ways, each of which fails closed:

    1. A frame showing the loading marker arrives before any frame showing the ready marker.
       A ready frame with no loading frame before it rejects the sample.
    2. The product kept reading after that frame: its read(2) byte count rose by at least
       `min_bytes` between the loading frame and the ready frame. Unreadable counters reject.
    3. Each `during` key is written after the loading frame and before any ready frame; its
       response shows its marker (and, unless keeps_loading is false, the loading marker),
       no ready frame comes first, and the product's read count rose by at least
       `min_bytes` between the key's write and the ready frame, so the key really occurred
       during a read and not after its end.
    """
    load_rx, ready_rx = rx(step["loading"]), rx(step["ready"])
    name = step["name"] if "name" in step else step["key"]
    p.drain()
    started, before = p.press(step["key"], delay_ms)
    first = p.wait(lambda f: loading_only(f, load_rx, ready_rx) or ready_only(f, load_rx, ready_rx), timeout, before,
                   what=f"{name}: a loading or ready frame after {step['key']!r}")
    out: dict = {"name": name, "key": step["key"], "during": []}
    if not loading_only(first, load_rx, ready_rx):
        out.update(reject(reason="ready-frame-with-no-loading-frame-before-it",
                          ready_ms=(first["at_ns"] - started) / 1e6))
        return out
    out["loading_ms"] = (first["at_ns"] - started) / 1e6
    pending: list[tuple[dict, int | None, dict]] = []
    final_ready_rx = ready_rx
    for d in during:
        record: dict = {"name": d.get("name", d["key"]), "key": d["key"]}
        out["during"].append(record)
        p.drain()
        already = next((f for f in p.frames[first["index"]:] if ready_only(f, load_rx, final_ready_rx)), None)
        if already is not None:
            record.update(reject(reason="read-finished-before-key-was-written", ready_frame=already["index"]))
            continue
        io0 = proc_io_rchar(p.pid)
        d_started, d_before = p.press(d["key"], delay_ms)
        want = rx(d["expect"])
        absent = rx(d["absent"]) if d.get("absent") else None
        resp = p.wait(lambda f: bool(want.search(f["text"])) or ready_only(f, load_rx, final_ready_rx), timeout, d_before,
                      what=f"{record['name']}: response to {d['key']!r} during the read")
        if ready_only(resp, load_rx, final_ready_rx) and not want.search(resp["text"]):
            record.update(reject(reason="read-finished-before-response", response_frame=resp["index"]))
            continue
        if ready_only(resp, load_rx, final_ready_rx):
            record.update(reject(reason="response-frame-already-shows-the-finished-read", response_frame=resp["index"]))
            continue
        if absent and absent.search(resp["text"]):
            # e.g. an overlay that still shows the finished read's status: the key did not occur during the read
            record.update(reject(reason="response-frame-shows-the-absent-marker", response_frame=resp["index"]))
            continue
        # An overlay replaces the body, so a key that opens one may name a looser loading marker (the footer's status alone).
        if d.get("keeps_loading", True) and not (rx(d["loading"]) if d.get("loading") else load_rx).search(resp["text"]):
            record.update(reject(reason="response-frame-lost-the-loading-marker", response_frame=resp["index"]))
            continue
        record["latency_ms"] = (resp["at_ns"] - d_started) / 1e6
        record["response_frame"] = resp["index"]
        pending.append((record, io0, resp))
        if d.get("ready"):
            final_ready_rx = rx(d["ready"])
    ready = p.wait(lambda f: ready_only(f, load_rx, final_ready_rx), timeout, first["index"],
                   what=f"{name}: the ready frame")
    out["ready_ms"] = (ready["at_ns"] - started) / 1e6
    out["read_window_ms"] = (ready["at_ns"] - first["at_ns"]) / 1e6
    out["frames_between_loading_and_ready"] = ready["index"] - first["index"] - 1
    if first["chunk"] == ready["chunk"]:
        out.update(reject(reason="loading-and-ready-frames-in-one-read"))  # one read(2): their times cannot be told apart
    elif first["rchar"] is None or ready["rchar"] is None:
        out.update(reject(reason="read-counter-unreadable"))
    else:
        out["bytes_read_after_loading_frame"] = ready["rchar"] - first["rchar"]
        out["accepted"] = out["bytes_read_after_loading_frame"] >= min_bytes
        if not out["accepted"]:
            out["reason"] = "no-read-activity-between-loading-and-ready-frames"
    for record, io0, resp in pending:
        if io0 is None or ready["rchar"] is None:
            record.update(reject(reason="read-counter-unreadable"))
        elif resp["index"] >= ready["index"]:
            record.update(reject(reason="response-did-not-precede-the-ready-frame"))
        elif resp["chunk"] == ready["chunk"]:
            record.update(reject(reason="response-and-ready-frames-in-one-read"))
        else:
            record["bytes_read_after_key"] = ready["rchar"] - io0
            record["accepted"] = record["bytes_read_after_key"] >= min_bytes
            if not record["accepted"]:
                record["reason"] = "no-read-activity-after-the-key"
    out["ready_frame"] = ready["index"]
    return out


def idle(p: Pty, seconds: float) -> dict:
    p.drain()
    before = (len(p.raw), len(p.frames), proc_cpu_ticks(p.pid), p.screen.text())
    started = time.perf_counter_ns()
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        p.pump(min(0.1, max(0.0, end - time.monotonic())))
    elapsed = (time.perf_counter_ns() - started) / 1e9
    ticks = os.sysconf("SC_CLK_TCK")
    after = proc_cpu_ticks(p.pid)
    if before[2] is None or after is None:
        raise Fail("process CPU counter unreadable")
    return {"seconds": elapsed, "cpu_ms": (after - before[2]) * 1000 / ticks,
            "cpu_percent": (after - before[2]) / ticks * 100 / elapsed,
            "output_bytes": len(p.raw) - before[0], "frames": len(p.frames) - before[1],
            "screen_unchanged": before[3] == p.screen.text()}


def esc_isolation(esc: list[float], alt: list[float], esc_wait_ms: float) -> dict:
    """Separate the product's work from the parser's wait for a lone Escape.

    The parser holds a lone Escape for ESC_WAIT before reporting it, so a close by Escape
    includes that wait and a close by another key does not. Measured two ways: the paired
    difference (Escape minus the other key, same screen, alternating order), and the
    Escape figure less the source constant. The other-key figure is the program time.
    The constant is an approximation: the wait runs from the read that saw the byte, and
    the parser's poll adds timer slack, so the paired difference is the better number.
    """
    out: dict = {"esc": summarize(esc), "esc_wait_constant_ms": esc_wait_ms}
    if esc:
        out["esc_minus_constant_median_ms"] = statistics.median(esc) - esc_wait_ms
    if alt:
        out["alt_key"] = summarize(alt)
        out["esc_minus_alt_key_median_ms"] = statistics.median(esc) - statistics.median(alt) if esc else None
        out["program_ms_median"] = statistics.median(alt)
    else:
        out["alt_key"] = {"n": 0, "error": "no alternative key given; only the constant can be subtracted"}
    return out


def esc_control(p: Pty, cfg: dict, pairs: int, delay_ms: float) -> dict:
    """The parser's Escape wait on a screen whose other close key costs nothing: open a
    screen, close it by Escape and by another key, alternating which goes first."""
    esc, alt = [], []
    for k in range(pairs):
        for which in (("esc", "alt") if k % 2 == 0 else ("alt", "esc")):
            score_key(p, cfg["open_key"], cfg["open_expect"], what="esc-control open")
            p.settle(0.15)
            r = score_key(p, "escape" if which == "esc" else cfg["alt_key"], cfg["close_expect"],
                          absent=cfg["open_expect"], delay_ms=delay_ms, what=f"esc-control close by {which}")
            (esc if which == "esc" else alt).append(r["latency_ms"])
            p.settle(0.15)
    return {"esc_ms": esc, "alt_ms": alt}


ESC_CONTROL_BOARD = {"open_key": "?", "open_expect": r"T3 Agents · help", "alt_key": "?",
                     "close_expect": r"AGENTS ─{60}"}


def board_keys(p: Pty, count: int, delay_ms: float = 0.0, pause: float = 0.12) -> list[float]:
    """j/k on the Live board: the selected row must move to the expected thread."""
    out, current = [], 0
    for _ in range(count):
        key, target = ("j", 1) if current == 0 else ("k", 0)
        r = score_key(p, key, rf"^▌.*Bench task {target:03d}", delay_ms=delay_ms, what="board selection")
        out.append(r["latency_ms"])
        current = target
        p.settle(pause, 2)
    return out


# ---------------------------------------------------------------------------- insights (new build alone)

def leave(p: Pty, exit_step: dict, which: str, delay_ms: float) -> float:
    key = exit_step["key"] if which == "esc" else exit_step["alt_key"]
    return score_key(p, key, exit_step["expect"], absent=exit_step.get("absent"), delay_ms=delay_ms,
                     what=f"exit by {which}")["latency_ms"]


def insights_trial(binary: Path, args, profile: dict, fixture: Path, scratch: Path, round_index: int) -> dict:
    db, _ = trial_db(fixture, scratch)
    p = Pty(product_command(binary, args.product_arg, db=db), scratch / "run", now_ms=DisplayClock.fixed_ms)
    delay = args.inject_delay_ms
    po = {**profile["open"], "name": "open"}
    ex = profile["exit"]
    has_alt = bool(ex.get("alt_key"))
    out: dict = {"round": round_index, "load_start": load(), "binary_sha256": sha(binary)}
    board_ready = rx(profile["board"]["ready"])
    keys = args.keys + args.keys % 2
    try:
        first = p.wait(lambda f: bool(board_ready.search(f["text"])), args.timeout, what="the Live board")
        out["board_ready_ms"] = (first["at_ns"] - p.start_ns) / 1e6
        p.settle(0.5)
        out["board_keys_before_ms"] = board_keys(p, keys, delay)           # Live
        out["idle_live"] = idle(p, args.idle_seconds)
        ec = esc_control(p, {**ESC_CONTROL_BOARD, **profile.get("esc_control", {})}, args.esc_pairs, delay)
        out["board_help_close"] = {"esc_ms": ec["esc_ms"], "alt_ms": ec["alt_ms"]}
        opens = [read_step(p, po, [], min_bytes=args.min_read_bytes, timeout=args.read_timeout, delay_ms=delay)]
        out["open_cold"] = opens[0]
        p.settle(0.3)
        out["switch"] = {}

        cur = dict(po["to"]) if po.get("to") else None   # the state the harness believes the screen is in (format 2 profiles)

        def run_steps(group: str) -> None:                                  # report switches, refresh, period changes
            nonlocal cur
            for s in profile.get(group, []):
                records = out["switch"].setdefault(s["name"], [])
                for i in range(s.get("repeat", 1)):
                    if cur is not None and s.get("to"):
                        assert_state(p, state_marker(profile, cur, "ready"), f"{s['name']} ({s['key']!r})")
                    if s.get("loading"):
                        rec = read_step(p, s, s.get("during", []), min_bytes=args.min_read_bytes,
                                        timeout=args.read_timeout, delay_ms=delay)
                        if s.get("to") and rec.get("reason") == "ready-frame-with-no-loading-frame-before-it":
                            raise Fail(f"state not recognized: {s['name']} ({s['key']!r}) is declared as a read but the product answered without a "
                                       f"loading frame (a snapshot, or the profile is out of date); screen:\n{p.screen.text()}")
                        records.append(rec)
                    else:
                        records.append(score_key(p, s["key"], sub_n(s["expect"], i),
                                                 absent=sub_n(s["absent"], i) if s.get("absent") else None,
                                                 forbid=s.get("forbid"), changed=s.get("changed", False), delay_ms=delay, what=s["name"]))
                    if s.get("to"):
                        cur = dict(s["to"])
                    p.settle(0.15, 3)

        run_steps("switch")
        out["reading_ms"] = {}
        if cur is not None and profile.get("reading"):
            assert_state(p, state_marker(profile, cur, "ready"), "the scroll steps")
        for s in profile.get("reading", []):                                # scrolling the report now open
            samples = out["reading_ms"].setdefault(s["name"], [])
            for i in range(s.get("repeat", 1)):
                samples.append(score_key(p, s["key"], sub_n(s["expect"], i),
                                         absent=sub_n(s["absent"], i) if s.get("absent") else None,
                                         changed=s.get("changed", False), delay_ms=delay, what=s["name"])["latency_ms"])
                p.settle(0.12, 2)
        if cur is not None and profile.get("reading"):
            cur = {**cur, "scrolled": True}                                 # the heading is above the window now
        out["idle_report"] = idle(p, args.idle_seconds)
        run_steps("after_reading")
        leave(p, ex, "alt" if has_alt else "esc", delay)
        p.settle(0.2)
        # Keys during an active read: reopen until enough are accepted, or the attempts run out.
        out["during_attempts"] = []
        need = {d.get("name", d["key"]): args.during_keys for d in po.get("during", [])}
        have = {n: 0 for n in need}
        for _ in range(args.during_attempts if need else 0):
            r = read_step(p, po, po["during"], min_bytes=args.min_read_bytes, timeout=args.read_timeout, delay_ms=delay)
            out["during_attempts"].append(r)
            for d in r["during"]:
                have[d["name"]] += 1 if d.get("accepted") else 0
            p.settle(0.3)
            if po.get("to"):
                # a key that was answered late, or twice, leaves the screen somewhere else (help still open, a
                # report switched): the state is not what the next step assumes, so stop rather than time a wrong screen
                assert_state(p, state_marker(profile, po["to"], "ready"), "leaving Insights after the during-read keys (a deferred key may have changed the screen)")
            leave(p, ex, "alt" if has_alt else "esc", delay)
            p.settle(0.2)
            if all(have[n] >= need[n] for n in need):
                break
        # Exit, with the parser's Escape wait separated out: Escape and another key, alternating.
        out["exit_ms"] = {"esc": [], "alt": []}
        for k in range(args.exit_pairs):
            for which in (("esc", "alt") if (k + round_index) % 2 == 0 else ("alt", "esc")):
                if which == "alt" and not has_alt:
                    continue
                r = read_step(p, po, [], min_bytes=args.min_read_bytes, timeout=args.read_timeout, delay_ms=delay)
                opens.append(r)
                p.settle(0.2)
                out["exit_ms"][which].append(leave(p, ex, which, delay))
                p.settle(0.2)
        out["opens_warm"] = opens[1:]
        out["returned_to_board"] = bool(rx(profile["board"].get("after_return", profile["board"]["ready"])).search(p.screen.text()))
        out["board_keys_after_ms"] = board_keys(p, keys, delay)
        out["cpu_seconds"] = p.cpu_seconds()
        out["harness"] = harness_cost(p)
        out["frames"], out["output_bytes"] = len(p.frames), len(p.raw)
        out["isolation"] = isolation_record(p, db)
        check_clock_unchanged(fixture_manifest(fixture), args.real_clock)
        out["load_end"] = load()
        return out
    finally:
        p.quit()


def isolation_record(p: Pty, db: Path) -> dict:
    """What the product was given, recorded so a result shows its own isolation: argv, the
    T3CODE_HOME it saw and what was in it when the run ended (must still be nothing)."""
    left = sorted(x.name for x in p.t3_home.iterdir())
    at = p.argv.index("--db") if "--db" in p.argv else -1
    return {"argv_db": p.argv[at + 1] if at >= 0 else None,
            "db_is_trial_copy": at >= 0 and p.argv[at + 1] == str(check_scratch(db)), "demo": "--demo" in p.argv,
            "t3code_home": str(p.t3_home), "t3code_home_discovery": p.discovery, "t3code_home_entries_at_end": left}


def insights_evidence(trials: list[dict], args, profile: dict) -> dict:
    """Count what was accepted and why the rest was not; decide validity. Absent evidence
    makes the run invalid; it is never reported as a faster number."""
    reasons: dict[str, int] = {}
    attempted = accepted = d_attempted = d_accepted = 0
    problems: list[str] = []
    for t in trials:
        for r in [t["open_cold"], *t["opens_warm"], *t["during_attempts"],
                  *[x for rs in t["switch"].values() for x in rs if "during" in x]]:  # read_step records
            attempted += 1
            if r.get("accepted"):
                accepted += 1
            else:
                reasons[r.get("reason", "?")] = reasons.get(r.get("reason", "?"), 0) + 1
            for d in r.get("during", []):
                d_attempted += 1
                if d.get("accepted"):
                    d_accepted += 1
                else:
                    reasons[f"during:{d.get('reason', '?')}"] = reasons.get(f"during:{d.get('reason', '?')}", 0) + 1
        if not t["returned_to_board"]:
            problems.append(f"round {t['round']}: did not return to the Live board")
        iso_ = t["isolation"]
        if iso_["t3code_home_entries_at_end"] or iso_["t3code_home_discovery"] or iso_["demo"] or not iso_["db_is_trial_copy"]:
            problems.append(f"round {t['round']}: isolation broken: {iso_}")
    # Per profile step: how many reads (or instant keys) were attempted and accepted. A step that is declared
    # as a read and never produced one accepted sample, or a during-read key that never landed, leaves that
    # part of the workload unmeasured, which is an invalid run and not a smaller number.
    steps: dict[str, dict] = {}
    remaining: list[float] = []
    for t in trials:
        for name, recs in t["switch"].items():
            e = steps.setdefault(name, {"reads_attempted": 0, "reads_accepted": 0, "instant_keys": 0, "during": {}})
            for r in recs:
                if "during" not in r:
                    e["instant_keys"] += 1
                    continue
                e["reads_attempted"] += 1
                e["reads_accepted"] += 1 if r.get("accepted") else 0
                for d in r["during"]:
                    row = e["during"].setdefault(d["name"], {"attempted": 0, "accepted": 0})
                    row["attempted"] += 1
                    row["accepted"] += 1 if d.get("accepted") else 0
    for t in trials:
        for r in [*t["during_attempts"], *[x for rs in t["switch"].values() for x in rs if "during" in x]]:
            for d in r.get("during", []):
                if d.get("accepted") and r.get("bytes_read_after_loading_frame"):
                    remaining.append(d["bytes_read_after_key"] / r["bytes_read_after_loading_frame"])
    for name, e in steps.items():
        if e["reads_attempted"] and not e["reads_accepted"]:
            problems.append(f"step {name!r} is a read and none of its {e['reads_attempted']} reads was accepted")
        for dn, row in e["during"].items():
            if row["attempted"] and not row["accepted"]:
                problems.append(f"step {name!r}: the during-read key {dn!r} never landed during a read ({row['attempted']} attempts)")
    during_needed = bool(profile["open"].get("during"))
    if accepted == 0:
        problems.append("no read was accepted: no loading frame demonstrably preceded the ready frame")
    if any(not t["open_cold"].get("accepted") for t in trials):
        problems.append("a cold open was not accepted: " + ", ".join(sorted({t["open_cold"].get("reason", "?") for t in trials if not t["open_cold"].get("accepted")})))
    if during_needed:
        per_trial = [sum(1 for a in t["during_attempts"] for d in a["during"] if d.get("accepted")) for t in trials]
        if any(n < args.during_keys for n in per_trial):
            problems.append(f"accepted during-read keys per round {per_trial}, need at least {args.during_keys} each")
    else:
        problems.append("profile has no open.during keys: nothing was scored during a read; no claim about keys during a read")
    if not profile["exit"].get("alt_key"):
        problems.append("profile exit has no alt_key: program time is only the Escape figure less the constant")
    outside = sum(t["harness"]["bytes_outside_sync_frames_after_first"] for t in trials)
    if outside:
        problems.append(f"claim: {outside} bytes were drawn outside a synchronized-output frame after the first frame; "
                        "the harness sees only frames, so progress drawn that way was not observed")
    if profile.get("kind") != "product":
        problems.append("claim: stand-in profile; nothing here is a product result")
    elif not profile.get("calibrated"):
        problems.append("claim: the product profile is not calibrated against a built screen; its markers come from design frames")
    return {"reads_attempted": attempted, "reads_accepted": accepted, "during_keys_attempted": d_attempted,
            "during_keys_accepted": d_accepted, "rejected_reasons": reasons, "steps": steps,
            "during_key_share_of_read_still_to_come": ({"median": statistics.median(remaining), "min": min(remaining), "n": len(remaining)} if remaining else None),
            "problems": problems,
            "valid": not [x for x in problems if not x.startswith(("profile has no", "profile exit has no", "claim:"))],
            "frames_sharing_a_read": sum(t["harness"]["frames_sharing_a_read"] for t in trials)}


# The three figures, named for what they are. All run from the key's write.
FIGURES = {"loading_ms": "first_visible_response_ms",    # a frame showing the loading marker
           "ready_ms": "report_ready_ms",                # the frame that completes the read
           "read_window_ms": "read_in_progress_ms"}      # between those two frames


def insights_summary(trials: list[dict], args, profile: dict) -> dict:
    esc_wait = source_esc_wait(Path(args.source_root) if args.source_root else None)
    ok = lambda r: r.get("accepted")
    s: dict = {
        "live_board_key_before_open": agg([t["board_keys_before_ms"] for t in trials]),
        "live_board_key_after_return": agg([t["board_keys_after_ms"] for t in trials]),
        "board_ready_ms": agg([[t["board_ready_ms"]] for t in trials]),
        "idle_cpu_percent": {"live": agg([[t["idle_live"]["cpu_percent"]] for t in trials]),
                             "report_open": agg([[t["idle_report"]["cpu_percent"]] for t in trials])},
        "idle_frames": {"live": [t["idle_live"]["frames"] for t in trials], "report_open": [t["idle_report"]["frames"] for t in trials]},
        "open_cold": {FIGURES[m]: agg([[t["open_cold"][m]] for t in trials if ok(t["open_cold"]) and m in t["open_cold"]])
                      for m in FIGURES},
        "open_warm": {FIGURES[m]: agg([[r[m] for r in t["opens_warm"] if ok(r) and m in r] for t in trials])
                      for m in FIGURES},
        "reading": {n: agg([t["reading_ms"][n] for t in trials]) for n in sorted({n for t in trials for n in t["reading_ms"]})},
    }
    during_names = sorted({d["name"] for t in trials for a in t["during_attempts"] for d in a["during"]})
    s["input_latency_during_an_active_read"] = {
        n: agg([[d["latency_ms"] for a in t["during_attempts"] for d in a["during"] if d["name"] == n and d.get("accepted")] for t in trials])
        for n in during_names}
    sw = {}
    for name in sorted({n for t in trials for n in t["switch"]}):
        recs = [t["switch"][name] for t in trials if name in t["switch"]]
        if any("during" in r for rs in recs for r in rs):
            sw[name] = {FIGURES[m]: agg([[r[m] for r in rs if ok(r) and m in r] for rs in recs]) for m in FIGURES}
            for n in sorted({d["name"] for rs in recs for r in rs for d in r.get("during", [])}):
                sw[name][f"during:{n}"] = agg([[d["latency_ms"] for r in rs for d in r.get("during", []) if d["name"] == n and d.get("accepted")] for rs in recs])
        else:
            sw[name] = agg([[r["latency_ms"] for r in rs] for rs in recs])
    s["switch"] = sw
    esc = [x for t in trials for x in t["exit_ms"]["esc"]]
    alt = [x for t in trials for x in t["exit_ms"]["alt"]]
    s["exit"] = esc_isolation(esc, alt, esc_wait)
    s["exit"]["per_round_esc_minus_alt_ms"] = [statistics.median(t["exit_ms"]["esc"]) - statistics.median(t["exit_ms"]["alt"])
                                               for t in trials if t["exit_ms"]["esc"] and t["exit_ms"]["alt"]]
    hc_esc = [x for t in trials for x in t["board_help_close"]["esc_ms"]]
    hc_alt = [x for t in trials for x in t["board_help_close"]["alt_ms"]]
    s["parser_escape_wait_on_live_board"] = esc_isolation(hc_esc, hc_alt, esc_wait)
    s["cpu_seconds_per_trial"] = [t["cpu_seconds"] for t in trials]
    s["harness_overhead"] = {"per_frame_handle_us_median": statistics.median([t["harness"]["handle_us_median"] for t in trials]),
                             "per_frame_handle_us_worst_p95": max(t["harness"]["handle_us_p95"] for t in trials),
                             "per_frame_handle_us_max": max(t["harness"]["handle_us_max"] for t in trials),
                             "proc_samples_per_frame": True,
                             "note": "see 'floor' for the measuring path's own latency; handling time is outside every figure but can delay the next read"}
    return s


def command_insights(args) -> int:
    fixture = check_scratch(Path(args.fixture))
    manifest = fixture_manifest(fixture)
    set_display_clock(manifest, args.real_clock, args.max_fixture_age_min)
    scratch = check_scratch(Path(args.scratch))
    profile = load_profile(Path(args.profile), args.profile_kind)
    scratch.mkdir(parents=True, exist_ok=True)
    if (scratch / "results.json").exists():
        raise ValueError(f"results already exist: {scratch}/results.json")
    binary = Path(args.new).resolve()
    sha0 = sha(binary)
    script_binary = binary_is_script(binary)
    trials = []
    for i in range(args.rounds):
        trial = insights_trial(binary, args, profile, fixture, scratch / f"round-{i:02d}", i)
        trials.append(trial)
        (scratch / f"round-{i:02d}.json").write_text(json.dumps(trial, indent=2) + "\n")
        print(f"round {i + 1}/{args.rounds}: cold open "
              f"{'ok' if trial['open_cold'].get('accepted') else 'REJECTED ' + trial['open_cold'].get('reason', '?')}, "
              f"read window {trial['open_cold'].get('read_window_ms', float('nan')):.0f} ms", flush=True)
    evidence = insights_evidence(trials, args, profile)
    if script_binary:
        evidence["problems"].append("claim: the binary is a script (a stand-in), not the product; nothing here is a product result")
    loads = [x for t in trials for x in (t["load_start"], t["load_end"])]
    report = {"format": 2, "kind": "insights-terminal-ux", "started_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
              "fixture": manifest, "profile": {"path": str(Path(args.profile).resolve()), "name": profile.get("name"), "kind": profile.get("kind"),
                                              "calibrated": profile.get("calibrated"),
                                              "status": profile.get("status"), "pending": profile.get("pending", [])},
              "binary": {"path": str(binary), "sha256": sha0, "commit": args.commit, "is_script": script_binary},
              "clock": clock_record(manifest, args.real_clock), "product_args": args.product_arg,
              "rounds": args.rounds, "evidence": evidence, "summary": insights_summary(trials, args, profile),
              "claim": claim_status(args.rounds, loads, args.load_limit, sha(binary) == sha0, evidence["problems"],
                                    "real" if args.real_clock else "fixed (periodic headroom refresh suppressed)"),
              "method": METHOD, "control": {"inject_delay_ms": args.inject_delay_ms}}
    (scratch / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"evidence": evidence, "claim": report["claim"], "exit": report["summary"]["exit"]}, indent=2))
    return 0 if evidence["valid"] else 2


METHOD = {
    "pty": "private openpty with a controlling terminal; no tmux, emulator or desktop",
    "acceptance": "a synchronized-output frame (ESC[?2026l) whose virtual-screen text matches the expected marker",
    "latency_clock": "perf_counter_ns immediately before the PTY write, to the read(2) that completed the accepted frame",
    "during_read": "key written after a loading frame and before any ready frame; its response lacks the ready marker; "
                   "the product's /proc/PID/io rchar rose by at least --min-read-bytes between the key and the ready frame",
    "escape": "a lone Escape is held by the parser for ESC_WAIT; Escape and another key are paired on the same screen "
              "in alternating order, and the Escape figure is also reported less the source constant",
    "figures": "first visible response (key write to the loading frame), report ready (key write to the ready frame) and "
               "input latency during a read (key written after the loading frame and before the ready frame, to its own response) "
               "are separate figures and are never merged",
    "instrumentation": "per-frame /proc/PID/io and stat reads in insights trials only; the live comparison reads none. "
                       "No product hook, flag or build is used: the product is observed from outside",
    "p95": "nearest rank",
    "summary": "median and range of the trials' medians (CONTRIBUTING), plus the pooled samples",
}


# ---------------------------------------------------------------------------- live board, old against new

class LiveWriter:
    """Writes the way T3 does: in one transaction, a thread's projection row, an
    orchestration_events row (application version 2, aggregate thread) and the
    thread-projections cursor. The event changes a visible title: "Bench task 003 E017"."""

    def __init__(self, db_path: Path, now_ms: float | None):
        # now_ms: the pinned display clock, events then stamped just after it; None stamps them with the real clock
        self.now_ms = now_ms
        self.db = sqlite3.connect(db_path, isolation_level=None)
        self.db.execute("PRAGMA busy_timeout=2000")
        if self.db.execute("PRAGMA journal_mode").fetchone()[0].lower() != "wal":
            raise Fail("fixture database is not in WAL mode")

    def apply(self, k: int, thread: int) -> tuple[str, int]:
        tid, title = f"bench-{thread:03d}", f"Bench task {thread:03d} E{k:03d}"
        at = iso((self.now_ms + k) if self.now_ms is not None else time.time() * 1000)
        db = self.db
        db.execute("BEGIN IMMEDIATE")
        db.execute(f"UPDATE {P}threads SET title=?, payload_json=json_set(payload_json,'$.title',?) WHERE thread_id=?", (title, title, tid))
        version = db.execute("SELECT COALESCE(MAX(stream_version),0)+1 FROM orchestration_events WHERE aggregate_kind='thread' AND stream_id=?", (tid,)).fetchone()[0]
        seq = db.execute("SELECT COALESCE(MAX(sequence),0)+1 FROM orchestration_events").fetchone()[0]
        db.execute("INSERT INTO orchestration_events(sequence,event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) "
                   "VALUES(?,?,'thread',?,?,'thread.meta-updated',?,'system','{\"note\":\"synthetic\"}','{}',2)", (seq, f"e-bench-{k}", tid, version, at))
        db.execute(f"UPDATE {P}metadata SET last_sequence=?, updated_at=? WHERE projection_name='thread-projections'", (seq, at))
        db.execute("COMMIT")
        return title, time.perf_counter_ns()

    def state_sha256(self) -> str:
        rows = self.db.execute(f"SELECT thread_id,title FROM {P}threads ORDER BY thread_id").fetchall()
        last = self.db.execute("SELECT MAX(sequence) FROM orchestration_events").fetchone()[0]
        return hashlib.sha256(json.dumps([rows, last]).encode()).hexdigest()

    def close(self) -> None:
        self.db.close()


def make_script(seed: int, events: int, keys: int, duration_ms: int, event_threads: list[int], key_gap_ms: int = 160) -> list[dict]:
    """The script both arms follow: the same offsets, the same events, the same keys. Seeded,
    so a replay is identical; the keys keep a minimum gap so one key's answer cannot be
    another's."""
    rng = random.Random(seed)
    items: list[dict] = []
    slot = duration_ms / max(1, events)
    for k in range(events):
        items.append({"at_ms": int(slot * (k + 0.1 + 0.8 * rng.random())), "kind": "event", "k": k, "thread": event_threads[k % len(event_threads)]})
    kslot = max(key_gap_ms, duration_ms / max(1, keys))
    for n in range(keys):
        target = 1 if n % 2 == 0 else 0
        items.append({"at_ms": int(kslot * (n + 0.1 + 0.8 * rng.random())), "kind": "key", "key": "j" if target else "k", "target": target})
    items.sort(key=lambda x: (x["at_ms"], x["kind"]))
    last = -10**9
    for it in (x for x in items if x["kind"] == "key"):
        it["at_ms"] = max(it["at_ms"], last + key_gap_ms)
        last = it["at_ms"]
    items.sort(key=lambda x: (x["at_ms"], x["kind"]))
    return items


def script_sha256(script: list[dict]) -> str:
    return hashlib.sha256(json.dumps(script, sort_keys=True).encode()).hexdigest()


def run_script(p: Pty, writer: LiveWriter, script: list[dict], key_timeout: float, event_timeout: float, delay_ms: float) -> dict:
    t0 = time.perf_counter_ns()
    probes: list[dict] = []
    keys: list[dict] = []
    events: list[dict] = []

    def resolve() -> None:
        for pr in list(probes):
            while pr["pos"] < len(p.frames):
                f = p.frames[pr["pos"]]
                pr["pos"] += 1
                if pr["kind"] == "key":
                    hit = any(line.startswith("▌") and f"Bench task {pr['target']:03d}" in line for line in f["text"].splitlines())
                else:
                    hit = pr["token"] in f["text"]
                if hit:
                    pr["latency_ms"] = (f["at_ns"] - pr["started"]) / 1e6
                    pr["frame"] = f["index"]
                    break
            if "latency_ms" in pr or time.perf_counter_ns() > pr["deadline"]:
                probes.remove(pr)
                (keys if pr["kind"] == "key" else events).append(pr)

    nxt = 0
    while nxt < len(script) or probes:
        now = time.perf_counter_ns()
        while nxt < len(script) and t0 + script[nxt]["at_ms"] * 1_000_000 <= now:
            it = script[nxt]
            nxt += 1
            p.drain()
            resolve()
            if it["kind"] == "key":
                started, before = p.press(it["key"], delay_ms)
                probes.append({"kind": "key", "key": it["key"], "target": it["target"], "started": started, "pos": before,
                               "offset_ms": it["at_ms"], "late_ms": (started - (t0 + it["at_ms"] * 1_000_000)) / 1e6,
                               "deadline": started + int(key_timeout * 1e9)})
            else:
                pos = len(p.frames)
                token, committed = writer.apply(it["k"], it["thread"])
                probes.append({"kind": "event", "k": it["k"], "thread": it["thread"], "token": token, "started": committed,
                               "pos": pos, "offset_ms": it["at_ms"], "late_ms": (committed - (t0 + it["at_ms"] * 1_000_000)) / 1e6,
                               "deadline": committed + int(event_timeout * 1e9)})
        resolve()
        wake = min([t0 + script[nxt]["at_ms"] * 1_000_000] if nxt < len(script) else [now + 20_000_000])
        wake = min(wake, now + 20_000_000)
        p.pump(max(0.0, (wake - time.perf_counter_ns()) / 1e9))
    return {"keys": keys, "events": events, "duration_ms": (time.perf_counter_ns() - t0) / 1e6}


def peak_rss(pid: int) -> int:
    try:
        m = re.search(r"^VmHWM:\s+(\d+) kB", Path(f"/proc/{pid}/status").read_text(), re.M)
        return int(m.group(1)) * 1024 if m else 0
    except OSError:
        return 0


def live_trial(label: str, binary: Path, args, fixture: Path, scratch: Path, script: list[dict], delay_ms: float) -> dict:
    db, db_sha = trial_db(fixture, scratch)
    p = Pty(product_command(binary, args.product_arg, db=db), scratch / "run", now_ms=DisplayClock.fixed_ms,
            proc_samples=False)
    writer = LiveWriter(db, DisplayClock.fixed_ms)
    out: dict = {"label": label, "binary": str(binary), "binary_sha256": sha(binary), "load_start": load(),
                 "initial_db_sha256": db_sha, "script_sha256": script_sha256(script)}
    ready_rx = rx(args.board_ready)
    try:
        first = p.wait(lambda f: bool(ready_rx.search(f["text"])), args.timeout, what="the Live board")
        p.settle(args.settle)
        out["ready_ms"] = (first["at_ns"] - p.start_ns) / 1e6
        out["initial_text"] = p.screen.text()
        for thread in sorted({e["thread"] for e in script if e["kind"] == "event"}):
            if f"Bench task {thread:03d}" not in out["initial_text"]:
                raise Fail(f"event thread {thread} is not on the first screen, so its change cannot be seen; screen:\n{out['initial_text']}")
        cpu0, bytes0, frames0 = proc_cpu_ticks(p.pid), len(p.raw), len(p.frames)
        res = run_script(p, writer, script, args.key_timeout, args.event_timeout, delay_ms)
        cpu1 = proc_cpu_ticks(p.pid)
        if cpu0 is None or cpu1 is None:
            raise Fail("process CPU counter unreadable")
        window = args.event_window_ms
        event_starts = [e["started"] for e in res["events"]]
        keys = res["keys"]
        for k in keys:
            k["near_event"] = any(0 <= k["started"] - s <= window * 1_000_000 for s in event_starts)
        missed_keys = [k for k in keys if "latency_ms" not in k]
        missed_events = [e for e in res["events"] if "latency_ms" not in e]
        if missed_keys:
            raise Fail(f"{len(missed_keys)} key(s) never showed the expected selection: {[k['offset_ms'] for k in missed_keys]}")
        if len(missed_events) > args.allow_missed_events:
            raise Fail(f"{len(missed_events)} event(s) never reached a frame (allowed {args.allow_missed_events}): "
                       f"{[e['token'] for e in missed_events]}")
        p.settle(0.3)
        ec = esc_control(p, ESC_CONTROL_BOARD, args.esc_pairs, delay_ms) if args.esc_pairs else {"esc_ms": [], "alt_ms": []}
        ticks = os.sysconf("SC_CLK_TCK")
        out.update({
            "help_close_esc_ms": ec["esc_ms"], "help_close_alt_ms": ec["alt_ms"],
            "key_ms": [k["latency_ms"] for k in keys],
            "key_quiet_ms": [k["latency_ms"] for k in keys if not k["near_event"]],
            "key_near_event_ms": [k["latency_ms"] for k in keys if k["near_event"]],
            "event_to_frame_ms": [e["latency_ms"] for e in res["events"] if "latency_ms" in e],
            "missed_events": len(missed_events), "late_fire_ms_max": max(x["late_ms"] for x in keys + res["events"]),
            "cpu_s": (cpu1 - cpu0) / ticks, "cpu_percent": (cpu1 - cpu0) / ticks * 100 / (res["duration_ms"] / 1000),
            "output_bytes": len(p.raw) - bytes0, "frames": len(p.frames) - frames0, "peak_rss_bytes": peak_rss(p.pid),
            "duration_ms": res["duration_ms"], "final_state_sha256": writer.state_sha256(),
            "harness": harness_cost(p), "harness_cpu_s": time.process_time(),
            "clock": check_clock_unchanged(fixture_manifest(fixture), args.real_clock) or ("real" if args.real_clock else "fixed"),
            "keys": [{k: v for k, v in x.items() if k not in ("pos", "deadline", "started")} for x in keys],
            "events": [{k: v for k, v in x.items() if k not in ("pos", "deadline", "started")} for x in res["events"]],
            "load_end": load()})
        return out
    finally:
        writer.close()
        p.quit()


def command_live(args) -> int:
    fixture = check_scratch(Path(args.fixture))
    manifest = fixture_manifest(fixture)
    set_display_clock(manifest, args.real_clock, args.max_fixture_age_min)
    scratch = check_scratch(Path(args.scratch))
    scratch.mkdir(parents=True, exist_ok=True)
    if (scratch / "results.json").exists():
        raise ValueError(f"results already exist: {scratch}/results.json")
    old, new = Path(args.old).resolve(), Path(args.new).resolve()
    hashes = {"old": sha(old), "new": sha(new)}
    visible = list(range(args.event_threads))
    scripts = {"old": binary_is_script(old), "new": binary_is_script(new)}
    script = make_script(args.seed, args.events, args.keys, args.duration_ms, visible)
    runs: list[dict] = []
    problems: list[str] = []
    if hashes["old"] == hashes["new"]:
        problems.append("old and new are the same file: this is an A/A control, not a comparison")
    if args.inject_delay_ms:
        problems.append(f"{args.inject_delay_ms} ms was injected into the new arm: this is a negative control")
    if any(scripts.values()):
        problems.append(f"claim: an arm is a script (a stand-in), not the product: {scripts}")
    for i in range(args.rounds):
        order = [("old", old), ("new", new)]
        if i % 2:
            order.reverse()
        for label, binary in order:
            run = live_trial(label, binary, args, fixture, scratch / f"round-{i:02d}-{label}", script,
                             args.inject_delay_ms if label == "new" else 0.0)
            run["round"] = i
            runs.append(run)
            (scratch / f"round-{i:02d}-{label}.json").write_text(json.dumps({k: v for k, v in run.items() if k != "initial_text"}, indent=2) + "\n")
            (scratch / f"round-{i:02d}-{label}-initial.txt").write_text(run["initial_text"] + "\n")
            print(f"round {i + 1}/{args.rounds} {label}: key {summarize(run['key_ms'])['median_ms']:.2f} ms median "
                  f"(n={len(run['key_ms'])}, near event {len(run['key_near_event_ms'])}), event to frame "
                  f"{summarize(run['event_to_frame_ms'])['median_ms']:.0f} ms, cpu {run['cpu_percent']:.1f}%", flush=True)
    arm = lambda label, field: [r[field] for r in runs if r["label"] == label]
    matched = {"script_sha256_equal": len({r["script_sha256"] for r in runs}) == 1,
               "initial_db_sha256_equal": len({r["initial_db_sha256"] for r in runs}) == 1,
               "final_state_sha256_equal_per_round": all(len({r["final_state_sha256"] for r in runs if r["round"] == i}) == 1 for i in range(args.rounds)),
               # pinned: the whole first screen must be equal. Real clock: the board's sparklines and ages scroll with time, so
               # only the thread rows on screen (names, in order) can be compared
               "initial_screen_equal": (len({r["initial_text"] for r in runs}) == 1 if not args.real_clock
                                        else len({tuple(re.findall(r"Bench task \d{3}", r["initial_text"])) for r in runs}) == 1),
               "initial_screen_compare": "thread rows only (real clock)" if args.real_clock else "whole screen",
               "order": [[r["label"] for r in runs if r["round"] == i] for i in range(args.rounds)]}
    for name in ("script_sha256_equal", "initial_db_sha256_equal", "final_state_sha256_equal_per_round"):
        if not matched[name]:
            problems.append(f"arms were not matched: {name} is false")
    summary = {}
    for metric in ("key_ms", "key_quiet_ms", "key_near_event_ms", "event_to_frame_ms"):
        summary[metric] = {"old": agg(arm("old", metric)), "new": agg(arm("new", metric)),
                           "new_minus_old": paired(arm("new", metric), arm("old", metric))}
    summary["cpu_percent"] = {"old": agg([[x] for x in arm("old", "cpu_percent")]), "new": agg([[x] for x in arm("new", "cpu_percent")]),
                              "new_minus_old": paired([[x] for x in arm("new", "cpu_percent")], [[x] for x in arm("old", "cpu_percent")])}
    esc_wait = source_esc_wait(Path(args.source_root) if args.source_root else None)
    summary["parser_escape_wait_on_live_board"] = {
        label: esc_isolation([x for r in runs if r["label"] == label for x in r["help_close_esc_ms"]],
                             [x for r in runs if r["label"] == label for x in r["help_close_alt_ms"]], esc_wait)
        for label in ("old", "new")}
    summary["harness_overhead"] = {label: {"per_frame_handle_us_median": statistics.median([r["harness"]["handle_us_median"] for r in runs if r["label"] == label]),
                                           "per_frame_handle_us_max": max(r["harness"]["handle_us_max"] for r in runs if r["label"] == label),
                                           "proc_samples_per_frame": False} for label in ("old", "new")}
    summary["output_bytes"] = {"old": arm("old", "output_bytes"), "new": arm("new", "output_bytes")}
    summary["frames"] = {"old": arm("old", "frames"), "new": arm("new", "frames")}
    loads = [x for r in runs for x in (r["load_start"], r["load_end"])]
    report = {"format": 2, "kind": "live-board-ux", "started_utc": dt.datetime.now(dt.timezone.utc).isoformat(), "fixture": manifest,
              "old": {"path": str(old), "sha256": hashes["old"], "commit": args.old_commit},
              "new": {"path": str(new), "sha256": hashes["new"], "commit": args.new_commit},
              "clock": clock_record(manifest, args.real_clock),
              "rounds": args.rounds, "script": {"seed": args.seed, "events": args.events, "keys": args.keys,
                                                "duration_ms": args.duration_ms, "event_threads": visible, "sha256": script_sha256(script)},
              "matched": matched, "summary": summary,
              "claim": claim_status(args.rounds, loads, args.load_limit, sha(old) == hashes["old"] and sha(new) == hashes["new"],
                                    problems, "real" if args.real_clock else "fixed (periodic headroom refresh suppressed)"),
              "method": {**METHOD, "events": "matched script: same seeded offsets, events and keys in both arms; an event is a title "
                         "change committed as T3 commits one; event-to-frame is commit return to the first frame showing the new title, "
                         "and includes the product's poll phase, so compare medians over many events, not single ones",
                         "near_event": f"a key written within {args.event_window_ms} ms after an event commit"},
              "control": {"inject_delay_ms_new": args.inject_delay_ms}, "product_args": args.product_arg}
    (scratch / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"claim": report["claim"], "matched": matched,
                      "key_ms": {k: summary["key_ms"][k] for k in ("new_minus_old",)},
                      "event_to_frame_ms": summary["event_to_frame_ms"]["new_minus_old"]}, indent=2))
    return 0


# ---------------------------------------------------------------------------- dates-check (the harness's period labels against the product)

DATES_CHECK_CLOCKS = ["2026-11-19T12:00:00", "2026-11-30T12:00:00", "2026-12-31T12:00:00", "2027-01-01T12:00:00",
                      "2027-03-01T12:00:00", "2028-03-01T12:00:00", "2027-05-02T12:00:00"]  # the last is a Sunday


def command_dates_check(args) -> int:
    """Compare the period labels this harness builds (`period_dates`) with what the product's period row says, at
    several pinned display clocks (a week that crosses a month and a year, the first of March in a common and a leap
    year, a Sunday). A marker tied to the wrong label could let one period's finished read stand for another's, so
    each of the four periods is opened at each clock and its row compared with the harness's label. Choose
    clocks that avoid fallback. When both periods are empty the product keeps the requested selection. When
    only the previous period is empty the product labels the current data it shows, so the requested label
    cannot serve as a reference. Fails closed on any difference."""
    fixture = check_scratch(Path(args.fixture))
    fixture_manifest(fixture)
    scratch = check_scratch(Path(args.scratch))
    scratch.mkdir(parents=True, exist_ok=True)
    binary = Path(args.binary).resolve()
    clocks = args.at or DATES_CHECK_CLOCKS
    rows, wrong = [], []
    for n, text in enumerate(clocks):
        now = dt.datetime.fromisoformat(text).replace(tzinfo=dt.timezone.utc).timestamp() * 1000
        DisplayClock.fixed_ms = now
        labels = period_dates(now)
        db, _ = trial_db(fixture, scratch / f"clock-{n:02d}")
        p = Pty(product_command(binary, args.product_arg, db=db), scratch / f"clock-{n:02d}/run", now_ms=now)
        try:
            p.wait(lambda f: bool(rx("^▌.*Bench task 000").search(f["text"])), args.timeout, what="the Live board")
            p.settle(0.4)
            for k, key in enumerate(("i", "right", "right", "right")):
                period = ("tw", "lw", "tm", "lm")[k]
                started, before = p.press(key)
                frame = p.wait(lambda f: bool(re.search(FOOTER_READY, f["text"], re.M)) and not re.search(FOOTER_LOADING, f["text"], re.M),
                               args.timeout, before, what=f"the {PERIOD_WORDS[period]} read at {text}")
                row = frame["text"].splitlines()[1]
                note = row.split("last month", 1)[1].strip() if "last month" in row else row.strip()
                ok = note.startswith(labels[period]["full"])
                rows.append({"clock": text, "period": period, "label": labels[period]["full"], "row": note, "match": ok})
                if not ok:
                    wrong.append(rows[-1])
                p.settle(0.1, 2)
        finally:
            p.quit()
        print(f"{text}  " + "  ".join(f"{r['period']}={'ok' if r['match'] else 'WRONG'}" for r in rows if r["clock"] == text), flush=True)
    DisplayClock.fixed_ms = None
    (scratch / "dates-check.json").write_text(json.dumps({"binary": {"path": str(binary), "sha256": sha(binary)}, "rows": rows}, indent=2, ensure_ascii=False) + "\n")
    if wrong:
        raise Fail("the harness's period labels differ from the product's: " + json.dumps(wrong, ensure_ascii=False))
    print(f"{len(rows)} period rows matched the harness's labels at {len(clocks)} clocks")
    return 0


# ---------------------------------------------------------------------------- storage (inherited from d627b28a)

def storage_done(text: str) -> bool:
    return "folders · scanned" in text or "scanned with gaps" in text


def storage_scanning(text: str) -> bool:
    """1.3.0 says "scanning…" on first entry and "last scan HH:MM:SS · refreshing" on a rescan
    (it keeps the old rows); 706060e said "scanning" for both."""
    return "scanning…" in text or "· refreshing" in text


def scan_thread_seen(pid: int) -> bool:
    try:
        for task in Path(f"/proc/{pid}/task").iterdir():
            try:
                if (task / "comm").read_text().strip() == "t3-storage":
                    return True
            except (FileNotFoundError, PermissionError):
                pass
    except FileNotFoundError:
        pass
    return False


def storage_entry(p: Pty, timeout: float = 120.0) -> dict:
    """`s` to the Storage screen: the first frame, and the frame that finishes the scan."""
    started, before = p.press("s")
    opened = p.wait(lambda f: "STORAGE" in f["text"] and "VOLUMES" in f["text"], 10, before, what="the Storage screen")
    finished = opened if storage_done(opened["text"]) else \
        p.wait(lambda f: storage_done(f["text"]), timeout, opened["index"], what="the end of the Storage scan")
    return {"first_frame_ms": (opened["at_ns"] - started) / 1e6, "scan_done_ms": (finished["at_ns"] - started) / 1e6,
            "scan_was_running_at_first_frame": not storage_done(opened["text"])}


def storage_scan_keys(p: Pty, count: int, attempts: int = 20) -> tuple[dict, dict]:
    """Keys while a scan runs. A scanning inventory has no selectable folder rows, so the
    Storage help overlay is the key: `?` and Escape. A pair counts only if closing the help
    reveals a frame still marked scanning, which proves both answers preceded the scan's end."""
    samples: dict[str, list[float]] = {"help_open_ms": [], "help_close_by_escape_ms": []}
    errors: list[str] = []
    scans, overlap, worker = 0, False, False
    for _ in range(attempts):
        if len(samples["help_open_ms"]) >= count:
            break
        p.drain()
        scan_started, before = p.press("r")
        opened = p.wait(lambda f: "STORAGE" in f["text"] and "VOLUMES" in f["text"], 10, before, what="Storage after r")
        scans += 1
        worker = worker or scan_thread_seen(p.pid)
        if storage_done(opened["text"]):
            errors.append("rescan finished before a scanning frame; no keys scored for this attempt")
            continue
        while len(samples["help_open_ms"]) < count:
            help_started, hb = p.press("?")
            help_frame = p.wait(lambda f: "STORAGE HELP" in f["text"], 10, hb, what="Storage help")
            close_started, cb = p.press("escape")
            close_frame = p.wait(lambda f: "STORAGE" in f["text"] and "VOLUMES" in f["text"], 10, cb, what="Storage after Escape")
            if storage_done(close_frame["text"]):
                break
            if not storage_scanning(close_frame["text"]):
                errors.append("help closed to neither a scanning nor a finished frame")
                break
            samples["help_open_ms"].append((help_frame["at_ns"] - help_started) / 1e6)
            samples["help_close_by_escape_ms"].append((close_frame["at_ns"] - close_started) / 1e6)  # includes the parser's wait
            overlap = overlap or worker
    return samples, {"scans": scans, "worker_thread_seen": worker, "overlap_confirmed": overlap, "errors": errors}


def command_storage(args) -> int:
    """The new build alone, Storage ready and scanning. Needs a fixture built with --worktrees
    (hundreds of thousands of entries, or the scan ends before a key can land and every
    scanning sample is rejected, by design). The Escape sample here includes the parser's
    wait; the pair's other member is `?`, so report them apart.

    This is the one scenario outside the `--db` rule. Storage finds worktrees by automatic
    discovery, which needs T3CODE_HOME to be a populated data folder, so the run gives the
    product a copy of a synthetic fixture as T3CODE_HOME and no `--db`. It never touches the
    real home, but it is not the isolation every other command has, so it refuses to start
    without the explicit flag. It was not run when the harness was repaired."""
    if not args.accept_discovery_outside_db_rule:
        raise Fail("storage needs automatic discovery, which is outside this harness's --db rule (T3CODE_HOME would be a "
                   "populated synthetic fixture copy). Pass --accept-discovery-outside-db-rule to run the Storage-only scenario")
    fixture = check_scratch(Path(args.fixture))
    manifest = fixture_manifest(fixture)
    set_display_clock(manifest, args.real_clock, args.max_fixture_age_min)
    if not manifest["worktrees"]:
        raise Fail("fixture has no worktrees; rebuild it with --worktrees and --files")
    scratch = check_scratch(Path(args.scratch))
    scratch.mkdir(parents=True, exist_ok=True)
    binary = Path(args.new).resolve()
    trials = []
    for i in range(args.rounds):
        home = trial_discovery_home(fixture, scratch / f"round-{i:02d}")
        p = Pty(product_command(binary, args.product_arg, discovery=True), scratch / f"round-{i:02d}/run",
                now_ms=DisplayClock.fixed_ms, discovery_home=home)
        try:
            p.wait(lambda f: "▌" in f["text"], args.timeout, what="the Live board")
            p.settle(0.5)
            entry = storage_entry(p)
            ready_keys, current = [], "project"
            for _ in range(args.keys):
                key, want = ("j", r"^▌.*Bench task 000") if current == "project" else ("k", r"^▌ +Atlas")
                ready_keys.append(score_key(p, key, want, timeout=10, what="Storage selection")["latency_ms"])
                current = "thread" if current == "project" else "project"
            scan_keys, meta = storage_scan_keys(p, args.keys)
            trials.append({"round": i, "entry": entry, "ready_keys_ms": ready_keys, "scan_keys_ms": scan_keys, "scan": meta,
                           "load_end": load()})
        finally:
            p.quit()
        print(f"round {i + 1}/{args.rounds}: first frame {entry['first_frame_ms']:.1f} ms, scan done {entry['scan_done_ms']:.0f} ms, "
              f"{len(scan_keys['help_open_ms'])} scanning pairs", flush=True)
    problems = []
    if any(len(t["scan_keys_ms"]["help_open_ms"]) < args.keys for t in trials):
        problems.append("fewer scanning-key samples than requested in a round; the tree is too small for the scan to outlast the keys, or the scan thread was not seen")
    if not all(t["scan"]["overlap_confirmed"] for t in trials):
        problems.append("a scan worker thread named t3-storage was not seen in a round")
    report = {"kind": "storage-ux", "fixture": manifest, "binary": {"path": str(binary), "sha256": sha(binary)}, "rounds": args.rounds,
              "ready_keys": agg([t["ready_keys_ms"] for t in trials]), "scanning_help_open": agg([t["scan_keys_ms"]["help_open_ms"] for t in trials]),
              "scanning_help_close_by_escape": agg([t["scan_keys_ms"]["help_close_by_escape_ms"] for t in trials]),
              "first_frame_ms": agg([[t["entry"]["first_frame_ms"]] for t in trials]),
              "scan_done_ms": agg([[t["entry"]["scan_done_ms"]] for t in trials]), "problems": problems,
              "note": "help_open is a key with no parser wait; help_close_by_escape includes ESC_WAIT (see the esc_control in the insights command for the isolated figure)",
              "trials": trials, "method": METHOD}
    (scratch / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: report[k] for k in ("ready_keys", "scanning_help_open", "scanning_help_close_by_escape", "problems")}, indent=2))
    return 0 if not problems else 2


# ---------------------------------------------------------------------------- floor

def command_floor(args) -> int:
    """The measuring path's own latency, with no product in it: a stand-in that redraws one
    number per key. Run with and without the per-frame /proc samples, alternating, so the
    cost of the evidence the insights command collects is a number and not an assumption.
    Subtract nothing from product figures with it: it is the least latency this harness can
    report, and the spread of that least."""
    fixture = check_scratch(Path(args.fixture))
    fixture_manifest(fixture)
    scratch = check_scratch(Path(args.scratch))
    scratch.mkdir(parents=True, exist_ok=True)
    standin = Path(args.standin).resolve()
    out: dict = {"proc_samples_off": [], "proc_samples_on": []}
    for r in range(args.rounds):
        for flag in ((False, True) if r % 2 == 0 else (True, False)):
            db, _ = trial_db(fixture, scratch / f"floor-{r:02d}-{int(flag)}")
            p = Pty(product_command(standin, ["--mode", "good"], db=db), scratch / f"floor-{r:02d}-{int(flag)}/run",
                    now_ms=LEGACY_NOW_MS, proc_samples=flag)
            try:
                p.wait(lambda f: "LIVE (stand-in)" in f["text"], args.timeout, what="stand-in board")
                p.settle(0.3)
                samples = []
                for n in range(1, args.keys + 1):
                    samples.append(score_key(p, "x", rf"tick {n}$", what="floor")["latency_ms"])
                    p.settle(0.02, 1)
                out["proc_samples_on" if flag else "proc_samples_off"].append(samples)
                out.setdefault("harness", {})["on" if flag else "off"] = harness_cost(p)
            finally:
                p.quit()
    report = {"kind": "measuring-path-floor", "rounds": args.rounds, "keys": args.keys,
              "proc_samples_off": agg(out["proc_samples_off"]), "proc_samples_on": agg(out["proc_samples_on"]),
              "harness_cost_last_trial": out["harness"], "load": load(),
              "note": "stand-in, not the product; the floor is the PTY round trip, the select wake-up and the virtual-screen parse. "
                      "The on/off difference is the cost of one /proc sample pair per frame."}
    print(json.dumps(report, indent=2))
    (scratch / "floor.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0


# ---------------------------------------------------------------------------- selftest

def run_cli(argv: list[str], patch: dict | None = None) -> tuple[int, str]:
    """Run this script's own command line in-process, capturing output. `patch` swaps module
    functions for the duration, to simulate a fault no stand-in can (an unreadable counter)."""
    saved = (sys.argv, sys.stdout, sys.stderr)
    originals = {k: globals()[k] for k in (patch or {})}
    buf = io.StringIO()
    sys.argv, sys.stdout, sys.stderr = ["terminal_ux.py", *argv], buf, buf
    globals().update(patch or {})
    try:
        try:
            main()
            code = 0
        except SystemExit as stop:
            code = stop.code if isinstance(stop.code, int) else 1
    finally:
        globals().update(originals)
        sys.argv, sys.stdout, sys.stderr = saved
    return code, buf.getvalue()


def command_selftest(args) -> int:
    """The harness's negative controls. Each fault-mode stand-in breaks exactly one property
    that a scored result depends on; the control passes only if the harness refuses it for
    the right reason. A good stand-in must pass, and a injected delay and the Escape wait
    must be recovered as numbers. Prints one line per control; exit 1 if any control
    behaves unexpectedly."""
    root = check_scratch(Path(args.scratch))
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        raise ValueError(f"selftest scratch must be empty: {root}")
    standin = str(Path(args.standin).resolve())
    standin_profile = str(HERE / "terminal_ux_standin_profile.json")  # never the product profile: see load_profile
    make_fixture(root / "fixture", REPO / "checks/fixtures/t3-schema.sql", 30, 12, 60, 3, 4, 2048, 0, 0, LEGACY_NOW_MS, None)
    fixture = str(root / "fixture")
    fast = ["--rounds", "1", "--keys", "4", "--idle-seconds", "0.4", "--esc-pairs", "2", "--exit-pairs", "2",
            "--during-keys", "1", "--during-attempts", "3", "--read-timeout", "6"]
    results: list[tuple[str, bool, str]] = []

    def insights(name: str, mode: str, extra: list[str] | None = None, patch: dict | None = None, esc_wait: float = 25,
                 profile: str | None = None, read_ms: float = 700) -> tuple[int, dict, str]:
        scratch = root / name
        code, text = run_cli(["insights", "--new", standin, "--fixture", fixture, "--scratch", str(scratch), *fast,
                              "--product-arg=--mode", f"--product-arg={mode}", f"--product-arg=--esc-wait-ms={esc_wait}",
                              f"--product-arg=--read-ms={read_ms}", "--source-root", str(REPO),
                              "--profile", profile or standin_profile, "--profile-kind", "stand-in", *(extra or [])], patch)
        report = json.loads((scratch / "results.json").read_text()) if (scratch / "results.json").exists() else {}
        return code, report, text

    def expect(name: str, ok: bool, detail: str) -> None:
        results.append((name, ok, detail))
        print(f"{'PASS' if ok else 'FAIL'}  {name}: {detail}", flush=True)

    def reasons(rep: dict) -> dict:
        return rep.get("evidence", {}).get("rejected_reasons", {})

    code, rep, _ = insights("good", "good")
    ev = rep.get("evidence", {})
    expect("good stand-in is accepted", code == 0 and ev.get("valid") and not reasons(rep) and ev.get("during_keys_accepted", 0) >= 1,
           f"exit {code}, valid={ev.get('valid')}, reads accepted {ev.get('reads_accepted')}/{ev.get('reads_attempted')}, during keys accepted {ev.get('during_keys_accepted')}")
    iso0 = json.loads((root / "good/round-00.json").read_text()).get("isolation", {})
    expect("a trial records its own isolation: --db on its copy, empty T3CODE_HOME at the end, stand-in profile not claimable",
           iso0.get("db_is_trial_copy") is True and iso0.get("t3code_home_entries_at_end") == [] and not iso0.get("demo")
           and not iso0.get("t3code_home_discovery") and any("stand-in profile" in x for x in rep["claim"]["reasons"])
           and not any("outside a synchronized" in x for x in rep["claim"]["reasons"]),
           f"{iso0}; claim {rep['claim']['reasons']}")
    exit_good = rep.get("summary", {}).get("exit", {})
    expect("Escape wait is recovered at 25 ms", 20 <= (exit_good.get("esc_minus_alt_key_median_ms") or -1) <= 32,
           f"Escape minus other key {exit_good.get('esc_minus_alt_key_median_ms')} ms, program time {exit_good.get('program_ms_median')} ms")

    code, rep, _ = insights("no-loading", "no-loading")
    expect("NEG no loading frame (ready frame only) is rejected", code == 2 and "ready-frame-with-no-loading-frame-before-it" in reasons(rep),
           f"exit {code}, reasons {reasons(rep)}")
    code, rep, _ = insights("blocked-keys", "blocked-keys")
    r = reasons(rep)
    expect("NEG key answered only after the read ends is rejected",
           code == 2 and any(k.startswith(("during:read-finished-before", "during:response-frame-already")) for k in r) and rep["evidence"]["during_keys_accepted"] == 0,
           f"exit {code}, reasons {r}")
    code, rep, _ = insights("instant", "instant")
    expect("NEG read that ends at once (no key can occur during it) is rejected",
           code == 2 and rep["evidence"]["during_keys_accepted"] == 0 and bool(reasons(rep)), f"exit {code}, reasons {reasons(rep)}")
    code, rep, _ = insights("no-io", "no-io")
    expect("NEG loading frame with no read activity is rejected",
           code == 2 and "no-read-activity-between-loading-and-ready-frames" in reasons(rep), f"exit {code}, reasons {reasons(rep)}")
    code, rep, _ = insights("min-bytes", "good", ["--min-read-bytes", str(10 ** 10)])
    expect("NEG evidence threshold is binding (good stand-in, 10 GB required)",
           code == 2 and "no-read-activity-between-loading-and-ready-frames" in reasons(rep), f"exit {code}, reasons {reasons(rep)}")
    code, rep, _ = insights("unreadable", "good", patch={"proc_io_rchar": lambda pid: None})
    expect("NEG unreadable read counter is absent evidence, not zero", code == 2 and "read-counter-unreadable" in reasons(rep),
           f"exit {code}, reasons {reasons(rep)}")
    bad = json.loads(Path(standin_profile).read_text())
    bad["open"]["ready"] = "this marker never appears"
    (root / "bad-profile.json").write_text(json.dumps(bad))
    code, rep, text = insights("bad-marker", "good", profile=str(root / "bad-profile.json"), extra=["--read-timeout", "3"])
    expect("NEG marker that never appears fails closed", code == 3 and "timed out" in text, f"exit {code}")
    for wait in (0, 60):
        code, rep, _ = insights(f"esc-{wait}", "good", esc_wait=wait)
        e = rep.get("summary", {}).get("exit", {})
        diff = e.get("esc_minus_alt_key_median_ms")
        low, high = (-3, 6) if wait == 0 else (52, 70)
        expect(f"Escape isolation at a {wait} ms parser wait", diff is not None and low <= diff <= high and (e.get("program_ms_median") or 99) < 6,
               f"Escape minus other key {diff} ms (want {low}..{high}), program time {e.get('program_ms_median')} ms")

    # -- evidence controls added with the isolation repair
    code, rep, _ = insights("coalesced", "coalesced")
    expect("NEG loading and ready frames in one read(2) are rejected", code == 2 and "loading-and-ready-frames-in-one-read" in reasons(rep),
           f"exit {code}, reasons {reasons(rep)}, frames sharing a read {rep.get('evidence', {}).get('frames_sharing_a_read')}")
    code, rep, _ = insights("outside-sync", "outside-sync")
    expect("NEG bytes drawn outside a synchronized frame make the run unclaimable", code == 0 and any("outside a synchronized" in x for x in rep["claim"]["reasons"]),
           f"exit {code}, claim {rep.get('claim', {}).get('reasons')}")
    code, rep, _ = insights("iso-broken", "good", patch={"isolation_record": lambda p, db: {"argv_db": None, "db_is_trial_copy": True, "demo": False,
                                                                                         "t3code_home": "x", "t3code_home_discovery": False,
                                                                                         "t3code_home_entries_at_end": ["userdata"]}})
    expect("NEG a T3CODE_HOME that is not empty at the end invalidates the run", code == 2 and any("isolation broken" in x for x in rep["evidence"]["problems"]),
           f"exit {code}")
    # Another report's finished read must not complete this report's: a composite marker needs the footer AND the report's own body.
    ready = rx({"all": [r"read at \d\d:\d\d:\d\d", "WHERE THE HOURS WENT"]})
    tools_done = "RECAP  TOOLS\n LONGEST CALL\n read at 10:20:00"
    recap_done = "RECAP  TOOLS\n WHERE THE HOURS WENT\n read at 10:20:00"
    recap_loading = "RECAP  TOOLS\n WHERE THE HOURS WENT\n reading since 10:20:00"
    expect("NEG a Tools frame that says 'read at' does not complete Recap's read", not ready.search(tools_done) and bool(ready.search(recap_done)),
           f"Tools-done {bool(ready.search(tools_done))}, Recap-done {bool(ready.search(recap_done))}")
    expect("an overlay hides the heading, so a read finished behind help is not yet complete",
           not ready.search("INSIGHTS HELP\n read at 10:20:00") and not ready_only({"text": recap_loading}, rx(r"reading since \d\d:\d\d:\d\d"), ready),
           "help over a finished footer and a still-loading Recap are both not ready")

    # -- isolation: each of these must be refused before any product is started
    base = ["insights", "--new", standin, "--fixture", fixture, "--profile", standin_profile, "--profile-kind", "stand-in", *fast]

    def refused(name: str, needle: str, argv: list[str], patch: dict | None = None) -> None:
        scratch = root / f"refused-{name}"
        code, text = run_cli(argv, patch)
        started = scratch.exists() and any(scratch.iterdir())
        expect(f"NEG isolation: {name} is refused", code == 3 and needle in text and not started and not (scratch / "results.json").exists(),
               f"exit {code}, started {started}, wanted {needle!r} in: {text.strip().splitlines()[-1] if text.strip() else ''}")

    def insights_args(name: str, *more: str, fixture_path: str | None = None) -> list[str]:
        out = list(base)
        out[out.index("--fixture") + 1] = fixture_path or fixture
        return [*out, "--scratch", str(root / f"refused-{name}"), *more]

    refused("an external --db given as an argument", "is not allowed", insights_args("db-eq", f"--product-arg=--db={root}/elsewhere.sqlite"))
    refused("an external --db given as two arguments", "is not allowed", insights_args("db-two", "--product-arg=--db", f"--product-arg={root}/elsewhere.sqlite"))
    refused("--demo passed through", "is not allowed", insights_args("demo", "--product-arg=--demo"))
    refused("an input snapshot flag", "is not allowed", insights_args("snapshot", f"--product-arg=--snapshot={root}/x.json"))
    refused("a bench flag that reads a database", "is not allowed", insights_args("bench", "--product-arg=--bench-source=3"))
    refused("a path as the value of an allowed flag", "not a plain word", insights_args("theme-path", "--product-arg=--theme=/etc/passwd"))
    (root / "fx-link").symlink_to(root / "fixture")
    refused("a fixture folder that is a symlink", "must not be a symlink", insights_args("fx-link", fixture_path=str(root / "fx-link")))
    shutil.copytree(root / "fixture", root / "fx-symdb", symlinks=True)
    (root / "fx-symdb/userdata/statev2.sqlite").unlink()
    (root / "fx-symdb/userdata/statev2.sqlite").symlink_to(root / "fixture/userdata/statev2.sqlite")
    refused("a fixture database that is a symlink", "symlink", insights_args("fx-symdb", fixture_path=str(root / "fx-symdb")))
    shutil.copytree(root / "fixture", root / "fx-linkdir", symlinks=True)
    shutil.rmtree(root / "fx-linkdir/userdata")
    (root / "fx-linkdir/userdata").symlink_to(root / "fixture/userdata")
    refused("a fixture folder inside which a directory is a symlink", "symlink", insights_args("fx-linkdir", fixture_path=str(root / "fx-linkdir")))
    shutil.copytree(root / "fixture", root / "fx-hard", symlinks=True)
    (root / "fx-hard/userdata/statev2.sqlite").unlink()
    shutil.copy2(root / "fixture/userdata/statev2.sqlite", root / "hard-source.sqlite")  # link a copy: never raise the fixture's own link count
    os.link(root / "hard-source.sqlite", root / "fx-hard/userdata/statev2.sqlite")
    refused("a fixture database that is a hard link to another file", "single link", insights_args("fx-hard", fixture_path=str(root / "fx-hard")))
    shutil.copytree(root / "fixture", root / "fx-swap", symlinks=True)
    with open(root / "fx-swap/userdata/statev2.sqlite", "ab") as swapped:
        swapped.write(b"not the file this script built")
    refused("a database that differs from its manifest", "differs from its manifest", insights_args("fx-swap", fixture_path=str(root / "fx-swap")))
    (root / "fx-foreign/userdata").mkdir(parents=True)
    (root / "fx-foreign/userdata/statev2.sqlite").write_bytes(b"stand-in for someone else's database")
    refused("a folder this script did not build (no manifest)", "cannot open", insights_args("fx-foreign", fixture_path=str(root / "fx-foreign")))
    (root / "real-scratch").mkdir()
    (root / "scratch-link").symlink_to(root / "real-scratch")
    refused("a scratch folder that is a symlink", "must not be a symlink", [*base, "--scratch", str(root / "scratch-link")])
    refused("a scratch folder under /tmp", "must not be under /tmp", [*base, "--scratch", f"/tmp/terminal-ux-refused-{os.getpid()}"])

    class FakePwd:  # points "the real T3 home" at a folder of ours, so no path under the real ~/.t3 is touched
        @staticmethod
        def getpwuid(_uid):
            return type("pw", (), {"pw_dir": str(root / "fake-home")})()
    refused("a scratch folder under the real T3 home", "must not be under the real T3 home",
            [*base, "--scratch", str(root / "fake-home/.t3/x")], patch={"pwd": FakePwd})
    refused("the Storage-only scenario without its explicit acknowledgement", "outside this harness's --db rule",
            ["storage", "--new", standin, "--fixture", fixture, "--scratch", str(root / "refused-storage")])
    refused("a stand-in profile on a product run", "needs a 'product' profile", [*base[:base.index("--profile-kind")], "--scratch", str(root / "refused-kind-a")])
    refused("the product profile on a stand-in run", "needs a 'stand-in' profile",
            [*base[:base.index("--profile")], "--profile", str(HERE / "terminal_ux_profile.json"), "--profile-kind", "stand-in",
             "--scratch", str(root / "refused-kind-b")])
    refused("snap with neither --fixture nor --demo", "exactly one of", ["snap", "--binary", standin, "--scratch", str(root / "refused-snap")])

    # The harness and the stand-in each refuse to start a run that could discover a database.
    populated = root / "populated-home"
    (populated / "userdata").mkdir(parents=True)
    (populated / "userdata/statev2.sqlite").write_bytes(b"x")
    done = subprocess.run([sys.executable, "-I", standin, "--db", str(root / "fixture/userdata/statev2.sqlite")], env={"T3CODE_HOME": str(populated), "PATH": "/usr/bin:/bin"},
                          capture_output=True, text=True, timeout=20, stdin=subprocess.DEVNULL)
    expect("NEG isolation: the stand-in will not start with a populated T3CODE_HOME", done.returncode != 0 and "must be an empty folder" in done.stderr,
           f"exit {done.returncode}: {done.stderr.strip()[-80:]}")
    try:
        Pty([standin], root / "pty-no-db", now_ms=LEGACY_NOW_MS)
        ok_, why = False, "started"
    except Fail as error:
        ok_, why = "needs --demo or --db" in str(error), str(error)
    expect("NEG isolation: Pty refuses a run with neither --db nor --demo (discovery)", ok_, why)
    try:
        (root / "pty-existing").mkdir()
        Pty(product_command(Path(standin), [], db=root / "fixture/userdata/statev2.sqlite"), root / "pty-existing", now_ms=LEGACY_NOW_MS)
        ok_, why = False, "started"
    except Fail as error:
        ok_, why = "already exists" in str(error), str(error)
    expect("NEG isolation: Pty refuses a scratch folder that already exists (no reuse of a populated T3CODE_HOME)", ok_, why)
    try:
        product_command(Path(standin), [], db=root / "fixture/userdata/statev2.sqlite", demo=True)
        ok_, why = False, "built"
    except Fail as error:
        ok_, why = "exactly one of" in str(error), str(error)
    expect("NEG isolation: --db and --demo together are refused", ok_, why)
    product_profile = str(HERE / "terminal_ux_profile.json")
    prof = load_profile(product_profile)
    expect("the product profile parses as a state-driven product profile with a during-read key on the open and on the refresh",
           prof["kind"] == "product" and prof.get("format") == 2 and len(prof["open"]["during"]) >= 1
           and any(s_.get("during") for s_ in prof["switch"]) and isinstance(prof["calibrated"], bool),
           f"calibrated={prof['calibrated']}, {len(prof['switch'])} switch steps")

    # -- the real product profile against product-shaped stand-ins: the profile's own markers, state tracking and
    #    evidence rules are what refuse a wrong report, a wrong period, a snapshot that reads, a deferred key response
    def product_run(name: str, mode: str, *more: str, screen: str = "product", read_ms: float = 300, timeout: str = "4") -> tuple[int, dict, str]:
        scratch = root / name
        code, text = run_cli(["insights", "--new", standin, "--fixture", fixture, "--scratch", str(scratch), "--rounds", "1", "--keys", "4",
                              "--idle-seconds", "0.3", "--esc-pairs", "1", "--exit-pairs", "1", "--during-keys", "1", "--during-attempts", "4",
                              "--read-timeout", timeout, "--product-arg=--screen=" + screen, f"--product-arg=--mode={mode}",
                              f"--product-arg=--read-ms={read_ms}", "--source-root", str(REPO), "--profile", product_profile, *more])
        return code, (json.loads((scratch / "results.json").read_text()) if (scratch / "results.json").exists() else {}), text

    code, rep, text = product_run("p-good", "good", timeout="6")
    ev = rep.get("evidence", {})
    step_rows = ev.get("steps", {})
    reads_ok = all(v["reads_accepted"] >= 1 for v in step_rows.values() if v["reads_attempted"])
    during_ok = all(row["accepted"] >= 1 for v in step_rows.values() for row in v["during"].values())
    expect("PROFILE a product-shaped good stand-in passes every step: reports, periods, r, scroll during a refresh, help during an open",
           code == 0 and ev.get("valid") and reads_ok and during_ok and len(step_rows) == len(prof["switch"]) + len(prof.get("after_reading", [])) and ev.get("during_keys_accepted", 0) >= 3,
           f"exit {code}, valid={ev.get('valid')}, steps {len(step_rows)}, reads ok {reads_ok}, during ok {during_ok}, during keys {ev.get('during_keys_accepted')}/{ev.get('during_keys_attempted')}, "
           f"share of read left after a key {ev.get('during_key_share_of_read_still_to_come')}")
    expect("PROFILE the stand-in is a script, so even a calibrated profile cannot make the run claimable",
           any("is a script" in x for x in rep.get("claim", {}).get("reasons", [])) and rep.get("binary", {}).get("is_script") is True,
           f"claim {rep.get('claim', {}).get('reasons')}")
    code, rep, text = product_run("p-wrong-report", "wrong-report", timeout="3")
    expect("NEG PROFILE a switch to Tools that finishes with Recap's body is refused", code == 3 and "timed out" in text and "report-next-first-read" in text, f"exit {code}")
    code, rep, text = product_run("p-wrong-period", "wrong-period", timeout="3")
    expect("NEG PROFILE a period change that finishes with the previous period's dates is refused", code == 3 and "timed out" in text and "period-next-first-read" in text, f"exit {code}")
    code, rep, text = product_run("p-reread", "reread-snapshots", timeout="4")
    expect("NEG PROFILE a revisit that should come from the snapshot but reads is refused as a state error (an instant key must not time a read)",
           code == 3 and "state not recognized" in text and "answered at once" in text, f"exit {code}")
    code, rep, text = product_run("p-noload", "no-loading", timeout="4")
    expect("NEG PROFILE a switch that is declared a read but shows no loading frame is refused as a state error",
           code == 3 and "state not recognized" in text and "declared as a read" in text, f"exit {code}")
    code, rep, text = product_run("p-blocked", "blocked-keys", timeout="4")
    expect("NEG PROFILE deferred key responses (the read blocks the key loop) are refused: no during-read key is accepted",
           (code == 2 and rep.get("evidence", {}).get("during_keys_accepted") == 0) or (code == 3 and "state not recognized" in text and "deferred key" in text),
           f"exit {code}, during keys accepted {rep.get('evidence', {}).get('during_keys_accepted')}; {text.strip().splitlines()[0][:150] if text.strip() else ''}")
    code, rep, text = product_run("p-noio", "no-io", timeout="4")
    expect("NEG PROFILE loading frames with no read activity are refused on every step", code == 2 and rep.get("evidence", {}).get("reads_accepted") == 0,
           f"exit {code}, reasons {rep.get('evidence', {}).get('rejected_reasons')}")
    code, rep, text = product_run("p-oldscreen", "good", screen="standin", timeout="3")
    expect("NEG PROFILE the product profile on the stand-in's old screen (the stand-in standing in for the product) fails closed", code == 3 and "timed out" in text,
           f"exit {code}")

    # -- the display clock, the dated fixture and the period labels
    labels = {k: period_dates(utc_ms) for k, utc_ms in {'nov30': 1796040000000, 'jan1': 1798804800000, 'leap': 1835524800000}.items()}
    expect("period labels at a week that crosses a month, a week that crosses a year, and a leap-year March",
           labels["nov30"]["tw"]["full"] == "Nov 30 – Dec 6, 2026" and labels["nov30"]["tm"]["full"] == "November 2026" and labels["nov30"]["lm"]["full"] == "October 2026"
           and labels["jan1"]["tw"]["full"] == "Dec 28 – Jan 3, 2027" and labels["jan1"]["lw"]["short"] == "Dec 21 – 27" and labels["jan1"]["lm"]["full"] == "December 2026"
           and labels["leap"]["lm"]["full"] == "February 2028",
           f"{labels['nov30']['tw']}, {labels['jan1']['tw']}, {labels['leap']['lm']}")
    thin_now = int(dt.datetime(2026, 11, 16, 0, 30, tzinfo=dt.timezone.utc).timestamp() * 1000)  # Monday, half an hour into the week
    try:
        make_fixture(root / "fx-thin", REPO / "checks/fixtures/t3-schema.sql", 5, 2, None, 3, 4, 512, 0, 0, thin_now, None)
        ok_, why = False, "built"
    except Fail as error:
        ok_, why = "fewer than" in str(error), str(error)[:140]
    expect("NEG a fixture clock that leaves a period with no records is refused when it is built", ok_, why)
    code, text = run_cli(["insights", "--new", standin, "--fixture", fixture, "--scratch", str(root / "refused-stale"), "--real-clock", *fast,
                          "--product-arg=--screen=product", "--profile", product_profile])
    expect("NEG a real-clock run on a fixture built long ago is refused before a product starts", code == 3 and "min old" in text and not (root / "refused-stale").exists(), f"exit {code}: {text.strip()[-120:]}")
    try:
        check_clock_unchanged({"period_id": ["1999-W01", "1999-01"]}, True)
        ok_, why = False, "passed"
    except Fail as error:
        ok_, why = "crossed a week or month" in str(error), str(error)[:100]
    expect("NEG a real-clock trial that crossed a week or month boundary is refused", ok_, why)
    real_fx = root / "fixture-real"
    try:
        make_fixture(real_fx, REPO / "checks/fixtures/t3-schema.sql", 30, 12, 150, 3, 4, 2048, 0, 0)
        built = True
    except Fail as error:
        built = False
        print(f"SKIP  real-clock control: {error}", flush=True)
    if built:
        scratch = root / "p-real"
        code, text = run_cli(["insights", "--new", standin, "--fixture", str(real_fx), "--scratch", str(scratch), "--real-clock", "--rounds", "1", "--keys", "4",
                              "--idle-seconds", "0.3", "--esc-pairs", "1", "--exit-pairs", "1", "--during-keys", "1", "--during-attempts", "4", "--read-timeout", "6",
                              "--product-arg=--screen=product", "--product-arg=--read-ms=300", "--source-root", str(REPO), "--profile", product_profile])
        rep = json.loads((scratch / "results.json").read_text()) if (scratch / "results.json").exists() else {}
        expect("a real-clock run on a fresh dated fixture passes every step (period labels follow the real clock)",
               code == 0 and rep.get("evidence", {}).get("valid") and rep.get("clock", {}).get("mode") == "real" and rep["claim"]["display_clock"] == "real",
               f"exit {code}, clock {rep.get('clock')}")

    # -- the instrument itself: the fast virtual screen equals the byte-at-a-time reference
    eq = screen_equivalence([], 1500)
    expect("the fast virtual screen equals the byte-at-a-time reference on 1500 random and malformed sequences", eq["mismatches"] == 0 and eq["cases"] == 1500, f"{eq}")

    def live(name: str, extra: list[str], mode: str = "good") -> tuple[int, dict, str]:
        scratch = root / name
        code, text = run_cli(["live", "--old", standin, "--new", standin, "--fixture", fixture, "--scratch", str(scratch),
                              "--rounds", "2", "--events", "6", "--keys", "12", "--duration-ms", "5000", "--settle", "0.5",
                              "--event-timeout", "2.5", "--product-arg=--mode", f"--product-arg={mode}", *extra])
        return code, (json.loads((scratch / "results.json").read_text()) if (scratch / "results.json").exists() else {}), text

    code, rep, _ = live("aa", [])
    d = rep.get("summary", {}).get("key_ms", {}).get("new_minus_old", {}).get("median_diff_ms")
    m = rep.get("matched", {})
    expect("A/A control: matched, no difference", code == 0 and d is not None and abs(d) < 1.0 and m.get("script_sha256_equal")
           and m.get("final_state_sha256_equal_per_round") and any("A/A" in x for x in rep["claim"]["reasons"]),
           f"new minus old key median {d} ms, matched {m.get('script_sha256_equal')}/{m.get('initial_db_sha256_equal')}/{m.get('final_state_sha256_equal_per_round')}, order {m.get('order')}")
    code, rep, _ = live("delay", ["--inject-delay-ms", "12"])
    d = rep.get("summary", {}).get("key_ms", {}).get("new_minus_old", {}).get("median_diff_ms")
    expect("NEG injected 12 ms in the new arm is recovered", code == 0 and d is not None and 9 <= d <= 16 and any("negative control" in x for x in rep["claim"]["reasons"]),
           f"new minus old key median {d} ms (want 9..16)")
    code, rep, text = live("stale", [], mode="stale")
    expect("NEG board that never shows the events fails closed", code == 3 and "never reached a frame" in text, f"exit {code}")
    bad = [n for n, ok, _ in results if not ok]
    print(f"\n{len(results) - len(bad)}/{len(results)} controls behaved as expected" + (f"; FAILED: {bad}" if bad else ""))
    return 1 if bad else 0


# ---------------------------------------------------------------------------- command line

def add_product_args(cmd: argparse.ArgumentParser) -> None:
    cmd.add_argument("--fixture", required=True)
    cmd.add_argument("--scratch", required=True)
    cmd.add_argument("--product-arg", action="append", default=None,
                     help="repeatable, passed to the product (default: --calm --theme frontier)")
    cmd.add_argument("--real-clock", action="store_true", help="the product's own clock; default pins T3_FIXED_NOW to the fixture's now_ms (deterministic reports, no headroom refresh). "
                     "With it the fixture must be fresh (--max-fixture-age-min) and in this week and month")
    cmd.add_argument("--max-fixture-age-min", type=float, default=90.0, help="with --real-clock: the oldest fixture, in minutes since it was built, a run accepts")
    cmd.add_argument("--timeout", type=float, default=30)
    cmd.add_argument("--load-limit", type=float, default=2.0, help="1-minute load average above which numbers are not claimable")
    cmd.add_argument("--lock", help="hold an exclusive flock on this file for the run, so two timing runs that share it cannot overlap")
    cmd.add_argument("--inject-delay-ms", type=float, default=0.0, help="negative control: add this to every scored write of the new arm")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    fx = sub.add_parser("fixture", help="build the synthetic T3CODE_HOME")
    fx.add_argument("--root", required=True)
    fx.add_argument("--schema", default=str(REPO / "checks/fixtures/t3-schema.sql"))
    fx.add_argument("--board", type=int, default=60, help="board threads")
    fx.add_argument("--running", type=int, default=24, help="of them, running")
    fx.add_argument("--history-threads", type=int, default=None, help=f"default: {HISTORY_PER_DAY} a day of history")
    fx.add_argument("--history-days", type=int, default=None, help="default: back to the start of last month, so all four periods have data")
    fx.add_argument("--now-ms", type=int, default=None, help="the instant the data is built relative to (default: the real clock, now)")
    fx.add_argument("--runs-per-thread", type=int, default=12)
    fx.add_argument("--items-per-run", type=int, default=14)
    fx.add_argument("--payload-bytes", type=int, default=2048)
    fx.add_argument("--worktrees", type=int, default=0, help="Storage only; 0 builds none")
    fx.add_argument("--files", type=int, default=12000, help="regular entries per worktree")

    sn = sub.add_parser("snap", help="print the screen after each key")
    sn.add_argument("--binary", required=True)
    sn.add_argument("--fixture", help="a fixture this script built; the product gets --db on a copy of its database")
    sn.add_argument("--demo", action="store_true", help="instead of --fixture: the product's --demo, with an empty T3CODE_HOME")
    sn.add_argument("--scratch", required=True)
    sn.add_argument("--keys", nargs="*", default=[], help="key names; wait:SECONDS pauses")
    sn.add_argument("--product-arg", action="append", default=[], help="repeatable; only --calm and plain-word --theme, --interval and stand-in flags are accepted")
    sn.add_argument("--quiet", type=float, default=0.4)
    sn.add_argument("--timeout", type=float, default=20)
    sn.add_argument("--quit-key", default="q")
    sn.add_argument("--real-clock", action="store_true")
    sn.add_argument("--max-fixture-age-min", type=float, default=90.0)

    lv = sub.add_parser("live", help="old and new on matched live-board events, alternating trials")
    lv.add_argument("--old", required=True)
    lv.add_argument("--new", required=True)
    lv.add_argument("--old-commit", default="")
    lv.add_argument("--new-commit", default="")
    add_product_args(lv)
    lv.add_argument("--rounds", type=int, default=5)
    lv.add_argument("--events", type=int, default=24)
    lv.add_argument("--keys", type=int, default=40)
    lv.add_argument("--duration-ms", type=int, default=24000)
    lv.add_argument("--event-threads", type=int, default=8, help="events cycle over the first N board rows")
    lv.add_argument("--seed", type=int, default=7)
    lv.add_argument("--settle", type=float, default=1.0)
    lv.add_argument("--key-timeout", type=float, default=5.0)
    lv.add_argument("--event-timeout", type=float, default=8.0)
    lv.add_argument("--event-window-ms", type=float, default=400.0)
    lv.add_argument("--allow-missed-events", type=int, default=0)
    lv.add_argument("--board-ready", default=r"^▌.*Bench task 000")
    lv.add_argument("--esc-pairs", type=int, default=4, help="help-overlay closes by Escape and by ? per arm, after the script; 0 skips")
    lv.add_argument("--source-root", help="checkout the binaries were built from; ESC_WAIT is read from it (default 25 ms)")

    ins = sub.add_parser("insights", help="the new build alone: open, read, switch, exit (profile-driven)")
    ins.add_argument("--new", required=True)
    ins.add_argument("--commit", default="")
    ins.add_argument("--profile", default=str(HERE / "terminal_ux_profile.json"))
    ins.add_argument("--profile-kind", choices=["product", "stand-in"], default="product",
                     help="which program the profile is for; the selftest uses stand-in, a real run never should")
    add_product_args(ins)
    ins.add_argument("--rounds", type=int, default=5)
    ins.add_argument("--keys", type=int, default=10, help="Live board j/k presses before open and after return")
    ins.add_argument("--idle-seconds", type=float, default=3.0)
    ins.add_argument("--esc-pairs", type=int, default=4)
    ins.add_argument("--exit-pairs", type=int, default=4)
    ins.add_argument("--during-keys", type=int, default=3, help="accepted during-read keys required per profile key per round")
    ins.add_argument("--during-attempts", type=int, default=12)
    ins.add_argument("--min-read-bytes", type=int, default=65536, help="read(2) bytes the product must keep asking for after a frame or key for a read to count as in progress")
    ins.add_argument("--read-timeout", type=float, default=60.0)
    ins.add_argument("--source-root", help="checkout the binary was built from; ESC_WAIT is read from it (default 25 ms)")

    sg = sub.add_parser("storage", help="the new build alone: Storage ready and scanning (inherited)")
    sg.add_argument("--new", required=True)
    add_product_args(sg)
    sg.add_argument("--rounds", type=int, default=5)
    sg.add_argument("--keys", type=int, default=20)
    sg.add_argument("--accept-discovery-outside-db-rule", action="store_true",
                    help="required: Storage discovers worktrees through T3CODE_HOME, so this scenario gives the product a synthetic "
                         "fixture copy as T3CODE_HOME and no --db")

    dc = sub.add_parser("dates-check", help="the harness's period labels against the product at pinned clocks")
    dc.add_argument("--binary", required=True)
    dc.add_argument("--fixture", required=True)
    dc.add_argument("--scratch", required=True)
    dc.add_argument("--product-arg", action="append", default=None)
    dc.add_argument("--at", action="append", help="a UTC clock as YYYY-MM-DDTHH:MM:SS (repeatable; default: seven that cover month, year and leap edges)")
    dc.add_argument("--timeout", type=float, default=30)

    fl = sub.add_parser("floor", help="the measuring path's own latency, on the stand-in")
    fl.add_argument("--fixture", required=True)
    fl.add_argument("--scratch", required=True)
    fl.add_argument("--standin", default=str(HERE / "terminal_ux_standin.py"))
    fl.add_argument("--rounds", type=int, default=6)
    fl.add_argument("--keys", type=int, default=100)
    fl.add_argument("--timeout", type=float, default=20)

    st = sub.add_parser("selftest", help="the harness's own negative controls, on the stand-in")
    st.add_argument("--scratch", required=True, help="a new empty folder on a real disk, not /tmp")
    st.add_argument("--standin", default=str(HERE / "terminal_ux_standin.py"))

    args = parser.parse_args()
    if args.command in ("live", "insights", "storage", "dates-check") and args.product_arg is None:
        args.product_arg = ["--calm", "--theme", "frontier"]
    if args.command == "fixture":
        print(json.dumps(make_fixture(Path(args.root), Path(args.schema), args.board, args.running, args.history_threads,
                                      args.runs_per_thread, args.items_per_run, args.payload_bytes, args.worktrees, args.files,
                                      args.now_ms, args.history_days), indent=2))
    else:
        if getattr(args, "rounds", 1) < 1:
            parser.error("rounds must be positive")
        if getattr(args, "lock", None):
            lock = os.fdopen(os.open(check_scratch(Path(args.lock)), os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW, 0o600), "a")
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                print(f"FAIL (fail closed): another timing run holds {args.lock}", file=sys.stderr)
                sys.exit(3)
        try:
            if getattr(args, "product_arg", None) is not None:
                args.product_arg = check_product_args(args.product_arg)
            sys.exit({"live": command_live, "insights": command_insights, "floor": command_floor, "selftest": command_selftest,
                      "storage": command_storage, "dates-check": command_dates_check, "snap": lambda a: command_snap(a) or 0}[args.command](args))
        except (Fail, ValueError) as error:
            print(f"FAIL (fail closed): {error}", file=sys.stderr)
            sys.exit(3)


if __name__ == "__main__":
    main()
