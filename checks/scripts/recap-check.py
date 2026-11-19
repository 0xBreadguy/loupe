#!/usr/bin/env python3
"""Check Recap command-line behavior against a built program."""
import fcntl
import json
import os
import pty
import re
import select
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

P = "orchestration_v2_projection_"
# 2026-11-18 22:26 in Los Angeles, a Wednesday: the last week (Nov 9 to 15) and the last month
# (October) are over, so the default periods are full, and November is 18 days in.
NOW = "1795069560000"
# 2026-11-16 03:00 in Los Angeles, a Monday before any made-up work has begun.
EARLY = "1794826800000"
SCHEMA = f"""
CREATE TABLE {P}threads(thread_id TEXT PRIMARY KEY,payload_json TEXT,deleted_at TEXT,archived_at TEXT);
CREATE TABLE {P}runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
CREATE TABLE {P}subagents(subagent_id TEXT PRIMARY KEY,thread_id TEXT,payload_json TEXT);
CREATE TABLE {P}nodes(node_id TEXT PRIMARY KEY,thread_id TEXT,parent_node_id TEXT,run_id TEXT);
CREATE TABLE {P}provider_turns(thread_id TEXT,provider_thread_id TEXT,ordinal INTEGER,payload_json TEXT,started_at TEXT,completed_at TEXT);
CREATE TABLE {P}turn_items(item_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,node_id TEXT,provider_thread_id TEXT,type TEXT,status TEXT,updated_at TEXT,ordinal INTEGER,payload_json TEXT);
CREATE TABLE {P}runtime_requests(request_id TEXT PRIMARY KEY,thread_id TEXT,node_id TEXT,kind TEXT,status TEXT,created_at TEXT,resolved_at TEXT);"""


def database(folder, name, fill=None):
    os.makedirs(folder, exist_ok=True)
    path = os.path.join(folder, name + ".sqlite")
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    if fill:
        fill(db)
    db.commit()
    db.close()
    return path


def thin_week(db):
    """Three short tasks on one Wednesday, one model, no pull request, nobody asked anything."""
    j = json.dumps
    db.execute(f"INSERT INTO {P}threads VALUES('t1',?,NULL,NULL)", (j({"projectId": "p", "createdAt": "2026-11-02T10:00:00.000Z",
               "lineage": {"relationshipToParent": None}, "pullRequests": []}),))
    for i, (a, b) in enumerate([("18:00", "18:14"), ("18:20", "18:33"), ("19:05", "19:18")]):
        run, day = f"r{i}", "2026-11-11T"
        db.execute(f"INSERT INTO {P}runs VALUES(?,?,?,?,?,?)", (run, "t1", i + 1, j({"status": "completed", "providerInstanceId": "claudeAgent",
                   "startedAt": f"{day}{a}:00.000Z", "modelSelection": {"model": "claude-sonnet-5-5"}}), f"{day}{a}:00.000Z", f"{day}{b}:00.000Z"))
        items = [("user_message", {"createdBy": "user", "creationSource": "web"}),
                 ("command_execution", {"startedAt": f"{day}{a}:30.000Z", "completedAt": f"{day}{a[:3]}{int(a[3:]) + 7:02d}:00.000Z"}),
                 ("checkpoint", {"files": [{"kind": "modified", "additions": 137 + i, "deletions": 12}, {"kind": "modified", "additions": 0, "deletions": i}]})]
        for n, (kind, payload) in enumerate(items):
            db.execute(f"INSERT INTO {P}turn_items VALUES(?,?,?,NULL,NULL,?,'completed',?,?,?)", (f"{kind}{i}", "t1", run, kind, f"{day}{b}:00.000Z", n + 1, j(payload)))
        db.execute(f"INSERT INTO {P}provider_turns VALUES('t1',NULL,?,?,?,?)", (i + 1, j({"turnTokenUsage": {"usageStatus": "complete", "inputTokens": 700000,
                   "outputTokens": 10300, "cachedInputTokens": 637000}, "tokenUsage": {"usedTokens": 200000, "maxTokens": 1000000}}), f"{day}{a}:00.000Z", f"{day}{b}:00.000Z"))



def cache_week(db):
    """Three eligible finished runs, two kinds of cards, and a run still in progress."""
    thin_week(db)
    db.execute(f"ALTER TABLE {P}runs ADD COLUMN status TEXT")
    db.execute(f"UPDATE {P}runs SET status='completed'")
    db.execute(f"CREATE INDEX demo_items_by_run ON {P}turn_items(run_id, ordinal)")
    started = "2026-11-11T19:25:00.000Z"
    db.execute(f"INSERT INTO {P}runs VALUES('live','t1',4,?,?,NULL,'running')",
               (json.dumps({"status": "running", "startedAt": started}), started))
    for item, run, kind, status, begin, end in [
        ("secret", "r0", "secret_request", "completed", "18:01", "18:04"),
        ("link", "r1", "link_request", "waiting", "18:22", None),
    ]:
        start = f"2026-11-11T{begin}:00.000Z"
        stop = f"2026-11-11T{end}:00.000Z" if end else None
        db.execute(f"INSERT INTO {P}turn_items VALUES(?,'t1',?,'node',NULL,?,?,?,4,?)",
                   (item, run, kind, status, stop or start, json.dumps({"startedAt": start, "completedAt": stop})))


def add_finished(db, first, count, status="completed"):
    """Append finished runs whose status and item facts can be checked independently."""
    for i in range(first, first + count):
        start = f"2026-11-12T18:{i:02d}:00.000Z"
        end = f"2026-11-12T19:{i:02d}:00.000Z"
        db.execute(f"INSERT INTO {P}runs VALUES(?,?,?,?,?,?,?)",
                   (f"new{i}", "t1", 10 + i, json.dumps({"status": status, "startedAt": start}), start, end, status))

def main(scratch):
    program = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/t3-agents-term").resolve())
    home = os.path.join(scratch, "home")
    os.makedirs(os.path.join(scratch, "emptyhome"))
    env = {"HOME": home, "XDG_CONFIG_HOME": home + "/.config", "XDG_DATA_HOME": home + "/.local/share", "XDG_CACHE_HOME": home + "/.cache",
           "XDG_STATE_HOME": home + "/.local/state", "PATH": "/usr/bin:/bin", "T3CODE_HOME": os.path.join(scratch, "emptyhome"),
           "TERM": "xterm-256color", "COLORTERM": "truecolor", "TZ": "America/Los_Angeles"}
    outputs = []
    def terminal(args, theme="paper", size=None):
        master, slave = pty.openpty()
        if size:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", size[1], size[0], 0, 0))
        proc = subprocess.Popen([program, *args, "--theme", theme, "--now-ms", NOW], stdout=slave, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, env=env)
        os.close(slave)
        out = b""
        deadline = time.monotonic() + 60
        try:
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not select.select([master], [], [], remaining)[0]:
                    raise subprocess.TimeoutExpired(proc.args, 60)
                try: chunk = os.read(master, 65536)
                except OSError: break
                if not chunk: break
                out += chunk
            _, err = proc.communicate(timeout=max(0.01, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            outputs.append((args, (out.decode(errors="replace"), "timeout", "")))
            raise
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            os.close(master)
        result = (out.decode().replace("\r\n", "\n"), proc.returncode, err.decode())
        outputs.append((args, result))
        return result
    def rows_of(text):
        return re.sub(r"\x1b\[[0-9;]*m", "", text).rstrip("\n").split("\n")
    def is_short_list(out, rows_expected=None):
        """A colored list: 39 cells a row, and the recap header on the first."""
        rows = rows_of(out)
        ok = bool(rows) and rows[0].startswith(" T3 Agents  RECAP") and all(len(row) == 39 for row in rows) and "\x1b" in out
        if rows_expected is not None:
            ok = ok and len(rows) == rows_expected
        return ok
    def narrow_in_a_wide_terminal():
        out, code, err = terminal(["--recap", "--narrow", "--demo"], size=(120, 40))
        return code == 0 and err == "" and is_short_list(out, 22)
    def narrow_into_a_pipe():
        run = piped(["--recap", "--narrow", "--demo"])
        rows = run.stdout.rstrip("\n").split("\n")
        return run.returncode == 0 and "\x1b" not in run.stdout and bool(rows) and all(len(row) <= 38 for row in rows)
    def narrow_conflicts_are_usage_errors():
        wide = piped(["--recap", "--narrow", "--wide"])
        compact = piped(["--recap", "--narrow", "--compact"])
        alone = piped(["--narrow"])
        return (wide.returncode == 2 and "cannot be used together" in wide.stderr
                and compact.returncode == 2 and "cannot be used together" in compact.stderr
                and alone.returncode == 2 and "go with --recap" in alone.stderr)
    def narrow_json_is_json():
        run = piped(["--recap", "--narrow", "--demo", "--json"])
        return run.returncode == 0 and json.loads(run.stdout)["period"] == "week"
    def original_does_not_know_narrow():
        run = piped(["--narrow"], {"T3_AGENTS_NO_THEMES": "1"})
        return run.returncode == 2 and "Unknown option '--narrow'" in run.stderr
    def list_wider_than_thirty_columns_says_why():
        note = "t3-agents: the list is 39 columns wide,\nthis terminal is 30, so its lines wrap.\n"
        asked = terminal(["--recap", "--narrow", "--demo"], size=(30, 24))
        auto = terminal(["--recap", "--demo"], size=(30, 24))
        return asked[1] == 0 and auto[1] == 0 and asked[2] == note and auto[2] == note
    def under_eighty_gets_the_list():
        listed = terminal(["--recap", "--demo"], size=(60, 24))
        compact = terminal(["--recap", "--demo", "--compact"], size=(60, 24))
        return is_short_list(listed[0]) and len(rows_of(compact[0])) == 23
    def sixty_columns_without_a_height_gets_the_list():
        unknown = terminal(["--recap", "--demo"], size=(60, 0))
        known = terminal(["--recap", "--demo"], size=(60, 24))
        return is_short_list(known[0]) and unknown[0] == known[0] and unknown[2] == ""
    def piped(args, extra=None, now=NOW, *, text=True):
        try:
            r = subprocess.run([program, *args, "--now-ms", now], capture_output=True, text=text, env={**env, **(extra or {})}, timeout=60)
        except subprocess.TimeoutExpired as exc:
            outputs.append((args, (exc.stdout, "timeout", exc.stderr)))
            raise
        outputs.append((args, (r.stdout, r.returncode, r.stderr)))
        return r
    dbs = os.path.join(scratch, "dbs")
    thin, empty = database(dbs, "thin", thin_week), database(dbs, "empty")
    def message_capability_does_not_add_waiting():
        """Only this fixture has the optional request payload column."""
        def fill(db):
            """Add one hour-long question to the ordinary thin-week fixture."""
            thin_week(db)
            db.execute(f"ALTER TABLE {P}runtime_requests ADD COLUMN payload_json TEXT")
            db.execute(f"INSERT INTO {P}runtime_requests VALUES('q','t1',NULL,'user_input','resolved',"
                       "'2026-11-11T18:00:00.000Z','2026-11-11T19:00:00.000Z',?)",
                       (json.dumps({"responseCapability": {"type": "message"}}),))
        path = database(dbs, "message-capability", fill)
        by_message = piped(["--recap", "--db", path, "--json"])
        with sqlite3.connect(path) as db:
            db.execute(f"UPDATE {P}runtime_requests SET payload_json='{{}}'")
        holding = piped(["--recap", "--db", path, "--json"])
        return (by_message.returncode == 0 and holding.returncode == 0
                and json.loads(by_message.stdout)["split"]["waiting"] == 0
                and json.loads(holding.stdout)["split"]["waiting"] == 1)
    def cache_fixture(name):
        """Give each cache check a separately built source and a private cache folder."""
        path = database(dbs, name, cache_week)
        root = Path(scratch) / name
        extra = {"XDG_CACHE_HOME": str(root), "T3_AGENTS_CACHE": "1"}
        time.sleep(0.025)
        return ["--recap", "--db", path], extra, root / "t3-agents" / "insights.sqlite3"

    def cache_result(args, extra, expected, stats):
        """Check the raw output, exit status, and entire diagnostic line."""
        result = piped(args, extra, text=False)
        assert result.returncode == 0
        assert result.stdout == expected
        assert result.stderr == stats

    def cache_stats(hits, finished, writes):
        """The single diagnostic line emitted for an open store."""
        return f"t3-agents: cache: {hits} of {finished} finished runs from the cache, {writes} written\n".encode()

    def cold_and_warm_cache():
        """Exercise both output formats with actual misses and hits, excluding the live run."""
        args, extra, cache = cache_fixture("cold-and-warm")
        diagnostic = {**extra, "T3_AGENTS_CACHE_STATS": "1"}
        off = {**diagnostic, "T3_AGENTS_NO_CACHE": "1"}
        for mode in ("--json", "--wide"):
            if cache.exists():
                cache.unlink()
            output_args = [*args, mode]
            expected = piped(output_args, off, text=False).stdout
            cache_result(output_args, off, expected, b"t3-agents: cache: not used\n")
            assert not cache.exists()
            cache_result(output_args, diagnostic, expected, cache_stats(0, 3, 3))
            cache_result(output_args, diagnostic, expected, cache_stats(3, 3, 0))
            cache_result(output_args, extra, expected, b"")
            with sqlite3.connect(cache) as store:
                assert store.execute("SELECT count(*) FROM facts").fetchone()[0] == 3
        return True

    def source_write_retires_cache():
        """Adding two runs invalidates the three entries that were already in the store."""
        args, extra, _ = cache_fixture("retired")
        args.append("--json")
        diagnostic = {**extra, "T3_AGENTS_CACHE_STATS": "1"}
        off = {**extra, "T3_AGENTS_NO_CACHE": "1"}
        before = piped(args, off, text=False).stdout
        cache_result(args, diagnostic, before, cache_stats(0, 3, 3))
        with sqlite3.connect(args[2]) as db:
            add_finished(db, 1, 2)
        time.sleep(0.025)
        expected = piped(args, off, text=False).stdout
        assert expected != before
        # On this made-up schema the store has nothing to prove a single run by, so any write
        # to the source retires every entry. With T3's own schema in WAL mode it keeps the
        # entries a commit did not touch; the store's own check covers that, on a database
        # built from T3's schema.
        cache_result(args, diagnostic, expected, cache_stats(0, 5, 5))
        cache_result(args, diagnostic, expected, cache_stats(5, 5, 0))
        return True

    def unusable_cache_is_silent():
        """A broken cache file or an occupied cache path leaves the command usable."""
        args, extra, cache = cache_fixture("unusable")
        off = {**extra, "T3_AGENTS_NO_CACHE": "1"}
        output = {mode: piped([*args, mode], off, text=False).stdout for mode in ("--json", "--wide")}
        cache_result([*args, "--json"], {**extra, "T3_AGENTS_CACHE_STATS": "1"}, output["--json"], cache_stats(0, 3, 3))
        with cache.open("r+b") as broken:
            broken.write(bytes(1024))
        for mode, expected in output.items():
            cache_result([*args, mode], extra, expected, b"")
        if cache.exists():
            cache.unlink()
        cache.mkdir()
        for mode, expected in output.items():
            cache_result([*args, mode], extra, expected, b"")
        return True

    def cache_requires_opt_in():
        """Demo and default reads never open the store, and the off switch wins."""
        args, extra, cache = cache_fixture("opt-in")
        diagnostic = {**extra, "T3_AGENTS_CACHE_STATS": "1"}
        off = {**diagnostic, "T3_AGENTS_NO_CACHE": "1"}
        default = {name: value for name, value in diagnostic.items() if name != "T3_AGENTS_CACHE"}
        for mode in ("--json", "--wide"):
            expected = piped([*args, mode], off, text=False).stdout
            cache_result([*args, mode], off, expected, b"t3-agents: cache: not used\n")
            cache_result([*args, mode], default, expected, b"t3-agents: cache: not used\n")
            cache_result([*args, mode], {**diagnostic, "T3_AGENTS_CACHE": "true"}, expected, b"t3-agents: cache: not used\n")
            assert not cache.exists()
            demo = ["--recap", "--demo", mode]
            expected = piped(demo, off, text=False).stdout
            cache_result(demo, diagnostic, expected, b"t3-agents: cache: not used\n")
            assert not cache.exists()
        return True

    def ineligible_finished_run_is_not_written():
        """The store may reject a run that Recap counts as finished."""
        args, extra, cache = cache_fixture("ineligible")
        args.append("--json")
        with sqlite3.connect(args[2]) as db:
            add_finished(db, 1, 1, status="unknown")
            add_finished(db, 2, 1, status="waiting")
        time.sleep(0.025)
        off = {**extra, "T3_AGENTS_NO_CACHE": "1"}
        expected = piped(args, off, text=False).stdout
        diagnostic = {**extra, "T3_AGENTS_CACHE_STATS": "1"}
        cache_result(args, diagnostic, expected, cache_stats(0, 4, 3))
        cache_result(args, diagnostic, expected, cache_stats(3, 4, 0))
        with sqlite3.connect(cache) as store:
            assert store.execute("SELECT count(*) FROM facts").fetchone()[0] == 3
        return True

    def reader_settings():
        """Exercise default skim, SQLite, and head reads at each worker count."""
        for skim in (None, "off", "head"):
            for workers in ("1", "4", "8"):
                settings = {"T3_AGENTS_READ_THREADS": workers}
                if skim is not None:
                    settings["T3_AGENTS_SKIM"] = skim
                yield settings

    def reader_modes_keep_cards():
        """Compare bytes with the serial SQLite read, including warm reads with live misses."""
        def fill(db):
            """Keep eight misses after the finished runs are cached."""
            cache_week(db)
            db.execute(f"""INSERT INTO {P}runs
                SELECT run_id || n, thread_id, ordinal + n, payload_json, requested_at, completed_at, status
                FROM {P}runs CROSS JOIN (SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4
                    UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7)
                WHERE run_id='live'""")
            db.execute(f"""INSERT INTO {P}turn_items
                SELECT 'live-call' || run_id, thread_id, run_id, 'node', NULL, 'command_execution',
                       'completed', requested_at, 1, '{{"startedAt":"2026-11-11T19:26:00.000Z",
                       "completedAt":"2026-11-11T19:30:00.000Z"}}'
                FROM {P}runs WHERE status='running'""")

        path = database(dbs, "reader-modes", fill)
        time.sleep(0.025)
        for mode in ("--json", "--wide"):
            args = ["--recap", "--db", path, mode]
            color = {"FORCE_COLOR": "1"}
            serial = {**color, "T3_AGENTS_SKIM": "off", "T3_AGENTS_READ_THREADS": "1"}
            expected = piped(args, serial, text=False)
            assert expected.returncode == 0 and expected.stderr == b""
            for index, settings in enumerate(reader_settings()):
                settings = {**color, **settings}
                cache = str(Path(scratch) / f"reader-modes{mode}-{index}")
                cache_result(args, settings, expected.stdout, b"")
                cached = {**settings, "XDG_CACHE_HOME": cache,
                          "T3_AGENTS_CACHE": "1", "T3_AGENTS_CACHE_STATS": "1"}
                cache_result(args, cached, expected.stdout, cache_stats(0, 3, 3))
                cache_result(args, cached, expected.stdout, cache_stats(3, 3, 0))
                cache_result(args, {**cached, "T3_AGENTS_CACHE_STATS": "0"}, expected.stdout, b"")
        return True

    def non_strict_week(db):
        """Use JSON5 in a short tool item so even head mode examines the whole value."""
        cache_week(db)
        db.execute(f"UPDATE {P}turn_items SET payload_json=? WHERE item_id='command_execution0'",
                   ("{startedAt:'2026-11-11T18:00:00.000Z'}",))

    def non_strict_payload_keeps_the_notice():
        """A strict JSON failure gives the same notice from a serial or parallel read."""
        path = database(dbs, "non-strict", non_strict_week)
        args = ["--recap", "--db", path, "--json"]
        expected = piped(args, {"T3_AGENTS_READ_THREADS": "1"}, text=False)
        assert expected.returncode == 1 and expected.stdout == b""
        assert b"a recorded item is not strict JSON" in expected.stderr
        for workers in ("1", "4", "8"):
            for mode in ({}, {"T3_AGENTS_SKIM": "head"}, {"T3_AGENTS_SKIM": "early"}):
                result = piped(args, {**mode, "T3_AGENTS_READ_THREADS": workers}, text=False)
                assert (result.returncode, result.stdout, result.stderr) == (1, b"", expected.stderr)
        return True

    def off_uses_sqlite_json():
        """SQLite accepts a JSON5 payload that the Recap skim rejects."""
        path = database(dbs, "sqlite-json", non_strict_week)
        args = ["--recap", "--db", path, "--json"]
        for workers in ("1", "4", "8"):
            result = piped(args, {"T3_AGENTS_SKIM": "off", "T3_AGENTS_READ_THREADS": workers}, text=False)
            assert result.returncode == 0 and result.stderr == b""
            assert json.loads(result.stdout)["runs"] == 4
        return True

    def skim_treats_numeric_times_as_absent():
        """A numeric start is absent with the skim and a type error with SQLite."""
        path = database(dbs, "numeric-time", cache_week)
        args = ["--recap", "--db", path, "--json"]
        with sqlite3.connect(path) as db:
            db.execute(f"""UPDATE {P}turn_items
                SET payload_json=json_remove(payload_json,'$.startedAt') WHERE item_id='command_execution0'""")
        expected = piped(args, text=False)
        assert expected.returncode == 0 and expected.stderr == b""
        with sqlite3.connect(path) as db:
            db.execute(f"""UPDATE {P}turn_items
                SET payload_json=json_set(payload_json,'$.startedAt',1) WHERE item_id='command_execution0'""")
        for mode in ({}, {"T3_AGENTS_SKIM": "head"}):
            cache_result(args, mode, expected.stdout, b"")
        sqlite = piped(args, {"T3_AGENTS_SKIM": "off"}, text=False)
        assert sqlite.returncode == 1 and sqlite.stdout == b""
        assert b"Invalid column type Integer" in sqlite.stderr
        return True

    def demo_ignores_reader_settings_in_its_output():
        """Both variables preserve the demo's JSON and wide card bytes."""
        for output in ("--json", "--wide"):
            args = ["--recap", "--demo", output]
            color = {"FORCE_COLOR": "1"}
            expected = piped(args, color, text=False)
            assert expected.returncode == 0 and expected.stderr == b""
            for settings in reader_settings():
                cache_result(args, {**color, **settings}, expected.stdout, b"")
        return True

    big = (120, 40)
    checks = [
        ("reader modes keep fixture JSON and wide cards across cache-off, cold, and warm reads", reader_modes_keep_cards),
        ("non-strict JSON gives the same notice at every worker count", non_strict_payload_keeps_the_notice),
        ("skim off uses SQLite's JSON parser", off_uses_sqlite_json),
        ("the skim treats numeric timestamps as absent", skim_treats_numeric_times_as_absent),
        ("demo bytes stay the same at every skim mode and worker count", demo_ignores_reader_settings_in_its_output),
        ("cache-off, cold and warm JSON and wide output match, with only finished runs stored", cold_and_warm_cache),
        ("adding two runs retires existing entries, preserving the JSON", source_write_retires_cache),
        ("damaged cache and directory paths keep normal output and stderr", unusable_cache_is_silent),
        ("cache needs the exact opt-in, the off switch wins, and demo leaves no cache", cache_requires_opt_in),
        ("a finished run rejected by the store is counted but never written", ineligible_finished_run_is_not_written),
        ("a message-answerable hour adds no waiting; without the capability it adds its hour", message_capability_does_not_add_waiting),
        ('a pipe gets no escape codes', lambda: (lambda plain: '\x1b' not in plain.stdout and plain.returncode == 0)(piped(["--recap", "--demo"]))),
        ('NO_COLOR is honored when color is forced', lambda: '\x1b' not in piped(['--recap', '--demo'], {'NO_COLOR': '1', 'FORCE_COLOR': '1'}).stdout),
        ('FORCE_COLOR paints into a pipe', lambda: '\x1b[38;2;' in piped(['--recap', '--demo'], {'FORCE_COLOR': '1'}).stdout),
        ('every row of the card is 80 cells or fewer', lambda: (lambda plain: all((len(row) <= 80 for row in plain.stdout.split('\n'))))(piped(["--recap", "--demo"]))),
        ('the card is 23 rows', lambda: (lambda plain: plain.stdout.rstrip('\n').count('\n') + 1 == 23)(piped(["--recap", "--demo"]))),
        ('--json is valid and names no thread', lambda: json.loads(piped(['--recap', '--demo', '--json']).stdout)['runs'] > 0),
        ('a missing database exits 1 with advice on stderr and nothing on stdout', lambda: (lambda r: r.returncode == 1 and r.stdout == '' and ('no T3 database' in r.stderr))(piped(['--recap', '--db', dbs + '/missing.sqlite']))),
        ('an empty database exits 0 and says there is nothing', lambda: (lambda r: r.returncode == 0 and 'No agent runs recorded' in r.stdout)(piped(['--recap', '--db', empty]))),
        ('--this-week on an empty database says so far', lambda: 'so far' in piped(['--recap', '--this-week', '--db', empty]).stdout),
        ('--json alone is a usage error', lambda: piped(['--json']).returncode == 2),
        ('a terminal of 120 by 40 gets the wide card: 39 rows of 120 cells, ending with how it is counted', lambda: (lambda wide_rows: len(wide_rows) == 39 and all((len(row) == 120 for row in wide_rows)) and wide_rows[36].startswith(' HOW THIS IS COUNTED'))(rows_of(terminal(["--recap", "--demo"], size=big)[0]))),
        ('a terminal of 119 by 40, or 120 by 39, gets the 80-column card', lambda: all(((lambda rows: len(rows) == 23 and len(rows[0]) == 80)(rows_of(terminal(['--recap', '--demo'], size=size)[0])) for size in ((119, 40), (120, 39))))),
        ('--compact in a terminal of 120 by 40 is the 80-column card', lambda: len(rows_of(terminal(['--recap', '--demo', '--compact'], size=big)[0])) == 23),
        ('--wide in a terminal of 80 by 24 is the wide card', lambda: len(rows_of(terminal(['--recap', '--demo', '--wide'], size=(80, 24))[0])) == 39),
        ('--wide into a pipe is 39 rows of 120 cells or fewer, with no escape codes', lambda: (lambda wide_plain: '\x1b' not in wide_plain.stdout and (lambda rows: len(rows) == 39 and all((len(row) <= 120 for row in rows)))(wide_plain.stdout.rstrip('\n').split('\n')))(piped(["--recap", "--demo", "--wide"]))),
        ('a terminal under 80 columns gets the short list, and --compact there still draws the card', under_eighty_gets_the_list),
        ('a terminal of 60 columns that does not give its height still gets the list, the same as one of 60 by 24', sixty_columns_without_a_height_gets_the_list),
        ('--narrow in a terminal of 120 by 40 draws the short list', narrow_in_a_wide_terminal),
        ('--narrow into a pipe has no escape codes and no row over 38 characters', narrow_into_a_pipe),
        ('--narrow with --wide or --compact, and --narrow without --recap, are usage errors', narrow_conflicts_are_usage_errors),
        ('--narrow --json is valid JSON', narrow_json_is_json),
        ("the original's command line does not know --narrow", original_does_not_know_narrow),
        ('a list wider than a terminal of 30 columns says why its lines wrap', list_wider_than_thirty_columns_says_why),
        ('a card asked for in a narrower terminal is drawn, and says after it why its lines wrap', lambda: (lambda compact, wide: len(rows_of(compact[0])) == 23 and compact[2] == 't3-agents: the card is 80 columns wide,\nthis terminal is 60, so its lines wrap.\nLeave out --compact for what fits here.\n' and len(rows_of(wide[0])) == 39 and 'the card is 120 columns wide,\nthis terminal is 100, so its lines wrap.' in wide[2] and 'Leave out --wide' in wide[2])(terminal(['--recap', '--demo', '--compact'], size=(60, 24)), terminal(['--recap', '--demo', '--wide'], size=(100, 40)))),
        ('a card that fits its terminal, or goes into a pipe, says nothing after it', lambda: terminal(['--recap', '--demo', '--compact'], size=big)[2] == '' and terminal(['--recap', '--demo', '--wide'], size=(120, 24))[2] == '' and terminal(['--recap', '--demo'], size=(80, 24))[2] == '' and piped(['--recap', '--demo', '--wide']).stderr == ''),
        ('--wide with --compact, and either without --recap, are usage errors', lambda: all((piped(args).returncode == 2 for args in (['--recap', '--wide', '--compact'], ['--wide'], ['--compact'])))),
        ("the original's command line does not know --recap", lambda: (lambda r: r.returncode == 2 and "Unknown option '--recap'" in r.stderr)(piped(['--recap', '--demo'], {'T3_AGENTS_NO_THEMES': '1'}))),
        ('--month is the last full calendar month: October 2026, on 23 rows of 80 cells or fewer', lambda: (lambda month_plain: (lambda rows: len(rows) == 23 and all((len(row) <= 80 for row in rows)) and rows[0].startswith(' T3 Agents  RECAP  October 2026'))(month_plain.stdout.rstrip('\n').split('\n')))(piped(["--recap", "--month", "--demo"]))),
        ("--month --wide is 39 rows of 120 cells or fewer, with a calendar and each week's hours", lambda: (lambda month_wide_plain: (lambda rows: len(rows) == 39 and all((len(row) <= 120 for row in rows)) and rows[8].startswith(' AGENTS AT WORK, BY DAY') and rows[9].startswith(' Mon    Tue    Wed    Thu    Fri    Sat    Sun        week'))(month_wide_plain.stdout.rstrip('\n').split('\n')))(piped(["--recap", "--month", "--demo", "--wide"]))),
        ("a month's JSON says it is a month, has a value for each of its 31 days, and the days add up to its hours", lambda: (lambda month_json: month_json['period'] == 'month' and month_json['days'] == 31 and (len(month_json['by_day']) == 31) and (len(month_json['by_hour']) == 31) and (abs(sum(month_json['by_day']) - month_json['hours']) < 1e-06))(json.loads(piped(["--recap", "--month", "--demo", "--json"]).stdout))),
        ("a week's JSON says it is a week, and has seven days", lambda: (lambda week_json: week_json['period'] == 'week' and len(week_json['by_day']) == 7 and (week_json['days'] == 7))(json.loads(piped(["--recap", "--demo", "--json"]).stdout))),
        ('in JSON a day before the records begin, or one that has not come, is null and not zero', lambda: (lambda thin_json: thin_json['by_day'][0] is None and thin_json['by_day'][1] == 0 and (thin_json['by_day'][10] > 0) and all((day is None for day in thin_json['by_day'][18:])) and (thin_json['by_hour'][0] is None) and (thin_json['by_hour'][29] is None) and (thin_json['days_active'] == 1))(json.loads(piped(["--recap", "--this-month", "--db", thin, "--json"]).stdout))),
        ('--this-month says so far, and on the wide card where it stops', lambda: 'so far · 17 days recorded' in piped(['--recap', '--this-month', '--db', thin]).stdout and 'so far, to Nov 18 · 17 days recorded' in piped(['--recap', '--this-month', '--db', thin, '--wide']).stdout),
        ('--month on an empty database says there is nothing in the month, or in this one so far', lambda: (lambda r: r.returncode == 0 and 'No agent runs recorded in October 2026, or this month so far.' in r.stdout)(piped(['--recap', '--month', '--db', empty]))),
        ('--month with no records in it shows this month so far', lambda: 'November 2026' in piped(['--recap', '--month', '--db', thin]).stdout),
        ('two periods at once, and --month without --recap, are usage errors', lambda: all((piped(args).returncode == 2 for args in (['--recap', '--month', '--this-month'], ['--recap', '--this-week', '--month'], ['--month'], ['--this-month'])))),
        ("the original's command line does not know --month", lambda: (lambda r: r.returncode == 2 and 'Unknown option' in r.stderr)(piped(['--recap', '--month', '--demo'], {'T3_AGENTS_NO_THEMES': '1'}))),
        ('made-up data asked for at 3 on a Monday morning ends: nothing yet this week, and the month up to the day before', lambda: (lambda r: r.returncode == 0 and 'No agent runs recorded Nov 16 – 22 so far.' in r.stdout)(piped(['--recap', '--this-week', '--demo'], now=EARLY)) and (lambda r: r['returncode'] == 0 and r['days_elapsed'] == 16 and (r['by_day'][14] > 0) and (r['by_day'][15] == 0))((lambda run: {'returncode': run.returncode, **json.loads(run.stdout)})(piped(['--recap', '--this-month', '--demo', '--json'], now=EARLY))))
    ]
    failed = False
    for label, check in checks:
        start = len(outputs)
        try:
            ok = check()
        except (Exception, SystemExit) as exc:
            ok = False
            print(f"  {type(exc).__name__}: {exc}", file=sys.stderr)
        print(f"{'pass' if ok else 'FAIL'}  {label}", flush=True)
        if not ok:
            failed = True
            for args, (out, code, err) in (outputs[start:] or outputs):
                print(f"  {args!r}: exit {code}\n  stdout: {out!r}\n  stderr: {err!r}", file=sys.stderr)
    return int(failed)

if __name__ == "__main__":
    scratch = tempfile.mkdtemp(prefix="recap-check-")
    try:
        sys.exit(main(scratch))
    finally:
        shutil.rmtree(scratch)
