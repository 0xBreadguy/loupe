#!/usr/bin/env python3
"""The bar's status watcher, original against Rust, on a lab database that keeps changing.

Three arms, each `--status --watch` on a working copy of one lab tier:

  original           node oracle/src/main.ts
  rust_incremental   t3-agents-term, the default: the incremental reader
  rust_full_read     t3-agents-term with T3_AGENTS_SYNC_READ=1: the original's full read

While an arm runs, the lab's write stream applies one write a second. Every trial starts from
an untouched copy and from step one, because that stream is sequential and runs dry after a few
dozen steps. Trials alternate arm order.

Recorded per trial: the watcher's CPU time over the sampling period as a share of one core, its
proportional memory at the end, how many writes were applied and how many status lines it
printed. Lines are counted and thrown away. Start-up and the first read are not counted.

Headless: no window, no terminal emulator. Synthetic data only.
"""
import argparse, json, os, shutil, statistics, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NODE = ["node", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]
TICK = os.sysconf("SC_CLK_TCK")
ARMS = ["original", "rust_incremental", "rust_full_read"]


def cpu_seconds(pid):
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / TICK


def pss_mib(pid):
    for line in open(f"/proc/{pid}/smaps_rollup"):
        if line.startswith("Pss:"):
            return int(line.split()[1]) / 1024
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tier", default="real")
    ap.add_argument("--dir", type=Path, default=ROOT / "checks/generated/scale/t3")
    ap.add_argument("--native", type=Path, default=ROOT / "target/release/t3-agents-term")
    ap.add_argument("--work", type=Path, default=ROOT / "checks/generated/tmp/watcher")
    ap.add_argument("--arms", nargs="*", default=ARMS)
    ap.add_argument("--trials", type=int, default=5)
    ap.add_argument("--warm", type=float, default=5)
    ap.add_argument("--seconds", type=float, default=36)
    ap.add_argument("--output", type=Path)
    args = ap.parse_args()
    source = args.dir / f"{args.tier}.sqlite"
    if not source.exists():
        print(f"missing {source}; run checks/scripts/scale-db.ts --tier {args.tier} --schema t3", file=sys.stderr)
        return 2
    # The watcher must never fall back to a real database: the path is explicit and the home is empty.
    env = dict(os.environ, T3CODE_HOME=str(args.work / "no-such-home"))
    revision = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    report = {"version": 1, "revision": revision, "tier": args.tier, "trials": args.trials, "warm_seconds": args.warm,
              "sample_seconds": args.seconds, "load_average_start": os.getloadavg(), "arms": {}, "errors": 0}
    rows = {arm: [] for arm in args.arms}
    for trial in range(args.trials):
        for arm in (args.arms if trial % 2 == 0 else list(reversed(args.arms))):
            shutil.rmtree(args.work, ignore_errors=True)
            args.work.mkdir(parents=True)
            db = args.work / "statev2.sqlite"
            subprocess.run(["cp", "--reflink=auto", str(source), str(db)], check=True)
            if (source.parent / "environment-id").exists():
                shutil.copy(source.parent / "environment-id", args.work / "environment-id")
            if arm == "original":
                argv = NODE + [str(ROOT / "oracle/src/main.ts"), "--status", "--watch", "--db", str(db)]
            else:
                argv = [str(args.native), "--status", "--watch", "--db", str(db)]
            arm_env = dict(env, T3_AGENTS_SYNC_READ="1") if arm == "rust_full_read" else env
            p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=arm_env)
            os.set_blocking(p.stdout.fileno(), False)
            lines = writes = step = 0
            time.sleep(args.warm)
            try:
                c0, t0 = cpu_seconds(p.pid), time.monotonic()
                while time.monotonic() - t0 < args.seconds:
                    step += 1
                    m = subprocess.run(NODE + [str(ROOT / "checks/scripts/scale-db.ts"), "--mutate", str(step), "--db", str(db)],
                                       capture_output=True, text=True, cwd=ROOT, env=env)
                    writes += m.returncode == 0
                    time.sleep(1)
                    try:
                        chunk = p.stdout.read()
                        lines += chunk.count(b"\n") if chunk else 0
                    except BlockingIOError:
                        pass
                dt = time.monotonic() - t0
                row = {"cpu_pct": 100 * (cpu_seconds(p.pid) - c0) / dt, "pss_mib": pss_mib(p.pid), "writes": writes, "status_lines": lines}
            except OSError as error:
                row = {"error": f"watcher went away: {error}"}
                report["errors"] += 1
            p.terminate()
            p.wait()
            rows[arm].append(row)
            print(trial, arm, {k: (round(v, 2) if isinstance(v, float) else v) for k, v in row.items()}, flush=True)
    shutil.rmtree(args.work, ignore_errors=True)
    for arm, trials in rows.items():
        good = [r for r in trials if "error" not in r]
        summary = {"trials": trials}
        for key in ("cpu_pct", "pss_mib", "writes", "status_lines"):
            values = [r[key] for r in good]
            if values:
                summary[key] = {"median": statistics.median(values), "min": min(values), "max": max(values)}
        report["arms"][arm] = summary
        if good:
            c, m = summary["cpu_pct"], summary["pss_mib"]
            print(f"{arm:17s} cpu% {c['median']:.2f} ({c['min']:.2f}-{c['max']:.2f})  pss MiB {m['median']:.1f} ({m['min']:.1f}-{m['max']:.1f})  "
                  f"writes {summary['writes']['median']:.0f}  lines {summary['status_lines']['median']:.0f}")
    report["load_average_end"] = os.getloadavg()
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 1 if report["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
