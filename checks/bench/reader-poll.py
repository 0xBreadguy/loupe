#!/usr/bin/env python3
"""What one poll of the incremental reader costs, next to the full reader.

For each tier this copies a real-schema lab database, starts `loupe-core
--bench-reader-poll` on the copy, and between polls applies synthetic writes with the lab's
mutator. It records how each poll was served and what it cost when nothing changed, when one
write landed, and when a burst touched 1% of threads. A second pass repeats the writes with
the reader's self-check on, where every poll also runs the full reader in the same
transaction: any disagreement is counted and fails the run.

The timed region is `Reader::poll` alone, on a kept-open connection. Excluded: process start,
the first full read, the writes themselves, and the self-check. Both readers use the fixed
synthetic epoch, so their SQL time windows match. Reports include the first blocking
fallback reason; later blockers remain masked. The full reader's cost comes
from `--bench-source` on the same file. Five additional live/full pairs alternate
on identical post-write states (full includes its own warmups and connection opening).
Use --profile for a separate instrumented pass. Phase scopes are inclusive: cap_ordering
is within cache_selection, and assembly contains structure/content scopes. Profiles never
include the self-check, writes, process startup, or the initial baseline in workload summaries.
"""
import argparse, json, os, shutil, statistics, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NOW = "1794484800000"  # same synthetic clock for the full and live readers
NODE = ["node", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]


def mutate(db, step, touch=None):
    cmd = NODE + [str(ROOT / "checks/scripts/scale-db.ts"), "--mutate", str(step), "--db", str(db)]
    if touch:
        cmd += ["--touch", str(touch)]
    p = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT)
    # Some steps need state the database no longer has (an open request, a free running thread).
    # The mutator's steps are sequential, so the stream ends there.
    if p.returncode != 0 and ("no open request" in p.stderr or "no running thread" in p.stderr):
        return False
    if p.returncode != 0:
        raise RuntimeError("\n".join(l for l in p.stderr.splitlines() if not l.startswith("    at "))[:600])
    return json.loads(p.stdout)


class Poller:
    def __init__(self, binary, db, self_check, profile=False, status_view=False):
        argv = [str(binary), "--bench-reader-poll", "--db", str(db), "--now-ms", NOW] + (["--self-check"] if self_check else [])
        env = os.environ.copy()
        env.pop("LOUPE_READER_POLL_PROFILE", None)
        env.pop("LOUPE_STATUS_ONLY", None)
        if profile:
            env["LOUPE_READER_POLL_PROFILE"] = "1"
        if status_view:
            # The status watcher's reader; its self-check compares with status_view of the full reader.
            env["LOUPE_STATUS_ONLY"] = "1"
        self.p = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def poll(self):
        self.p.stdin.write("\n")
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError("reader exited: " + self.p.stderr.read()[-300:])
        return json.loads(line)

    def rss_mib(self):
        for line in open(f"/proc/{self.p.pid}/status"):
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) / 1024
        return None

    def pss(self):
        return smaps(self.p.pid)

    def close(self):
        self.p.stdin.close()
        code = self.p.wait(timeout=30)
        if code:
            raise RuntimeError(f"reader exited {code}: " + self.p.stderr.read()[-300:])


def smaps(pid):
    """Proportional and resident set, MiB, from /proc/PID/smaps_rollup."""
    out = {}
    for line in open(f"/proc/{pid}/smaps_rollup"):
        parts = line.split()
        if parts[0] in ("Rss:", "Pss:", "Pss_Anon:", "Pss_File:"):
            out[parts[0][:-1].lower()] = int(parts[1]) / 1024
    return out


class Watcher:
    """The bar's status watcher state machine (`--status --probe-status-db`): one tick per
    stdin line at the given clock, answered with the last line it would have printed. The
    watcher re-reads only when the database file changed, as `--status --watch` does."""

    def __init__(self, binary, db, env_extra=None):
        env = os.environ.copy()
        env.pop("LOUPE_SYNC_READ", None)
        env.update(env_extra or {})
        self.p = subprocess.Popen([str(binary), "--status", "--probe-status-db", "--db", str(db)], env=env,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def tick(self):
        self.p.stdin.write(NOW + "\n")
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError("watcher exited: " + self.p.stderr.read()[-300:])
        return line.rstrip("\n")

    def close(self):
        self.p.stdin.close()
        code = self.p.wait(timeout=30)
        if code:
            raise RuntimeError(f"watcher exited {code}: " + self.p.stderr.read()[-300:])


def memory_trial(binary, source, env_dir, writes, tmp):
    """One fresh copy of a tier: the timed reader and the watcher read it while `writes`
    writes land (single writes until the mutator runs dry, then 1% bursts). PSS of both
    after their first read and after the last write."""
    work = Path(tempfile.mkdtemp(prefix="t3-reader-memory-", dir=tmp))
    try:
        db = work / "state.sqlite"
        shutil.copy(source, db)
        if (env_dir / "environment-id").exists():
            shutil.copy(env_dir / "environment-id", work / "environment-id")
        threads = int(subprocess.run(["sqlite3", str(db), "select count(*) from orchestration_v2_projection_threads"],
                                     capture_output=True, text=True).stdout.strip())
        reader = Poller(binary, db, False)
        watcher = Watcher(binary, db)
        first = reader.poll()
        watcher.tick()
        row = {"baseline": {"reader": reader.pss(), "watcher": smaps(watcher.p.pid), "served": first["served"]}}
        served, polls, step, single = {}, [], 0, True
        while step < writes:
            mutation = mutate(db, step + 1) if single else False
            if not mutation:
                single = False
                mutation = mutate(db, step + 1, touch=max(1, threads // 100))
            step += 1
            r = reader.poll()
            served[r["served"]] = served.get(r["served"], 0) + 1
            polls.append(r["poll_ms"])
            line = watcher.tick()
        row["after"] = {"reader": reader.pss(), "watcher": smaps(watcher.p.pid), "writes": step, "served": served,
                        "poll_ms_median": statistics.median(polls)}
        # The watcher's line must be the one a fresh full read prints for this file.
        oracle = subprocess.run([str(binary), "--status", "--db", str(db), "--now-ms", NOW],
                                capture_output=True, text=True).stdout.rstrip("\n")
        row["watcher_line_matches_full_read"] = line == oracle
        reader.close()
        watcher.close()
        return row
    finally:
        shutil.rmtree(work, ignore_errors=True)


def memory_main(args):
    """--memory: alternate the given binaries over --trials fresh copies of each tier."""
    tmp = Path(os.environ.get("TMPDIR") or tempfile.gettempdir())
    report = {"version": 1, "mode": "memory", "load_average_start": os.getloadavg(), "writes": args.writes,
              "trials": args.trials, "binaries": [str(b) for b in args.native], "tiers": {}, "errors": 0,
              "measure": "PSS from /proc/PID/smaps_rollup after the first read and after the last write"}
    for tier in args.tiers:
        source = args.dir / f"{tier}.sqlite"
        rows = {str(b): [] for b in args.native}
        for trial in range(args.trials):
            order = args.native if trial % 2 == 0 else list(reversed(args.native))
            for binary in order:
                row = memory_trial(binary, source, args.dir, args.writes, tmp)
                row["load_average"] = os.getloadavg()
                rows[str(binary)].append(row)
                if not row["watcher_line_matches_full_read"]:
                    report["errors"] += 1
                b, a = row["baseline"], row["after"]
                print(f"tier {tier} trial {trial + 1} {Path(binary).name}: reader pss {b['reader']['pss']:.1f} -> "
                      f"{a['reader']['pss']:.1f} MiB | watcher pss {b['watcher']['pss']:.1f} -> {a['watcher']['pss']:.1f} MiB | "
                      f"{a['writes']} writes {a['served']} | poll median {a['poll_ms_median']:.2f} ms | "
                      f"watcher line matches full read: {row['watcher_line_matches_full_read']}", flush=True)
        summary_rows = {}
        for binary, trials in rows.items():
            cell = {}
            for phase in ("baseline", "after"):
                for proc in ("reader", "watcher"):
                    values = [t[phase][proc]["pss"] for t in trials]
                    cell[f"{proc}_{phase}_pss"] = {"median": statistics.median(values), "min": min(values), "max": max(values)}
            summary_rows[binary] = cell
            print(f"tier {tier} {Path(binary).name} median [range] MiB: " + ", ".join(
                f"{k} {v['median']:.1f} [{v['min']:.1f}-{v['max']:.1f}]" for k, v in cell.items()), flush=True)
        report["tiers"][tier] = {"trials": rows, "summary": summary_rows}
    report["load_average_end"] = os.getloadavg()
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 1 if report["errors"] else 0


def summary(rows):
    values = [r["poll_ms"] for r in rows]
    profiles = [r["profile"] for r in rows if r.get("profile") is not None]
    phases = {}
    if profiles:
        for name in sorted(set().union(*(p["phases_ns"] for p in profiles))):
            samples = [p["phases_ns"].get(name, 0) / 1e6 for p in profiles]
            phases[name] = {"median_ms": statistics.median(samples), "min_ms": min(samples), "max_ms": max(samples)}
    for name in ("read_ms", "reconcile_ms"):
        samples = [r[name] for r in rows]
        phases[name.removesuffix("_ms")] = {"median_ms": statistics.median(samples), "min_ms": min(samples), "max_ms": max(samples)}
    return {"phases": phases, "median_ms": statistics.median(values), "min_ms": min(values), "max_ms": max(values), "polls": len(values),
            "served": {kind: sum(1 for r in rows if r["served"] == kind) for kind in sorted({r["served"] for r in rows})},
            "fallback_reasons": {reason: sum(r["fallback_reason"] == reason for r in rows)
                                 for reason in sorted({r["fallback_reason"] for r in rows if r["fallback_reason"] is not None})},
            "threads_reread_median": statistics.median(r["threads_reread"] for r in rows),
            "changed_agents_median": statistics.median(r["changed"] for r in rows)}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", type=Path, default=ROOT / "checks/generated/scale/t3")
    ap.add_argument("--tiers", nargs="*", default=["s", "m", "l"])
    ap.add_argument("--native", type=Path, action="append",
                    help="binary to measure; repeat with --memory to alternate several (default: the release build)")
    ap.add_argument("--writes", type=int, default=24)
    ap.add_argument("--bursts", type=int, default=5)
    ap.add_argument("--output", type=Path)
    ap.add_argument("--profile", action="store_true", help="opt-in phase timings; run separately from uninstrumented measurements")
    ap.add_argument("--memory", action="store_true",
                    help="PSS of the reader and the status watcher after the first read and after --writes writes")
    ap.add_argument("--trials", type=int, default=5, help="--memory trials per binary")
    ap.add_argument("--status-view", action="store_true",
                    help="run both readers as the status watcher's (live::status_view, no item text)")
    args = ap.parse_args()
    args.native = args.native or [ROOT / "target/release/loupe-core"]
    if args.memory:
        return memory_main(args)
    if len(args.native) > 1:
        ap.error("several --native binaries only with --memory")
    args.native = args.native[0]
    revision = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    report = {"version": 1, "revision": revision, "load_average_start": os.getloadavg(), "tiers": [], "errors": 0,
              "now_ms": int(NOW), "profile": args.profile, "status_view": args.status_view,
              "timed_region": "Reader::poll on a kept-open connection; excludes startup, first read, writes, self-check"}
    for tier in args.tiers:
        source = args.dir / f"{tier}.sqlite"
        if not source.exists():
            print(f"missing {source}; run checks/scripts/scale-db.ts --tier {tier} --schema t3", file=sys.stderr)
            return 2
        work = Path(tempfile.mkdtemp(prefix=f"t3-reader-poll-{tier}-"))
        try:
            db = work / "state.sqlite"
            shutil.copy(source, db)
            if (args.dir / "environment-id").exists():
                shutil.copy(args.dir / "environment-id", work / "environment-id")
            threads = int(subprocess.run(["sqlite3", str(db), "select count(*) from orchestration_v2_projection_threads"],
                                         capture_output=True, text=True).stdout.strip())
            full = json.loads(subprocess.run([str(args.native), "--bench-source", "5", "--db", str(db), "--now-ms", NOW],
                                             capture_output=True, text=True).stdout)
            if full.get("errors", 0):
                raise RuntimeError(f"full reader reported errors: {full}")
            row = {"full_reader": {k: full[k] for k in ("median_ms", "min_ms", "max_ms", "operations", "errors")}, "tier": tier, "threads": threads, "full_reader_ms_per_read": full.get("ms_per_op", full.get("msPerOp"))}
            # Two readers watch the same database: one is timed, the other runs with the
            # self-check on. Writes are applied once, in the mutator's own order.
            timed = Poller(args.native, db, False, args.profile, args.status_view)
            checked = Poller(args.native, db, True, status_view=args.status_view)
            first = timed.poll()
            checked.poll()
            idle = [timed.poll() for _ in range(5)]
            single, burst, alternating, step = [], [], [], 0
            shapes = {}
            shape_rows = {}
            for _ in range(args.writes):
                mutation = mutate(db, step + 1)
                if not mutation:
                    break
                step += 1
                single.append(timed.poll())
                shape_rows.setdefault(mutation["kind"], []).append(single[-1])
                verdict_step = checked.poll()
                shape = shapes.setdefault(mutation["kind"], {"writes": 0, "mismatches": 0})
                shape["writes"] += 1
                shape["mismatches"] += 1 if verdict_step.get("mismatch") else 0
                # Five same-state alternating live/full pairs; full opens a connection.
                # This is outside Reader::poll and outside the phase profile.
                if len(alternating) < 5:
                    result = subprocess.run([str(args.native), "--bench-source", "1", "--db", str(db), "--now-ms", NOW],
                                            capture_output=True, text=True, check=True)
                    paired = json.loads(result.stdout)
                    if paired.get("errors", 0):
                        raise RuntimeError(f"paired full reader errors: {paired}")
                    alternating.append({"live_ms": single[-1]["poll_ms"], "full_ms": paired["median_ms"]})
            for _ in range(args.bursts):
                mutation = mutate(db, step + 1, touch=max(1, threads // 100))
                if not mutation:
                    break
                step += 1
                burst.append(timed.poll())
                verdict_step = checked.poll()
                shape = shapes.setdefault(mutation["kind"], {"writes": 0, "mismatches": 0})
                shape["writes"] += 1
                shape["mismatches"] += 1 if verdict_step.get("mismatch") else 0
            last, verdict = timed.poll(), checked.poll()
            row["timed"] = {"first_read_ms": first["poll_ms"], "agents": last["agents"], "rss_mib": timed.rss_mib(),
                            "unchanged": summary(idle), "one_write": summary(single) if single else None,
                            "burst_1pct": summary(burst) if burst else None}
            row["alternating_same_state_pairs"] = alternating
            row["fallback_reasons"] = summary([first, *idle, *single, *burst, last])["fallback_reasons"]
            row["self_check"] = {"mismatches": verdict["mismatches"], "polls": 2 + len(single) + len(burst)}
            row["write_shapes"] = shapes
            row["by_shape"] = {k: summary(v) for k, v in sorted(shape_rows.items())}
            report["errors"] += verdict["mismatches"]
            timed.close()
            checked.close()
            report["tiers"].append(row)
            t = row["timed"]
            if args.profile:
                for workload in ("one_write", "burst_1pct"):
                    if t[workload]:
                        print(f"tier {tier} {workload} phase ms: " + json.dumps(t[workload]["phases"], sort_keys=True), flush=True)
            cell = lambda x: "none applied" if x is None else f"{x['median_ms']:.2f} ms {x['served']}"
            print(f"tier {tier} ({threads} threads, {t['agents']} agents): full reader {row['full_reader_ms_per_read']:.1f} ms | "
                  f"unchanged {t['unchanged']['median_ms']:.3f} ms | one write {cell(t['one_write'])} | "
                  f"fallbacks {t['one_write']['fallback_reasons'] if t['one_write'] else {}} | "
                  f"1% burst {cell(t['burst_1pct'])} | self-check mismatches {row['self_check']['mismatches']} "
                  f"in {row['self_check']['polls']} polls | rss {t['rss_mib']:.0f} MiB", flush=True)
            print(f"tier {tier} write shapes: {json.dumps(shapes, sort_keys=True)}", flush=True)
            for shape, rows in sorted(shape_rows.items()):
                s = summary(rows)
                ph = s["phases"]
                sel = ph.get("cache_selection", {}).get("median_ms")
                asm = ph.get("assembly", {}).get("median_ms")
                extra = f" | cache_selection {sel:.2f} ms, assembly {asm:.2f} ms" if sel is not None else ""
                print(f"tier {tier} {shape}: {s['median_ms']:.2f} ms over {s['polls']} polls {s['served']}"
                      f" | rows re-read median {s['threads_reread_median']} threads{extra}", flush=True)
        finally:
            shutil.rmtree(work, ignore_errors=True)
    report["load_average_end"] = os.getloadavg()
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 1 if report["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
