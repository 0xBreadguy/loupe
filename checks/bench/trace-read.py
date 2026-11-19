#!/usr/bin/env python3
"""Trace's read cost, old build against new, on made-up databases.

Two measurements, each as alternating rounds whose order rotates:

  cold   one read of a task, repeated on one connection (the cost of opening Trace and of
         every refresh that rereads everything); the round's figure is the median of its reads
  watch  the live watch at the dashboard's 2 s period for SECONDS, as CPU milliseconds per
         refresh. On the synthetic running task: nothing writing; another thread written
         five times a second (a busy machine); the task itself growing by one step a second.
         On a finished task: nothing writing; a busy machine

The databases come from checks/scripts/scale-db.ts with the running task of
checks/bench/trace-live.py added. Nothing reads ~/.t3; the watch settings run on a copy.

  python3 -I checks/bench/trace-read.py --base OLD_BENCH --new NEW_BENCH --dbs FOLDER \\
      [--rounds 5] [--reads 10] [--seconds 20] [--only cold|watch]

OLD_BENCH and NEW_BENCH are builds of checks/bench/trace-read against the old and the new
core. Not measured here: the dashboard's drawing (checks/bench/trace-live.py), memory.
"""
import argparse, json, os, shutil, sqlite3, statistics, subprocess, sys, threading, time
from contextlib import closing
from pathlib import Path

P = "orchestration_v2_projection_"

def finished_targets(db):
    """Select finished runs with many and few items from the generated scale tier."""
    uri = Path(db).resolve().as_uri() + "?mode=ro"
    with closing(sqlite3.connect(uri, uri=True)) as con:
        rows = con.execute(f"""
            SELECT r.run_id, count(*) AS steps
            FROM {P}runs r JOIN {P}turn_items i ON i.run_id=r.run_id
            WHERE r.completed_at IS NOT NULL AND NOT EXISTS (
                SELECT 1 FROM {P}runs active
                WHERE active.thread_id=r.thread_id AND active.completed_at IS NULL
            )
            GROUP BY r.run_id ORDER BY steps, r.run_id
        """).fetchall()
    if len(rows) < 2:
        raise ValueError("the synthetic fixture needs at least two finished runs with items")
    return "run:" + rows[-1][0], "run:" + rows[0][0]


def cold(bench, db, target, reads):
    out = subprocess.run([bench, "cold", db, target, str(reads)], capture_output=True, text=True, check=True).stdout.split()
    return statistics.median(float(x) for x in out)


def writer(db, kind, target, stop):
    """Writes as T3 would while the watch runs: another thread, or the task growing."""
    con = sqlite3.connect(db, timeout=5)
    con.execute("PRAGMA journal_mode=WAL")
    busy_thread = None
    if kind == "busy":
        target_type, target_id = target.split(":", 1)
        selected = target_id if target_type == "thread" else con.execute(
            f"SELECT thread_id FROM {P}runs WHERE run_id=?", (target_id,)
        ).fetchone()[0]
        busy_thread = con.execute(
            f"SELECT thread_id FROM {P}threads WHERE thread_id<>? ORDER BY thread_id LIMIT 1",
            (selected,),
        ).fetchone()[0]
    n = 0
    while not stop.is_set():
        now = time.time() * 1000
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(now / 1000)) + ".%03dZ" % (int(now) % 1000)
        if kind == "busy":
            con.execute(f"UPDATE {P}threads SET updated_at=? WHERE thread_id=?", (stamp, busy_thread))
            delay = 0.2
        else:
            n += 1
            body = json.dumps({"startedAt": stamp, "completedAt": stamp, "input": "git status", "output": "made up " * 100})
            con.execute(f"INSERT INTO {P}turn_items(turn_item_id,thread_id,run_id,node_id,provider_thread_id,provider_turn_id,parent_item_id,ordinal,type,status,updated_at,payload_json) "
                        "VALUES(?,'big','big-1',NULL,'pt-big',NULL,NULL,?,'command_execution','completed',?,?)", (f"grow-{os.getpid()}-{n}", 100_000 + n, stamp, body))
            delay = 1.0
        con.commit()
        stop.wait(delay)
    con.close()


def watch(bench, db, scratch, kind, target, seconds):
    copy = os.path.join(scratch, "watch.sqlite")
    for suffix in ("", "-wal", "-shm"):
        if os.path.exists(copy + suffix):
            os.remove(copy + suffix)
    shutil.copyfile(db, copy)
    stop = threading.Event()
    kind = kind.split()[-1]
    w = threading.Thread(target=writer, args=(copy, kind, target, stop)) if kind != "quiet" else None
    if w:
        w.start()
    try:
        out = subprocess.run([bench, "watch", copy, target, str(seconds)], capture_output=True, text=True, check=True).stdout.split()
    finally:
        stop.set()
        if w:
            w.join()
    fields = dict(zip(out[::2], out[1::2]))
    return float(fields["cpu_ms_per_refresh"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True)
    ap.add_argument("--new", required=True)
    ap.add_argument("--dbs", required=True)
    ap.add_argument("--scratch", default=None)
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--reads", type=int, default=10)
    ap.add_argument("--seconds", type=float, default=20)
    ap.add_argument("--only", choices=["cold", "watch"])
    args = ap.parse_args()
    arms = {"base": os.path.abspath(args.base), "new": os.path.abspath(args.new)}
    scratch = args.scratch or args.dbs
    finished, small = finished_targets(os.path.join(args.dbs, "real.sqlite"))
    running = "thread:big"
    cold_cases = [(tier, running) for tier in ("xxl", "xl", "real")] + [("real", finished), ("real", small)]
    watch_cases = [(kind, running) for kind in ("quiet", "busy", "growing")] + [
        ("ended quiet", finished), ("ended busy", finished),
    ]
    results = {}
    print(f"load {os.getloadavg()[0]:.1f} on {os.cpu_count()} cores", flush=True)
    for r in range(args.rounds):
        order = list(arms) if r % 2 == 0 else list(arms)[::-1]
        if args.only != "watch":
            for tier, target in cold_cases:
                for arm in order:
                    ms = cold(arms[arm], os.path.join(args.dbs, tier + ".sqlite"), target, args.reads)
                    results.setdefault(f"cold {tier} {target}", {}).setdefault(arm, []).append(ms)
                    print(f"round {r + 1} cold {tier:4s} {target:16s} {arm:4s} {ms:8.3f} ms", flush=True)
        if args.only != "cold":
            for kind, target in watch_cases:
                for arm in order:
                    ms = watch(arms[arm], os.path.join(args.dbs, "real.sqlite"), scratch, kind, target, args.seconds)
                    results.setdefault(f"watch {target} {kind}", {}).setdefault(arm, []).append(ms)
                    print(f"round {r + 1} watch {kind:11s} {arm:4s} {ms:8.3f} ms CPU per refresh", flush=True)
    print(f"load {os.getloadavg()[0]:.1f}")
    print()
    summary = {}
    for name, by_arm in results.items():
        row = {arm: {"median": round(statistics.median(v), 3), "min": round(min(v), 3), "max": round(max(v), 3), "n": len(v)} for arm, v in by_arm.items()}
        summary[name] = row
        b, n = row["base"]["median"], row["new"]["median"]
        print(f"{name:34s} base {b:8.3f} ({row['base']['min']:.3f}..{row['base']['max']:.3f})  new {n:8.3f} ({row['new']['min']:.3f}..{row['new']['max']:.3f})  {n / b:5.2f}x")
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
