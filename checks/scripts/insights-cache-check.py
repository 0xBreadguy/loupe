#!/usr/bin/env python3
"""Check the built Recap against its live reader and inspect its disposable cache."""

import argparse
from contextlib import ExitStack, closing
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import random
import sqlite3
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
P = "orchestration_v2_projection_"
SEED = 961704
CANARIES = {
    field: f"recap-private-{field}-a387cd4e"
    for field in ("title", "prompt", "command", "output", "path", "reasoning", "failure", "answer")
}
LAST_MONTH = ("--month",)
THIS_MONTH = ("--this-month",)
THIS_WEEK = ("--this-week",)
WEEK = ()
PERIODS = (WEEK, THIS_WEEK, LAST_MONTH, THIS_MONTH)
RUN_COUNTS = {WEEK: 4, THIS_WEEK: 2, LAST_MONTH: 2, THIS_MONTH: 6}
FORMS = (("--json",), (), ("--compact",), ("--wide",), ("--narrow",))
NOW = "1795069560000"  # 2026-11-19 06:26 UTC
FACT_KEY = "source, kind, version, run"
PAGE_SIZE = 4096
WAL_HEADER_BYTES = 32
WAL_FRAME_HEADER_BYTES = 24


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def equal(actual, expected, label):
    if actual == expected:
        return
    for kind, left, right in zip(("exit code", "stdout", "stderr"), actual, expected):
        if left != right:
            raise AssertionError(f"{label}: {kind} differs\n  cached: {left!r}\n  live: {right!r}")


def stamp(day, minute=0):
    at = datetime.fromisoformat(day).replace(hour=12, tzinfo=timezone.utc)
    return (at + timedelta(minutes=minute)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def put(db, table, **fields):
    names = ", ".join(fields)
    marks = ", ".join("?" for _ in fields)
    db.execute(f"INSERT INTO {P}{table} ({names}) VALUES ({marks})", tuple(fields.values()))


def source(path, version="main-096", additions=19, open_call=False):
    schema = ROOT / "checks" / "fixtures" / f"t3-schema-{version}.sql"
    with closing(sqlite3.connect(path)) as db:
        db.execute(f"PRAGMA page_size={PAGE_SIZE}")
        db.executescript(schema.read_text())
        for i, (run, day) in enumerate((
            ("oct", "2026-10-14"),
            ("delete", "2026-11-11"),
            ("reopen", "2026-11-11"),
            ("archive", "2026-11-12"),
            ("keep", "2026-11-12"),
            ("current", "2026-11-18"),
            ("oct-second", "2026-10-19"),
            ("current-second", "2026-11-18"),
        )):
            started, middle, ended = (stamp(day, i * 30 + m) for m in (0, 10, 20))
            thread, node, provider_thread = f"thread-{run}", f"node-{run}", f"provider-{run}"
            put(
                db, "threads", thread_id=thread, project_id=f"project-{i % 3}",
                title=CANARIES["title"], default_provider="claude", runtime_mode="standard",
                interaction_mode="default", created_at=stamp("2026-10-01"), updated_at=ended,
                payload_json=json.dumps({"title": CANARIES["title"], "projectId": f"project-{i % 3}",
                                         "createdAt": stamp("2026-10-01")}),
            )
            put(
                db, "runs", run_id=run, thread_id=thread, ordinal=1, provider="claude",
                provider_thread_id=provider_thread, status="completed", requested_at=started,
                completed_at=ended, payload_json=json.dumps({
                    "startedAt": started, "completedAt": ended, "status": "completed",
                    "providerInstanceId": "claudeAgent", "modelSelection": {"model": "claude-sonnet-5-5"},
                    "prompt": CANARIES["prompt"],
                }),
            )
            put(
                db, "nodes", node_id=node, root_node_id=node, run_id=run, thread_id=thread,
                kind="root", status="completed", started_at=started, completed_at=ended,
                payload_json="{}",
            )
            put(
                db, "provider_threads", provider_thread_id=provider_thread, thread_id=thread,
                owner_node_id=node, provider="claude", status="idle", updated_at=ended,
                payload_json="{}",
            )
            put(
                db, "provider_turns", provider_turn_id=f"turn-{run}", thread_id=thread,
                provider_thread_id=provider_thread, node_id=node, ordinal=1, status="completed",
                started_at=started, completed_at=ended, payload_json=json.dumps({
                    "turnTokenUsage": {"usageStatus": "complete", "inputTokens": 2500 + i,
                                       "outputTokens": 420, "cachedInputTokens": 110},
                    "tokenUsage": {"usedTokens": 9900, "maxTokens": 10000},
                    "reasoning": CANARIES["reasoning"],
                }),
            )
            put(
                db, "runtime_requests", runtime_request_id=f"ask-{run}", thread_id=thread,
                node_id=node, kind="user_input", status="resolved", created_at=started,
                resolved_at=middle, payload_json=json.dumps({
                    "responseCapability": {"type": "message"}, "answer": CANARIES["answer"],
                }),
            )
            running = open_call and run == "keep"
            items = (
                ("user_message", "completed", {"createdBy": "user", "creationSource": "web",
                                              "text": CANARIES["prompt"]}),
                ("command_execution", "running" if running else "completed", {
                    "startedAt": started, "command": CANARIES["command"], "output": CANARIES["output"],
                    **({} if running else {"completedAt": ended}),
                }),
                ("checkpoint", "completed", {"files": [{"kind": "modified", "path": CANARIES["path"],
                                                        "additions": additions + i, "deletions": i}]}),
                ("error", "completed", {"failure": {"class": "provider_error", "message": CANARIES["failure"]}}),
            )
            for ordinal, (kind, status, payload) in enumerate(items, 1):
                put(
                    db, "turn_items", turn_item_id=f"{run}-{kind}", thread_id=thread, run_id=run,
                    node_id=node, provider_thread_id=provider_thread, ordinal=ordinal,
                    type=kind, status=status, updated_at=middle if kind == "command_execution" else ended,
                    payload_json=json.dumps(payload),
                )
        db.commit()
    return path


def settle_source():
    # The source stamp intentionally bypasses caching during a filesystem clock tick
    # that might still include a write. Start a fresh read after that tick.
    time.sleep(0.025)


def facts(db):
    return db.execute(f"SELECT {FACT_KEY}, payload, integrity FROM facts ORDER BY {FACT_KEY}").fetchall()


def cache_sources(db):
    return db.execute("SELECT source, generation FROM sources ORDER BY source").fetchall()


def remove_alternate_facts(db, full):
    with db:
        db.executemany(
            "DELETE FROM facts WHERE (source, kind, version, run) = (?, ?, ?, ?)",
            [row[:4] for row in full[::2]],
        )


class Scenario:
    def __init__(self, program, root):
        self.program = program
        home = root / "home"
        self.t3 = root / "empty-t3-home"
        self.t3.mkdir(parents=True)
        home.mkdir()
        self.cache_home = root / "cache"
        self.cache = self.cache_home / "t3-agents" / "insights.sqlite3"
        self.env = {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_DATA_HOME": str(home / "data"),
            "XDG_STATE_HOME": str(home / "state"),
            "XDG_CACHE_HOME": str(self.cache_home),
            "T3CODE_HOME": str(self.t3),
            "PATH": "/usr/bin:/bin",
            "TZ": "UTC",
            "TERM": "xterm-256color",
            "NO_COLOR": "1",
        }

    def command(self, path, args, off=False, color=False, cache_stats=False, opt_in=True):
        env = dict(self.env)
        if opt_in:
            env["T3_AGENTS_CACHE"] = "1"
        if off:
            env["T3_AGENTS_NO_CACHE"] = "1"
        if color:
            del env["NO_COLOR"]
            env["FORCE_COLOR"] = "1"
        if cache_stats:
            env["T3_AGENTS_CACHE_STATS"] = "1"
        database = ["--db", str(path)] if path else ["--demo"]
        return [str(self.program), "--recap", *database, "--now-ms", NOW, *args], env

    def recap(self, path, args=("--json",), **flags):
        cmd, env = self.command(path, args, **flags)
        r = subprocess.run(cmd, env=env, capture_output=True, timeout=30)
        check(r.returncode == 0 and r.stdout, f"{cmd}: exit {r.returncode}, stderr {r.stderr!r}")
        return r.returncode, r.stdout, r.stderr

    def compare(self, path, args=("--json",), **flags):
        live = self.recap(path, args, off=True, **flags)
        equal(self.recap(path, args, **flags), live, f"{path.name} {' '.join(args)}")
        return live

    def no_cache(self):
        check(not self.cache_home.exists(), "a disabled or demo Recap created cache state")
        check(not any(self.t3.iterdir()), "Recap wrote to the empty T3 home")

    def connect(self):
        check(self.cache.is_file(), f"Recap created no cache database at {self.cache}")
        return closing(sqlite3.connect(self.cache))

    def privacy(self):
        with self.connect() as db:
            values = "\n".join(db.iterdump()).encode()
        for path in self.cache_home.rglob("*"):
            if path.is_file():
                values += path.read_bytes()
        for field, canary in CANARIES.items():
            check(canary.encode() not in values, f"cache persisted {field} canary")


def check_demo(s):
    for period in PERIODS:
        args = (*period, "--json")
        equal(s.recap(None, args), s.recap(None, args, off=True), f"demo {period}")
    s.no_cache()


def check_opt_in(s, path):
    live = s.recap(path, off=True, opt_in=False)
    equal(s.recap(path, opt_in=False), live, "Recap without cache opt-in")
    s.no_cache()


def check_format(s, path, period, form, color):
    args = (*period, *form)
    label = f"{args} color={color}"
    live = s.recap(path, args, off=True, color=color)
    s.no_cache()
    cold = s.recap(path, args, color=color)
    equal(cold, live, f"cold {label}")
    check((b"\x1b[" in cold[1]) == color, f"wrong color mode for {label}")
    with s.connect() as writer, s.connect() as observer:
        full = facts(writer)
        check(len(full) == RUN_COUNTS[period], f"cold {label} did not store one fact per settled run")
        version = observer.execute("PRAGMA data_version").fetchone()
        equal(s.recap(path, args, color=color), live, f"warm {label}")
        check(observer.execute("PRAGMA data_version").fetchone() == version, f"warm {label} rewrote the cache")
        check(facts(observer) == full, f"warm {label} changed the cache facts")

        remove_alternate_facts(writer, full)
        check(facts(observer) == full[1::2], "half-fill fixture removed the wrong facts")
        equal(s.recap(path, args, off=True, color=color), live, f"bypass half-filled {label}")
        check(facts(observer) == full[1::2], f"bypassed {label} refilled the cache")
        equal(s.recap(path, args, color=color), live, f"half-filled {label}")
        check(facts(observer) == full, f"half-filled {label} did not refill")


def check_formats(program, scratch, path):
    # Start with a fresh cache directory for each of the display paths.
    for i, period in enumerate(PERIODS):
        for j, form in enumerate(FORMS):
            colors = (False,) if form == ("--json",) else (False, True)
            for color in colors:
                scenario = Scenario(program, scratch / f"{i}-{j}-{int(color)}")
                check_format(scenario, path, period, form, color)


def expect_stats(s, path, live, label, *, hits=None, writes=None, off=False):
    status, stdout, stderr = live
    finished = RUN_COUNTS[WEEK]
    message = "not used" if off else f"{hits} of {finished} finished runs from the cache, {writes} written"
    diagnostic = f"t3-agents: cache: {message}\n".encode()
    equal(
        s.recap(path, off=off, cache_stats=True),
        (status, stdout, stderr + diagnostic),
        f"{path.name} {label} diagnostics",
    )


def check_diagnostics(s, path):
    live = s.recap(path, off=True)
    finished = RUN_COUNTS[WEEK]
    check(json.loads(live[1])["runs"] == finished, "diagnostic fixture has the wrong run count")

    expect_stats(s, path, live, "disabled", off=True)
    s.no_cache()
    expect_stats(s, path, live, "empty", hits=0, writes=finished)
    with s.connect() as db:
        full = facts(db)
        check(len(full) == finished, "diagnostic cold writes did not commit")
        expect_stats(s, path, live, "full", hits=finished, writes=0)
        check(facts(db) == full, "diagnostic warm hits changed the stored facts")

        remove_alternate_facts(db, full)
        partial = facts(db)
        check(partial == full[1::2], "diagnostic half-fill removed the wrong facts")
        expect_stats(s, path, live, "disabled with a half-filled cache", off=True)
        check(facts(db) == partial, "disabled diagnostic invocation wrote missing facts")
        expect_stats(s, path, live, "half-filled", hits=len(partial), writes=finished - len(partial))
        check(facts(db) == full, "diagnostic half-fill writes did not commit")


def check_corrupt_fact(s, path):
    live = s.compare(path)
    # Changing a fact without updating its integrity must never change Recap output.
    with s.connect() as db:
        full = facts(db)
        with db:
            db.execute(
                "UPDATE facts SET payload=? WHERE (source, kind, version, run) = (?, ?, ?, ?)",
                (b"broken cached fact", *full[0][:4]),
            )
        equal(s.recap(path), live, "a corrupted cached fact")
        check(facts(db) == full, "Recap did not restore the corrupted fact")


def committed_wal(wal, index):
    # SQLite's WAL-index header has two copies. mxFrame in the header bounds the
    # committed prefix even when the file contains additional uncommitted frames.
    index.seek(0)
    header = index.read(96)
    check(len(header) == 96 and header[:48] == header[48:], "fixture WAL index is unstable")
    count = int.from_bytes(header[16:20], sys.byteorder)
    wal.seek(0)
    raw = wal.read()
    page_size = int.from_bytes(raw[8:12], "big")
    frame_size = page_size + WAL_FRAME_HEADER_BYTES
    end = WAL_HEADER_BYTES + frame_size * count
    check(page_size == PAGE_SIZE and count > 0 and end <= len(raw), "fixture WAL has no committed frames")
    check(int.from_bytes(raw[end - frame_size + 4:end - frame_size + 8], "big") > 0,
          "fixture mxFrame is not a commit boundary")
    pages = [int.from_bytes(raw[offset:offset + 4], "big")
             for offset in range(WAL_HEADER_BYTES, end, frame_size)]
    return raw[:end], pages


def append_unrelated_event(db, label):
    db.execute(
        "INSERT INTO orchestration_v2_events(event_id,thread_id,event_type,occurred_at,payload_json) "
        "VALUES(?,'thread-current','thread.meta-updated',?,'{}')",
        (f"selective-wal-{label}", stamp("2026-11-18")),
    )


def check_selective_wal(s, path):
    table = P + "turn_items"
    target = "keep-checkpoint"
    padding = {"before": "a" * 10240, "after": "b" * 10240}

    def payload(additions):
        return json.dumps(
            {"before": padding["before"], "files": [{"additions": additions, "deletions": 4}],
             "after": padding["after"]},
            separators=(",", ":"),
        )

    def write_checkpoint(db, value):
        cursor = db.execute(f"UPDATE {table} SET payload_json=? WHERE turn_item_id=?", (payload(value), target))
        check(cursor.rowcount == 1, "selective WAL checkpoint row is missing")

    def open_wal(db):
        check(db.execute("PRAGMA journal_mode=WAL").fetchone() == ("wal",), "selective fixture has no WAL")
        db.execute("PRAGMA wal_autocheckpoint=0")

    first, late, reset = 123, 987, 456
    check(len(payload(first)) == len(payload(late)) == len(payload(reset)) > 16384,
          "selective fixture needs an equal-length overflow edit")
    with closing(sqlite3.connect(path)) as db:
        with db:
            write_checkpoint(db, first)
    restore_image = path.read_bytes()
    # Keep the inspection descriptors open through the SQLite connection's
    # lifetime. Closing another descriptor for the same inode drops POSIX locks.
    with ExitStack() as files, closing(sqlite3.connect(path)) as db:
        pages = dict(db.execute("SELECT pageno, pagetype FROM dbstat WHERE name=?", (table,)))
        event_root, = db.execute("SELECT rootpage FROM sqlite_schema WHERE name='orchestration_v2_events'").fetchone()
        check(list(pages.values()).count("overflow") > 1, "selective fixture has no overflow chain")
        open_wal(db)
        with db:
            append_unrelated_event(db, "seed")
        main_file = files.enter_context(path.open("rb", buffering=0))
        wal = files.enter_context(Path(f"{path}-wal").open("rb", buffering=0))
        index = files.enter_context(Path(f"{path}-shm").open("rb", buffering=0))
        main = main_file.read()
        started, seed_frames = committed_wal(wal, index)
        settle_source()
        live = s.recap(path, off=True)
        finished = RUN_COUNTS[WEEK]
        expect_stats(s, path, live, "nonempty WAL cold", hits=0, writes=finished)
        with s.connect() as cache:
            check(cache.execute("SELECT count(*) FROM facts WHERE length(witness)>16").fetchone() == (1,),
                  "cold cache did not witness exactly one run's overflow")
            check(cache.execute("SELECT wal_stamp IS NOT NULL FROM sources").fetchone() == (1,),
                  "cold cache did not record the nonempty WAL")

        with db:
            db.execute(f"UPDATE {P}threads SET title='an unrelated title' WHERE thread_id='thread-current'")
        unrelated, unrelated_frames = committed_wal(wal, index)
        check(unrelated.startswith(started) and len(unrelated_frames) > len(seed_frames),
              "unrelated commit changed WAL lineage")
        check(not set(unrelated_frames[len(seed_frames):]) & pages.keys(),
              "unrelated commit touched the turn-items table")
        settle_source()
        equal(s.recap(path, off=True), live, "unrelated thread update changed the live report")
        expect_stats(s, path, live, "unrelated WAL commit", hits=finished, writes=0)

        summary = (
            f"SELECT turn_item_id,status,updated_at,length(payload_json) FROM {table} "
            f"INDEXED BY {table}_run_ordinal_idx WHERE run_id='keep' ORDER BY ordinal"
        )
        prior = db.execute(summary).fetchall()
        db.execute("BEGIN IMMEDIATE")
        write_checkpoint(db, late)
        append_unrelated_event(db, "same-transaction")
        equal(s.compare(path), live, "Recap while the WAL writer has uncommitted changes")
        db.commit()
        change, changed_frames = committed_wal(wal, index)
        check(change.startswith(unrelated) and len(changed_frames) > len(unrelated_frames),
              "late payload edit changed WAL lineage")
        appended = changed_frames[len(unrelated_frames):]
        item_pages = {pg for pg in appended if pg in pages}
        check(len(item_pages) == 1 and all(pages[pg] == "overflow" for pg in item_pages),
              f"late edit must dirty only an overflow page in the turn-items table, got {item_pages}")
        page = item_pages.pop()
        frame = changed_frames.index(page, len(unrelated_frames))
        record = WAL_HEADER_BYTES + frame * (PAGE_SIZE + WAL_FRAME_HEADER_BYTES) + WAL_FRAME_HEADER_BYTES
        next_page = int.from_bytes(change[record:record + 4], "big")
        check(pages.get(next_page) == "overflow"
              and f'"additions":{late}'.encode() in change[record:record + PAGE_SIZE],
              "late edit did not change the known value inside the overflow chain")
        check(event_root in appended, "same transaction had no unrelated event frames")
        check(db.execute(summary).fetchall() == prior, "late edit changed the item count, status, timestamp or length")
        main_file.seek(0)
        check(main_file.read() == main, "selective WAL commits changed the main database")
        settle_source()
        changed = s.recap(path, off=True)
        check(json.loads(changed[1])["lines_added"] - json.loads(live[1])["lines_added"] == late - first,
              "interior overflow edit did not change the live line count as expected")
        expect_stats(s, path, changed, "overflow edit with an unrelated event", hits=finished - 1, writes=1)

        check(db.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone() == (0, 0, 0), "fixture WAL reset failed")
        with db:
            write_checkpoint(db, reset)
            append_unrelated_event(db, "reset")
        restarted, _ = committed_wal(wal, index)
        check(restarted[16:24] != change[16:24], "WAL reset reused its previous salts")
        settle_source()
        after_reset = s.recap(path, off=True)
        check(json.loads(after_reset[1])["lines_added"] - json.loads(changed[1])["lines_added"] == reset - late,
              "WAL reset edit did not change the live line count as expected")
        expect_stats(s, path, after_reset, "new WAL after reset", hits=0, writes=finished)

    # Reopen the original source image at the same path with the prior mtime.
    prior_time = path.stat()
    replacement = path.with_suffix(".restore.sqlite")
    replacement.write_bytes(restore_image)
    os.utime(replacement, ns=(prior_time.st_atime_ns, prior_time.st_mtime_ns))
    replacement.replace(path)
    with ExitStack() as files, closing(sqlite3.connect(path)) as db:
        open_wal(db)
        with db:
            append_unrelated_event(db, "restored")
        wal = files.enter_context(Path(f"{path}-wal").open("rb", buffering=0))
        index = files.enter_context(Path(f"{path}-shm").open("rb", buffering=0))
        committed_wal(wal, index)
        settle_source()
        equal(s.recap(path, off=True), live, "restored source did not restore the live report")
        expect_stats(s, path, live, "WAL after source restore", hits=0, writes=finished)


def run_mutations(s, path):
    with path.open("rb", buffering=0) as main_file, closing(sqlite3.connect(path)) as db:
        check(db.execute("PRAGMA journal_mode=WAL").fetchone() == ("wal",), "fixture has no WAL")
        db.execute("PRAGMA wal_autocheckpoint=0")
        db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        settle_source()
        source_before = main_file.read()
        mtime_before = path.stat().st_mtime_ns
        prior = s.compare(path)
        with s.connect() as cache:
            old_generation = cache_sources(cache)
            check(facts(cache), "the first WAL snapshot populated no run facts")
        day = "2026-11-12"
        table = P + "turn_items"
        summary = f"SELECT count(*), max(updated_at) FROM {table} WHERE run_id='keep'"
        count_max = db.execute(summary).fetchone()
        db.execute(
            f"UPDATE {table} SET status='completed', payload_json=json_set(payload_json, '$.completedAt', ?) "
            "WHERE turn_item_id='keep-command_execution'",
            (stamp(day, 4 * 30 + 15),),
        )
        equal(s.compare(path), prior, "late completion before the WAL commit")
        db.commit()
        check(db.execute(summary).fetchone() == count_max, "late completion changed count or max timestamp")
        main_file.seek(0)
        check(main_file.read() == source_before, "late completion escaped the WAL")
        check(path.stat().st_mtime_ns == mtime_before, "late completion changed the main database mtime")
        settle_source()
        after = s.compare(path)
        check(json.loads(after[1])["split"]["tools"] > json.loads(prior[1])["split"]["tools"],
              "synthetic late completion did not increase tool time")
        with s.connect() as cache:
            check(cache_sources(cache) != old_generation, "the committed WAL retained its old cache generation")
            check(len(facts(cache)) == RUN_COUNTS[WEEK], "the committed WAL did not repopulate settled runs")
            version = cache.execute("PRAGMA data_version").fetchone()
            equal(s.recap(path), after, "warm Recap on the committed WAL")
            check(cache.execute("PRAGMA data_version").fetchone() == version, "the warm WAL read rewrote its cache")

        writes = [
            (f"UPDATE {table} SET status='failed' WHERE turn_item_id='reopen-command_execution'", ()),
            (f"UPDATE {table} SET payload_json=json_set(payload_json, '$.files[0].additions', 777) "
             "WHERE turn_item_id='archive-checkpoint'", ()),
            (f"UPDATE {table} SET type='compaction' WHERE turn_item_id='delete-error'", ()),
            (f"DELETE FROM {table} WHERE turn_item_id='delete-checkpoint'", ()),
            (f"UPDATE {P}provider_turns SET payload_json=json_set(payload_json, '$.turnTokenUsage.outputTokens', 9100) "
             "WHERE provider_turn_id='turn-keep'", ()),
            (f"UPDATE {P}runtime_requests SET payload_json=json_set(payload_json, '$.responseCapability.type', 'form') "
             "WHERE runtime_request_id='ask-reopen'", ()),
            (f"UPDATE {P}runs SET payload_json=json_set(payload_json, '$.modelSelection.model', 'claude-haiku') "
             "WHERE run_id='keep'", ()),
        ]
        random.Random(SEED).shuffle(writes)
        for sql, values in writes:
            with db:
                check(db.execute(sql, values).rowcount == 1, f"fixture mutation matched no row: {sql}")
            settle_source()
            updated = s.compare(path)
            check(after != updated, f"fixture mutation did not change the live output: {sql}")
            after = updated
        # Resolving a request later can affect a cached run's split without touching
        # the run or any of its items.
        with db:
            db.execute(f"UPDATE {P}runtime_requests SET resolved_at=? WHERE runtime_request_id='ask-reopen'",
                       (stamp("2026-11-11", 2 * 30 + 30),))
        settle_source()
        resolved = s.compare(path)
        check(json.loads(resolved[1])["split"]["waiting"] > json.loads(after[1])["split"]["waiting"],
              "synthetic request resolution did not increase waiting")
        main_file.seek(0)
        check(main_file.read() == source_before, "mutation run wrote the source database")


def check_pruning(s, path):
    october = s.compare(path, (*LAST_MONTH, "--json"))
    with s.connect() as cache:
        oct_facts = facts(cache)
        check(oct_facts, "October populated no run facts")
        generation = cache_sources(cache)
        s.compare(path)
        november = set(facts(cache)) - set(oct_facts)
        check(len(november) == RUN_COUNTS[WEEK], "the cache did not retain facts from both spans")
        deleted, archived = stamp("2026-11-18"), stamp("2026-11-17")
        with closing(sqlite3.connect(path)) as db, db:
            db.execute(f"UPDATE {P}threads SET deleted_at=? WHERE thread_id='thread-delete'", (deleted,))
            db.execute(f"UPDATE {P}threads SET archived_at=? WHERE thread_id='thread-archive'", (archived,))
            db.execute(f"UPDATE {P}runs SET completed_at=NULL, status='running', "
                       "payload_json=json_set(json_remove(payload_json, '$.completedAt'), '$.status', 'running') "
                       "WHERE run_id='reopen'")
        settle_source()
        # Those runs start in November and cannot be discovered by scanning October runs.
        equal(s.compare(path, (*LAST_MONTH, "--json")), october, "November lifecycle writes changed October")
        check(cache_sources(cache) != generation, "the cache did not change generation after lifecycle writes")
        check(facts(cache) == oct_facts, "November run facts survived an October Recap after lifecycle writes")
        november_report = json.loads(s.compare(path)[1])
        check(november_report["runs"] == RUN_COUNTS[WEEK], "cache eligibility changed the live run count")
        check(len(set(facts(cache)) - set(oct_facts)) == 1, "deleted, archived or reopened run was cached again")


def check_concurrent(s, path):
    periods = (WEEK, LAST_MONTH)
    calls = [(*period, "--json") for period in periods]
    live = [s.recap(path, args, off=True) for args in calls]
    s.no_cache()
    with ExitStack() as stack:
        children = []
        for args in calls:
            cmd, env = s.command(path, args)
            children.append(stack.enter_context(
                subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            ))
        try:
            for child, output in zip(children, live):
                stdout, stderr = child.communicate(timeout=30)
                equal((child.returncode, stdout, stderr), output, "concurrent cache creation")
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
    with s.connect() as db:
        check(db.execute("PRAGMA quick_check").fetchone() == ("ok",), "concurrent writers damaged cache")
        concurrent = set(facts(db))
        check(concurrent, "neither concurrent process wrote the cache")
        # A busy cache can send one writer to the live path. A serial pass should
        # refill any misses and retain whichever facts the first two calls stored.
        for args, output in zip(calls, live):
            equal(s.recap(path, args), output, "recap after concurrent writers")
        serial = set(facts(db))
        check(concurrent <= serial and len(serial) == sum(RUN_COUNTS[p] for p in periods),
              "retry after concurrent cache writes lost or failed to fill facts")


def check_unusable(s, future, path):
    live = s.recap(path, off=True)
    equal(s.recap(path), live, "seed the corrupt-cache scenario")
    s.cache.write_bytes(b"this is a synthetic corrupt cache")
    equal(s.recap(path), live, "corrupt cache")
    equal(s.recap(path), live, "second run after corrupt cache")
    equal(future.recap(path), live, "seed the future-cache scenario")
    with future.connect() as db:
        db.execute("PRAGMA user_version=2147483647")
        db.commit()
    original = future.cache.read_bytes()
    equal(future.recap(path), live, "future cache")
    check(future.cache.read_bytes() == original, "Recap rewrote a cache from a future version")


def check_switch(s, initial, changed, replacement):
    before = s.compare(initial)
    with s.connect() as cache:
        original = cache_sources(cache)
        check(len(original) == 1 and facts(cache), "the first source populated no run facts")
        other = s.compare(changed)
        check(before != other, "second synthetic source has identical output")
        switched = cache_sources(cache)
        check(len(switched) == 1 and switched[0][0] != original[0][0], "the cache kept the original source")
        equal(s.compare(initial), before, "switch back to the first source")
        check(cache_sources(cache) == original, "source switch did not restore the original generation")
        # Keep the pathname unchanged and imitate the old mtime across the atomic replacement.
        age = initial.stat()
        os.utime(replacement, ns=(age.st_atime_ns, age.st_mtime_ns))
        replacement.replace(initial)
        settle_source()
        replaced = s.compare(initial)
        check(replaced != before and replaced != other, "replacement source has identical output")
        fresh = cache_sources(cache)
        check(len(fresh) == 1 and fresh != original, "the cache kept the replaced source generation")


def main(program, scratch):
    fixtures = scratch / "fixtures"
    fixtures.mkdir()
    main_db = source(fixtures / "main.sqlite")
    selective = source(fixtures / "selective.sqlite")
    mutations = source(fixtures / "mutations.sqlite", open_call=True)
    pruning = source(fixtures / "prune.sqlite")
    older = source(fixtures / "older.sqlite", version="older-074")
    changed = source(fixtures / "changed.sqlite", additions=37)
    replacement = source(fixtures / "replacement.sqlite", additions=61)
    settle_source()
    check_opt_in(Scenario(program, scratch / "no-opt-in"), main_db)
    print("pass  Recap without an opt-in creates no cache", flush=True)
    check_demo(Scenario(program, scratch / "demo"))
    print("pass  demo creates no cache", flush=True)
    check_formats(program, scratch / "forms", main_db)
    print("pass  all output forms match live bytes cold, warm, bypassed and half-filled", flush=True)
    check_corrupt_fact(Scenario(program, scratch / "corrupt-fact"), main_db)
    print("pass  corrupt facts are rebuilt", flush=True)
    lifecycle = Scenario(program, scratch / "lifecycle")
    run_mutations(lifecycle, mutations)
    lifecycle.privacy()
    print(f"pass  WAL-only source mutations (seed {SEED}), late completion, and privacy canaries", flush=True)
    check_selective_wal(Scenario(program, scratch / "selective-wal"), selective)
    print("pass  selective WAL retains unrelated runs, refreshes an overflow edit, and misses after reset or restore",
          flush=True)
    check_pruning(Scenario(program, scratch / "pruning"), pruning)
    print("pass  deleted, reopened and archived runs prune outside their span", flush=True)
    check_concurrent(Scenario(program, scratch / "concurrent"), main_db)
    check_unusable(Scenario(program, scratch / "corrupt"), Scenario(program, scratch / "future"), main_db)
    print("pass  concurrent, corrupt and future cache files", flush=True)
    legacy = Scenario(program, scratch / "old-schema")
    check(json.loads(legacy.compare(older)[1])["runs"] == RUN_COUNTS[WEEK], "older fixture has no work")
    print("pass  older installed projection schema", flush=True)
    for path in (main_db, older):
        check_diagnostics(Scenario(program, scratch / f"diagnostics-{path.stem}"), path)
    print("pass  decoded cache hits and committed writes on both installed schemas", flush=True)
    check_switch(Scenario(program, scratch / "switch"), main_db, changed, replacement)
    print("pass  switching and replacing sources", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("program", nargs="?", type=Path, default=ROOT / "target/release/t3-agents-term")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="t3-insights-cache-") as scratch:
        main(args.program.resolve(), Path(scratch))
