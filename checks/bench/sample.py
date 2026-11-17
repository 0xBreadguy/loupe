#!/usr/bin/env python3
"""Sample explicit Linux process trees. Never launches a GUI.

For OpenTUI use the TERMINAL PID, then launch Node in that same terminal;
measure that terminal alone first for incremental costs. For native use its
PID. Optionally include the box compositor with another --root. Never compare
compositor-inclusive totals with app-only ones. GPU allocations are absent.
"""
import argparse
import json
import os
from pathlib import Path
import statistics
import threading
import time


def read_process(pid, memory=True):
    try:
        raw = Path(f"/proc/{pid}/stat").read_text()
        name = raw[raw.index("(") + 1:raw.rindex(")")]
        v = raw[raw.rindex(")") + 2:].split()
        result = {"pid": pid, "ppid": int(v[1]), "name": name,
                  "cpu_ticks": int(v[11]) + int(v[12]), "start_ticks": int(v[19]),
                  "rss_kib": int(v[21]) * os.sysconf("SC_PAGE_SIZE") / 1024}
        try:
            if not memory:
                return result
            rollup = Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()
            fields = {line.split(":")[0]: int(line.split()[1]) for line in rollup
                      if ":" in line and line.split()[1].isdigit()}
            result["pss_kib"] = fields["Pss"]
            result["uss_kib"] = fields.get("Private_Clean", 0) + fields.get("Private_Dirty", 0)
        except (OSError, KeyError):
            pass
        return result
    except (OSError, ValueError, IndexError):
        return None


def snapshot(roots):
    # Discover parents using tiny stat files; only inspect memory maps belonging
    # to the selected application tree, not every unrelated host process.
    all_processes = {int(p.name): process for p in Path("/proc").iterdir()
                     if p.name.isdigit() and (process := read_process(int(p.name), memory=False)) is not None}
    selected = set(roots)
    while True:
        more = {p["pid"] for p in all_processes.values() if p["ppid"] in selected}
        if more <= selected:
            break
        selected.update(more)
    return [process for p in sorted(selected) if p in all_processes
            and (process := read_process(p)) is not None]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--root", type=int, action="append", required=True)
    p.add_argument("--seconds", type=float, default=5)
    p.add_argument("--interval", type=float, default=0.25)
    p.add_argument("--label", required=True)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    if args.seconds <= 0 or args.seconds > 60 or args.interval < 0.05:
        p.error("duration must be (0,60] seconds, interval at least .05 seconds")
    if any(pid <= 1 for pid in args.root):
        p.error("explicit application PID roots required")
    if any(read_process(pid) is None for pid in args.root):
        p.error("root process not running")
    started = time.monotonic()
    samples = []
    stop = threading.Event()
    while True:
        samples.append({"at_seconds": time.monotonic() - started, "processes": snapshot(args.root)})
        remaining = args.seconds - (time.monotonic() - started)
        if remaining <= 0:
            break
        stop.wait(min(args.interval, remaining))
    duration = samples[-1]["at_seconds"] - samples[0]["at_seconds"]
    cpu_ticks = 0
    for before, after in zip(samples, samples[1:]):
        previous = {(q["pid"], q["start_ticks"]): q for q in before["processes"]}
        for proc in after["processes"]:
            identity = (proc["pid"], proc["start_ticks"])
            if identity in previous:
                cpu_ticks += max(0, proc["cpu_ticks"] - previous[identity]["cpu_ticks"])
    cpu_seconds = cpu_ticks / os.sysconf("SC_CLK_TCK")
    totals = {field: [sum(q.get(field, 0) for q in sample["processes"]) for sample in samples]
              for field in ("rss_kib", "pss_kib", "uss_kib")}
    result = {"label": args.label, "roots": args.root, "duration_seconds": duration,
              "interval_seconds": args.interval, "samples": samples,
              "summary": {"cpu_seconds": cpu_seconds, "cpu_percent_one_core": 100 * cpu_seconds / duration,
                          **{field: {"median": statistics.median(v), "min": min(v), "max": max(v)}
                             for field, v in totals.items()}},
              "limitations": ["Sampled process-tree CPU; very short-lived children can escape samples.",
                              "RSS sums can double count shared mappings; use PSS for proportional resident memory.",
                              "PSS/USS require readable smaps_rollup; absent fields are not zero usage.",
                              "Excludes kernel/compositor/GPU memory and CPU outside selected trees.",
                              "No input-to-present timing or output correctness instrumentation here."],
              "missing_roots_at_end": [pid for pid in args.root if read_process(pid) is None]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"output": str(args.output), "summary": result["summary"],
                      "missing_roots_at_end": result["missing_roots_at_end"]}, indent=2))


if __name__ == "__main__":
    main()
