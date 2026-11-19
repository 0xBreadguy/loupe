#!/usr/bin/env python3
"""Correctness gate and optional warm benchmark for the Linux metadata walker.

The fixture is deterministic, synthetic, and confined to a fresh home-volume
scratch directory. It has active, settled, missing
("gone"), and shared worktrees; nested target/node_modules; attributed and shared
scratch; cross-row and cross-bucket hard links; and symlinks to an outside sentinel.

`check` compares every inventory total and ownership row with an independent
Python scandir/lstat walk. `bench` alternates the built Rust harness and that
reference for at least five measured pairs. Both do an untimed walk immediately
before each timed walk. No caches are dropped, and these are warm results only.

Build checks/storage-walker first, then for example:

  python3 checks/bench/storage.py check --binary PATH
  python3 checks/bench/storage.py bench --binary PATH --root "$TMPDIR/swalk-bench" \
      --trials 7 --output "$TMPDIR/swalk-results.json"

Each command removes only its own marked fixture unless --keep is supplied. T3CODE_HOME
is a new empty folder inside that fixture. No T3 database is opened or needed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
GENERATED = ROOT / "checks/generated"
HOME = Path.home().resolve()
ACCOUNT = HOME / ".t3"
SCRATCH = Path(os.environ.get("TMPDIR") or HOME / ".cache").resolve()
if not SCRATCH.is_relative_to(HOME) or SCRATCH.is_relative_to(ACCOUNT):
    SCRATCH = HOME / ".cache"
UID = 4242
SESSION = {
    "active": "10000000-0000-4000-8000-000000000001",
    "settled": "10000000-0000-4000-8000-000000000002",
    "shared": "10000000-0000-4000-8000-000000000003",
    "gone": "10000000-0000-4000-8000-000000000004",
}
USAGE_KEYS = ("worktree_bytes", "dependency_bytes", "scratch_bytes", "other_bytes")


def usage():
    return {key: 0 for key in USAGE_KEYS}


def add_usage(target, source):
    for key in USAGE_KEYS:
        target[key] += source[key]


def bucket_usage(bucket, size):
    kind, dependency = bucket
    row = usage()
    if kind == "worktree":
        row["worktree_bytes"] = size
        if dependency:
            row["dependency_bytes"] = size
    elif kind == "scratch":
        row["scratch_bytes"] = size
    else:
        row["other_bytes"] = size
    return row


def merge_bucket(a, b):
    if a[0] == "worktree" or b[0] == "worktree":
        return ("worktree", a[0] == b[0] == "worktree" and a[1] and b[1])
    if a[0] == "scratch" or b[0] == "scratch":
        return ("scratch", False)
    return ("other", False)


def encoded(path):
    text = str(path)
    assert text.isascii(), "this fixture deliberately uses an ASCII cwd key"
    return "".join(c if c.isascii() and c.isalnum() else "-" for c in text)


def make_files(base, groups, per_group, label):
    payload = bytes((len(label) % 251 + 1,)) * 768
    for group in range(groups):
        folder = base / f"group-{group:02d}"
        folder.mkdir(parents=True)
        for index in range(per_group):
            (folder / f"file-{index:03d}").write_bytes(payload)


def make_scratch(root, cwd, name, groups, per_group):
    session = root / "temp" / f"claude-{UID}" / encoded(cwd) / SESSION[name]
    make_files(session / "tasks", groups, per_group, f"{name}-tasks")
    (session / "scratchpad").mkdir()
    (session / "scratchpad/note").write_bytes(b"synthetic scratch\n")
    return session


def scratch_path(argument):
    path = Path(argument).resolve()
    if path.is_relative_to(ACCOUNT) or path.is_relative_to(Path("/tmp")):
        raise ValueError("use a scratch folder outside the account data and system temp")
    if not any(base in path.parents for base in (GENERATED.resolve(), SCRATCH)):
        raise ValueError(f"choose a folder under {SCRATCH} or the checkout's checks/generated")
    return path


def prepare_root(argument):
    root = scratch_path(argument)
    if root.exists():
        raise ValueError("fixture root must be a new, non-default folder")
    root.parent.mkdir(parents=True, exist_ok=True)
    if root.parent.stat().st_dev != Path.home().stat().st_dev:
        raise ValueError("fixture parent is not on the home volume")
    if shutil.disk_usage(root.parent).free < 2 * 1024**3:
        raise ValueError("less than 2 GiB free; refusing to build the fixture")
    root.mkdir()
    (root / ".t3-storage-synthetic").write_bytes(b"synthetic metadata fixture\n")
    (root / "t3home").mkdir()
    return root


def build_fixture(root):
    managed = root / "worktrees"
    active = managed / "repo/active"
    settled = managed / "repo/settled"
    shared = managed / "repo/shared"
    gone = managed / "repo/gone"  # Deliberately referenced but absent.
    project = root / "checkout/repo"
    project.mkdir(parents=True)
    (project / "never-scan-project-content").write_bytes(b"x" * 8192)
    (root / "temp").mkdir()
    (root / "t3home").mkdir(exist_ok=True)

    make_files(active / "src/groups", 12, 50, "active-source")
    make_files(active / "src/target", 16, 50, "active-target")
    make_files(active / "node_modules", 14, 50, "active-modules")
    make_files(active / "vendor/package/node_modules", 8, 50, "active-nested-modules")
    make_files(settled / "src", 6, 50, "settled-source")
    make_files(settled / "nested/target", 8, 50, "settled-target")
    make_files(shared / "src", 4, 50, "shared-source")
    make_files(shared / "nested/node_modules", 6, 50, "shared-modules")

    # One inode changes from dependency to ordinary usage; one crosses folder rows.
    os.link(active / "src/target/group-00/file-000", active / "src/linked-source")
    os.link(active / "src/groups/group-00/file-000", settled / "cross-row-link")

    outside = root / "outside"
    make_files(outside, 1, 3, "outside-sentinel")
    os.symlink(outside, active / "points-outside")
    os.symlink(outside / "group-00/file-000", active / "points-outside-file")

    sessions = {
        "active": make_scratch(root, active, "active", 3, 50),
        "settled": make_scratch(root, settled, "settled", 2, 50),
        "shared": make_scratch(root, shared, "shared", 2, 50),
        # The settled thread still owns its scratch after its managed cwd disappears.
        "gone": make_scratch(root, gone, "gone", 1, 50),
    }
    unrelated = root / "temp" / f"claude-{UID}" / "logs/extra"
    make_files(unrelated, 1, 10, "unrelated-log")
    (root / "temp/unrelated-file").write_bytes(b"not under the Claude UID folder\n")
    return {
        "managed": managed,
        "project": project,
        "active": active,
        "settled": settled,
        "shared": shared,
        "gone": gone,
        "outside": outside,
        "sessions": sessions,
    }


class ReferenceWalk:
    """Independent lstat/scandir accounting; never follows a symlink."""

    def __init__(self, root, paths):
        self.root = root
        self.paths = paths
        self.folders = []
        self.projects = {"repo": usage()}
        self.threads = {name: usage() for name in ("active", "settled", "shared-a", "shared-b", "gone")}
        self.other = usage()
        self.total = usage()
        self.linked = {}

    def credit(self, value, folder):
        if folder is None:
            add_usage(self.other, value)
        else:
            add_usage(self.folders[folder]["usage"], value)

    def add(self, metadata, bucket, folder):
        size = metadata.st_blocks * 512
        if not stat.S_ISDIR(metadata.st_mode) and metadata.st_nlink > 1:
            key = (metadata.st_dev, metadata.st_ino)
            if key in self.linked:
                block = self.linked[key]
                if block["folder"] != folder:
                    block["folder"] = None
                block["bucket"] = merge_bucket(block["bucket"], bucket)
            else:
                self.linked[key] = {"bytes": size, "bucket": bucket, "folder": folder}
        else:
            self.credit(bucket_usage(bucket, size), folder)

    def walk(self, path, bucket, folder, skip=frozenset()):
        metadata = path.lstat()
        self.add(metadata, bucket, folder)
        if not stat.S_ISDIR(metadata.st_mode):
            return
        with os.scandir(path) as entries:
            for entry in entries:
                child = path / entry.name
                if child in skip:
                    continue
                metadata = entry.stat(follow_symlinks=False)
                child_bucket = bucket
                if stat.S_ISDIR(metadata.st_mode):
                    if bucket[0] == "worktree" and entry.name in ("target", "node_modules"):
                        child_bucket = ("worktree", True)
                    self.walk(child, child_bucket, folder, skip)
                else:
                    self.add(metadata, child_bucket, folder)

    def folder(self, path, kind, owner, project_id):
        index = len(self.folders)
        self.folders.append({
            "path": path.relative_to(self.root).as_posix(),
            "kind": kind,
            "owner": owner,
            "project_id": project_id,
            "coverage": "Unavailable",
            "usage": usage(),
        })
        return index

    def run(self):
        p = self.paths
        claims = [
            ("active", p["active"], {"kind": "Thread", "id": "active", "settled": False}, "repo"),
            ("settled", p["settled"], {"kind": "Thread", "id": "settled", "settled": True}, "repo"),
            ("shared", p["shared"], {"kind": "Shared"}, None),
            ("gone", p["gone"], {"kind": "Thread", "id": "gone", "settled": True}, "repo"),
        ]
        candidates = {path for _, path, _, _ in claims}
        for name, path, owner, project_id in claims:
            index = self.folder(path, "Worktree", owner, project_id)
            if name == "gone":
                self.folders[index]["coverage"] = "Missing"
            else:
                self.walk(path, ("worktree", False), index, candidates)
                self.folders[index]["coverage"] = "Complete"

        temp = self.root / "temp"
        uid_root = temp / f"claude-{UID}"
        self.add(uid_root.lstat(), ("other", False), None)
        session_claims = {
            p["sessions"]["active"]: ({"kind": "Thread", "id": "active", "settled": False}, "repo"),
            p["sessions"]["settled"]: ({"kind": "Thread", "id": "settled", "settled": True}, "repo"),
            p["sessions"]["shared"]: ({"kind": "Shared"}, None),
            p["sessions"]["gone"]: ({"kind": "Thread", "id": "gone", "settled": True}, "repo"),
        }
        with os.scandir(uid_root) as namespaces:
            for namespace in namespaces:
                namespace_path = uid_root / namespace.name
                metadata = namespace.stat(follow_symlinks=False)
                if not stat.S_ISDIR(metadata.st_mode):
                    self.add(metadata, ("other", False), None)
                    continue
                self.add(metadata, ("other", False), None)
                with os.scandir(namespace_path) as sessions:
                    for session in sessions:
                        session_path = namespace_path / session.name
                        metadata = session.stat(follow_symlinks=False)
                        if not stat.S_ISDIR(metadata.st_mode):
                            self.add(metadata, ("other", False), None)
                            continue
                        claim = session_claims.get(session_path)
                        if claim is None:
                            self.walk(session_path, ("other", False), None)
                        else:
                            owner, project_id = claim
                            index = self.folder(session_path, "Scratch", owner, project_id)
                            self.walk(session_path, ("scratch", False), index)
                            self.folders[index]["coverage"] = "Complete"

        for block in self.linked.values():
            self.credit(bucket_usage(block["bucket"], block["bytes"]), block["folder"])
        for row in self.folders:
            if row["project_id"] is not None:
                add_usage(self.projects[row["project_id"]], row["usage"])
                if row["owner"]["kind"] == "Thread":
                    add_usage(self.threads[row["owner"]["id"]], row["usage"])
            else:
                add_usage(self.other, row["usage"])
        add_usage(self.total, self.other)
        for value in self.projects.values():
            add_usage(self.total, value)
        self.folders.sort(key=lambda row: row["path"])
        return {
            "worktrees": "Complete",
            "claude": "Complete",
            "folders": self.folders,
            "projects": self.projects,
            "threads": self.threads,
            "other": self.other,
            "total": self.total,
        }


def native(binary, root, warmup=False):
    env = os.environ.copy()
    env["T3CODE_HOME"] = str(root / "t3home")
    command = [str(binary), "--fixture", str(root)]
    if warmup:
        command.append("--warmup")
    completed = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    if completed.returncode:
        raise RuntimeError(f"native walker exited {completed.returncode}: {completed.stderr[:1000]}")
    return json.loads(completed.stdout)


def fixture_stats(root):
    counts = {"directories": 0, "regular_files": 0, "symlinks": 0, "hardlink_entries": 0,
              "entries": 0, "unique_allocated_bytes": 0}
    seen = set()
    for directory, subdirs, files in os.walk(root, followlinks=False):
        for name in subdirs + files:
            path = Path(directory) / name
            metadata = path.lstat()
            counts["entries"] += 1
            if stat.S_ISLNK(metadata.st_mode):
                counts["symlinks"] += 1
            elif stat.S_ISDIR(metadata.st_mode):
                counts["directories"] += 1
            else:
                counts["regular_files"] += 1
                if metadata.st_nlink > 1:
                    counts["hardlink_entries"] += 1
            key = (metadata.st_dev, metadata.st_ino)
            if stat.S_ISDIR(metadata.st_mode) or key not in seen:
                counts["unique_allocated_bytes"] += metadata.st_blocks * 512
            seen.add(key)
    if counts["regular_files"] < 4000 or counts["entries"] < 4200:
        raise AssertionError(f"fixture is not representative: {counts}")
    if counts["unique_allocated_bytes"] > 128 * 1024**2:
        raise AssertionError(f"fixture allocation exceeds 128 MiB: {counts}")
    return counts


def comparable(document):
    return {key: value for key, value in document.items() if key != "elapsed_ns"}


def verify(actual, expected, root, paths, stats):
    if comparable(actual) != expected:
        print("EXPECTED\n" + json.dumps(expected, indent=2, sort_keys=True), file=sys.stderr)
        print("ACTUAL\n" + json.dumps(comparable(actual), indent=2, sort_keys=True), file=sys.stderr)
        raise AssertionError("native inventory differs from the independent metadata walk")
    rows = {row["path"]: row for row in actual["folders"]}
    shared = rows["worktrees/repo/shared"]
    shared_scratch = rows[paths["sessions"]["shared"].relative_to(root).as_posix()]
    assert shared["owner"] == {"kind": "Shared"}
    assert shared_scratch["owner"] == {"kind": "Shared"}
    assert actual["threads"]["shared-a"] == usage() and actual["threads"]["shared-b"] == usage()
    assert rows["worktrees/repo/gone"]["coverage"] == "Missing"
    assert not any(row["path"].startswith("outside") for row in actual["folders"])
    assert all(path.lstat().st_size > 0 for path in (paths["outside"] / "group-00").iterdir())
    assert actual["total"]["worktree_bytes"] < stats["unique_allocated_bytes"]
    assert not any((root / "t3home").iterdir()), "T3CODE_HOME did not stay empty"


def timed_reference(root, paths):
    ReferenceWalk(root, paths).run()  # warm, untimed
    start = time.perf_counter_ns()
    result = ReferenceWalk(root, paths).run()
    return (time.perf_counter_ns() - start) / 1e6, result


def timed_native(binary, root):
    result = native(binary, root, warmup=True)
    return result["elapsed_ns"] / 1e6, result


def summary(values):
    return {"median_ms": statistics.median(values), "min_ms": min(values), "max_ms": max(values)}


def cleanup(root):
    root = scratch_path(root)
    if (root / ".t3-storage-synthetic").is_file():
        shutil.rmtree(root)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("check", "bench"))
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--root", default=SCRATCH / f"t3-storage-fixture-{os.getpid()}", help="new home-volume fixture directory")
    parser.add_argument("--trials", type=int, default=7)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"built harness not found: {binary}")
    if args.command == "bench" and args.trials < 5:
        parser.error("bench requires at least five alternating pairs")
    root = prepare_root(args.root)
    try:
        paths = build_fixture(root)
        stats = fixture_stats(root)
        expected = ReferenceWalk(root, paths).run()
        actual = native(binary, root)
        verify(actual, expected, root, paths, stats)
        report = {"mode": "check", "result": "PASS", "entries": stats,
                  "total": actual["total"], "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
        if args.command == "bench":
            # One explicit warmup on each side precedes all recorded pairs. Each measured
            # invocation also does its own untimed warmup immediately before its timed walk.
            timed_native(binary, root)
            timed_reference(root, paths)
            samples = {"native": [], "reference": []}
            pairs = []
            load_before = os.getloadavg()
            for trial in range(args.trials):
                order = (("native", timed_native), ("reference", timed_reference))
                if trial % 2:
                    order = tuple(reversed(order))
                pair = {"trial": trial + 1, "order": [name for name, _ in order]}
                for name, run in order:
                    elapsed, result = run(binary, root) if name == "native" else run(root, paths)
                    if name == "native":
                        verify(result, expected, root, paths, stats)
                    elif result != expected:
                        raise AssertionError("reference changed between identical warm walks")
                    samples[name].append(elapsed)
                    pair[f"{name}_ms"] = elapsed
                pairs.append(pair)
            report = {
                "mode": "warm benchmark",
                "result": "PASS",
                "cache": "warm; untimed warmup before every timed walk; no cache dropping; no cold measurement",
                "timed_region": "one complete inventory or independent reference walk; process startup and fixture creation excluded",
                "trials": args.trials,
                "entries": stats,
                "total": actual["total"],
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "native": summary(samples["native"]),
                "reference": summary(samples["reference"]),
                "pairs": pairs,
                "load_average_before": load_before,
                "load_average_after": os.getloadavg(),
            }
        text = json.dumps(report, indent=2)
        if args.output:
            output = scratch_path(args.output)
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(text + "\n")
        print(text)
    finally:
        if not args.keep:
            cleanup(root)


if __name__ == "__main__":
    main()
