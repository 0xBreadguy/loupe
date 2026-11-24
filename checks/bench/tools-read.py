#!/usr/bin/env python3
"""Time labelled read commands on one synthetic Tools lab, fairly and read-only.

Each command must contain {db} and {now}; substitutions use shlex, never a shell.
Commands warm up once, then run for at least five trials in alternating order.
Every child receives a new empty T3CODE_HOME.  Executable identities and mtimes
are checked before and after.  Wall time is spawn through process exit, including
startup, JSON formatting and printing (stdout is captured).  The shared file
cache is warm; a cold-cache measurement is deliberately not claimed.

With --cold, before every warmup and trial the database and its -wal and -shm
are fsynced and dropped from the page cache with posix_fadvise(DONTNEED), and
mincore(2) reports how many pages were resident before and after.  A run whose
file is still resident fails rather than being reported as cold.

A command may be `env NAME=value PROGRAM ...` to set an environment variable
for that label; the executable checked before and after is PROGRAM.

    python3 checks/bench/tools-read.py --db checks/generated/tools-lab/week.sqlite \
      --now-ms 1794816000000 --trials 5 \
      recap='target/release/loupe-term --recap --json --db {db} --now-ms {now}' \
      recap_again='target/release/loupe-term --db {db} --now-ms {now} --json --recap'

Add --payload-cost to alternate the two Python sqlite3 queries described in
the lab's notes: the Tools read with and without length($.input), over every
run in the lab.  --out saves the complete report, including every trial.
"""

import argparse
import ctypes
import hashlib
import json
import mmap
import os
from pathlib import Path
import shlex
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
GENERATED = ROOT / "checks/generated/tools-read"
ITEMS = "orchestration_v2_projection_turn_items"
RUNS = "orchestration_v2_projection_runs"
TOOL_TYPES = "('command_execution','dynamic_tool','file_change','file_search','web_search')"
FIELDS = "type, status, json_extract(payload_json,'$.startedAt'), json_extract(payload_json,'$.completedAt')"


def protected(path):
    path = path.expanduser().resolve()
    # T3's attached worktree may be below ~/.t3/worktrees; its ignored generated
    # directory contains only synthetic labs.  No other ~/.t3 path is accepted.
    if os.environ.get("T3CODE_HOME"):
        home = Path(os.environ["T3CODE_HOME"]).expanduser().resolve()
        if path == home or home in path.parents:
            return home
    generated = (ROOT / "checks/generated").resolve()
    if generated == path or generated in path.parents:
        return None
    homes = [Path("~/.t3").expanduser()]
    for home in homes:
        home = home.resolve()
        if path == home or home in path.parents:
            return home
    return None


def fingerprint(path):
    stat = path.stat()
    return {"path": str(path), "device": stat.st_dev, "inode": stat.st_ino,
            "bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns}


def executable(argv):
    name = argv[0]
    index = 0
    if name == "env":
        # `env NAME=value PROGRAM ...` names its program after the assignments;
        # that program is the one whose identity must not change.
        index = 1
        while index < len(argv) and "=" in argv[index] and not argv[index].startswith("-"):
            index += 1
        if index >= len(argv):
            raise RuntimeError(f"env needs a program: {argv}")
        name = argv[index]
    if "/" in name:
        path = Path(name)
        path = (ROOT / path).resolve() if not path.is_absolute() else path.resolve()
    else:
        found = shutil.which(name)
        if not found:
            raise RuntimeError(f"executable not found: {name}")
        path = Path(found).resolve()
    if not path.is_file():
        raise RuntimeError(f"not an executable file: {path}")
    return path


PAGE = os.sysconf("SC_PAGE_SIZE")
LIBC = ctypes.CDLL(None, use_errno=True)
LIBC.mmap.restype = ctypes.c_void_p
LIBC.mmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_long]
LIBC.munmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
LIBC.mincore.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_char_p]


def resident_pages(path):
    """How many of a file's pages are in the page cache, by mincore(2).

    The file is mapped read-only so mincore has an address to inspect; the
    mapping itself does not fault pages in.  PAGE is the system page size.
    """
    size = path.stat().st_size
    if size == 0:
        return 0, 0
    pages = (size + PAGE - 1) // PAGE
    handle = os.open(path, os.O_RDONLY)
    try:
        address = LIBC.mmap(None, size, mmap.PROT_READ, mmap.MAP_PRIVATE, handle, 0)
    finally:
        os.close(handle)
    if address in (None, 0, ctypes.c_void_p(-1).value):
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(path))
    try:
        vector = ctypes.create_string_buffer(pages)
        if LIBC.mincore(ctypes.c_void_p(address), ctypes.c_size_t(size), vector) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error), str(path))
        return sum(1 for byte in vector.raw if byte & 1), pages
    finally:
        LIBC.munmap(ctypes.c_void_p(address), ctypes.c_size_t(size))


def evict(path):
    """fsync, then ask the kernel to drop a file's clean pages.

    A file's owner may do this without root: posix_fadvise(POSIX_FADV_DONTNEED)
    drops clean pages, and an fsync first makes sure none are still dirty.
    """
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(fd)


def cold_start(database):
    """Evict the database and its WAL/SHM, reporting residency before and after."""
    observation = []
    for suffix in ("", "-wal", "-shm"):
        path = Path(str(database) + suffix)
        if not path.is_file() or path.stat().st_size == 0:
            continue
        before, pages = resident_pages(path)
        evict(path)
        after, pages = resident_pages(path)
        observation.append({"file": path.name, "bytes": path.stat().st_size, "pages": pages,
                            "resident_before": before, "resident_after": after})
    return observation


def summary(values):
    return {"median_s": statistics.median(values), "min_s": min(values), "max_s": max(values),
            "median_ms": statistics.median(values) * 1000,
            "range_ms": [min(values) * 1000, max(values) * 1000]}


def loadavg():
    return Path("/proc/loadavg").read_text().strip()


def invoke(label, argv, home_root):
    home = Path(tempfile.mkdtemp(prefix=f"home-{label}-", dir=home_root))
    env = os.environ.copy()
    env["T3CODE_HOME"] = str(home)
    started = time.perf_counter()
    try:
        proc = subprocess.run(argv, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        elapsed = time.perf_counter() - started
    finally:
        shutil.rmtree(home)
    if proc.returncode:
        raise RuntimeError(f"{label} exited {proc.returncode}: {proc.stderr.decode(errors='replace')[:1200]}")
    return {"label": label, "seconds": elapsed, "exit": proc.returncode,
            "stdout_bytes": len(proc.stdout), "stdout_sha256": hashlib.sha256(proc.stdout).hexdigest(),
            "stderr_bytes": len(proc.stderr)}


def payload_pass(connection, run_ids, query):
    cursor = connection.cursor()
    started = time.perf_counter()
    rows = 0
    checksum = 0
    for run_id in run_ids:
        result = cursor.execute(query, (run_id,)).fetchall()
        rows += len(result)
        checksum += sum(len(row) for row in result)
        checksum += sum(row[-1] or 0 for row in result if isinstance(row[-1], int))
    return time.perf_counter() - started, rows, checksum


def payload_cost(database, trials, home_root):
    """Two queries, over all run-attached calls, on a warm read-only connection: with and without the command line's length."""
    home = Path(tempfile.mkdtemp(prefix="home-payload-cost-", dir=home_root))
    old_home = os.environ.get("T3CODE_HOME")
    os.environ["T3CODE_HOME"] = str(home)
    try:
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        connection.execute("PRAGMA query_only=ON")
        run_ids = [row[0] for row in connection.execute(f"SELECT run_id FROM {RUNS} ORDER BY run_id")]
        queries = {
            "without_input": f"SELECT {FIELDS} FROM {ITEMS} WHERE run_id=? AND type IN {TOOL_TYPES}",
            "with_input": f"SELECT {FIELDS}, length(json_extract(payload_json,'$.input')) FROM {ITEMS} WHERE run_id=? AND type IN {TOOL_TYPES}",
        }
        for label in ("without_input", "with_input"):
            _, rows, checksum = payload_pass(connection, run_ids, queries[label])
            print(f"payload warmup {label}: rows={rows} checksum={checksum}", flush=True)
        results = {label: [] for label in queries}
        observations = []
        for trial in range(trials):
            order = list(queries) if trial % 2 == 0 else list(reversed(queries))
            for label in order:
                seconds, rows, checksum = payload_pass(connection, run_ids, queries[label])
                results[label].append(seconds)
                observations.append({"trial": trial + 1, "order": order, "label": label,
                                     "seconds": seconds, "rows": rows, "checksum": checksum})
                print(f"payload trial {trial + 1} {label}: {seconds:.6f} s rows={rows}", flush=True)
        connection.close()
    finally:
        if old_home is None:
            os.environ.pop("T3CODE_HOME", None)
        else:
            os.environ["T3CODE_HOME"] = old_home
        shutil.rmtree(home)
    return {"runs": len(run_ids), "rows_per_pass": rows, "queries": queries,
            "sqlite_version": sqlite3.sqlite_version, "trials": observations,
            "summary": {label: summary(values) for label, values in results.items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", required=True, help="existing synthetic lab; protected T3 homes are refused")
    parser.add_argument("--now-ms", required=True, help="clock substituted for {now}")
    parser.add_argument("--trials", type=int, default=5, help="timed trials per label (minimum 5)")
    parser.add_argument("--cold", action="store_true",
                        help="evict the database and its WAL/SHM before every warmup and trial, verified with mincore")
    parser.add_argument("--payload-cost", action="store_true", help="also compare the two sqlite3 payload queries")
    parser.add_argument("--out", help="optional JSON report path")
    parser.add_argument("commands", nargs="+", metavar="LABEL=COMMAND", help="at least two; each needs {db} and {now}")
    args = parser.parse_args()
    if args.trials < 5:
        parser.error("--trials must be at least 5")
    if len(args.commands) < 2:
        parser.error("at least two labelled commands are required")
    database = Path(args.db).expanduser().resolve()
    blocked = protected(database)
    if blocked:
        parser.error(f"refusing protected T3 home {blocked}")
    if not database.is_file():
        parser.error(f"lab does not exist: {database}")

    labels = []
    commands = {}
    for entry in args.commands:
        if "=" not in entry:
            parser.error(f"expected LABEL=COMMAND, got {entry!r}")
        label, template = entry.split("=", 1)
        if not label or label in commands:
            parser.error(f"empty or duplicate label: {label!r}")
        if "{db}" not in template or "{now}" not in template:
            parser.error(f"{label}: command must contain both {{db}} and {{now}}")
        rendered = template.format(db=str(database), now=args.now_ms)
        argv = shlex.split(rendered)
        if not argv:
            parser.error(f"{label}: empty command")
        commands[label] = {"template": template, "argv": argv, "executable": str(executable(argv))}
        labels.append(label)

    GENERATED.mkdir(parents=True, exist_ok=True)
    report = {"database": str(database), "database_bytes": database.stat().st_size,
              "now_ms": int(args.now_ms), "trials_per_label": args.trials,
              "timed_region": ("spawn through exit; page cache evicted before every run with "
                              "fsync + posix_fadvise(DONTNEED), verified with mincore; stdout captured"
                              if args.cold else
                              "spawn through exit; warm file cache; stdout captured"),
              "cold": bool(args.cold),
              "load_before": os.getloadavg(), "loadavg_before": loadavg(),
              "commands": commands, "warmups": [], "trials": []}
    print(f"loadavg before: {report['loadavg_before']}", flush=True)
    before = {label: fingerprint(Path(commands[label]["executable"])) for label in labels}
    results = {label: [] for label in labels}

    def observe(label, phase, **extra):
        cold = cold_start(database) if args.cold else None
        observed = invoke(label, commands[label]["argv"], home_root)
        if cold is not None:
            observed["cache_eviction"] = cold
            observed["cold"] = phase
            resident = ", ".join(f"{entry['file']} {entry['resident_after']}/{entry['pages']} pages resident"
                                 for entry in cold)
            print(f"{phase} {label}: after eviction {resident}", flush=True)
            if any(entry["resident_after"] for entry in cold):
                raise RuntimeError(f"{label}: the page cache did not let go of {database.name}; "
                                   f"these are not cold trials")
        observed.update(extra)
        return observed

    with tempfile.TemporaryDirectory(prefix="run-", dir=GENERATED) as home_root:
        for label in labels:
            observed = observe(label, "warmup")
            report["warmups"].append(observed)
            print(f"warmup {label}: {observed['seconds']:.6f} s", flush=True)
        for trial in range(args.trials):
            order = labels if trial % 2 == 0 else list(reversed(labels))
            for label in order:
                observed = observe(label, "trial", trial=trial + 1, order=order)
                report["trials"].append(observed)
                results[label].append(observed["seconds"])
                print(f"trial {trial + 1} {label}: {observed['seconds']:.6f} s", flush=True)
        if args.payload_cost:
            report["payload_cost"] = payload_cost(database, args.trials, home_root)
    after = {label: fingerprint(Path(commands[label]["executable"])) for label in labels}
    report["executables_before"] = before
    report["executables_after"] = after
    report["executables_unchanged"] = before == after
    report["summary"] = {label: summary(values) for label, values in results.items()}
    report["load_after"] = os.getloadavg()
    report["loadavg_after"] = loadavg()
    print(f"loadavg after: {report['loadavg_after']}", flush=True)
    if not report["executables_unchanged"]:
        raise RuntimeError("an executable changed during the measurement; discard these timings")
    for label in labels:
        values = results[label]
        stat = report["summary"][label]
        print(f"{label}: median {stat['median_s']:.6f} s; range {stat['min_s']:.6f}..{stat['max_s']:.6f} s; "
              f"trials {[round(v, 6) for v in values]}", flush=True)
    if args.payload_cost:
        for label, stat in report["payload_cost"]["summary"].items():
            print(f"payload {label}: median {stat['median_s']:.6f} s; range "
                  f"{stat['min_s']:.6f}..{stat['max_s']:.6f} s", flush=True)
    text = json.dumps(report, indent=2) + "\n"
    if args.out:
        output = Path(args.out).expanduser().resolve()
        blocked = protected(output)
        if blocked:
            parser.error(f"refusing report under protected T3 home {blocked}")
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(text)
        print(f"report {output}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, sqlite3.Error) as error:
        sys.exit(str(error))
