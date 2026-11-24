#!/usr/bin/env python3
"""Check the merged Recap and Tools readers against one disposable Insights cache.

The report-specific checks exercise each reader separately. This check covers
what happens when both reports use the same cache:

  * one cache file holds both readers' facts and both get real hits from it;
  * each reader opts in on its own (`LOUPE_CACHE=1`), and one reader without
    the opt-in leaves the other's cache exactly as it found it;
  * `LOUPE_NO_CACHE=1` overrides an opt-in for both readers and creates
    nothing;
  * a changed run and a deleted run never come back from the cache;
  * cached output is byte-for-byte the direct output for every period and form.

The database is built here from the checked-in schema, and every run gets a new
empty `T3CODE_HOME` and its own `XDG_CACHE_HOME`. No real T3 database, cache or
install is touched. Standard library only.

The Linux CI suite runs this check. To run it separately:

    cargo build --release --locked
    python3 checks/scripts/recap-tools-cache-check.py

Pass `--program PATH` (or set `LOUPE_BINARY`) for a build elsewhere, and
`--only NAME` to run one section while investigating a failure. The scratch
folder is made under `checks/generated/` and removed when the run ends unless
`--keep` is given. Linux only: the cache fails closed where the filesystem
clock cannot be checked.
"""

import argparse
from contextlib import closing
import ctypes
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
P = "orchestration_v2_projection_"
SCHEMA = ROOT / "checks" / "fixtures" / "t3-schema-main-096.sql"
NOW = "1795069560000"  # 2026-11-19 06:26 UTC: last week is Nov 9-15

# A period's arguments and the finished runs each reader should see in it. The
# fixture below has four finished runs in the week, two this week, two in the
# last calendar month and six this month.
WEEK = ()
THIS_WEEK = ("--this-week",)
MONTH = ("--month",)
THIS_MONTH = ("--this-month",)
FINISHED = {WEEK: 4, THIS_WEEK: 2, MONTH: 2, THIS_MONTH: 6}

# Every form either reader can draw into a pipe. `--narrow` is Recap's only.
FORMS = (("--json",), (), ("--wide",), ("--compact",))
RECAP_FORMS = FORMS + (("--narrow",),)
TOOLS_FORMS = FORMS

STATS = re.compile(rb"^loupe: cache: (\d+) of (\d+) finished runs from the cache, (\d+) written$", re.M)
CACHE_LINE = re.compile(rb"^loupe: cache:.*\n", re.M)
CANARY = "recap-tools-cache-canary-4f1c2a"


class Failure(AssertionError):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)


def stamp(day, minute=0):
    at = datetime.fromisoformat(day).replace(hour=12, tzinfo=timezone.utc)
    return (at + timedelta(minutes=minute)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def put(db, table, **fields):
    names = ", ".join(fields)
    marks = ", ".join("?" for _ in fields)
    db.execute(f"INSERT INTO {P}{table} ({names}) VALUES ({marks})", tuple(fields.values()))


def build_source(path):
    """The synthetic database every section starts from. Two tool calls a run."""
    with closing(sqlite3.connect(path)) as db:
        db.executescript(SCHEMA.read_text())
        runs = (
            ("oct", "2026-10-14"),
            ("delete", "2026-11-11"),
            ("reopen", "2026-11-11"),
            ("archive", "2026-11-12"),
            ("keep", "2026-11-12"),
            ("current", "2026-11-18"),
            ("oct-second", "2026-10-19"),
            ("current-second", "2026-11-18"),
        )
        for i, (run, day) in enumerate(runs):
            started, middle, ended = (stamp(day, i * 30 + m) for m in (0, 10, 20))
            thread, node, provider_thread = f"thread-{run}", f"node-{run}", f"provider-{run}"
            put(
                db, "threads", thread_id=thread, project_id=f"project-{i % 3}",
                title="a synthetic thread", default_provider="claude",
                runtime_mode="standard", interaction_mode="default",
                created_at=stamp("2026-10-01"), updated_at=ended,
                payload_json=json.dumps({
                    "title": "a synthetic thread", "projectId": f"project-{i % 3}",
                    "createdAt": stamp("2026-10-01"),
                }),
            )
            put(
                db, "runs", run_id=run, thread_id=thread, ordinal=1, provider="claude",
                provider_thread_id=provider_thread, status="completed",
                requested_at=started, completed_at=ended,
                payload_json=json.dumps({
                    "startedAt": started, "completedAt": ended, "status": "completed",
                    "providerInstanceId": "claudeAgent",
                    "modelSelection": {"model": "claude-sonnet-5-5"},
                    "prompt": CANARY,
                }),
            )
            put(
                db, "nodes", node_id=node, root_node_id=node, run_id=run,
                thread_id=thread, kind="root", status="completed",
                started_at=started, completed_at=ended, payload_json="{}",
            )
            put(
                db, "provider_threads", provider_thread_id=provider_thread,
                thread_id=thread, owner_node_id=node, provider="claude",
                status="idle", updated_at=ended, payload_json="{}",
            )
            put(
                db, "provider_turns", provider_turn_id=f"turn-{run}", thread_id=thread,
                provider_thread_id=provider_thread, node_id=node, ordinal=1,
                status="completed", started_at=started, completed_at=ended,
                payload_json=json.dumps({
                    "turnTokenUsage": {
                        "usageStatus": "complete", "inputTokens": 2500 + i,
                        "outputTokens": 420, "cachedInputTokens": 110,
                    },
                    "tokenUsage": {"usedTokens": 9900, "maxTokens": 10000},
                    "reasoning": CANARY,
                }),
            )
            put(
                db, "runtime_requests", runtime_request_id=f"ask-{run}",
                thread_id=thread, node_id=node, kind="user_input",
                status="resolved", created_at=started, resolved_at=middle,
                payload_json=json.dumps({
                    "responseCapability": {"type": "message"}, "answer": CANARY,
                }),
            )
            items = (
                ("user_message", "completed", {
                    "createdBy": "user", "creationSource": "web", "text": CANARY,
                }),
                ("command_execution", "completed", {
                    "startedAt": started, "completedAt": ended,
                    "command": "cargo test", "output": CANARY,
                }),
                ("dynamic_tool", "completed", {
                    "startedAt": started, "completedAt": ended,
                    "toolName": "Read", "input": {"file_path": CANARY},
                }),
                ("checkpoint", "completed", {
                    "files": [{
                        "kind": "modified", "path": CANARY,
                        "additions": 19 + i, "deletions": i,
                    }],
                }),
                ("error", "completed", {
                    "failure": {"class": "provider_error", "message": CANARY},
                }),
            )
            for ordinal, (kind, status, payload) in enumerate(items, 1):
                put(
                    db, "turn_items", turn_item_id=f"{run}-{kind}", thread_id=thread,
                    run_id=run, node_id=node, provider_thread_id=provider_thread,
                    ordinal=ordinal, type=kind, status=status,
                    updated_at=ended, payload_json=json.dumps(payload),
                )
        db.commit()
    return path


class Timespec(ctypes.Structure):
    _fields_ = [("tv_sec", ctypes.c_long), ("tv_nsec", ctypes.c_long)]


CLOCK_REALTIME_COARSE = 5  # Linux's coarse clock, the one cache.rs compares against
LIBC = None


def coarse_now_ns():
    global LIBC
    if LIBC is None:
        LIBC = ctypes.CDLL("libc.so.6", use_errno=True)
    stamp = Timespec()
    if LIBC.clock_gettime(CLOCK_REALTIME_COARSE, ctypes.byref(stamp)) != 0:
        raise OSError(ctypes.get_errno(), "clock_gettime(CLOCK_REALTIME_COARSE)")
    return stamp.tv_sec * 1_000_000_000 + stamp.tv_nsec


def settle(path):
    # `FileStamp::of` in crates/core/src/cache.rs refuses a source whose mtime or
    # ctime is not older than CLOCK_REALTIME_COARSE, because a write in that tick
    # may not be visible in the captured stamp. Wait for the same condition.
    for _ in range(500):
        coarse = coarse_now_ns()
        files = [path, Path(f"{path}-wal")]
        stamps = [file.stat() for file in files if file.exists()]
        if all(stamp.st_mtime_ns < coarse and stamp.st_ctime_ns < coarse for stamp in stamps):
            return
        time.sleep(0.005)
    raise Failure(f"the source clock did not settle for {path}")


def open_writer(path):
    """A WAL writer held open across reads so commits do not reset the WAL lineage."""
    db = sqlite3.connect(path)
    check(db.execute("PRAGMA journal_mode=WAL").fetchone() == ("wal",), "the fixture has no WAL")
    db.execute("PRAGMA wal_autocheckpoint=0")
    check(db.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone() == (0, 0, 0), "the fixture WAL did not reset")
    db.commit()
    return db


class Scenario:
    def __init__(self, program, scratch, source):
        self.program = program
        self.source = source
        self.root = scratch
        home = scratch / "home"
        t3 = scratch / "empty-t3-home"
        home.mkdir(parents=True)
        t3.mkdir()
        self.cache_home = scratch / "cache"
        self.cache = self.cache_home / "loupe" / "insights.sqlite3"
        self.env = {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_DATA_HOME": str(home / "data"),
            "XDG_STATE_HOME": str(home / "state"),
            "XDG_CACHE_HOME": str(self.cache_home),
            "T3CODE_HOME": str(t3),
            "PATH": "/usr/bin:/bin",
            "TZ": "UTC",
            "TERM": "xterm-256color",
            "NO_COLOR": "1",
            "LANG": "C.UTF-8",
        }

    def run(self, flag, args=("--json",), *, cache="off", stats=False, db=None, t3_home=None):
        env = dict(self.env)
        if t3_home is not None:
            env["T3CODE_HOME"] = str(t3_home)
        if cache == "on":
            env["LOUPE_CACHE"] = "1"
        elif cache == "direct":
            env["LOUPE_NO_CACHE"] = "1"
        elif cache == "override":
            env["LOUPE_CACHE"] = "1"
            env["LOUPE_NO_CACHE"] = "1"
        elif cache == "zero":
            env["LOUPE_CACHE"] = "0"
        elif cache != "off":
            raise AssertionError(f"unknown cache mode {cache}")
        if stats:
            env["LOUPE_CACHE_STATS"] = "1"
        command = [
            str(self.program), flag, "--db", str(db or self.source),
            "--now-ms", NOW, *args,
        ]
        result = subprocess.run(command, env=env, capture_output=True, timeout=120)
        if result.returncode == 2 and b"Unknown option" in result.stderr:
            raise Failure(
                f"{flag} is not in this build; run this check on a merged --recap/--tools binary\n"
                f"  {command}\n  {result.stderr.decode(errors='replace').strip()}"
            )
        return result

    def direct(self, flag, args=("--json",), **kw):
        return self.run(flag, args, cache="direct", **kw)

    def cached(self, flag, args=("--json",), **kw):
        return self.run(flag, args, cache="on", **kw)

    def same_bytes(self, flag, args, label, stats=False, cache="on"):
        """Cached and direct runs agree byte for byte (minus the requested diagnostic)."""
        direct = self.direct(flag, args)
        cached = self.run(flag, args, cache=cache, stats=stats)
        check(cached.returncode == direct.returncode,
              f"{label}: exit {cached.returncode} cached, {direct.returncode} direct")
        check(cached.stdout == direct.stdout,
              f"{label}: stdout differs\n  cached: {cached.stdout[:400]!r}\n  direct: {direct.stdout[:400]!r}")
        stripped = CACHE_LINE.sub(b"", cached.stderr)
        check(stripped == direct.stderr,
              f"{label}: stderr differs beyond the cache line\n  cached: {stripped[:400]!r}\n  direct: {direct.stderr[:400]!r}")
        return cached, direct

    def assert_stats(self, result, hits, finished, written, label):
        matches = STATS.findall(result.stderr)
        check(len(matches) == 1,
              f"{label}: wanted one cache diagnostic, found {len(matches)} in {result.stderr!r}")
        actual = tuple(int(value) for value in matches[0])
        check(actual == (hits, finished, written),
              f"{label}: cache line says {actual}, wanted {(hits, finished, written)}")

    def assert_not_used(self, result, label):
        check(b"loupe: cache: not used\n" in result.stderr,
              f"{label}: cache was used\n  {result.stderr!r}")
        check(not STATS.search(result.stderr),
              f"{label}: a disabled cache reported hits or writes\n  {result.stderr!r}")

    def connect_cache(self):
        check(self.cache.is_file(), f"no cache database at {self.cache}")
        return closing(sqlite3.connect(f"file:{self.cache}?mode=ro", uri=True))

    def facts(self):
        with self.connect_cache() as db:
            return db.execute("SELECT count(*) FROM facts").fetchone()[0]

    def cache_files(self):
        return [path for path in self.cache_home.rglob("*") if path.is_file()]

    def fingerprint(self):
        digest = hashlib.sha256()
        for path in sorted(self.cache_files()):
            digest.update(str(path.relative_to(self.cache_home)).encode())
            digest.update(path.read_bytes())
        return digest.hexdigest()

    def assert_only_cache(self):
        files = self.cache_files()
        allowed = {
            "insights.sqlite3", "insights.sqlite3-wal", "insights.sqlite3-shm",
            "insights.sqlite3-journal", "insights.lock", "insights.owner",
        }
        names = {path.name for path in files}
        check(names and names <= allowed, f"unexpected files in the cache folder: {sorted(names)}")
        check((self.cache_home / "loupe" / "insights.sqlite3").is_file(),
              "both readers did not share the one cache database")

    def assert_t3_untouched(self):
        check(not any((self.root / "empty-t3-home").iterdir()), "the readers wrote to the empty T3 home")


def fresh_fixture(scratch, name):
    path = scratch / f"{name}.sqlite"
    return build_source(path)


def read_json(result, label):
    try:
        return json.loads(result.stdout)
    except ValueError as error:
        raise Failure(f"{label}: stdout is not JSON: {error}") from error


# ── sections ──────────────────────────────────────────────────────────────────

def section_single_cache(program, scratch):
    source = fresh_fixture(scratch, "single-cache")
    scenario = Scenario(program, scratch / "single-cache", source)
    settle(source)

    cached, _ = scenario.same_bytes("--recap", ("--json",), "Recap cold fill", stats=True)
    scenario.assert_stats(cached, 0, FINISHED[WEEK], FINISHED[WEEK], "Recap cold fill")
    check(scenario.facts() == 4, f"Recap wrote {scenario.facts()} facts, wanted 4")

    cached, _ = scenario.same_bytes("--tools", ("--json",), "Tools cold fill", stats=True)
    scenario.assert_stats(cached, 0, FINISHED[WEEK], FINISHED[WEEK], "Tools cold fill")
    check(scenario.facts() == 8,
          f"the single cache holds {scenario.facts()} facts after both readers, wanted 8")
    scenario.assert_only_cache()

    cached, _ = scenario.same_bytes("--recap", ("--json",), "Recap warm read", stats=True)
    scenario.assert_stats(cached, FINISHED[WEEK], FINISHED[WEEK], 0, "Recap warm read")
    cached, _ = scenario.same_bytes("--tools", ("--json",), "Tools warm read", stats=True)
    scenario.assert_stats(cached, FINISHED[WEEK], FINISHED[WEEK], 0, "Tools warm read")

    # Both readers keep their own facts for other periods in the same store.
    cached, _ = scenario.same_bytes("--recap", (*MONTH, "--json"), "Recap month fill", stats=True)
    scenario.assert_stats(cached, 0, FINISHED[MONTH], FINISHED[MONTH], "Recap month fill")
    cached, _ = scenario.same_bytes("--tools", (*MONTH, "--json"), "Tools month fill", stats=True)
    scenario.assert_stats(cached, 0, FINISHED[MONTH], FINISHED[MONTH], "Tools month fill")
    check(scenario.facts() == 12,
          f"the cache holds {scenario.facts()} facts after the month fills, wanted 12")
    scenario.assert_only_cache()
    scenario.assert_t3_untouched()


def section_independent_opt_ins(program, scratch):
    source = fresh_fixture(scratch, "opt-in")

    # Recap opts in, Tools does not: Tools must not open the cache at all.
    recap_first = Scenario(program, scratch / "recap-first", source)
    settle(source)
    recap_cold, _ = recap_first.same_bytes("--recap", ("--json",), "Recap opted-in fill", stats=True)
    recap_first.assert_stats(recap_cold, 0, FINISHED[WEEK], FINISHED[WEEK], "Recap opted-in fill")
    before = recap_first.fingerprint()
    quiet, _ = recap_first.same_bytes("--tools", ("--json",), "Tools without opt-in",
                                      stats=True, cache="off")
    recap_first.assert_not_used(quiet, "Tools without opt-in")
    check(recap_first.fingerprint() == before,
          "Tools without an opt-in changed Recap's cache")

    tools_cold, _ = recap_first.same_bytes("--tools", ("--json",), "Tools opted-in fill", stats=True)
    recap_first.assert_stats(tools_cold, 0, FINISHED[WEEK], FINISHED[WEEK], "Tools opted-in fill")
    check(recap_first.facts() == 8, "Tools did not add its facts beside Recap's")

    # The reverse order: Tools opts in, Recap does not.
    tools_first = Scenario(program, scratch / "tools-first", source)
    settle(source)
    tools_cold, _ = tools_first.same_bytes("--tools", ("--json",), "Tools opted-in fill first", stats=True)
    tools_first.assert_stats(tools_cold, 0, FINISHED[WEEK], FINISHED[WEEK], "Tools opted-in fill first")
    before = tools_first.fingerprint()
    quiet, _ = tools_first.same_bytes("--recap", ("--json",), "Recap without opt-in",
                                      stats=True, cache="off")
    tools_first.assert_not_used(quiet, "Recap without opt-in")
    check(tools_first.fingerprint() == before,
          "Recap without an opt-in changed Tools' cache")

    recap_cold, _ = tools_first.same_bytes("--recap", ("--json",), "Recap opted-in fill second", stats=True)
    tools_first.assert_stats(recap_cold, 0, FINISHED[WEEK], FINISHED[WEEK], "Recap opted-in fill second")
    check(tools_first.facts() == 8, "Recap did not add its facts beside Tools'")

    # Only `1` opts in; `0` is not an opt-in even beside the stats request.
    explicit_zero = Scenario(program, scratch / "explicit-zero", source)
    settle(source)
    for flag in ("--recap", "--tools"):
        result, _ = explicit_zero.same_bytes(flag, ("--json",), f"{flag} with LOUPE_CACHE=0",
                                             cache="zero")
        expected = explicit_zero.run(flag, ("--json",), cache="zero", stats=True)
        explicit_zero.assert_not_used(expected, f"{flag} with LOUPE_CACHE=0")
        check(result.returncode == expected.returncode and result.stdout == expected.stdout,
              f"{flag} with LOUPE_CACHE=0 changed its output")
    check(not explicit_zero.cache_home.exists(),
          "LOUPE_CACHE=0 still created cache state")


def section_changed_run(program, scratch):
    source = fresh_fixture(scratch, "changed")
    scenario = Scenario(program, scratch / "changed", source)
    writer = open_writer(source)
    try:
        settle(source)
        recap_before = scenario.cached("--recap", ("--json",), stats=True)
        scenario.assert_stats(recap_before, 0, FINISHED[WEEK], FINISHED[WEEK], "Recap before change")
        tools_before = scenario.cached("--tools", ("--json",), stats=True)
        scenario.assert_stats(tools_before, 0, FINISHED[WEEK], FINISHED[WEEK], "Tools before change")

        with writer:
            updated = writer.execute(
                f"UPDATE {P}turn_items SET status='failed', "
                "payload_json=json_set(payload_json, '$.completedAt', ?) "
                "WHERE turn_item_id='keep-command_execution'",
                (stamp("2026-11-12", 4 * 30 + 25),),
            )
        check(updated.rowcount == 1, "the change matched no turn item")
        settle(source)

        for flag, before in (("--recap", recap_before), ("--tools", tools_before)):
            after, _ = scenario.same_bytes(flag, ("--json",), f"{flag} after a changed run", stats=True)
            check(after.stdout != before.stdout,
                  f"{flag} did not see the changed run; its output is unchanged")
            hits, finished, written = (int(value) for value in STATS.findall(after.stderr)[0])
            scenario.assert_stats(after, hits, finished, written, f"{flag} after a changed run")
            check(written >= 1, f"{flag} served a changed run without writing it back")
            check(finished == FINISHED[WEEK],
                  f"{flag} counted {finished} finished runs after the change, wanted {FINISHED[WEEK]}")
        check(scenario.facts() == 8,
              f"the cache holds {scenario.facts()} facts after the change, wanted 8")
    finally:
        writer.close()


def section_deleted_run(program, scratch):
    source = fresh_fixture(scratch, "deleted")
    scenario = Scenario(program, scratch / "deleted", source)
    writer = open_writer(source)
    try:
        settle(source)
        recap_before = scenario.cached("--recap", ("--json",), stats=True)
        scenario.assert_stats(recap_before, 0, FINISHED[WEEK], FINISHED[WEEK], "Recap before delete")
        tools_before = scenario.cached("--tools", ("--json",), stats=True)
        scenario.assert_stats(tools_before, 0, FINISHED[WEEK], FINISHED[WEEK], "Tools before delete")

        with writer:
            deleted = writer.execute(f"DELETE FROM {P}runs WHERE run_id='keep'")
        check(deleted.rowcount == 1, "the delete matched no run")
        settle(source)

        remaining = FINISHED[WEEK] - 1
        for flag, before in (("--recap", recap_before), ("--tools", tools_before)):
            after, _ = scenario.same_bytes(flag, ("--json",), f"{flag} after a deleted run", stats=True)
            check(after.stdout != before.stdout,
                  f"{flag} still shows the deleted run")
            matches = STATS.findall(after.stderr)
            check(len(matches) == 1,
                  f"{flag} after a deleted run: found {len(matches)} cache diagnostics")
            check(int(matches[0][1]) == remaining,
                  f"{flag} after a deleted run: counted {int(matches[0][1])} finished runs, wanted {remaining}")
            report = read_json(after, flag)
            if flag == "--recap":
                check(report["runs"] == remaining,
                      f"Recap reports {report['runs']} runs after the delete, wanted {remaining}")
            else:
                check(report["calls"] == 2 * remaining,
                      f"Tools reports {report['calls']} calls after the delete, wanted {2 * remaining}")
        check(scenario.facts() == 6,
              f"the cache holds {scenario.facts()} facts after the delete, wanted 6")
    finally:
        writer.close()


def section_off_override(program, scratch):
    source = fresh_fixture(scratch, "override")

    # The override wins from the first run: no cache is created at all.
    fresh = Scenario(program, scratch / "override-fresh", source)
    settle(source)
    for flag in ("--recap", "--tools"):
        result, _ = fresh.same_bytes(flag, ("--json",), f"{flag} with the override from the start",
                                     stats=True, cache="override")
        fresh.assert_not_used(result, f"{flag} with the override from the start")
    check(not fresh.cache_home.exists(),
          "LOUPE_NO_CACHE=1 did not stop the cache from being created")

    # The override wins beside a full cache: its bytes are not touched.
    warm = Scenario(program, scratch / "override-warm", source)
    settle(source)
    for flag in ("--recap", "--tools"):
        filled, _ = warm.same_bytes(flag, ("--json",), f"{flag} warm fill before override", stats=True)
        warm.assert_stats(filled, 0, FINISHED[WEEK], FINISHED[WEEK], f"{flag} warm fill before override")
    before = warm.fingerprint()
    for flag in ("--recap", "--tools"):
        result, _ = warm.same_bytes(flag, ("--json",), f"{flag} with the override",
                                    stats=True, cache="override")
        warm.assert_not_used(result, f"{flag} with the override")
    check(warm.fingerprint() == before,
          "LOUPE_NO_CACHE=1 changed an existing cache")


def section_identical_bytes(program, scratch):
    source = fresh_fixture(scratch, "bytes")
    scenario = Scenario(program, scratch / "bytes", source)
    settle(source)
    cases = []
    for flag, forms in (("--recap", RECAP_FORMS), ("--tools", TOOLS_FORMS)):
        for period in (WEEK, THIS_WEEK, MONTH, THIS_MONTH):
            for form in forms:
                cases.append((flag, (*period, *form)))
    for flag, args in cases:
        label = f"{flag} {' '.join(args) or '(default)'}"
        scenario.same_bytes(flag, args, f"cold {label}")
        scenario.same_bytes(flag, args, f"warm {label}")
    scenario.assert_only_cache()

    # The canaries in prompts, commands, paths and answers must not reach the cache.
    with scenario.connect_cache() as db:
        stored = "\n".join(db.iterdump()).encode()
    for path in scenario.cache_files():
        stored += path.read_bytes()
    check(CANARY.encode() not in stored, "the shared cache retained a private canary")
    scenario.assert_t3_untouched()


COMMAND_TITLE = "Command input contract"
ACTIVE_MARK = "cmd-input-active-8a137f"
FINISHED_MARK = "cmd-input-finished-8a137f"
SECOND_LINE = "cmd-input-second-line-8a137f"
PAST_LIMIT = "cmd-input-past-limit-8a137f"
COMMAND_MARKS = (ACTIVE_MARK, FINISHED_MARK, SECOND_LINE, PAST_LIMIT)
ACTIVE_LINE = f"cargo test {ACTIVE_MARK}\tA\x1bB\rC  "
ACTIVE_INPUT = ACTIVE_LINE + f"\n{SECOND_LINE}\nDo you prefer option two?\n" + "x" * 240 + PAST_LIMIT
FINISHED_LINE = f"cargo test {FINISHED_MARK}\tA\x1bB\rC "
# Put a visible boundary character at position 240, with a unique suffix after it.
FINISHED_INPUT = FINISHED_LINE.ljust(239, "y") + "Z" + PAST_LIMIT


def build_command_source(path):
    """One thread currently running, with a failed call in its previous completed run."""
    with closing(sqlite3.connect(path)) as db:
        db.executescript(SCHEMA.read_text())
        began, failed, finished, resumed, asked, working = (
            f"2026-11-19T06:25:{second:02d}.000Z" for second in (0, 10, 20, 30, 40, 50)
        )
        put(db, "threads", thread_id="command-thread", project_id="synthetic",
            title=COMMAND_TITLE, default_provider="claude", runtime_mode="standard",
            interaction_mode="default", created_at=began, updated_at=working,
            payload_json=json.dumps({"id": "command-thread", "title": COMMAND_TITLE, "projectId": "synthetic"}))
        for ordinal, run, state, start, end in (
            (1, "finished-run", "completed", began, finished),
            (2, "active-run", "running", resumed, None),
        ):
            node = f"node-{run}"
            put(db, "runs", run_id=run, thread_id="command-thread", ordinal=ordinal,
                provider="claude", status=state, requested_at=start, completed_at=end,
                payload_json=json.dumps({"id": run, "threadId": "command-thread", "provider": "claude",
                                         "startedAt": start, "status": state}))
            put(db, "nodes", node_id=node, root_node_id=node, thread_id="command-thread",
                run_id=run, kind="root", status=state, started_at=start,
                completed_at=end, payload_json="{}")
            done = end is not None
            payload = {"input": FINISHED_INPUT if done else ACTIVE_INPUT, "startedAt": start}
            if done:
                payload.update(completedAt=failed, exitCode=2)
            put(db, "turn_items", turn_item_id=f"cmd-{run}", thread_id="command-thread",
                run_id=run, node_id=node, ordinal=ordinal, type="command_execution",
                status="failed" if done else "running", updated_at=failed if done else working,
                payload_json=json.dumps(payload))
        put(db, "runtime_requests", runtime_request_id="exact-ask", thread_id="command-thread",
            node_id="node-active-run", kind="user_input", status="pending", created_at=asked,
            payload_json=json.dumps({"responseCapability": {"type": "message"}}))
        put(db, "messages", message_id="estimated-ask", thread_id="command-thread",
            run_id="active-run", node_id="node-active-run",
            role="assistant", created_at=asked, updated_at=asked, streaming=0,
            payload_json=json.dumps({"text": "Do you prefer option one?"}))
        db.commit()
    return path


def no_command_input(data, label):
    for marker in COMMAND_MARKS:
        check(marker.encode() not in data, f"{label}: leaked command input marker {marker}")


def section_command_input(program, scratch):
    source = build_command_source(scratch / "sentinel.sqlite")
    baseline = scratch / "neutral.sqlite"
    shutil.copyfile(source, baseline)
    with closing(sqlite3.connect(baseline)) as db, db:
        changed = db.execute(f"""UPDATE {P}turn_items
            SET payload_json=json_set(payload_json,'$.input','cargo test') WHERE type='command_execution'
        """)
        check(changed.rowcount == 2, "expected to replace both command inputs in the comparison")
    observed = Scenario(program, scratch / "sentinel", source)
    neutral = Scenario(program, scratch / "neutral", baseline)

    def run(scenario, flag, args=(), *, cached=False):
        with tempfile.TemporaryDirectory(dir=scenario.root, prefix="t3-") as t3_home:
            result = scenario.run(flag, args, cache="on" if cached else "direct",
                                  stats=cached, t3_home=t3_home)
            check(not any(Path(t3_home).iterdir()), f"{flag} {args}: wrote to the empty T3 home")
        check(result.returncode == 0, f"{flag} {args}: exit {result.returncode}: {result.stderr!r}")
        check(cached or not result.stderr, f"{flag} {args}: unexpected stderr: {result.stderr!r}")
        check(result.stdout.strip(), f"{flag} {args}: no stdout")
        return result

    # Check the reader's 240-character projection separately from rendering. The
    # latter cleans controls and shows only line one (oracle/src/view.ts itemLabel).
    raw_snapshot = run(observed, "--dump-snapshot")
    check(PAST_LIMIT.encode() not in raw_snapshot.stdout, "dashboard snapshot includes input past 240 characters")
    snapshot = read_json(raw_snapshot, "dashboard snapshot")
    agent, = snapshot["agents"]
    check((agent["now"] or {}).get("command") == ACTIVE_INPUT[:240],
          "dashboard current command excerpt changed")
    failure, = agent["recentFailures"]
    check(failure["command"] == FINISHED_INPUT[:240], "dashboard failed command excerpt changed")

    frame = run(observed, "--frame-text", ("--grid", "320x70")).stdout.decode()
    columns = [line.partition("│") for line in frame.splitlines()]
    table = "\n".join(left for left, _, _ in columns)
    detail = "\n".join(right for _, _, right in columns)
    active = f"cargo test {ACTIVE_MARK} A B C"
    finished = f"{FINISHED_MARK} A B C"
    check(any(COMMAND_TITLE in line and f"❯ {active}" in line for line in table.splitlines()),
          "dashboard table row is missing its cleaned current command")
    check(all(marker not in table for marker in COMMAND_MARKS[1:]),
          "dashboard table includes history, a second line, or text past the excerpt")
    check(f"now  ❯ {active}" in detail, "dashboard detail Now is missing its cleaned command")
    before_activity, heading, activity = detail.partition("ACTIVITY")
    check(heading and "latest run" in activity and active in activity and FINISHED_MARK not in activity,
          "dashboard detail Activity should show the latest run's cleaned command only")
    _, heading, failures = before_activity.partition("RECENT FAILURES")
    check(heading and "exit 2 · earlier run cargo test" in failures
          and finished in failures and ("y" * 20 + "Z") in failures,
          "dashboard detail Recent Failures is missing the earlier run's cleaned command")
    check(all(marker not in detail for marker in (SECOND_LINE, PAST_LIMIT)),
          "dashboard detail includes a second line or text past the excerpt")
    check(table.count(ACTIVE_MARK) == 1 and detail.count(ACTIVE_MARK) == 2
          and detail.count(FINISHED_MARK) == 1,
          "command excerpts appear outside the expected dashboard display paths")

    def unchanged(flag, args=()):
        result = run(observed, flag, args)
        label = f"{flag} {' '.join(args)}".strip()
        no_command_input(result.stdout + result.stderr, label)
        check(result.stdout == run(neutral, flag, args).stdout,
              f"{label}: changing only command input changed the output")
        return result

    once = unchanged("--once").stdout.decode()
    check(COMMAND_TITLE in once and "1 running" in once, "--once did not list the running thread")
    status = read_json(unchanged("--status"), "status v1")
    check((status["v"], status["running"], status["waiting"], status["failed"]) == (1, 1, 0, 1),
          f"unexpected status v1 totals: {status}")
    current, = status["agents"]
    check((current["title"], current["state"], current["now"]) == (COMMAND_TITLE, "running", "shell"),
          f"unexpected status v1 current activity: {current}")
    for scope in (status, current):
        check((scope["openAsks"], scope["estimatedAsks"]) == (1, 1),
              f"expected one formal ask and one prose estimate: {scope}")

    for target, state, call_state in (
        ("active-run", "running", "running"), ("finished-run", "completed", "failed")
    ):
        flag = f"--trace={target}"
        plain = unchanged(flag).stdout
        check(COMMAND_TITLE.encode() in plain and b"cargo" in plain, f"Trace text omitted {target}'s call")
        trace = read_json(unchanged(flag, ("--json",)), target)
        check(trace["state"] == state and [r["id"] for r in trace["runs"]] == [target],
              f"Trace did not select {target}")
        calls = [(span["label"], span["status"]) for span in trace["spans"] if span["category"] == "shell"]
        check(calls == [("cargo", call_state)], f"Trace {target} shell spans: {calls}")
        check(trace["steps"]["asks"] == (1 if target == "active-run" else 0),
              f"Trace {target} ask counts: {trace['steps']}")

    settle(observed.source)
    for flag, text_marker in (("--recap", b"1 finished"), ("--tools", b"cargo")):
        plain = unchanged(flag, THIS_WEEK).stdout
        check(text_marker in plain, f"{flag}: expected report content missing from text")
        direct = unchanged(flag, (*THIS_WEEK, "--json"))
        report = read_json(direct, flag)
        if flag == "--recap":
            check(report["runs"] == 2 and report["tasks"]["finished"] == 1
                  and report["commands"] == 2 and report["commands_failed"] == 1
                  and report["asks"]["count"] == 0,
                  f"Recap run, command, or ask counts differ: {report}")
        else:
            cargo = [row for row in report["programs"] if row["name"] == "cargo"]
            check(report["calls"] == 2 and report["open"] == 1
                  and len(cargo) == 1 and cargo[0]["calls"] == 2 and cargo[0]["failed"] == 1,
                  f"Tools did not classify both calls and the failure: {report}")

        for stage, args, expected, output in (
            ("cold JSON write", (*THIS_WEEK, "--json"), (0, 1, 1), direct.stdout),
            ("warm JSON hit", (*THIS_WEEK, "--json"), (1, 1, 0), direct.stdout),
            ("warm text hit", THIS_WEEK, (1, 1, 0), plain),
        ):
            label = f"{flag} {stage}"
            cached = run(observed, flag, args, cached=True)
            observed.assert_stats(cached, *expected, label)
            no_command_input(cached.stdout + cached.stderr, label)
            check(cached.stdout == output, f"{label}: output differs from the direct read")
            observed.assert_only_cache()
            for path in observed.cache_files():
                no_command_input(path.read_bytes(), f"{label} {path.name}")
    check(observed.facts() == 2, "expected one fact from each report in the shared cache")


SECTIONS = (
    ("single-cache", section_single_cache),
    ("opt-in", section_independent_opt_ins),
    ("changed", section_changed_run),
    ("deleted", section_deleted_run),
    ("override", section_off_override),
    ("bytes", section_identical_bytes),
    ("command-input", section_command_input),
)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "program", nargs="?", type=Path,
        default=Path(os.environ.get("LOUPE_BINARY") or ROOT / "target/release/loupe-term"),
    )
    parser.add_argument("--program", dest="program_flag", type=Path, help="the built program to check")
    parser.add_argument("--only", action="append", default=[], metavar="NAME",
                        help="run only this section; repeat for more")
    parser.add_argument("--scratch", type=Path, help="make the run folder under this path")
    parser.add_argument("--keep", action="store_true", help="keep the scratch folder and print its path")
    args = parser.parse_args(argv)

    if not sys.platform.startswith("linux"):
        print("SKIP  the Insights cache fails closed without Linux's checked filesystem clock")
        return 0

    unknown = set(args.only) - {name for name, _ in SECTIONS}
    if unknown:
        print(f"unknown section(s): {', '.join(sorted(unknown))}", file=sys.stderr)
        return 2

    program = (args.program_flag or args.program).resolve()
    if not program.is_file() or not os.access(program, os.X_OK):
        print(f"no executable built program at {program}", file=sys.stderr)
        return 2

    base = (args.scratch or ROOT / "checks" / "generated" / "recap-tools-cache-check").resolve()
    base.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(dir=str(base), prefix="run-"))
    selected = [name for name, _ in SECTIONS if not args.only or name in args.only]

    failures = 0
    try:
        for name, section in SECTIONS:
            if name not in selected:
                continue
            folder = run / name
            folder.mkdir()
            try:
                section(program, folder)
            except Failure as error:
                failures += 1
                print(f"FAIL  {name}: {error}", flush=True)
            except Exception as error:  # keep the other sections running
                failures += 1
                print(f"FAIL  {name}: {type(error).__name__}: {error}", flush=True)
            else:
                print(f"pass  {name}", flush=True)
    finally:
        if args.keep:
            print(f"scratch: {run}")
        else:
            shutil.rmtree(run, ignore_errors=True)

    if failures:
        print(f"{failures} section(s) failed")
        return 1
    print(f"all {len(selected)} section(s) passed on {program}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
