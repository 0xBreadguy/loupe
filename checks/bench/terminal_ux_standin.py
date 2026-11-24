#!/usr/bin/env python3
"""A stand-in terminal program for terminal_ux.py's own negative controls.

It is not the product and measures nothing about it. It draws a Live board and an Insights
screen as synchronized-output frames, reads a real file in a worker thread while "reading"
(so /proc/PID/io shows real read activity), and has fault modes that each break exactly one
property the harness must notice:

  good          loading frame first, keys answered during the read, ready frame when the read ends
  no-loading    the read blocks before any frame; only the ready frame is drawn
  blocked-keys  loading frame, then the read blocks the key loop; keys are answered after ready
  instant       loading and ready frames at once; no read
  no-io         loading for --read-ms, but nothing is read (a sleep dressed as a read)
  stale         the board never shows database changes (live events are never seen)
  coalesced     after the read, the loading and ready frames leave in one write (one read(2) for the harness)
  outside-sync  a finished report redraws with bytes ahead of the synchronized frame
  wrong-report  (--screen product) a switch to Tools finishes with Recap's body under the finished footer
  wrong-period  (--screen product) a period change finishes with the previous period's dates and body
  reread-snapshots  (--screen product) a revisit that should come from the snapshot reads again

--screen product draws what the real Insights screen draws where the harness looks: the header tabs, the
period row (dates computed here, independently of the harness), the report headings, the footer status
(`reading since` / `read at`, kept under help), the loading sentence, `▲ N above`, the first-read and
stale-snapshot rules, and an `r` that keeps the old body while it reads. It exists so the real product
profile can be run against known-bad programs. It still measures nothing about the product.

--esc-wait-ms sets the parser's wait for a lone Escape, to check the harness separates it.
Stdlib only. Reads the file named by --db read-only; writes nothing. Like the product under the
harness it is given --db and an EMPTY T3CODE_HOME, and it refuses to start if that folder holds
anything, so a harness that let it discover a database would be caught here as well.
"""
import argparse
import datetime as dt
import os
import re
import select
import sqlite3
import sys
import termios
import threading
import time
import tty
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--mode", default="good", choices=["good", "no-loading", "blocked-keys", "instant", "no-io", "stale", "coalesced", "outside-sync",
                                                    "wrong-report", "wrong-period", "reread-snapshots"])
ap.add_argument("--screen", default="standin", choices=["standin", "product"])
ap.add_argument("--read-ms", type=float, default=700)
ap.add_argument("--read-bytes", type=int, default=3 * 1024 * 1024)
ap.add_argument("--esc-wait-ms", type=float, default=25)
ap.add_argument("--interval", type=float, default=0.5, help="board poll period, seconds")
ap.add_argument("--db", required=True, help="the synthetic database to read; there is no discovery")
# Accepted and ignored, so the harness can pass the product's usual flags.
ap.add_argument("--calm", action="store_true")
ap.add_argument("--theme", default=None)
args = ap.parse_args()

home = Path(os.environ.get("T3CODE_HOME", ""))
if not home.is_dir() or any(home.iterdir()):
    sys.exit(f"stand-in: T3CODE_HOME must be an empty folder, got {home}")
DB = Path(args.db)
OUT = sys.stdout.buffer
state = {"screen": "board", "sel": 0, "help": False, "report": "Recap", "ready": False, "scroll": 0, "tick": 0}
done_r, done_w = os.pipe()


def titles() -> list[str]:
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    try:
        return [r[0] for r in db.execute("SELECT title FROM orchestration_v2_projection_threads ORDER BY thread_id LIMIT 16")]
    finally:
        db.close()


cache = {"titles": titles()}


def lines() -> list[str]:
    if args.screen == "product" and state["screen"] == "insights":
        return product_rows()
    if state["screen"] == "insights":
        out = [f" INSIGHTS  {state['report']} · this week", ""]
        if state["ready"]:
            out += ["  79.3 agent hours · ready", f"  scroll {state['scroll']}"]
        else:
            out += ["  reading…"]
        return out + ["", " tab report · j k scroll · i Esc back"]
    out = [" Loupe  ● LIVE (stand-in)" + (f" tick {state['tick']}" if state["tick"] else ""), ""]
    for i, t in enumerate(cache["titles"]):
        out.append(f"{'▌' if i == state['sel'] else ' '} ◉ {t}")
    out += ["", " AGENTS " + "─" * 90, " ? help  i insights  q quit"]
    if state["help"]:
        out[2] = out[2][:10] + "╭─ Loupe · help ─────────╮"
        out[len(out) - 2] = " " * 8 + "│ AGENTS help"
    return out


last = {"text": None}


def frame_bytes() -> bytes | None:
    """The synchronized frame for the current state, or None when nothing changed."""
    rows = lines()
    text = "\n".join(rows)
    if text == last["text"]:
        return None
    last["text"] = text
    buf = b"\x1b[?2026h\x1b[2J"
    for y, row in enumerate(rows):
        buf += f"\x1b[{y + 1};1H{row}".encode()
    buf += b"\x1b[?2026l"
    if args.mode == "outside-sync" and state["screen"] == "insights" and state["ready"]:
        buf = b"\x1b]0;insights\x07" + buf  # drawn outside any frame
    return buf


def draw() -> None:
    buf = frame_bytes()
    if buf is not None:
        OUT.write(buf)
        OUT.flush()


# ---------------------------------------------------------------------------- --screen product

PERIODS = ["tw", "lw", "tm", "lm"]
WORDS = {"tw": "this week", "lw": "last week", "tm": "this month", "lm": "last month"}
ABBR = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
FULL = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"]
pv = {"report": "Recap", "period": 0, "scroll": {"Recap": 0, "Tools": 0}, "cutoff": 0, "views": {}, "reading": None, "gen": 0}


def clock_ms() -> float:
    raw = os.environ.get("T3_FIXED_NOW", "")
    return float(raw) if raw.strip().isdigit() else time.time() * 1000


def hms() -> str:
    return dt.datetime.fromtimestamp(clock_ms() / 1000, dt.timezone.utc).strftime("%H:%M:%S")


def labels() -> dict:
    """The period row's dates for the four periods at the display clock: written here on purpose, not shared
    with the harness, so that the two have to agree."""
    today = dt.datetime.fromtimestamp(clock_ms() / 1000, dt.timezone.utc).date()
    monday = today - dt.timedelta(days=today.weekday())
    first = today.replace(day=1)
    last_end = first - dt.timedelta(days=1)

    def week(a: dt.date) -> tuple[str, str]:
        b = a + dt.timedelta(days=6)
        short = f"{ABBR[a.month - 1]} {a.day} – {b.day}" if a.month == b.month else f"{ABBR[a.month - 1]} {a.day} – {ABBR[b.month - 1]} {b.day}"
        return short, f"{short}, {b.year}"

    month = lambda d: (f"{FULL[d.month - 1]} {d.year}",) * 2
    return {"tw": week(monday), "lw": week(monday - dt.timedelta(days=7)), "tm": month(first), "lm": month(last_end)}


def view_key() -> tuple:
    return (pv["report"], pv["period"])


def body_lines(report: str, period: int) -> list[str]:
    if report == "Recap":
        return ["  AGENT HOURS        TASKS        MOST AT ONCE", "  79.3h", "", " AGENTS AT WORK, BY HOUR ─────────  WHERE THE HOURS WENT ────────", *[f"  row {i}" for i in range(16)],
                " HOW THIS IS COUNTED ───────────"]
    return ["  CALLS          CALL TIME        FAILED        LONGEST CALL", "  992", "", " HOW LONG THEY TOOK ───────", *[f"  tool row {i}" for i in range(30)],
            " LONGEST CALLS ─────────────────────────", *[f"  call {i}" for i in range(12)]]


def product_rows() -> list[str]:
    H, W = 40, 120
    reading, key = pv["reading"], view_key()
    view = pv["views"].get(key)
    fresh = view is not None and view["cutoff"] == pv["cutoff"] and reading is None
    shown = reading is not None and reading["key"] == key
    view_period = view.get("wrong_dates", pv["period"]) if view is not None else pv["period"]
    dates = labels()[PERIODS[view_period if (fresh or (shown and reading["body"] == "snapshot")) else pv["period"]]]
    right = lambda text, row: row + " " * max(1, W - len(row) - len(text)) + text
    rows = [right(f"lane 30m  {hms()}", " Loupe  ● LIVE 2s   " + ("RECAP  TOOLS"))]
    tabs = "  " + "   ".join(WORDS[p] for p in PERIODS)
    # the body the screen shows: a finished read, or the old snapshot while a refresh runs, else the loading sentence
    scrollable = fresh or (shown and reading["body"] == "snapshot" and view is not None)
    if scrollable:
        note = f"{dates[1]} · so far · 4 days recorded"
        body = view["lines"]
    elif shown:
        note = dates[0]
        body = [f"  Reading {pv['report']} for {WORDS[PERIODS[pv['period']]]} from T3, started {reading['since']}.", "  Nothing is shown until the period is counted.",
                "  i returns to Live. A read may finish in the background."]
    else:
        note, body = dates[0], []
    status = f"reading since {reading['since']}" if reading is not None else f"read at {view['read_at']}" if view else "read at"
    rows.append(right(note, tabs))
    avail = H - 3
    if state["help"]:
        body = [" INSIGHTS HELP " + "─" * 100, "  tab / ⇧tab   Recap or Tools", "  ← →          previous or next period", "  ↑↓ / j k     scroll a row", "  i / Esc      dashboard · Esc closes help first"]
        keys = " ? close help  i back  q quit"
        scroll = 0
    else:
        keys = " ? help  i back  tab report  ←→ period  r refresh  ↑↓ scroll  q quit"
        scroll = pv["scroll"][pv["report"]] if scrollable else 0
    window = body[scroll:scroll + avail]
    if scroll > 0 and window:
        window[0] = f" ▲ {scroll} above"
    rows += window + [""] * (avail - len(window))
    rows.append(right(status, keys))
    return rows


def start_product_read(snapshot: bool) -> None:
    key = view_key()
    old = pv["views"].get(key)
    pv["reading"] = {"key": key, "since": hms(), "body": "snapshot" if snapshot and old else "loading", "cutoff": pv["cutoff"], "gen": pv["gen"]}
    read_cutoff = pv["cutoff"]
    if args.mode == "no-loading":
        read_work()
        finish_product_read()
        return
    if args.mode == "coalesced":
        loading = frame_bytes()
        read_work()
        finish_product_read(draw_now=False)
        OUT.write((loading or b"") + (frame_bytes() or b""))
        OUT.flush()
        return
    draw()
    if args.mode == "instant":
        finish_product_read()
    elif args.mode == "blocked-keys":
        read_work()
        finish_product_read()
    else:
        def worker() -> None:
            read_work()
            os.write(done_w, b"p")
        threading.Thread(target=worker, daemon=True).start()


def finish_product_read(draw_now: bool = True) -> None:
    reading = pv["reading"]
    if reading is None or reading["gen"] != pv["gen"]:
        return
    report, period = reading["key"]
    shown_report = "Recap" if (args.mode == "wrong-report" and report == "Tools") else report
    shown_period = max(0, period - 1) if (args.mode == "wrong-period" and period > 0) else period
    pv["views"][(report, period)] = {"cutoff": reading["cutoff"], "read_at": hms(), "lines": body_lines(shown_report, shown_period)}
    if args.mode == "wrong-period" and period > 0:
        pv["views"][(report, period)]["wrong_dates"] = shown_period
    pv["reading"] = None
    if draw_now:
        draw()


def navigate() -> None:
    key = view_key()
    view = pv["views"].get(key)
    if args.mode == "reread-snapshots" and view is not None:
        view = None
    if view is None or view["cutoff"] != pv["cutoff"]:
        start_product_read(False)
    else:
        pv["reading"] = None
        draw()


def product_key(name: str) -> bool:
    s = state
    if s["screen"] == "board":
        if name == "q":
            return False
        if name == "?":
            s["help"] = not s["help"]
        elif name == "escape" and s["help"]:
            s["help"] = False
        elif name == "j":
            s["sel"] = min(len(cache["titles"]) - 1, s["sel"] + 1)
        elif name == "k":
            s["sel"] = max(0, s["sel"] - 1)
        elif name == "i":
            s["screen"] = "insights"
            s["help"] = False
            pv.update(report="Recap", period=0, cutoff=0, views={}, reading=None, gen=pv["gen"] + 1, scroll={"Recap": 0, "Tools": 0})
            start_product_read(False)
            return True
        draw()
        return True
    if s["help"]:
        if name in ("?", "escape"):
            s["help"] = False
        draw()
        return True
    if name == "q":
        return False
    if name in ("i", "escape"):
        s["screen"] = "board"
        pv["gen"] += 1
        pv["reading"] = None
    elif name == "?":
        s["help"] = True
    elif name in ("tab", "shift-tab"):
        pv["report"] = "Tools" if pv["report"] == "Recap" else "Recap"
        navigate()
        return True
    elif name in ("right", "left"):
        step = 1 if name == "right" else -1
        new = min(3, max(0, pv["period"] + step))
        if new != pv["period"]:
            pv["period"] = new
            pv["scroll"][pv["report"]] = 0
            navigate()
            return True
    elif name == "r":
        pv["cutoff"] += 1
        start_product_read(True)
        return True
    elif name in ("j", "k", "home", "end"):
        view = pv["views"].get(view_key())
        if view is not None and pv["report"] == "Tools":
            top = pv["scroll"]["Tools"]
            top = {"j": top + 1, "k": top - 1, "home": 0, "end": 10 ** 6}[name]
            pv["scroll"]["Tools"] = max(0, min(top, len(view["lines"]) - (40 - 3)))
    draw()
    return True


def read_work() -> None:
    """The 'read': real file reads in chunks over --read-ms, or a bare sleep in no-io mode."""
    chunk = 128 * 1024
    steps = max(1, args.read_bytes // chunk)
    pause = args.read_ms / 1000 / steps
    with open(DB, "rb", buffering=0) as handle:
        for _ in range(steps):
            if args.mode == "no-io":
                time.sleep(pause)
                continue
            data = handle.read(chunk)
            if not data:
                handle.seek(0)
            time.sleep(pause)


def finish_read() -> None:
    state["ready"] = True
    draw()


def start_read() -> None:
    state.update(screen="insights", ready=False, scroll=0, report="Recap")
    if args.mode == "no-loading":
        read_work()          # blocks before any frame
        finish_read()
        return
    if args.mode == "coalesced":
        loading = frame_bytes()
        read_work()          # blocks, then both frames go out in a single write
        state["ready"] = True
        OUT.write((loading or b"") + (frame_bytes() or b""))
        OUT.flush()
        return
    draw()                   # the loading frame
    if args.mode == "instant":
        finish_read()
    elif args.mode == "blocked-keys":
        read_work()          # blocks the key loop; keys wait in the tty queue
        finish_read()
    else:
        def worker() -> None:
            read_work()
            os.write(done_w, b"x")
        threading.Thread(target=worker, daemon=True).start()


ARROWS = {b"\x1b[C": "right", b"\x1b[D": "left", b"\x1b[Z": "shift-tab", b"\x1b[H": "home", b"\x1b[F": "end"}
NAMES = {b"\t": "tab", b"\x1b": "escape"}


def key(k: bytes) -> bool:
    if args.screen == "product":
        return product_key(NAMES.get(k, k.decode("latin-1")))
    s = state
    if s["screen"] == "board":
        if k == b"q":
            return False
        if k == b"?":
            s["help"] = not s["help"]
        elif k == b"\x1b" and s["help"]:
            s["help"] = False
        elif k == b"j":
            s["sel"] = min(len(cache["titles"]) - 1, s["sel"] + 1)
        elif k == b"k":
            s["sel"] = max(0, s["sel"] - 1)
        elif k == b"x":      # the timing floor: any key that redraws one number
            s["tick"] += 1
        elif k == b"i":
            start_read()
            return True
    else:
        if k in (b"i", b"\x1b"):
            s["screen"] = "board"
        elif k == b"\t":
            s["report"] = "Tools" if s["report"] == "Recap" else "Recap"
        elif k == b"j" and s["ready"]:
            s["scroll"] += 1
        elif k == b"k" and s["ready"]:
            s["scroll"] = max(0, s["scroll"] - 1)
    draw()
    return True


def main() -> None:
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    tty.setraw(fd)
    try:
        draw()
        buf = b""
        esc_at = None
        poll_at = time.monotonic() + args.interval
        version = sqlite3.connect(f"file:{DB}?mode=ro", uri=True).execute("PRAGMA data_version").fetchone()[0]
        while True:
            timeout = max(0.0, poll_at - time.monotonic())
            if esc_at is not None:
                timeout = min(timeout, max(0.0, esc_at - time.monotonic()))
            ready, _, _ = select.select([fd, done_r], [], [], timeout)
            if done_r in ready:
                os.read(done_r, 16)
                if args.screen == "product":
                    finish_product_read()
                else:
                    finish_read()
            if fd in ready:
                data = os.read(fd, 256)
                if not data:
                    return
                buf += data
            while buf:
                if buf[:1] == b"\x1b":
                    if len(buf) == 1:
                        if esc_at is None:
                            esc_at = time.monotonic() + args.esc_wait_ms / 1000
                        break
                    if buf[1:2] == b"[":
                        found = re.match(rb"\x1b\[[0-9;]*[@-~]", buf)
                        if found is None:
                            break
                        seq = found.group(0)
                    else:
                        seq = buf[:2]
                    buf = buf[len(seq):]
                    esc_at = None
                    if args.screen == "product" and seq in ARROWS and not product_key(ARROWS[seq]):
                        return
                    continue
                esc_at = None
                k, buf = buf[:1], buf[1:]
                if not key(k):
                    return
            if esc_at is not None and time.monotonic() >= esc_at:
                esc_at = None
                buf = b""
                if not key(b"\x1b"):
                    return
            if time.monotonic() >= poll_at:
                poll_at = time.monotonic() + args.interval
                if args.mode != "stale":
                    cache["titles"] = titles()
                    draw()
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)


main()
