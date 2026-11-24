#!/usr/bin/env python3
"""Benchmark Recap end to end on a private synthetic WAL database (Linux, with cc)."""

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import sqlite3
import statistics
import subprocess
import time
from contextlib import closing

P = "orchestration_v2_projection_"
SCHEMA = Path(__file__).resolve().parents[1] / "fixtures" / "t3-schema-main-096.sql"
NOW = "1795069560000"
# Midnight in Los Angeles after the last full week at NOW.
WEEK_END = dt.datetime(2026, 11, 16, 8, tzinfo=dt.timezone.utc).timestamp()
ITEMS = 31


def iso(seconds):
    """Spell fixture times the way T3 does."""
    return dt.datetime.fromtimestamp(seconds, dt.timezone.utc).isoformat(
        timespec="milliseconds"
    ).replace("+00:00", "Z")


def compact(value):
    """Encode strict JSON with no insignificant whitespace."""
    return json.dumps(value, separators=(",", ":"))


def build_database(db, days, payload_bytes):
    """Write about 580 runs per week, with synthetic tools, requests, and cards."""
    db.executescript(SCHEMA.read_text())
    assert db.execute("PRAGMA journal_mode=WAL").fetchone() == ("wal",)
    db.execute("PRAGMA wal_autocheckpoint=0")
    start = WEEK_END - days * 86400
    run_count = days * 83
    threads = max(1, run_count // 5)
    rng = random.Random(7)
    for n in range(threads):
        db.execute(
            f"""INSERT INTO {P}threads (thread_id, project_id, title, default_provider,
                runtime_mode, interaction_mode, created_at, updated_at, payload_json)
                VALUES (?, ?, '', 'codex', '', '', ?, ?, ?)""",
            (f"t{n}", f"p{n % 12}", iso(start), iso(start),
             compact({"projectId": f"p{n % 12}", "createdAt": iso(start), "pullRequests": []})),
        )

    item_sql = f"""INSERT INTO {P}turn_items (turn_item_id, thread_id, run_id, node_id,
        ordinal, type, status, updated_at, payload_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"""
    for i in range(run_count):
        a = start + i * (86400 / 83)
        b = a + rng.uniform(120, 1000)
        run, thread, node = f"r{i}", f"t{rng.randrange(threads)}", f"n{i}"
        db.execute(
            f"""INSERT INTO {P}runs (run_id, thread_id, ordinal, provider, status,
                requested_at, completed_at, payload_json) VALUES (?, ?, ?, 'codex', 'completed', ?, ?, ?)""",
            (run, thread, i, iso(a), iso(b), compact({"startedAt": iso(a + 2),
             "status": "completed", "providerInstanceId": "codex", "modelSelection": {"model": "gpt-6.1-sol"}})),
        )
        for j in range(ITEMS):
            s = a + 5 + j * (b - a - 10) / ITEMS
            kind = ("command_execution", "dynamic_tool", "reasoning", "file_change")[j % 4] if j else "user_message"
            fields = {"startedAt": iso(s), "completedAt": iso(s + 3), "updatedAt": iso(s + 3), "type": kind}
            if j == 0:
                fields.update(createdBy="user", creationSource="web")
            fields["output"] = ""
            fields["output"] = "x" * (payload_bytes - len(compact(fields)))
            payload = compact(fields)
            assert len(payload) == payload_bytes
            db.execute(item_sql, (f"i{i}-{j}", thread, run, node, i * 100 + j, kind, "completed", iso(s + 3), payload))

        if i % 40 == 0:
            kind = "user_input" if i % 80 else "command_approval"
            request = f"q{i}"
            db.execute(
                f"""INSERT INTO {P}runtime_requests (runtime_request_id, thread_id, node_id,
                    kind, status, created_at, resolved_at, payload_json)
                    VALUES (?, ?, ?, ?, 'resolved', ?, ?, ?)""",
                (request, thread, node, kind, iso(a + 30), iso(a + 90),
                 compact({"responseCapability": {"type": "message"}}) if i % 120 == 0 else "{}"),
            )
            item = "user_input_request" if kind == "user_input" else "approval_request"
            db.execute(item_sql, (f"a{i}", thread, run, node, i * 100 + 50, item, "completed", iso(a + 60),
                                  compact({"startedAt": iso(a + 30), "completedAt": iso(a + 60), "requestId": request})))
        if i % 110 == 0:
            db.execute(item_sql, (f"s{i}", thread, run, node, i * 100 + 60, "secret_request", "completed",
                                  iso(a + 90), compact({"startedAt": iso(a + 40), "completedAt": iso(a + 90)})))
        if i % 900 == 0:
            db.execute(item_sql, (f"l{i}", thread, run, node, i * 100 + 70, "link_request", "completed",
                                  iso(b + 3600), compact({"startedAt": iso(a + 50), "completedAt": iso(b + 3600)})))
            db.execute(
                f"""INSERT INTO {P}messages (message_id, thread_id, role, streaming, created_at, updated_at, payload_json)
                    VALUES (?, ?, 'user', 0, ?, ?, '{{"createdBy":"user"}}')""",
                (f"m{i}", thread, iso(b + 1800), iso(b + 1800)),
            )
    db.commit()
    # Keep WAL mode and its sidecars, but avoid retaining a second copy of the data.
    assert db.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone() == (0, 0, 0)
    db.execute("PRAGMA user_version=1")
    return run_count


def build_meter(scratch):
    """Use a tiny parent for wait4 so the child's peak RSS excludes Python's footprint."""
    meter = scratch / "meter"
    source = r"""
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 3) return 125;
    struct timespec start, end;
    if (clock_gettime(CLOCK_MONOTONIC, &start)) return 125;
    pid_t child = fork();
    if (child < 0) return 125;
    if (child == 0) {
        execv(argv[2], &argv[2]);
        perror("execv");
        _exit(127);
    }
    int status;
    struct rusage usage;
    if (wait4(child, &status, 0, &usage) < 0) return 125;
    if (clock_gettime(CLOCK_MONOTONIC, &end)) return 125;
    FILE *stats = fopen(argv[1], "w");
    if (!stats) return 125;
    double elapsed = (end.tv_sec - start.tv_sec) * 1000.0 + (end.tv_nsec - start.tv_nsec) / 1e6;
    double cpu = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1000.0
               + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1000.0;
    fprintf(stats, "%.6f %.6f %.6f\n", elapsed, usage.ru_maxrss / 1024.0, cpu);
    if (fclose(stats)) return 125;
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return 128 + WTERMSIG(status);
}
"""
    subprocess.run(["cc", "-O2", "-x", "c", "-", "-o", meter], input=source, text=True, check=True)
    return meter


def invoke(meter, program, source, env):
    """Run the actual CLI and collect elapsed time and the child's resource usage."""
    stats = meter.with_suffix(".stats")
    result = subprocess.run(
        [meter, stats, program, "--recap", "--db", source, "--json", "--now-ms", NOW],
        env=env, stdin=subprocess.DEVNULL, capture_output=True,
    )
    if result.returncode:
        raise RuntimeError(f"{program} exited {result.returncode}: {result.stderr!r}")
    return result.stdout, result.stderr, tuple(map(float, stats.read_text().split()))


def benchmark(args, scratch, source, meter):
    """Alternate configurations and verify all timed outputs against the baseline."""
    modes = [
        ("before", args.before, {}),
        ("off/1", args.after, {"LOUPE_SKIM": "off", "LOUPE_READ_THREADS": "1"}),
        ("full/1", args.after, {"LOUPE_READ_THREADS": "1"}),
        ("full/4", args.after, {"LOUPE_READ_THREADS": "4"}),
        ("full/8", args.after, {"LOUPE_READ_THREADS": "8"}),
        ("head/4", args.after, {"LOUPE_SKIM": "head", "LOUPE_READ_THREADS": "4"}),
    ]
    empty = scratch / "empty"
    empty.mkdir()
    base_env = {"HOME": str(empty), "T3CODE_HOME": str(empty), "PATH": os.defpath,
                "TZ": "America/Los_Angeles"}
    uncached = {**base_env, "XDG_CACHE_HOME": str(scratch / "uncached")}
    baseline, stderr, _ = invoke(meter, args.before, source, uncached)
    assert not stderr
    report = json.loads(baseline)
    expected_runs = min(args.days, 7) * 83
    assert report["runs"] == expected_runs and report["tool_calls"] > expected_runs
    print(f"{expected_runs} runs, {report['tool_calls']} tool calls; JSON SHA-256 {hashlib.sha256(baseline).hexdigest()}")
    for index, (name, program, settings) in enumerate(modes):
        cache = str(scratch / f"verification-cache-{index}")
        env = {**base_env, **settings, "XDG_CACHE_HOME": cache, "LOUPE_CACHE": "1",
               "LOUPE_CACHE_STATS": "1"}
        for hits, writes in [(0, expected_runs), (expected_runs, 0)]:
            out, stderr, _ = invoke(meter, program, source, env)
            expected = f"loupe: cache: {hits} of {expected_runs} finished runs from the cache, {writes} written\n".encode()
            if out != baseline or stderr != expected:
                raise RuntimeError(f"{name} cache verification failed: {stderr!r}, expected {expected!r}")

    results = {(state, name): [] for state in ("off", "cold", "warm") for name, _, _ in modes}
    print(f"Load before trials (1/5/15 min): {os.getloadavg()}; allowed CPUs: {len(os.sched_getaffinity(0))}", flush=True)
    for trial in range(args.trials):
        ordered = modes if trial % 2 == 0 else modes[::-1]
        for index, (name, program, settings) in enumerate(ordered):
            out, stderr, stats = invoke(meter, program, source, {**uncached, **settings})
            if (out, stderr) != (baseline, b""):
                raise RuntimeError(f"trial {trial + 1}: {name} uncached output differs")
            results["off", name].append(stats)
            cache = str(scratch / f"trial-{trial}-cache-{index}")
            env = {**base_env, **settings, "XDG_CACHE_HOME": cache, "LOUPE_CACHE": "1"}
            for state in ("cold", "warm"):
                out, stderr, stats = invoke(meter, program, source, env)
                if (out, stderr) != (baseline, b""):
                    raise RuntimeError(f"trial {trial + 1}: {name} {state} output differs")
                results[state, name].append(stats)

    print(f"Load after trials (1/5/15 min): {os.getloadavg()}")
    print("| Cache | Mode | Median ms | Range ms | Peak RSS MiB | Median CPU ms |")
    print("|---|---|---:|---:|---:|---:|")
    for (state, name), samples in results.items():
        elapsed, rss, cpu = zip(*samples)
        print(f"| {state} | {name} | {statistics.median(elapsed):.1f} | "
              f"{min(elapsed):.1f}–{max(elapsed):.1f} | {max(rss):.1f} | {statistics.median(cpu):.1f} |")


def main():
    """Parse the fixture size and binary paths, then remove only our scratch folder."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path, help="baseline release executable")
    parser.add_argument("--after", type=Path, default=Path("target/release/loupe-term"))
    parser.add_argument("--payload-bytes", type=int, default=400, help="size of each regular item")
    parser.add_argument("--days", type=int, default=7, help="days of history before the last full week's end")
    parser.add_argument("--trials", type=int, default=7)
    args = parser.parse_args()
    if args.days < 1 or args.trials < 1 or args.payload_bytes < 256:
        parser.error("days and trials must be positive and payload size must be at least 256 bytes")
    args.before = args.before.resolve(strict=True)
    args.after = args.after.resolve(strict=True)
    scratch = Path(subprocess.check_output(["mktemp", "-d", "-t", "recap-bench-XXXXXXXX"], text=True).strip())
    try:
        meter = build_meter(scratch)
        source = scratch / "recap.sqlite"
        with closing(sqlite3.connect(source)) as db:
            runs = build_database(db, args.days, args.payload_bytes)
            print(f"{args.trials} alternating trials, {runs} history runs, {args.payload_bytes} bytes per regular item")
            # The store declines to prove file stamps near the filesystem clock's tick.
            time.sleep(0.025)
            benchmark(args, scratch, source, meter)
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    main()
