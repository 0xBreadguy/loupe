#!/usr/bin/env python3
"""What Trace costs the running dashboard: CPU with Trace open against closed, and against a
build without Trace.

Each trial starts the dashboard on a copy of a synthetic real-schema database inside tmux on
a private socket (a terminal in memory; no window opens), waits for it to settle, opens Trace
with `w` where the arm asks for it, then reads the process's CPU time from /proc over the
sampling period. Arms rotate their order every round. The database is the scale lab's tier
(checks/scripts/scale-db.ts) with one more thread added: a synthetic stream of 1,024 items with padded payloads, so Trace has a
large task to follow across refreshes.

  python3 -I checks/bench/trace-live.py --base OLD_BINARY --new NEW_BINARY --db LAB.sqlite \\
      --scratch FOLDER [--rounds 5] [--warm 4] [--seconds 20] [--compare-open]

--compare-open measures two builds that both have Trace, open, against each other (normal
and calm), instead of a build without Trace against one with it.

Every database is made up. Nothing reads ~/.t3: the environment is built from nothing, with
a stand-in home and an empty T3CODE_HOME. Not measured: the terminal's own work (tmux here),
memory, and the cost with a writer changing the database while it runs.
"""
import argparse, json, os, shutil, sqlite3, statistics, subprocess, sys, time

ITEM_COUNT = 1024
TICK = os.sysconf("SC_CLK_TCK")
P = "orchestration_v2_projection_"


def iso(ms):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(ms / 1000)) + ".%03dZ" % (int(ms) % 1000)


def add_running_task(path, now):
    """Add the running task used by both timing harnesses."""
    db = sqlite3.connect(path)
    a = now - 112 * 60_000
    m = lambda minutes: a + minutes * 60_000
    payload = {"id": "big", "projectId": "atlas", "title": "Fix the flaky checkout tests", "modelSelection": {"model": "gpt-6.1-sol", "options": []},
               "updatedAt": iso(now), "lineage": {"relationshipToParent": None}, "pullRequests": []}
    db.execute(f"INSERT INTO {P}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,active_provider_thread_id,created_at,updated_at,payload_json,provider_instance_id) "
               "VALUES('big','atlas','Fix the flaky checkout tests','codex','full-access','default','pt-big',?,?,?,'codex')", (iso(a), iso(now), json.dumps(payload)))
    run = {"id": "big-1", "threadId": "big", "status": "running", "providerThreadId": "pt-big", "requestedAt": iso(a), "startedAt": iso(a + 100),
           "providerInstanceId": "codex", "modelSelection": {"model": "gpt-6.1-sol", "options": []}}
    db.execute(f"INSERT INTO {P}runs(run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,completed_at,payload_json,provider_instance_id) "
               "VALUES('big-1','big',1,'codex','pt-big','running',?,NULL,?,'codex')", (iso(a), json.dumps(run)))
    filler = "made up " * 512
    kinds = ["command_execution", "file_search", "file_change", "reasoning", "command_execution", "dynamic_tool"]
    commands = ["rg -n flaky src", "bash -lc 'cd x && cargo test 2>&1 | tail -40'", "git status", "sed -n 1,80p x", "python3 run.py"]
    for i in range(ITEM_COUNT):
        start = m(0.1 + 100 * i / ITEM_COUNT)
        kind = kinds[i % len(kinds)]
        last = i + 1 == ITEM_COUNT
        end = None if last else start + (400 if i % 7 else 25_000)
        body = {"startedAt": iso(start), "completedAt": iso(end) if end else None, "schema": 2, "output": filler,
                "input": commands[i % len(commands)], "toolName": "mcp__t3-code__task_status"}
        db.execute(f"INSERT INTO {P}turn_items(turn_item_id,thread_id,run_id,node_id,provider_thread_id,provider_turn_id,parent_item_id,ordinal,type,status,updated_at,payload_json) "
                   "VALUES(?,'big','big-1',NULL,'pt-big',NULL,NULL,?,?,?,?,?)",
                   (f"big-i{i}", i + 1, kind, "running" if last else "completed", iso(end or start), json.dumps(body)))
    db.commit()
    db.close()


def cpu_seconds(pid):
    fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / TICK


def trial(program, db, env, arm, warm, seconds, socket):
    tmux = lambda *a: subprocess.run(["tmux", "-L", socket, "-f", "/dev/null", *a], env=env, capture_output=True, text=True)
    calm = " --calm" if arm.endswith("calm") else ""
    tmux("new-session", "-d", "-x", "120", "-y", "40", "-s", "bench", f"exec {program} --db {db}{calm}")
    try:
        time.sleep(warm)
        pid = int(tmux("display", "-p", "-t", "bench", "#{pane_pid}").stdout.strip())
        if "open" in arm:
            # The dashboard selects what needs you first; step down its list until Trace opens
            # on the added task.
            for _ in range(80):
                tmux("send-keys", "-t", "bench", "w")
                time.sleep(0.4)
                screen = tmux("capture-pane", "-t", "bench", "-p").stdout
                if "TRACE" in screen and "Fix the flaky checkout tests" in screen:
                    break
                tmux("send-keys", "-t", "bench", "w", "j")
                time.sleep(0.2)
            else:
                raise SystemExit(f"{arm}: Trace did not open on the running task:\n{screen}")
            time.sleep(2)
        before, t0 = cpu_seconds(pid), time.monotonic()
        time.sleep(seconds)
        after, t1 = cpu_seconds(pid), time.monotonic()
        return 100 * (after - before) / (t1 - t0)
    finally:
        tmux("kill-server")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True, help="a build without Trace")
    ap.add_argument("--new", required=True, help="the build with Trace")
    ap.add_argument("--db", required=True, help="the scale lab's database (checks/scripts/scale-db.ts --tier m)")
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--warm", type=float, default=4)
    ap.add_argument("--seconds", type=float, default=20)
    ap.add_argument("--compare-open", action="store_true", help="old and new builds, both with Trace open")
    args = ap.parse_args()
    scratch = os.path.abspath(args.scratch)
    home, empty = os.path.join(scratch, "home"), os.path.join(scratch, "emptyhome")
    os.makedirs(home, exist_ok=True)
    os.makedirs(empty, exist_ok=True)
    env = {"HOME": home, "XDG_CONFIG_HOME": home + "/.config", "XDG_DATA_HOME": home + "/.local/share", "XDG_CACHE_HOME": home + "/.cache",
           "XDG_STATE_HOME": home + "/.local/state", "PATH": "/usr/bin:/bin", "T3CODE_HOME": empty, "TERM": "xterm-256color",
           "COLORTERM": "truecolor", "LANG": "C.UTF-8"}
    arms = {"base": args.base, "closed": args.new, "open": args.new, "closed-calm": args.new, "open-calm": args.new}
    if args.compare_open:
        arms = {"old-open": args.base, "new-open": args.new, "old-open-calm": args.base, "new-open-calm": args.new}
    results = {arm: [] for arm in arms}
    socket = f"t3-trace-bench-{os.getpid()}"
    names = list(arms)
    for round_ in range(args.rounds):
        order = names[round_ % len(names):] + names[:round_ % len(names)]
        for arm in order:
            db = os.path.join(scratch, f"trial-{arm}.sqlite")
            shutil.copyfile(args.db, db)
            add_running_task(db, time.time() * 1000)
            percent = trial(os.path.abspath(arms[arm]), db, env, arm, args.warm, args.seconds, socket)
            results[arm].append(percent)
            print(f"round {round_ + 1} {arm:10s} {percent:6.2f}% CPU", flush=True)
            os.remove(db)
    print()
    for arm, values in results.items():
        print(f"{arm:10s} median {statistics.median(values):6.2f}%  range {min(values):.2f}..{max(values):.2f}%  trials {len(values)}")
    print(json.dumps(results))


if __name__ == "__main__":
    main()
