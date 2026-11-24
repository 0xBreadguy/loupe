"""Shared harness for the open-asks integration tests (tests/scripts/asks_*.py).

Everything here is synthetic. A test run owns one scratch folder, builds its databases there
with T3's recorded schema (checks/fixtures/t3-schema.sql, read only), and runs the built
program against them with a curated environment. It never touches a real T3 Code database,
the installed program, /tmp, or a desktop.

Environment (set by tests/scripts/asks.sh, or by hand):
    TMPDIR, CARGO_TARGET_DIR, TMUX_TMPDIR   folders under ~/.cache, not /tmp
    T3CODE_HOME                              an empty folder under ~/.cache
    LOUPE_BIN, LOUPE_CORE_BIN        optional; default to the release build in CARGO_TARGET_DIR
    ASKS_KEEP=1                              keep the run's scratch folder for inspection
"""
import hashlib
import json
import os
import queue
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import threading
import unittest
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HOME = Path.home().resolve()
CACHE = HOME / ".cache"
NOW_MS = 1794484800000  # 2026-11-12T12:00:00Z, the clock the reader's own tests use
P = "orchestration_v2_projection_"

# One id per run, so a marker from an earlier run, or from a different test, can never match.
RUN = uuid.uuid4().hex[:12]
MSG_MARK = f"MSG-{RUN}"      # opens every synthetic assistant and human message
BODY_MARK = "zebraquartz"    # in every message body: a second token, in case the first is cut off
PRIV_MARK = f"PRIV-{RUN}"    # opens every other private string: card labels, urls, payload bodies


# ── environment ─────────────────────────────────────────────────────────────

def _under_cache(name):
    value = os.environ.get(name)
    if not value:
        raise SystemExit(f"{name} is not set; run tests/scripts/asks.sh (it sets the scratch environment)")
    path = Path(value).resolve()
    if HOME not in path.parents:
        raise SystemExit(f"{name}={path} must be under {HOME}")
    if Path("/tmp") in path.parents or path == Path("/tmp"):
        raise SystemExit(f"{name} must not be under /tmp")
    return path


def environment():
    """Check the scratch environment and return the folders the tests use."""
    paths = {n: _under_cache(n) for n in ("TMPDIR", "CARGO_TARGET_DIR", "TMUX_TMPDIR", "T3CODE_HOME")}
    home = paths["T3CODE_HOME"]
    if not home.is_dir() or any(home.iterdir()):
        raise SystemExit(f"T3CODE_HOME={home} must be an existing, empty folder")
    if home == HOME / ".t3" or (HOME / ".t3") in home.parents:
        raise SystemExit("T3CODE_HOME must not be the real T3 folder")
    return paths


_work = None


def work():
    """This run's scratch folder: one new folder inside TMPDIR, removed at the end."""
    global _work
    if _work is None:
        _work = environment()["TMPDIR"] / f"asks-{RUN}"
        _work.mkdir(mode=0o700)
        (_work / "home").mkdir()
    return _work


def cleanup():
    if _work is not None and os.environ.get("ASKS_KEEP") != "1":
        shutil.rmtree(_work, ignore_errors=True)


def binary(kind="term"):
    name, var = (("loupe-term", "LOUPE_BIN") if kind == "term"
                 else ("loupe-core", "LOUPE_CORE_BIN"))
    path = Path(os.environ.get(var) or environment()["CARGO_TARGET_DIR"] / "release" / name)
    if not path.is_file():
        raise SystemExit(f"{path} is missing; build it first (tests/scripts/asks.sh does)")
    return path


def child_env(**extra):
    """The environment of every program run here, built from nothing. HOME is empty scratch, so
    a default path can never reach a real account; XDG folders are not passed at all."""
    env = {
        "HOME": str(work() / "home"),
        "PATH": "/usr/bin:/bin",
        "T3CODE_HOME": str(environment()["T3CODE_HOME"]),
        "TMPDIR": str(environment()["TMPDIR"]),
        "SQLITE_TMPDIR": str(environment()["TMPDIR"]),
        "LOUPE_NO_THEMES": "1",
        "TERM": "xterm-256color",
        "LANG": "C.UTF-8",
        "NO_COLOR": "1",
    }
    env.update(extra)
    return env


def run(args, kind="term", timeout=60, stdin=None, **env):
    """Run the built program; returns (returncode, stdout, stderr) as text."""
    p = subprocess.run([str(binary(kind)), *args], env=child_env(**env), input=stdin, text=True,
                       capture_output=True, timeout=timeout, errors="replace", cwd=work())
    return p.returncode, p.stdout, p.stderr


# crates/core/src/status.rs sets INTERVAL to 2s. The watcher re-reads when the
# database or its -wal file changes, and otherwise at RECHECK_MS (30s). A wait
# in this harness is one poll period, repeated a bounded number of times.
WATCH_POLL_S = 2.0


class StatusWatch:
    """One built `loupe-term --status --watch --db` process.

    This is the installed entry's real watch loop, not `--probe-status-db`.
    Stop it by the pid captured at start. A recycled pid whose command line is
    not this process is left alone.
    """

    def __init__(self, db_path):
        self.db = Path(db_path)
        self.proc = None
        self.threads = []
        self.stdout = []
        self._stderr = []
        self._lines = queue.Queue()
        command = [str(binary("term")), "--status", "--watch", "--db", str(self.db)]
        self.proc = subprocess.Popen(command, env=child_env(), stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, cwd=work())
        self.pid = self.proc.pid
        try:
            self.threads = [
                threading.Thread(target=self._pump_out, daemon=True),
                threading.Thread(target=self._pump_err, daemon=True),
            ]
            for thread in self.threads:
                thread.start()
        except Exception:
            self.stop()
            raise

    def _pump_out(self):
        for raw in iter(self.proc.stdout.readline, b""):
            text = raw.decode(errors="replace")
            self.stdout.append(text)
            self._lines.put(text)

    def _pump_err(self):
        data = self.proc.stderr.read()
        if data:
            self._stderr.append(data.decode(errors="replace"))

    def stderr(self):
        return "".join(self._stderr)

    def output(self):
        return "".join(self.stdout) + self.stderr()

    def alive(self):
        return self.proc is not None and self.proc.poll() is None and self.proc.pid == self.pid

    def next_line(self, polls=3):
        """The next stdout line, waiting at most `polls` poll periods."""
        for _ in range(polls):
            try:
                return self._lines.get(timeout=WATCH_POLL_S)
            except queue.Empty:
                if not self.alive():
                    raise AssertionError(f"watcher {self.pid} exited {self.proc.returncode}: {self.stderr()}")
        raise AssertionError(f"watcher {self.pid} emitted nothing within {polls} poll periods; stderr={self.stderr()!r}")

    def collect(self, polls):
        """Lines that arrive within `polls` poll periods. Empty means the line did not change."""
        found = []
        for _ in range(polls):
            try:
                found.append(self._lines.get(timeout=WATCH_POLL_S))
            except queue.Empty:
                if not self.alive():
                    raise AssertionError(f"watcher {self.pid} exited {self.proc.returncode}: {self.stderr()}")
        return found

    def _cmdline(self):
        try:
            return Path(f"/proc/{self.pid}/cmdline").read_bytes()
        except FileNotFoundError:
            return None

    def stop(self):
        proc = self.proc
        if proc is None:
            return
        if proc.poll() is None:
            command = self._cmdline()
            if command is not None:
                expected = (b"loupe-term", b"--status", b"--watch", os.fsencode(str(self.db)))
                if not all(part in command for part in expected):
                    raise RuntimeError(f"refusing to signal {self.pid}: {command!r}")
                os.kill(self.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=WATCH_POLL_S)
                except subprocess.TimeoutExpired:
                    if proc.poll() is None and self._cmdline() is not None:
                        os.kill(self.pid, signal.SIGKILL)
                        proc.wait(timeout=WATCH_POLL_S)
        for thread in self.threads:
            thread.join(timeout=WATCH_POLL_S)
        for pipe in (proc.stdout, proc.stderr):
            if pipe is not None and not pipe.closed:
                pipe.close()


# ── leak detection ──────────────────────────────────────────────────────────

_ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")


def normalized(text):
    """What a reader of the output could see: escape sequences gone, JSON strings decoded."""
    text = _ANSI.sub("", text)
    seen = [text]
    for line in text.splitlines():
        line = line.strip()
        if line[:1] in "{[":
            try:
                seen.append(json.dumps(json.loads(line), ensure_ascii=False))
            except ValueError:
                pass
    return "\n".join(seen)


def leaks(text, extra=()):
    """The private tokens found in `text`, case-insensitively, after decoding. Empty means clean."""
    hay = normalized(text).lower()
    return [t for t in (MSG_MARK, BODY_MARK, PRIV_MARK, *extra) if t.lower() in hay]


# ── synthetic T3 databases ──────────────────────────────────────────────────

SECRET_ITEM_STATUS = {"pending": "waiting", "saved": "completed", "declined": "cancelled", "cancelled": "cancelled"}
LINK_ITEM_STATUS = {"pending": "waiting", "linked": "completed", "failed": "failed",
                    "declined": "cancelled", "cancelled": "cancelled"}
T0 = "2026-11-12T11:30:00.000Z"
CARD_AT = "2026-11-12T11:31:00.000Z"


def stamp(minute, second=0):
    return f"2026-11-12T11:{minute:02d}:{second:02d}.000Z"


class Fx:
    """One synthetic database. Every assistant and human message gets MSG_MARK, a counter and
    BODY_MARK at its start; every other private string starts with PRIV_MARK."""

    def __init__(self, name, messages=True, wal=False, schema_tail=""):
        folder = work() / "db"
        folder.mkdir(exist_ok=True)
        self.path = folder / f"{name}.sqlite"
        if self.path.exists():
            raise AssertionError(f"fixture {name} built twice")
        self.db = sqlite3.connect(self.path, isolation_level=None)
        self.db.executescript((REPO / "checks/fixtures/t3-schema.sql").read_text())
        if wal:
            self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at)"
                        " VALUES('p','Synthetic','/synthetic','[]','','')")
        self.db.execute(f"INSERT INTO {P}metadata VALUES('thread-projections',1,0,'')")
        if messages:
            self.db.executescript(f"CREATE TABLE {P}messages(message_id TEXT PRIMARY KEY, thread_id TEXT,"
                                  f" role TEXT, created_at TEXT, updated_at TEXT, streaming INTEGER, payload_json TEXT);"
                                  f"CREATE INDEX synthetic_messages_by_thread ON {P}messages(thread_id);")
        if schema_tail:
            self.db.executescript(schema_tail)
        self.n = 0
        self.texts = []

    def seq(self):
        self.n += 1
        return self.n

    def event(self, tid, kind="synthetic"):
        """An event on a thread, with the private prompt T3 keeps in its log, and the feed cursor moved."""
        n = self.seq()
        self.db.execute(f"INSERT INTO {P.replace('projection_', '')}events(event_id,thread_id,event_type,occurred_at,payload_json)"
                        " VALUES(?,?,?,'',?)", (f"{tid}-ev{n}", tid, kind, json.dumps({"prompt": f"{PRIV_MARK}-prompt"})))
        self.db.execute(f"UPDATE {P}metadata SET last_sequence=(SELECT max(sequence) FROM {P.replace('projection_', '')}events)"
                        " WHERE projection_name='thread-projections'")

    def thread(self, tid, title=None, archived=False, deleted=False):
        body = {"id": tid, "projectId": "p", "title": title or f"Thread {tid}", "prompt": f"{PRIV_MARK}-prompt"}
        self.db.execute(f"INSERT INTO {P}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,"
                        "created_at,updated_at,archived_at,deleted_at,payload_json)"
                        " VALUES(?,'p','Synthetic','codex','full-access','default','','',?,?,?)",
                        (tid, T0 if archived else None, T0 if deleted else None, json.dumps(body)))
        self.event(tid)
        return tid

    def run_row(self, tid, rid, status, ordinal=1, completed=None):
        done = completed if completed is not None else (stamp(40) if status in ("completed", "failed", "cancelled") else None)
        self.db.execute(f"INSERT INTO {P}runs(run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json)"
                        " VALUES(?,?,?,'codex',?,'',?,?)",
                        (rid, tid, ordinal, status, done, json.dumps({"id": rid, "threadId": tid, "status": status, "startedAt": T0})))
        self.event(tid)
        return rid

    def item(self, tid, iid, kind, status, run=None, node="n", payload=None, at=CARD_AT):
        self.db.execute(f"INSERT INTO {P}turn_items(turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)"
                        " VALUES(?,?,?,?,?,?,?,?,?)", (iid, tid, run, node, self.seq(), kind, status, at, json.dumps(payload or {})))
        self.event(tid)

    def working(self, tid, run, at=None):
        """A command in progress, so a thread that 'keeps working' is observably doing so.

        `at` sets the item time when the caller needs it inside the watcher's wall clock.
        The default stays the recorded card time.
        """
        when = CARD_AT if at is None else at
        self.item(tid, f"{tid}-cmd", "command_execution", "running", run=run, node="n2", at=when,
                  payload={"input": "cargo test", "title": "Test", "startedAt": T0 if at is None else when})

    def request(self, tid, rid, kind="user_input", status="pending", capability="message", node="root",
                resolved=None, raw=None):
        payload = raw if raw is not None else json.dumps(
            {"responseCapability": {"type": capability, "providerSessionId": f"{PRIV_MARK}-session"},
             "questions": [f"{PRIV_MARK}-question"], "answer": f"{PRIV_MARK}-answer"})
        self.db.execute(f"INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,resolved_at,payload_json)"
                        " VALUES(?,?,?,?,?,?,?,?)", (rid, tid, node, kind, status, stamp(31), resolved, payload))
        self.event(tid)

    def secret(self, tid, iid, run, secret_status="pending", node="n", started=CARD_AT):
        self.item(tid, iid, "secret_request", SECRET_ITEM_STATUS[secret_status], run=run, node=node, at=started,
                  payload={"label": f"{PRIV_MARK}-secret-label", "reason": f"{PRIV_MARK}-reason", "url": f"{PRIV_MARK}-url",
                           "placeholder": f"{PRIV_MARK}-ph", "secretStatus": secret_status, "startedAt": started})

    def link(self, tid, iid, run, link_status="pending", node="n", started=CARD_AT, with_started=True):
        payload = {"url": f"https://{PRIV_MARK}.invalid/pair", "reason": f"{PRIV_MARK}-reason", "label": f"{PRIV_MARK}-link-label",
                   "hint": f"{PRIV_MARK}-hint", "linkStatus": link_status}
        if with_started:
            payload["startedAt"] = started
        self.item(tid, iid, "link_request", LINK_ITEM_STATUS[link_status], run=run, node=node, at=started, payload=payload)

    def message(self, tid, role, at, created_by="user", body="Which option should I pick, and when?", extra=None, mid=None):
        """A human (role user) or assistant message. The text always carries the markers."""
        n = self.seq()
        text = f"{MSG_MARK}-{n:04d} {BODY_MARK} {body}"
        payload = {"text": text}
        if created_by is not None:
            payload["createdBy"] = created_by
        payload.update(extra or {})
        self.db.execute(f"INSERT INTO {P}messages VALUES(?,?,?,?,?,0,?)", (mid or f"m{n}", tid, role, at, at, json.dumps(payload)))
        if role == "assistant":
            self.item(tid, f"am{n}", "assistant_message", "completed", node="n3", at=at, payload={"text": text})
        self.texts.append(text)
        self.event(tid, "message.updated")
        return text

    def close(self):
        self.db.close()
        return self.path


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def folder_state(path):
    """Names, sizes and content hashes in a folder: proves a run created, changed or removed nothing."""
    return {p.name: (p.stat().st_size, sha(p)) for p in sorted(Path(path).iterdir()) if p.is_file()}


def status_json(db_path, now=NOW_MS, **env):
    code, out, err = run(["--status", "--db", str(db_path), "--now-ms", str(now)], **env)
    assert code == 0 and not err, (code, out, err)
    return json.loads(out), out


def tmux_capture(name, args, cols=140, rows=44, settle=2.5):
    """The dashboard's own screen, in tmux on a private socket whose server this function starts and stops."""
    import time
    sock = f"asks-{RUN}-{name}"
    env = child_env()
    command = "exec " + " ".join(["env", "-i", *[f"{k}={v}" for k, v in env.items()], str(binary()), *map(str, args)])
    tmux = ["tmux", "-L", sock, "-f", "/dev/null"]
    tenv = {"PATH": "/usr/bin:/bin", "HOME": env["HOME"], "TMUX_TMPDIR": str(environment()["TMUX_TMPDIR"]), "TERM": "xterm-256color"}
    subprocess.run([*tmux, "new-session", "-d", "-x", str(cols), "-y", str(rows), "-s", "c", command], env=tenv, check=True)
    try:
        time.sleep(settle)
        shot = subprocess.run([*tmux, "capture-pane", "-t", "c", "-e", "-p", "-N"], env=tenv, text=True, capture_output=True, errors="replace")
        return shot.stdout
    finally:
        subprocess.run([*tmux, "kill-server"], env=tenv, capture_output=True)


class Base(unittest.TestCase):
    pass


def main(module_name):
    """Entry point for one test file: run it, print a gate summary, exit 0 / 1."""
    try:
        program = unittest.main(module=module_name, exit=False, verbosity=2)
    finally:
        cleanup()
    result = program.result
    print(f"\n{module_name}: {result.testsRun - len(result.skipped)} ran, "
          f"{len(result.skipped)} skipped, {len(result.failures)} failed, {len(result.errors)} errors")
    sys.exit(0 if result.wasSuccessful() else 1)
