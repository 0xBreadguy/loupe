#!/usr/bin/env python3
"""Soak the incremental reader against the full reader under an endless, randomized write stream.

One run copies a lab database, starts `scale-db.ts --stream SEED --serve` on the copy, and
starts two `loupe-core --bench-reader-poll` processes on it: a timed reader (no self-check,
so its poll time and resident memory are the reader's own) and a checking reader (`--self-check`:
every poll also runs the full reader in the same transaction and says whether the two agree).
Each round asks the stream for one write (now and then several) and then polls both readers at
the clock the stream reports, so time passes in the poll too: the stream's "time-passes" shape
writes nothing and the poll that follows sees only the clock move.

Per poll it records how the poll was served, the fallback reason, the poll time (timed reader,
`Reader::poll` alone: no writes, no self-check), and the write shapes behind it. It samples the
timed reader's resident memory with the database's row counts every --sample polls. Poll time by
shape uses polls that followed exactly one write of that shape; polls that followed several are
their own group ("multi").

After a poll that follows a restore-older-file step it also asks a fresh process for a full read of
the file and compares its agent count with the timed reader's (a "restore witness"): the checking
reader's own full read goes through the same kept-open connection, so it cannot see a connection left
on a deleted -shm. A disagreement counts as a mismatch.

On the first mismatch (or a reader error) it stops and writes a reproducer directory: the seed,
tier, poll and step, the shapes, the schedule seed, a consistent copy of the database as the
checking reader saw it, the snapshot the reader served (LOUPE_DUMP_MISMATCH), the full
reader's snapshot of the same file, the first differing paths, and the last writes. With
--continue-after-mismatch it only counts them.

The schedule (how many writes each poll covers) comes from --schedule-seed (default derived from
--seed) and nothing else, so a run is reproducible: same base database, same seed, same schedule
gives the same writes and the same poll sequence. `--replay DIR` re-runs a reproducer's run up to
its failing step and keeps the database; `--stop-at-step N` does the same for any run.

Synthetic data only: the readers are always given --db on a lab copy, and T3CODE_HOME points at an
empty directory inside the work directory. Nothing under the real home is read.
The legacy tier name "real" selects the skewed synthetic workload. Regenerate an existing
base with the current scale-db.ts before establishing a new baseline.

  python3 checks/bench/reader-soak.py --tier real --seed 1 --writes 20000
  python3 checks/bench/reader-soak.py --tier m --seed 2 --writes 5000 --exclude prune-log
  python3 checks/bench/reader-soak.py --replay checks/generated/soak/out/real-1/repro
"""
import argparse, json, os, random, shutil, sqlite3, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NODE = ["node", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]
SCALE_DB = ROOT / "checks/scripts/scale-db.ts"
PREFIX = "orchestration_v2_projection_"


def nearest(values, p):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, -(-len(ordered) * p // 100) - 1)]


def pct(values):
    return {"n": len(values), "p50": nearest(values, 50), "p90": nearest(values, 90), "p99": nearest(values, 99),
            "max": max(values) if values else None, "mean": (sum(values) / len(values)) if values else None}


class Stream:
    """scale-db.ts --stream --serve: one line in (a step count), step records and a batch line out."""

    def __init__(self, db, seed, exclude, include, env, log):
        argv = NODE + [str(SCALE_DB), "--db", str(db), "--stream", str(seed), "--serve"]
        if exclude:
            argv += ["--exclude", ",".join(exclude)]
        if include:
            argv += ["--include", ",".join(include)]
        self.p = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True, cwd=ROOT)
        ready = self.read()
        if not ready.get("ready"):
            raise RuntimeError(f"stream did not start: {ready}")
        self.start_step = ready["step"]
        self.now_ms = ready["now_ms"]

    def read(self):
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError(f"stream exited ({self.p.poll()})")
        return json.loads(line)

    def steps(self, k):
        self.p.stdin.write(f"{k}\n")
        self.p.stdin.flush()
        records = []
        while True:
            record = self.read()
            if "batch" in record:
                return records, record
            records.append(record)

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=30)
        except Exception:
            self.p.kill()


class Reader:
    def __init__(self, binary, db, now_ms, self_check, env):
        argv = [str(binary), "--bench-reader-poll", "--db", str(db), "--now-ms", str(int(now_ms))] + (["--self-check"] if self_check else [])
        self.p = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def poll(self, now_ms):
        self.p.stdin.write(f"{int(now_ms)}\n")
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        if not line:
            self.p.wait(timeout=10)
            raise RuntimeError("reader exited: " + self.p.stderr.read()[-600:])
        return json.loads(line)

    def rss_mib(self):
        try:
            for line in open(f"/proc/{self.p.pid}/status"):
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) / 1024
        except OSError:
            return None

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=60)
        except Exception:
            self.p.kill()


def witness(native, db, now_ms, env):
    """Agent count of a full read by a fresh process: no connection the readers hold is involved."""
    p = subprocess.run([str(native), "--db", str(db), "--now-ms", str(int(now_ms)), "--dump-snapshot"], capture_output=True, text=True, env=env)
    if p.returncode != 0:
        return None
    return len(json.loads(p.stdout)["agents"])


def counts(db):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=10)
    try:
        return {t: c.execute(f"SELECT COUNT(*) FROM {PREFIX}{t}").fetchone()[0] for t in ("threads", "runs", "nodes", "turn_items", "subagents")}
    finally:
        c.close()


def diff_paths(a, b, path, out, cap=25):
    if len(out) >= cap or a == b:
        return
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            diff_paths(a.get(k), b.get(k), f"{path}.{k}" if path else k, out, cap)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append(f"{path}: length {len(a)} (reader) != {len(b)} (full)")
        for i in range(min(len(a), len(b))):
            diff_paths(a[i], b[i], f"{path}[{i}]", out, cap)
    else:
        out.append(f"{path}: {json.dumps(a)[:120]} (reader) != {json.dumps(b)[:120]} (full)")


def save_repro(out, work, db, args, state, reason, native, env, dump):
    repro = out / "repro"
    shutil.rmtree(repro, ignore_errors=True)
    repro.mkdir(parents=True)
    # A consistent copy of the file as the readers last saw it (the backup API reads under a snapshot).
    src = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=30)
    dst = sqlite3.connect(repro / "state.sqlite")
    src.backup(dst)
    dst.execute("PRAGMA journal_mode=DELETE")
    dst.close()
    src.close()
    if (work / "environment-id").exists():
        shutil.copy(work / "environment-id", repro / "environment-id")
    for f in sorted(dump.glob("mismatch-*.json")):
        shutil.copy(f, repro / f.name)
    paths = []
    served = next(iter(sorted(repro.glob("mismatch-*.json"))), None)
    full = subprocess.run([str(native), "--db", str(repro / "state.sqlite"), "--now-ms", str(int(state["now_ms"])), "--dump-snapshot"],
                          capture_output=True, text=True, env=env)
    if full.returncode == 0:
        (repro / "full-reader-snapshot.json").write_text(full.stdout)
        if served:
            try:
                diff_paths(json.loads(served.read_text()), json.loads(full.stdout), "", paths)
            except Exception as e:  # keep the reproducer even if the diff cannot be built
                paths.append(f"diff failed: {e}")
    info = {
        "reason": reason, "tier": args.tier, "seed": args.seed, "schedule_seed": args.schedule_seed, "base": str(args.base),
        "exclude": args.exclude, "include": args.include, "poll": state["poll"], "step_from": state["step_from"], "step_to": state["step_to"],
        "stop_at_step": state["step_to"], "shapes": state["shapes"], "now_ms": state["now_ms"], "writes_so_far": state["writes"],
        "first_differing_paths": paths, "poll_record": state["record"], "last_writes": state["recent"][-40:],
        "replay": f"python3 checks/bench/reader-soak.py --replay {repro}",
        "by_hand": ["copy the base database and environment-id", f"node ... scale-db.ts --db COPY --stream {args.seed} --count N",
                    "or open state.sqlite here and run loupe-core --bench-reader-poll --self-check --db state.sqlite --now-ms NOW"],
    }
    (repro / "repro.json").write_text(json.dumps(info, indent=2) + "\n")
    return repro


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tier", choices=["real", "m", "s", "l"], default="real")
    ap.add_argument("--seed", type=int, default=1, help="the stream's seed")
    ap.add_argument("--schedule-seed", type=int, help="how many writes each poll covers (default: derived from --seed)")
    ap.add_argument("--writes", type=int, default=1000, help="committed writes to apply (time-passes steps are extra)")
    ap.add_argument("--base", type=Path, default=ROOT / "checks/generated/soak/base", help="directory holding <tier>.sqlite and environment-id")
    ap.add_argument("--out", type=Path, help="result directory (default checks/generated/soak/out/<tier>-<seed>)")
    ap.add_argument("--native", type=Path, default=ROOT / "target/release/loupe-core")
    ap.add_argument("--exclude", default="", help="comma-separated shapes to leave out of the stream")
    ap.add_argument("--include", default="", help="comma-separated off-by-default shapes to put in")
    ap.add_argument("--sample", type=int, default=100, help="sample resident memory and row counts every N polls")
    ap.add_argument("--continue-after-mismatch", action="store_true")
    ap.add_argument("--stop-at-step", type=int, help="stop after this stream step (a replay), keeping the working database")
    ap.add_argument("--keep", action="store_true", help="keep the working database at the end")
    ap.add_argument("--replay", type=Path, help="re-run a reproducer directory's run up to its failing step")
    ap.add_argument("--work", type=Path, help="working directory (default <out>/work)")
    args = ap.parse_args()
    if args.replay:
        info = json.loads((args.replay / "repro.json").read_text())
        args.tier, args.seed, args.schedule_seed = info["tier"], info["seed"], info["schedule_seed"]
        args.exclude, args.include = ",".join(info["exclude"]), ",".join(info["include"])
        args.base = Path(info["base"])
        args.stop_at_step = info["stop_at_step"]
        args.writes = 10**9
        args.keep = True
        args.out = args.out or args.replay.parent / "replay"
        args.continue_after_mismatch = True
    args.exclude = [s for s in args.exclude.split(",") if s]
    args.include = [s for s in args.include.split(",") if s]
    if args.schedule_seed is None:
        args.schedule_seed = args.seed * 7919 + 1
    out = args.out or ROOT / f"checks/generated/soak/out/{args.tier}-{args.seed}"
    work = args.work or out / "work"
    shutil.rmtree(out, ignore_errors=True)
    work.mkdir(parents=True)
    source = args.base / f"{args.tier}.sqlite"
    if not source.exists():
        print(f"missing {source}; build it with checks/scripts/scale-db.ts --tier {args.tier} --schema t3 --db {source}", file=sys.stderr)
        return 2
    db = work / "state.sqlite"
    shutil.copy(source, db)
    shutil.copy(args.base / "environment-id", work / "environment-id")
    home = work / "home"  # T3CODE_HOME: empty, so nothing outside the lab copy can be found
    home.mkdir()
    dump = work / "dump"
    dump.mkdir()
    env = {**os.environ, "T3CODE_HOME": str(home), "T3_STREAM_TIMING": "1"}
    env.pop("LOUPE_READER_POLL_PROFILE", None)
    reader_env = {k: v for k, v in env.items() if k != "T3_STREAM_TIMING"}
    stream_log = open(out / "stream.stderr", "w")
    stream = Stream(db, args.seed, args.exclude, args.include, env, stream_log)
    timed = checked = None
    schedule = random.Random(args.schedule_seed)
    writes_log = open(out / "writes.jsonl", "w")
    polls_log = open(out / "polls.jsonl", "w")
    polls, recent, samples = [], [], []
    started = time.time()
    mismatches, disagreements, writes, step, errors = 0, 0, 0, stream.start_step, []
    witnessed, witness_failures = 0, 0
    first_bad = None
    last_now = stream.now_ms
    batch_records = []
    stream_ms = []
    try:
        timed = Reader(args.native, db, stream.now_ms, False, reader_env)
        checked = Reader(args.native, db, stream.now_ms, True, {**reader_env, "LOUPE_DUMP_MISMATCH": str(dump)})
        now = stream.now_ms
        first = timed.poll(now)
        verdict = checked.poll(now)
        samples.append({"poll": 0, "writes": 0, "rss_mib": timed.rss_mib(), "checked_rss_mib": checked.rss_mib(), **counts(db)})
        index = 0
        while writes < args.writes and not (args.stop_at_step is not None and step >= args.stop_at_step):
            roll = schedule.random()
            k = 1 if roll < 0.86 else schedule.randint(2, 5) if roll < 0.96 else schedule.randint(6, 12)
            if args.stop_at_step is not None:
                k = min(k, args.stop_at_step - step)
            batch_records, batch = None, None
            records, batch = stream.steps(k)
            batch_records = records
            index += 1
            for r in records:
                recent.append(r)
                stream_ms.append(r.get("ms", 0))
                writes_log.write(json.dumps(r) + "\n")
                writes += r["writes"]
            recent = recent[-200:]
            step = batch["step"]
            now = last_now = batch["now_ms"]
            a = timed.poll(now)
            b = checked.poll(now)
            shapes = [r["shape"] for r in records]
            row = {"poll": index, "step_from": records[0]["step"], "step_to": records[-1]["step"], "shapes": shapes, "now_ms": now,
                   "served": a["served"], "fallback": a["fallback_reason"], "poll_ms": a["poll_ms"], "rows_read": a["rows_read"],
                   "threads_reread": a["threads_reread"], "agents": a["agents"], "mismatch": bool(b["mismatch"]),
                   "checked_served": b["served"], "checked_fallback": b["fallback_reason"]}
            restored = "restore-older-file" in shapes
            if restored:
                seen = witness(args.native, db, now, reader_env)
                witnessed += 1
                row["witness_agents"] = seen
                if seen != a["agents"] or seen != b["agents"]:
                    witness_failures += 1
                    row["witness_failed"] = True
                    print(f"RESTORE WITNESS at poll {index}: fresh full read {seen} agents, timed reader {a['agents']}, checking reader {b['agents']}", flush=True)
            polls.append(row)
            polls_log.write(json.dumps(row) + "\n")
            if (b["served"], b["fallback_reason"]) != (a["served"], a["fallback_reason"]):
                disagreements += 1
            if b["mismatch"] or row.get("witness_failed"):
                mismatches += 1
                if first_bad is None:
                    first_bad = {"poll": index, "step_from": row["step_from"], "step_to": row["step_to"], "shapes": shapes, "now_ms": now,
                                 "writes": writes, "record": row, "recent": list(recent)}
                    print(f"MISMATCH at poll {index}, steps {row['step_from']}..{row['step_to']}, shapes {shapes}", flush=True)
                    if not args.continue_after_mismatch:
                        break
            if index % args.sample == 0:
                samples.append({"poll": index, "writes": writes, "rss_mib": timed.rss_mib(), "checked_rss_mib": checked.rss_mib(), **counts(db)})
            if index % 500 == 0:
                inc = sum(1 for p in polls if p["served"] == "Incremental")
                print(f"poll {index}: {writes} writes, step {step}, mismatches {mismatches}, incremental {inc}/{len(polls)}, "
                      f"rss {samples[-1]['rss_mib']:.0f} MiB, {time.time() - started:.0f} s", flush=True)
        samples.append({"poll": len(polls), "writes": writes, "rss_mib": timed.rss_mib(), "checked_rss_mib": checked.rss_mib(), **counts(db)})
        final_clock = now
    except Exception as e:
        errors.append(str(e)[:900])
        print(f"ERROR: {errors[-1]}", flush=True)
        if first_bad is None:
            # The reader failed on the poll that followed the last batch: that batch is the reproducer.
            last = batch_records or []
            first_bad = {"poll": len(polls) + 1, "step_from": last[0]["step"] if last else step, "step_to": last[-1]["step"] if last else step,
                         "shapes": [r["shape"] for r in last], "now_ms": last_now if last else stream.now_ms, "writes": writes,
                         "record": {"error": errors[-1], "records": last}, "recent": list(recent)}
        final_clock = None
    finally:
        for r in (timed, checked):
            if r:
                try:
                    r.p.stdin.close()
                except Exception:
                    pass
        stream.close()
        for r in (timed, checked):
            if r:
                try:
                    r.p.wait(timeout=60)
                except Exception:
                    r.p.kill()
        writes_log.close()
        polls_log.close()

    repro = None
    if first_bad is not None and not args.replay:
        repro = save_repro(out, work, db, args, first_bad, "reader error: " + errors[-1] if errors else "self-check mismatch", args.native, reader_env, dump)

    # ── the report ──
    by_served, by_reason, by_shape_reason = {}, {}, {}
    by_shape, by_served_ms = {}, {}
    for p in polls:
        by_served[p["served"]] = by_served.get(p["served"], 0) + 1
        by_served_ms.setdefault(p["served"], []).append(p["poll_ms"])
        shape = p["shapes"][0] if len(p["shapes"]) == 1 else "multi"
        by_shape.setdefault(shape, []).append(p["poll_ms"])
        if p["fallback"]:
            by_reason[p["fallback"]] = by_reason.get(p["fallback"], 0) + 1
            by_shape_reason.setdefault(shape, {})
            by_shape_reason[shape][p["fallback"]] = by_shape_reason[shape].get(p["fallback"], 0) + 1
    # Which write shapes' polls were served how.
    served_by_shape = {}
    for p in polls:
        shape = p["shapes"][0] if len(p["shapes"]) == 1 else "multi"
        served_by_shape.setdefault(shape, {})
        served_by_shape[shape][p["served"]] = served_by_shape[shape].get(p["served"], 0) + 1
    changed = [p for p in polls if p["served"] != "Skipped"]
    n = len(polls)
    inc = by_served.get("Incremental", 0)
    mem = [s for s in samples if s["rss_mib"] is not None]
    report = {
        "tier": args.tier, "seed": args.seed, "schedule_seed": args.schedule_seed, "exclude": args.exclude, "include": args.include,
        "binary": str(args.native), "writes": writes, "stream_steps": step - stream.start_step, "polls": n, "mismatches": mismatches,
        "reader_vs_checker_disagreements": disagreements, "restore_witness_checks": witnessed, "restore_witness_failures": witness_failures,
        "errors": errors, "elapsed_s": round(time.time() - started, 1),
        "served_without_a_full_read_share": (n - by_served.get("Full", 0)) / n if n else None,
        "served": by_served, "incremental_share_of_all_polls": inc / n if n else None,
        "incremental_share_of_polls_that_were_not_skipped": inc / len(changed) if changed else None,
        "fallback_reasons": dict(sorted(by_reason.items(), key=lambda kv: -kv[1])),
        "fallback_reasons_by_shape": by_shape_reason, "served_by_shape": served_by_shape,
        "poll_ms": pct([p["poll_ms"] for p in polls]), "poll_ms_by_served": {k: pct(v) for k, v in sorted(by_served_ms.items())},
        "poll_ms_by_shape": {k: pct(v) for k, v in sorted(by_shape.items())},
        "memory_mib": {"start": mem[0]["rss_mib"] if mem else None, "middle": mem[len(mem) // 2]["rss_mib"] if mem else None,
                       "end": mem[-1]["rss_mib"] if mem else None, "peak": max((s["rss_mib"] for s in mem), default=None),
                       "samples": samples},
        "stream_generation_ms": pct([m for m in stream_ms if m]), "first_failure": first_bad and {k: first_bad[k] for k in ("poll", "step_from", "step_to", "shapes")},
        "repro": str(repro) if repro else None, "stopped_at_step": args.stop_at_step, "final_now_ms": final_clock,
    }
    # Memory against rows: a fit of resident size on turn-item count over the second half of the run.
    half = mem[len(mem) // 2:]
    if len(half) >= 4 and half[-1]["turn_items"] != half[0]["turn_items"]:
        report["memory_mib"]["mib_per_1000_items_second_half"] = (half[-1]["rss_mib"] - half[0]["rss_mib"]) / ((half[-1]["turn_items"] - half[0]["turn_items"]) / 1000)
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    shapes_line = {k: v["p50"] for k, v in report["poll_ms_by_shape"].items()}
    print(json.dumps({k: report[k] for k in ("tier", "seed", "writes", "stream_steps", "polls", "mismatches", "restore_witness_checks",
                                                "restore_witness_failures", "elapsed_s", "served", "served_without_a_full_read_share",
                                                "incremental_share_of_all_polls", "incremental_share_of_polls_that_were_not_skipped",
                                                "fallback_reasons", "poll_ms")}, indent=1))
    print("poll ms p50 by shape:", json.dumps(shapes_line, sort_keys=True))
    print("memory MiB start/middle/end/peak:", {k: (round(v, 1) if v else v) for k, v in report["memory_mib"].items() if k != "samples"})
    if repro:
        print("reproducer:", repro)
    if not args.keep and not args.replay:
        shutil.rmtree(work, ignore_errors=True)
    elif args.replay:
        print("working database kept at", db)
    return 1 if (mismatches or errors) else 0


if __name__ == "__main__":
    sys.exit(main())
