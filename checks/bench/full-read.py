#!/usr/bin/env python3
"""What one full read costs, per query, on real-shaped payloads.

The lab tier `real` (`scale-db.ts --tier real --schema t3`) has the recorded row
counts and payload sizes, but its payloads are a few keys and one long filler
string, which SQLite parses far faster than T3's nested JSON. `build` copies that
tier and rewrites every turn item's filler into densely nested JSON of the same
byte length: arrays of small objects, numbers, short strings with escapes, the
way tool results, command output chunks and reasoning summaries are shaped. The
fields the reader selects (startedAt, toolName, exitCode, title, label, input,
fileName, steps, schema, status) keep their exact values, so the dense database
must give the same snapshot as the lab tier; `build` checks that.

`run` times `t3-agents-core --bench-source N` for several binaries on several
databases, alternating the binary order on every trial, and records per-query
milliseconds (prepare + step, from the binary's own phase timing), the whole
read, the snapshot checksum (which must be identical across binaries) and the
peak resident memory of each process (`wait4` rusage). The timed region is one
`source::snapshot` call: open, read transaction, every query, assembly, close.
Process start and the two warm-up reads are outside it; peak memory includes
them. Load average is recorded before and after.

Synthetic data only. Every child gets an empty T3CODE_HOME and an explicit --db.

    python3 checks/bench/full-read.py build
    python3 checks/bench/full-read.py run --binary before=PATH --binary after=PATH \
        [--db lab=PATH --db dense=PATH] [--trials 5] [--ops 7]
"""
import argparse, json, os, random, shutil, sqlite3, statistics, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NOW = "1794484800000"
NODE = ["node", "--experimental-ffi", "--disable-warning=ExperimentalWarning"]
GEN = ROOT / "checks/generated/fullread"
LAB = ROOT / "checks/generated/scale/t3/real.sqlite"
DENSE = GEN / "dense.sqlite"
ITEMS = "orchestration_v2_projection_turn_items"

def home():
    """An empty T3 home for the children, beside the other generated files (not /tmp)."""
    GEN.mkdir(parents=True, exist_ok=True)
    return tempfile.mkdtemp(prefix="home-", dir=GEN)


# ── Dense payloads ───────────────────────────────────────────────────────────

WORDS = ("cargo test build src lib core reader poll item event node thread run agent "
         "plan step query index sorter limit window payload json parse value array "
         "object field string number match line column span path file diff hunk").split()
PATHS = ["src/live.rs", "src/source.rs", "crates/core/tests/live.rs", "oracle/src/app.ts",
         "checks/scripts/scale-db.ts", "README.md", "Cargo.toml", "src/model.rs"]


def text(r, n):
    """Short text with the escapes real output carries (quotes, tabs, newlines, unicode)."""
    out = []
    for _ in range(n):
        w = r.choice(WORDS)
        roll = r.random()
        if roll < 0.08:
            w += "\n"
        elif roll < 0.12:
            w = '"' + w + '"'
        elif roll < 0.14:
            w += "\t"
        elif roll < 0.15:
            w += " é→"
        out.append(w)
    return " ".join(out)


def fragment(r, family):
    """One small nested JSON value, shaped by payload family."""
    if family == "tool":
        if r.random() < 0.5:
            return {"type": "text", "text": text(r, r.randint(3, 14)),
                    "annotations": {"audience": ["assistant"], "priority": round(r.random(), 3)}}
        return {"path": r.choice(PATHS), "line": r.randint(1, 9000), "col": r.randint(1, 120),
                "spans": [[r.randint(0, 40), r.randint(41, 90)] for _ in range(r.randint(1, 3))],
                "kind": r.choice(["match", "context", "def", "ref"]), "ok": r.random() < 0.9,
                "meta": {"lang": r.choice(["rust", "ts", "md"]), "bytes": r.randint(10, 99999)}}
    if family == "command":
        return {"stream": r.choice(["stdout", "stdout", "stderr"]), "seq": r.randint(0, 99999),
                "t": r.randint(0, 600000), "text": text(r, r.randint(4, 16))}
    if family == "reasoning":
        return {"type": "summary_text", "index": r.randint(0, 40), "text": text(r, r.randint(4, 12)),
                "refs": [r.randint(0, 999) for _ in range(r.randint(0, 4))]}
    if family == "edit":
        return {"op": r.choice(["add", "del", "ctx"]), "old": r.randint(1, 4000), "new": r.randint(1, 4000),
                "line": text(r, r.randint(2, 10))}
    return {"k": r.choice(WORDS), "v": text(r, r.randint(2, 8)), "n": r.randint(0, 9999)}


FAMILY = {"dynamic_tool": "tool", "web_search": "tool", "subagent": "tool", "handoff": "tool",
          "command_execution": "command", "reasoning": "reasoning", "assistant_message": "reasoning",
          "file_change": "edit"}
DUMP = dict(separators=(",", ":"), ensure_ascii=False)


def pools():
    r = random.Random(42)
    return {f: [json.dumps(fragment(r, f), **DUMP) for _ in range(4000)]
            for f in ("tool", "command", "reasoning", "edit", "other")}


def dense(kept, size, family, pool, r):
    """`kept` plus nested content, serialized to exactly `size` UTF-8 bytes when it fits."""
    head = json.dumps(kept, **DUMP)[:-1]
    head += ("," if len(kept) else "") + '"detail":{"items":['
    tail = lambda pad: '],"pad":"' + pad + '"}}'
    # Group fragments in nested arrays of 2-6 to add depth, then pad the rest.
    parts, used = [], len(head.encode()) + len(tail("").encode())
    while True:
        k = r.randint(2, 6)
        group = "[" + ",".join(r.choice(pool) for _ in range(k)) + "]"
        cost = len(group.encode()) + (1 if parts else 0)
        if used + cost > size:
            break
        parts.append(group)
        used += cost
    pad = "x" * max(0, size - used)
    return head + ",".join(parts) + tail(pad)


def build(args):
    GEN.mkdir(parents=True, exist_ok=True)
    lab = Path(args.lab)
    if not lab.exists():
        subprocess.run(NODE + [str(ROOT / "checks/scripts/scale-db.ts"), "--tier", "real", "--schema", "t3",
                               "--db", str(lab)], check=True, cwd=ROOT)
    out = Path(args.out)
    for suffix in ("", "-wal", "-shm"):
        Path(str(out) + suffix).unlink(missing_ok=True)
    src = sqlite3.connect(f"file:{lab}?mode=ro", uri=True)
    dst = sqlite3.connect(out)
    src.backup(dst)
    src.close()
    pool = pools()
    r = random.Random(7)
    rows = dst.execute(f"SELECT rowid, type, payload_json FROM {ITEMS} ORDER BY rowid").fetchall()
    total_before = total_after = misses = 0
    dst.execute("BEGIN")
    for rowid, kind, payload in rows:
        size = len(payload.encode())
        total_before += size
        kept = {k: v for k, v in json.loads(payload).items() if k != "pad"}
        new = dense(kept, size, FAMILY.get(kind, "other"), pool[FAMILY.get(kind, "other")], r)
        parsed = json.loads(new)  # valid JSON, and the selected fields are untouched
        assert all(parsed[k] == v for k, v in kept.items()), rowid
        total_after += len(new.encode())
        misses += len(new.encode()) != size
        dst.execute(f"UPDATE {ITEMS} SET payload_json=? WHERE rowid=?", (new, rowid))
    dst.execute("COMMIT")
    dst.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    dst.close()
    print(json.dumps({"dense": str(out), "rows": len(rows), "bytes_before": total_before,
                      "bytes_after": total_after, "rows_with_other_size": misses}))
    # The reader must not see a difference: compare snapshots from the core binary.
    binary = Path(args.binary)
    env = {**os.environ, "T3CODE_HOME": home()}
    snaps = []
    for db in (lab, out):
        p = subprocess.run([str(binary), "--db", str(db), "--now-ms", NOW, "--dump-snapshot"],
                           capture_output=True, text=True, env=env, check=True)
        snap = json.loads(p.stdout)
        snap.pop("path", None)
        snaps.append(snap)
    # --dump-snapshot prints an empty snapshot stamped at 0 when the read fails.
    same = snaps[0] == snaps[1] and snaps[0]["at"] != 0 and len(snaps[0]["agents"]) > 0
    print(json.dumps({"snapshot_equal_to_lab": same, "agents": len(snaps[0]["agents"])}))
    if not same:
        sys.exit(1)


# ── Timing ───────────────────────────────────────────────────────────────────

def one(binary, db, ops, env):
    proc = subprocess.Popen([binary, "--bench-source", str(ops), "--db", db, "--now-ms", NOW],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    out, err = proc.stdout.read(), proc.stderr.read()
    _, status, usage = os.wait4(proc.pid, 0)
    proc.returncode = os.waitstatus_to_exitcode(status)
    if proc.returncode != 0:
        raise RuntimeError(f"{binary} exited {proc.returncode}: {err.decode()[:400]}")
    data = json.loads(out)
    calls = max(1, data["operations"])
    queries = {k: (v["prepare_ns"] + v["step_ns"]) / 1e6 / calls for k, v in data["phases"]["queries"].items()}
    rows = {k: v["rows"] // calls for k, v in data["phases"]["queries"].items()}
    phases = {k: v / 1e6 / calls for k, v in data["phases"].get("phases_ns", {}).items()}
    return {"phases_ms": phases, "median_ms": data["median_ms"], "errors": data["errors"], "checksum": data["checksum"],
            "queries_ms": queries, "rows": rows, "maxrss_kib": usage.ru_maxrss,
            "sqlite": data.get("sqlite_version")}


def summary(values):
    return {"median": round(statistics.median(values), 2), "min": round(min(values), 2), "max": round(max(values), 2)}


def run(args):
    binaries = [b.split("=", 1) for b in args.binary]
    dbs = [d.split("=", 1) for d in (args.db or [f"lab={LAB}", f"dense={DENSE}"])]
    env = {**os.environ, "T3CODE_HOME": home()}
    load_before = os.getloadavg()
    results = {db: {name: [] for name, _ in binaries} for db, _ in dbs}
    for trial in range(args.trials):
        order = binaries if trial % 2 == 0 else binaries[::-1]
        for db_name, db in dbs:
            for name, binary in order:
                results[db_name][name].append(one(binary, db, args.ops, env))
    load_after = os.getloadavg()
    report = {"now": NOW, "trials": args.trials, "ops_per_trial": args.ops,
              "load_before": load_before, "load_after": load_after, "dbs": {}}
    failures = 0
    for db_name, _ in dbs:
        report["dbs"][db_name] = {}
        checksums = {json.dumps(t["checksum"], sort_keys=True) for ts in results[db_name].values() for t in ts}
        if len(checksums) != 1:
            failures += 1
        for name, _ in binaries:
            ts = results[db_name][name]
            labels = sorted({k for t in ts for k in t["queries_ms"]})
            report["dbs"][db_name][name] = {
                "read_ms": summary([t["median_ms"] for t in ts]),
                "maxrss_mib": summary([t["maxrss_kib"] / 1024 for t in ts]),
                "errors": sum(t["errors"] for t in ts),
                "sql_ms": summary([sum(t["queries_ms"].values()) for t in ts]),
                "queries_ms": {k: summary([t["queries_ms"].get(k, 0.0) for t in ts]) for k in labels},
                "phases_ms": {k: summary([t["phases_ms"].get(k, 0.0) for t in ts])
                              for k in sorted({k for t in ts for k in t["phases_ms"]})},
                "rows": ts[-1]["rows"],
                "checksum": ts[-1]["checksum"],
                "sqlite": ts[-1]["sqlite"],
            }
        report["dbs"][db_name]["checksums_identical"] = len(checksums) == 1
    text = json.dumps(report, indent=1)
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(text + "\n")
    print(text)
    sys.exit(1 if failures else 0)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("--lab", default=str(LAB))
    b.add_argument("--out", default=str(DENSE))
    b.add_argument("--binary", default=str(ROOT / "target/release/t3-agents-core"))
    r = sub.add_parser("run")
    r.add_argument("--binary", action="append", required=True, help="NAME=PATH, repeatable")
    r.add_argument("--db", action="append", help="NAME=PATH, repeatable (default: lab real and dense)")
    r.add_argument("--trials", type=int, default=5)
    r.add_argument("--ops", type=int, default=7)
    r.add_argument("--out")
    args = p.parse_args()
    build(args) if args.cmd == "build" else run(args)


if __name__ == "__main__":
    main()
