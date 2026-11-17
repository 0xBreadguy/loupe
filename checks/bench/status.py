#!/usr/bin/env python3
"""Old against new: the status watcher's resident memory and CPU time.

Runs `oracle/src/main.ts --status --watch` (Node) and
`t3-agents-core --status --watch` against the same synthetic demo database for
60 s per trial, five alternating trials per case, and reports the median and
range of Pss, CPU time, and time to the first line. Nothing here is a gate: the
numbers are exploratory, the machine is shared, and only the watcher processes
are sampled (the demo writer and the shell are excluded).

Databases come from the `Demo` class in `oracle/src/demo.ts`: one fresh
static copy per trial, or one keeper process rebuilding every 2 s, so both
implementations in a trial read the same input.

Timed region: spawn to 60 s later. CPU includes process startup and the first
line; memory samples start after the first second so module loading is not
counted as steady-state. Pss is proportional resident memory from
/proc/PID/smaps_rollup; CPU is utime+stime from /proc/PID/stat.
"""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import threading
import time

from sample import read_process

ROOT = Path(__file__).resolve().parents[2]
DEMO = ROOT / "oracle" / "src" / "demo.ts"
BINARY = ROOT / "target" / "release" / "t3-agents-core"
NODE = ["node", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]

DEMO_CODE = """
import { copyFileSync } from "node:fs"
import { Demo } from __DEMO__
const refresh = Number(process.env.T3_BENCH_REFRESH ?? "0")
const demo = new Demo({ now: Number(process.env.T3_BENCH_EPOCH), refreshMs: refresh })
process.on("SIGTERM", () => { demo.stop(); process.exit(0) })
if (process.env.T3_BENCH_COPY) {
  copyFileSync(demo.path, process.env.T3_BENCH_COPY)
  demo.stop()
} else {
  process.stdout.write(demo.path + "\\n")
  setInterval(() => {}, 1 << 30)
}
"""


def demo_code():
    return DEMO_CODE.replace("__DEMO__", json.dumps(DEMO.as_uri()))


def static_database(destination, epoch):
    """Build one demo database and copy it out, so every watcher in a trial reads it."""
    env = {**os.environ, "T3_BENCH_COPY": str(destination), "T3_BENCH_REFRESH": "0",
           "T3_BENCH_EPOCH": str(epoch)}
    subprocess.run(NODE + ["--input-type=module", "-e", demo_code()], cwd=ROOT, env=env,
                   check=True, capture_output=True, text=True)


class Writer:
    """A Demo instance rebuilding every two seconds; the watcher input for the busy case."""

    def __init__(self, epoch):
        self.proc = subprocess.Popen(
            NODE + ["--input-type=module", "-e", demo_code()], cwd=ROOT,
            env={**os.environ, "T3_BENCH_REFRESH": "2000", "T3_BENCH_EPOCH": str(epoch)},
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.path = Path(self.proc.stdout.readline().strip())
        if not self.path.exists():
            self.stop()
            raise RuntimeError("demo writer did not produce a database")

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def watcher_argv(kind, database):
    if kind == "node":
        return NODE + [str(ROOT / "oracle" / "src" / "main.ts"), "--status", "--watch", "--db", str(database)]
    if kind == "rust":
        return [str(BINARY), "--status", "--watch", "--db", str(database)]
    raise ValueError(kind)


def trial(kind, database, seconds):
    """Spawn one watcher; sample its memory and CPU; return its observed cost."""
    argv = watcher_argv(kind, database)
    spawned = time.perf_counter()
    proc = subprocess.Popen(argv, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    observed = {"kind": kind, "argv": argv, "pid": proc.pid, "samples": []}
    first = {"at": None, "line": None}
    lines = []
    stderr = []

    def read_stdout():
        for line in proc.stdout:
            if first["at"] is None:
                first["at"] = time.perf_counter() - spawned
                first["line"] = line.rstrip("\n")
            if line.strip():
                lines.append(line.rstrip("\n"))

    def read_stderr():
        for line in proc.stderr:
            stderr.append(line.rstrip("\n"))

    out_thread = threading.Thread(target=read_stdout, daemon=True)
    err_thread = threading.Thread(target=read_stderr, daemon=True)
    out_thread.start()
    err_thread.start()

    start = read_process(proc.pid)
    while True:
        elapsed = time.perf_counter() - spawned
        if elapsed >= seconds:
            break
        time.sleep(min(1.0, seconds - elapsed))
        sample = read_process(proc.pid)
        if sample is None:
            break
        sample["at_seconds"] = time.perf_counter() - spawned
        observed["samples"].append(sample)
    end = read_process(proc.pid)
    wall = time.perf_counter() - spawned
    if proc.poll() is None:
        proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    out_thread.join(timeout=2)
    err_thread.join(timeout=2)

    ticks = os.sysconf("SC_CLK_TCK")
    cpu = None
    if start and end:
        cpu = (end["cpu_ticks"] - start["cpu_ticks"]) / ticks
    steady = [s["pss_kib"] for s in observed["samples"] if s.get("at_seconds", 0) >= 1 and "pss_kib" in s]
    all_pss = [s["pss_kib"] for s in observed["samples"] if "pss_kib" in s]
    observed.update({
        "exit_code": proc.returncode,
        "wall_seconds": wall,
        "first_line_ms": None if first["at"] is None else first["at"] * 1000,
        "lines": len(lines),
        "first_line": first["line"],
        "last_line": lines[-1] if lines else None,
        "cpu_seconds": cpu,
        "cpu_percent_one_core": None if cpu is None else 100 * cpu / wall,
        "pss_kib_median": statistics.median(steady) if steady else None,
        "pss_kib_min": min(steady) if steady else None,
        "pss_kib_max": max(steady) if steady else None,
        "pss_kib_peak_any": max(all_pss) if all_pss else None,
        "stderr": stderr[-5:],
    })
    return observed


def summary(trials, field):
    values = [t[field] for t in trials if t.get(field) is not None]
    if not values:
        return {"median": None, "min": None, "max": None, "trials": 0}
    return {"median": statistics.median(values), "min": min(values), "max": max(values), "trials": len(values)}


def case(name, database_for_trial, trials, seconds):
    records = {"name": name, "trials": []}
    for index in range(trials):
        epoch = int(time.time() * 1000)
        database, writer = database_for_trial(epoch)
        order = ["rust", "node"] if index % 2 == 0 else ["node", "rust"]
        try:
            pair = [trial(kind, database, seconds) for kind in order]
        finally:
            if writer:
                writer.stop()
        record = {"trial": index, "order": order, "results": pair}
        if len(pair) == 2 and all(r["first_line"] for r in pair):
            record["identical_first_line"] = pair[0]["first_line"] == pair[1]["first_line"]
        records["trials"].append(record)
        print(json.dumps({"case": name, "trial": index, "order": order,
                          "rust": {"pss_kib": pair[order.index("rust")]["pss_kib_median"],
                                   "cpu_seconds": pair[order.index("rust")]["cpu_seconds"],
                                   "first_line_ms": pair[order.index("rust")]["first_line_ms"],
                                   "lines": pair[order.index("rust")]["lines"]},
                          "node": {"pss_kib": pair[order.index("node")]["pss_kib_median"],
                                   "cpu_seconds": pair[order.index("node")]["cpu_seconds"],
                                   "first_line_ms": pair[order.index("node")]["first_line_ms"],
                                   "lines": pair[order.index("node")]["lines"]},
                          "identical_first_line": record.get("identical_first_line")}), flush=True)
    records["summary"] = {
        kind: {
            "pss_kib": summary([r["results"][r["order"].index(kind)] for r in records["trials"]], "pss_kib_median"),
            "cpu_seconds": summary([r["results"][r["order"].index(kind)] for r in records["trials"]], "cpu_seconds"),
            "cpu_percent_one_core": summary([r["results"][r["order"].index(kind)] for r in records["trials"]], "cpu_percent_one_core"),
            "first_line_ms": summary([r["results"][r["order"].index(kind)] for r in records["trials"]], "first_line_ms"),
            "lines": [r["results"][r["order"].index(kind)]["lines"] for r in records["trials"]],
        } for kind in ("rust", "node")
    }
    records["identical_first_lines"] = [r.get("identical_first_line") for r in records["trials"]]
    return records


def main():
    global BINARY
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--seconds", type=float, default=60)
    parser.add_argument("--binary", type=Path, default=BINARY)
    parser.add_argument("--output", type=Path, default=ROOT / "checks" / "generated" / "status-bench.json")
    args = parser.parse_args()
    BINARY = args.binary.resolve()
    if not BINARY.exists():
        parser.error(f"missing binary {BINARY}; cargo build --release -p t3-agents-core first")
    if args.trials < 5:
        parser.error("at least five alternating trials are required")
    if args.seconds <= 0:
        parser.error("seconds must be positive")

    load_before = os.getloadavg()
    started_iso = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    with tempfile.TemporaryDirectory(prefix="t3-status-bench-") as directory:
        directory = Path(directory)

        def static(epoch):
            path = directory / f"static-{epoch}.sqlite"
            static_database(path, epoch)
            return path, None

        def changing(epoch):
            writer = Writer(epoch)
            return writer.path, writer

        cases = [case("static", static, args.trials, args.seconds),
                 case("changing", changing, args.trials, args.seconds)]

    result = {
        "version": 1,
        "exploratory": True,
        "note": "Shared machine: other builds run here. Only the watcher process is sampled; "
                "the demo writer, shell and kernel are excluded. Not a gate.",
        "started": started_iso,
        "seconds_per_trial": args.seconds,
        "trials_per_case": args.trials,
        "binary": str(BINARY),
        "binary_bytes": BINARY.stat().st_size,
        "node": subprocess.run(["node", "--version"], capture_output=True, text=True).stdout.strip(),
        "load_average_before": load_before,
        "load_average_after": os.getloadavg(),
        "cases": cases,
        "timed_region": "spawn to 60 s later; CPU includes process startup and the first line; "
                        "Pss median uses samples after the first second; excludes setup and teardown",
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"output": str(args.output), "binary_bytes": result["binary_bytes"],
                      "load_average": [load_before, result["load_average_after"]],
                      "summaries": {c["name"]: c["summary"] for c in cases}}, indent=2))


if __name__ == "__main__":
    main()
