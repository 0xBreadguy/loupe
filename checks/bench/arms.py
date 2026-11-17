#!/usr/bin/env python3
"""The original and the Rust terminal presenter as whole applications, side by side.

Two arms, each started inside an omabox desktop, never on the real one:

  typescript   foot running `node oracle/src/main.ts`
  rust_term    foot running `t3-agents-term`

Two scenarios:

  demo   the built-in demo in calm mode, left alone
  live   a synthetic real-schema database that a writer changes once a second

For each trial this records the time until the window exists, then, after a warm-up, the
application's CPU over the sampling period and its proportional memory at the end. The
terminal emulator is reported separately. Trials alternate arm order.

Not counted on either arm: GPU memory, the compositor, the kernel. Time to window includes
omabox's own launch overhead, the same for both arms; it is not time to first frame.

The GPU window arm this used to carry was removed with the window.
"""
import argparse, json, os, shutil, statistics, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NODE = ["node", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]
RELEASE = ROOT / "target/release"
TICK = os.sysconf("SC_CLK_TCK")


# Extra arms for comparing two Rust builds: `--binary NAME=PATH` adds an arm NAME that runs the
# program at PATH (a frozen copy of an earlier build, for example).
BINARIES = {}


def command(arm, scenario, db_in_box):
    source = ["--demo"] if scenario == "demo" else ["--db", db_in_box]
    if arm == "typescript":
        return NODE + [str(ROOT / "oracle/src/main.ts")] + source + ["--calm"]
    return [str(BINARIES.get(arm, RELEASE / "t3-agents-term"))] + source + ["--calm"]


def processes():
    out = {}
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            raw = open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")
            stat = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
            out[int(pid)] = ([c.decode("utf8", "replace") for c in raw if c], int(stat[1]))
        except OSError:
            pass
    return out


def cpu_seconds(pid):
    f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / TICK


def pss_mib(pid):
    for line in open(f"/proc/{pid}/smaps_rollup"):
        if line.startswith("Pss:"):
            return int(line.split()[1]) / 1024
    return None


def trial(box, arm, scenario, db_in_box, warm, seconds):
    tag = f"arm-{arm}-{int(time.time() * 1000) % 1000000}"
    argv = command(arm, scenario, db_in_box)
    launch = ["foot", f"--app-id={tag}", "--title=arm"] + argv
    started = time.monotonic()
    subprocess.run(["omabox", "-b", box, "run", "-d", "--"] + launch, capture_output=True, text=True)
    selector = tag
    waited = subprocess.run(["omabox", "-b", box, "wait", "window", selector], capture_output=True, text=True)
    to_window = time.monotonic() - started
    if waited.returncode != 0:
        return {"error": f"no window: {waited.stderr.strip()[-120:]}"}
    time.sleep(warm)
    table = processes()
    foot = next((p for p, (c, _) in table.items() if c and c[0].endswith("foot") and f"--app-id={tag}" in c), None)
    app = next((p for p, (_, parent) in table.items() if parent == foot), None) if foot else None
    if app is None:
        return {"error": "application process not found"}
    try:
        a0, f0, t0 = cpu_seconds(app), cpu_seconds(foot) if foot else 0.0, time.monotonic()
        time.sleep(seconds)
        dt = time.monotonic() - t0
        row = {"to_window_s": to_window, "app_cpu_pct": 100 * (cpu_seconds(app) - a0) / dt, "app_pss_mib": pss_mib(app)}
        if foot:
            row["terminal_cpu_pct"] = 100 * (cpu_seconds(foot) - f0) / dt
            row["terminal_pss_mib"] = pss_mib(foot)
    except OSError as error:
        row = {"error": f"process went away: {error}"}
    for pid in (foot, app):
        if pid:
            try:
                os.kill(pid, 15)
            except OSError:
                pass
    subprocess.run(["omabox", "-b", box, "wait", "window", selector, "--gone"], capture_output=True, text=True)
    return row


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--box", required=True, help="an omabox desktop that is already up")
    ap.add_argument("--box-home", type=Path, required=True, help="the box's home directory as the host sees it")
    ap.add_argument("--arms", nargs="*", default=["typescript", "rust_term"])
    ap.add_argument("--binary", action="append", default=[], metavar="NAME=PATH",
                    help="an arm NAME that runs the Rust program at PATH; name it in --arms")
    ap.add_argument("--scenarios", nargs="*", default=["demo", "live"])
    ap.add_argument("--tier", default="m", help="real-schema lab tier for the live scenario")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--warm", type=float, default=5)
    ap.add_argument("--seconds", type=float, default=30)
    ap.add_argument("--output", type=Path)
    args = ap.parse_args()
    for pair in args.binary:
        name, _, path = pair.partition("=")
        BINARIES[name] = Path(path).resolve()
    revision = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    report = {"version": 1, "revision": revision, "load_average_start": os.getloadavg(), "trials": args.trials,
              "warm_seconds": args.warm, "sample_seconds": args.seconds, "tier": args.tier, "scenarios": {}, "errors": 0,
              "excluded": "GPU memory, compositor and kernel on every arm"}
    for scenario in args.scenarios:
        writer, step, db_in_box = None, 0, None
        if scenario == "live":
            source = ROOT / f"checks/generated/scale/t3/{args.tier}.sqlite"
            if not source.exists():
                print(f"missing {source}; run checks/scripts/scale-db.ts --tier {args.tier} --schema t3", file=sys.stderr)
                return 2
            live = args.box_home / "arms-live"
            db_in_box = "/home/sbx/arms-live/state.sqlite"

            def fresh():
                # The lab's write stream is sequential and runs dry after a few dozen steps,
                # so every trial starts from an untouched copy and from step one.
                shutil.rmtree(live, ignore_errors=True)
                live.mkdir(parents=True)
                shutil.copy(source, live / "state.sqlite")
                if (source.parent / "environment-id").exists():
                    shutil.copy(source.parent / "environment-id", live / "environment-id")
        rows = {arm: [] for arm in args.arms}
        for t in range(args.trials):
            for arm in (args.arms if t % 2 == 0 else list(reversed(args.arms))):
                if scenario == "live":
                    fresh()
                    step = 0
                    # One T3-style write a second for the whole trial, the same stream for every arm.
                    script = (f"import subprocess,sys,time\\nstep={step}\\nend=time.time()+{args.warm + args.seconds + 20}\\n"
                              f"while time.time()<end:\\n step+=1\\n r=subprocess.run({NODE!r}+[{str(ROOT / 'checks/scripts/scale-db.ts')!r},"
                              f"'--mutate',str(step),'--db',{str(args.box_home / 'arms-live/state.sqlite')!r}],capture_output=True,text=True,cwd={str(ROOT)!r})\\n"
                              f" print(step if r.returncode==0 else -step,flush=True)\\n time.sleep(1)\\n")
                    writer = subprocess.Popen([sys.executable, "-c", script.replace("\\n", "\n")], stdout=subprocess.PIPE, text=True)
                row = trial(args.box, arm, scenario, db_in_box, args.warm, args.seconds)
                if writer:
                    writer.terminate()
                    applied = [int(x) for x in writer.stdout.read().split() if x.lstrip("-").isdigit()]
                    good = [x for x in applied if x > 0]
                    step = max(good) if good else step
                    row["writes_applied"] = len(good)
                    # The mutator's steps are sequential: a failed step cannot be retried, so stop the stream there.
                    if any(x < 0 for x in applied):
                        row["writer_stopped_early"] = True
                rows[arm].append(row)
                report["errors"] += 1 if "error" in row else 0
                print(scenario, t, arm, {k: (round(v, 2) if isinstance(v, float) else v) for k, v in row.items()}, flush=True)
        summary = {}
        for arm, trials in rows.items():
            good = [r for r in trials if "error" not in r]
            summary[arm] = {"trials": trials}
            for key in ("to_window_s", "app_cpu_pct", "app_pss_mib", "terminal_cpu_pct", "terminal_pss_mib"):
                values = [r[key] for r in good if key in r]
                if values:
                    summary[arm][key] = {"median": statistics.median(values), "min": min(values), "max": max(values)}
        report["scenarios"][scenario] = summary
    report["load_average_end"] = os.getloadavg()
    for scenario, summary in report["scenarios"].items():
        for arm, s in summary.items():
            cell = lambda k: f"{s[k]['median']:.2f} ({s[k]['min']:.2f}-{s[k]['max']:.2f})" if k in s else "-"
            print(f"{scenario:5s} {arm:12s} cpu% {cell('app_cpu_pct')}  pss MiB {cell('app_pss_mib')}  "
                  f"terminal cpu% {cell('terminal_cpu_pct')} pss {cell('terminal_pss_mib')}  window after s {cell('to_window_s')}")
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 1 if report["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
