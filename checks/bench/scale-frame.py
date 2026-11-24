#!/usr/bin/env python3
"""How one frame's composition cost grows with the number of agents.

Times the complete cell-grid draw plus a hash of every output field, at a fixed grid, for
snapshots of increasing size. The window shows the same number of rows at every size, so a
cost that grows with the snapshot is work spent on agents nobody can see.

Excluded from the timed region: process start, JSON parsing, SQLite, terminal or GPU
presentation. Input and snapshot acceptance are included. Each side warms 20 draws first. Trials alternate between the sides.
A ratio is reported only when both sides produced the same output hash.
"""
import argparse, json, os, re, statistics, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NODE = ["node", "--max-old-space-size=12288", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]


def revision() -> str:
    return subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()


def run(argv):
    p = subprocess.run([sys.executable, "-c", "import resource,subprocess,sys; p=subprocess.run(sys.argv[1:]); print('T3_PEAK_RSS_KIB='+str(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss),file=sys.stderr); sys.exit(p.returncode)"] + argv, capture_output=True, text=True, cwd=ROOT)
    if p.returncode != 0:
        lines = [line for line in p.stderr.strip().splitlines() if not line.startswith("T3_PEAK_RSS_KIB=")]
        return None, f"exit {p.returncode}: " + (lines or ["failed without diagnostic"])[-1][:200]
    try:
        data = json.loads(p.stdout)
    except (ValueError, TypeError):
        return None, "invalid benchmark JSON output"
    rss = re.search(r"T3_PEAK_RSS_KIB=(\d+)", p.stderr)
    data["peak_rss_kib"] = int(rss[1]) if rss else None
    if data.get("errors", 0) != 0 or data.get("operations", 0) <= 0:
        return None, "reported errors or no operations"
    return data, None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", type=Path, default=ROOT / "checks/generated/scale")
    ap.add_argument("--sizes", type=int, nargs="*", default=[25, 256, 1024, 4096, 16384])
    ap.add_argument("--workloads", nargs="+", default=["repeat","navigate","scroll","clock","clock10s","replace","delta","burst","structural","host"])
    ap.add_argument("--scroll-steps", type=int, help="explicit bounded down/end/home run (default: traverse all agents)")
    ap.add_argument("--frames", type=int, default=300)
    ap.add_argument("--grid", default="233x55")
    ap.add_argument("--usage", type=Path, default=ROOT / "checks/generated/scale/usage.json")
    ap.add_argument("--native", type=Path, default=ROOT / "target/release/loupe-core")
    ap.add_argument("--trials", type=int, default=5)
    ap.add_argument("--skip-baseline", action="store_true", help="native only (no ratio)")
    ap.add_argument("--output", type=Path)
    args = ap.parse_args()
    if args.scroll_steps is not None and args.scroll_steps < 1:
        ap.error("--scroll-steps must be positive")
    if args.trials < 1:
        ap.error("--trials must be positive")

    report = {"version": 1, "revision": revision(), "grid": args.grid, "trials": args.trials,
              "load_average_start": os.getloadavg(), "native": str(args.native.resolve()),
              "timed_region": "input or replacement (including clone), cell-grid composition and hash chain of every cell; host workload includes State::advance_frame; TS draw always includes host bookkeeping; excludes parse, warmup, I/O and presentation",
              "memory_region": "process peak RSS including parsing, warmup and workload",
              "sizes": []}
    failures = 0
    for workload, agents in ((w,a) for w in args.workloads for a in args.sizes):
        snapshot = args.dir / f"agents-{agents}.json"
        if not snapshot.exists():
            print(f"missing {snapshot}; run checks/scripts/scale-fixtures.ts", file=sys.stderr)
            return 2
        operations = (args.scroll_steps if args.scroll_steps is not None else agents) + 2 if workload == "scroll" else args.frames
        common = ["--snapshot", str(snapshot), "--usage", str(args.usage), "--grid", args.grid, "--frame-workload", workload, "--frames", str(operations)]
        sides = {"native": [str(args.native)] + common}
        if not args.skip_baseline:
            sides["baseline"] = NODE + ["checks/bench/frame-baseline.ts"] + common
        samples = {name: [] for name in sides}
        memory = {name: [] for name in sides}
        hashes = {name: set() for name in sides}
        errors = {name: 0 for name in sides}
        for trial in range(args.trials):
            order = list(sides) if trial % 2 == 0 else list(reversed(sides))
            for name in order:
                data, error = run(sides[name])
                if data is None:
                    errors[name] += 1
                    print(f"{name} agents={agents}: {error}", file=sys.stderr)
                    continue
                memory[name].append(data["peak_rss_kib"])
                samples[name].append(data["draw_ms"] / data["operations"])
                hashes[name].add(data["output_hash"])
        row = {"workload": workload, "agents": agents, "operations_per_trial": operations, "scroll_steps": args.scroll_steps if workload == "scroll" else None}
        for name in sides:
            values = samples[name]
            failures += errors[name]
            row[name] = {"ms_per_frame": values, "median": statistics.median(values) if values else None,
                         "min": min(values) if values else None, "max": max(values) if values else None,
                         "errors": errors[name], "output_hashes": sorted(hashes[name]), "peak_rss_kib": memory[name]}
        match = None if args.skip_baseline else (all(len(samples[n]) == args.trials and errors[n] == 0 for n in sides)
                 and len(hashes["native"]) == 1 and hashes["native"] == hashes["baseline"])
        if not args.skip_baseline and not match: failures += 1
        row["outputs_match"] = match
        row["baseline_over_native"] = (row["baseline"]["median"] / row["native"]["median"]
                                       if match and row["native"]["median"] else None)
        report["sizes"].append(row)
        line = f"{workload:10s} agents={agents:6d}  native {(row['native']['median'] or 0):.3f} ms"
        if not args.skip_baseline and row["baseline"]["median"] is not None:
            line += f"  baseline {row['baseline']['median']:.3f} ms  outputs_match={match}"
            if match:
                line += f"  ratio {row['baseline_over_native']:.2f}x"
        print(line, flush=True)
    report["load_average_end"] = os.getloadavg()
    report["errors"] = failures
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
