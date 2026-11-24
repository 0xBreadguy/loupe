# Terminal UX benchmark

`terminal_ux.py` times the real terminal presenter on a private PTY: no tmux, no emulator, no
desktop, no network, and no hook in the product. It answers the program's start-up queries like
a foot terminal, applies its output to a small virtual screen, and accepts a latency only when a
synchronized-output frame (`ESC[?2026l`) shows the expected marker. Standard library only.

`terminal_ux_standin.py` is a stand-in program with fault modes, used only for the harness's own
negative controls.

The product profile matches the Insights screen at 120×40. Run `selftest` to exercise
the profile against deliberately broken behavior. Each timing report includes its own
eligibility result in `claim.claimable` and lists the reasons when a run misses a requirement.

## Isolation: what the product is given

Every run that starts a product (`live`, `insights`, `snap`, `floor`, the selftest) gives it:

| What | Value |
| --- | --- |
| Data | `--db FILE`, a copy of a fixture's database made for this trial inside the scratch folder; or `--demo` (`snap` only) |
| `T3CODE_HOME` | a new **empty** folder in the scratch; checked empty at the start and again at the end |
| `HOME`, four XDG folders, `TMPDIR`, `SQLITE_TMPDIR`, `XDG_RUNTIME_DIR` | new folders in the scratch; nothing is inherited |
| `LOUPE_NO_CACHE` | `1` (the persistent report cache is opt-in; measure without it) |
| `T3_FIXED_NOW` | the fixture's own `now_ms` (pinned runs), or unset (`--real-clock`) |

`product_command` is the only place that builds the argument list. `--db` and `--demo` are added
there and nowhere else, never both and never neither. The extra arguments a caller may pass are an
allowlist (`--calm`, and plain-word `--theme`, `--interval` and the stand-in's flags); anything else,
including `--db`, `--demo`, any snapshot or `--bench-*` flag, and any value with a slash, is refused
before a file is touched. So a command-line override cannot point the product at another database.

Files are checked before they are opened:

- A fixture is accepted only if it has the manifest this script writes (`synthetic: true`). Its
  files are opened with `O_NOFOLLOW` at every level below the fixture folder, so a symlink, inside
  or escaping, is refused, and so is a file that is not regular or has more than one link.
- The database is read once, hashed while it is copied, and must equal the manifest's hash. A
  swapped or edited file is refused.
- A fixture, scratch folder or lock file that is itself a symlink is refused. `/tmp` and the real
  `~/.t3` are refused after resolving. Folders the harness creates must be new (`mkdir` without
  `exist_ok`), so an existing folder cannot supply a populated home.

**The Storage-only scenario is the one exception.** Storage finds worktrees by automatic
discovery, which needs `T3CODE_HOME` to be a populated data folder, so `storage` cannot run under the
`--db` rule. It still works for its original scope: it gives the product a copy of a synthetic
fixture as `T3CODE_HOME` and no `--db`, and it refuses to start without
`--accept-discovery-outside-db-rule`. It never touches the real home, but it is a weaker isolation
than every other command, so run it only on purpose and on a fixture built with `--worktrees`. It was
not run when the harness was repaired.

## The clock and the dated fixture

The fixture is built relative to an instant, `now_ms`, which is the real clock at the moment `fixture` runs
(`--now-ms` pins another one). Every timestamp, the board's running threads, the history and the manifest
use it. History reaches back to the start of last month at about 10.7 threads a day (the density of the
original 150 threads over 14 days), so this week, last week, this month and last month each hold records.
The build refuses a clock at which one of them would hold fewer than three runs (early on a Monday or on the
first of a month): the product shows another period's data under its own label when one is empty, which
would make a period marker lie. Thread payloads carry `createdAt`, which is how the product knows when the
records begin (without it a report says `0 days recorded`). The fixture is 50 days and about 390 MB on
2026-11-19; its size follows the date.

| Run | Display clock | Rule |
| --- | --- | --- |
| default | `T3_FIXED_NOW` = the fixture's `now_ms` | reproducible reports; the periodic headroom refresh is suppressed and the header clock never changes, so a still report draws nothing |
| `--real-clock` | the product's own clock, at its normal 2 s refresh | the fixture must be younger than `--max-fixture-age-min` (90), built before now, and in this week and month; every trial checks it did not cross a week or month boundary |

Build the fixture just before a real-clock run. Live events are stamped with the real clock under
`--real-clock` and with the pinned clock plus the event number otherwise. Under the real clock the header
clock redraws about once a second, so a report that is open is not idle on the wire (`idle_frames`).

The harness names a period by what the screen says. The period row says `Nov 16 – 22` while a first read is
only requested, `Nov 16 – 22, 2026 · so far · N days recorded` once read (a refresh keeps the old body and
the long form), and a month `November 2026`. `<<dates:tw>>` (this week; also `lw`, `tm`, `lm`) and
`<<short:...>>` are expanded at the display clock, at the moment of use, and `dates-check` compares them with
the real product at seven pinned clocks (a week across a month and across a year, a leap March, a Sunday). A
clock inside the fixture's records is not a valid check there, because an empty previous period beside a
current one with data shows the current period under its own label.

## Two profiles, kept apart

| File | For | Used by |
| --- | --- | --- |
| `terminal_ux_standin_profile.json` | `terminal_ux_standin.py` | the selftest, with `--profile-kind stand-in` |
| `terminal_ux_profile.json` | the Insights screen | `insights`, `snap` marker choice, and the selftest's product-shaped stand-in |

Each file says `kind`; a profile of the other kind is refused, so a stand-in profile can never time a
product. The product profile also says `calibrated`. While it is `false`, every result carries a claim reason
and `claimable` is false. A binary that begins with `#!` (a script, which the stand-in is and the product never
is) is recorded as `binary.is_script` and makes the run unclaimable whatever the profile says.

The product profile is **state-driven** (`"format": 2`). A step says which screen it ends on, `to` (a
report, a period, and whether the report may be scrolled), whether it `reads`, and the harness builds every
marker from that: the footer status (`reading since HH:MM:SS` / `read at HH:MM:SS`, kept under help), the
period row's dates, the report's body heading (Recap `WHERE THE HOURS WENT`, Tools `LONGEST CALL`) and, for a
first read, the loading sentence that names the report and the period. It also tracks the state it believes
the screen is in and checks the screen against it before every key. Markers can still be written by hand
(`{"all": [...]}`, `{"any": [...]}`, a regular expression); the stand-in profile does.

What the candidate does, as the profile records it: `i` opens Recap for this week and reads; a first visit to
a report or period reads; a revisit in the same visit is answered from the snapshot; `r` reads the shown
report again and keeps its old body until it finishes; and a refresh makes the other report's snapshot
stale, so the next switch to it reads. The profile's walk covers open, report switch (first read, stale
re-read, from snapshot), `r` with scrolling during the read, period switches in both directions (first read
and from snapshot, weeks and months), scrolling, Escape and `i` out.

## Three figures, never merged

All start at the PTY write of the key, taken with `perf_counter_ns` immediately before the write,
and stop at the `read(2)` that completed the accepted frame (not at the later parse).

| Figure | From the key to | Reported as |
| --- | --- | --- |
| First visible response | a frame showing the loading marker | `first_visible_response_ms` |
| Report ready | the frame that completes the read (ready marker, no loading marker) | `report_ready_ms` |
| Input latency during a read | the key's own response, for a key written after the loading frame and before the ready frame | `input_latency_during_an_active_read` |

`read_in_progress_ms` (loading frame to ready frame) is reported beside them. The Live board's own
key latency is reported before the first open and after the return (`live_board_key_*`), and idle
CPU is reported with the board and with a report open, so Live, an open report and an active read
each have their own figures. Summaries are the median and range of the trials' medians
(CONTRIBUTING), plus the pooled samples and a nearest-rank p95.

**Keys during a read must not supersede it.** Two kinds of key are scored during a read, and neither starts
another read: help (`?` to open and to close, on the open's first read, where the footer keeps saying
`reading since` and the body is replaced by the help text and then the loading sentence again), and scrolling
(`j`) during an `r` refresh, which scrolls the old snapshot while the new read runs (`▲ 1 above`, `▲ 2 above`,
footer still reading). A report or period switch starts another read and would end the one being timed, so no
switch is used for this figure. Scrolling a long table during a read is the more telling workload; help is the
cheapest, and is kept as the second figure.

## What makes a sample count (all fail closed)

- **Frame before read completion.** A loading frame must arrive before any ready frame. A ready
  frame with no loading frame before it rejects the sample. A reopen that shows no loading frame is
  therefore rejected too, which is the right result if reopening is meant to reread.
- **Separate reads.** If the loading frame and the ready frame came out of one `read(2)`, their
  times cannot be told apart, and the sample is rejected. The same holds for a key's response and
  the ready frame. Frames that shared a read are counted in `evidence.frames_sharing_a_read`.
- **The read really was running.** `/proc/PID/io` `rchar` (bytes asked of `read(2)`, all threads)
  must rise by at least `--min-read-bytes` (64 KiB) between the loading frame and the ready frame.
  An unreadable counter is absent evidence and rejects the sample; it is never taken as zero.
- **A scored key really occurred during a read.** It is written after the loading frame, with no
  ready frame yet seen. Its response must show its marker and none of its `absent` markers (for
  help, the finished status), no ready frame may come first, the response keeps the loading marker
  (unless the profile says otherwise), and `rchar` must rise by at least `--min-read-bytes` between
  the key's write and the ready frame. Each rejection carries its reason in
  `evidence.rejected_reasons`.
- **State is recognized at every step (fail closed, never a rejected sample).** Before each key of a
  state-driven profile the screen must show the state the harness believes it is in (footer status, report
  heading or a scrolled body, the period's dates). A step declared as a read that is answered with no loading
  frame is an error, because the product answered from a snapshot or the profile is out of date. A step
  declared instant (a key answered from what is already read) forbids any frame showing the loading status
  between the key and its answer: the old rule of "first frame that shows the marker" would have timed a read
  as a fast key, and on this screen a switch back to a stale report does exactly that. After a during-read
  attempt the screen must be back on the open's finished state before leaving, so a deferred key that opened
  help late is caught as a state error. `scroll-reset` (`Home`) is a step so the scroll steps start at the top.
- **Every part of the workload was measured.** The run is invalid if a step declared as a read never produced
  an accepted sample, or a during-read key never landed during a read. `evidence.steps` lists, per step, reads
  attempted and accepted, instant keys and each during-read key's attempts and acceptances, and
  `during_key_share_of_read_still_to_come` says what share of the read's bytes was still to be read when the
  key landed (a key at the very end of a read measures little).
- **Synchronized frames.** The harness sees only frames bracketed by `ESC[?2026h`/`ESC[?2026l`
  (the encoder wraps every frame this way and skips identical ones). Bytes drawn outside a frame
  after the first are counted (`bytes_outside_sync_frames_after_first`) and make the run unclaimable,
  because progress drawn that way would not have been observed. A change the product draws as no
  frame at all (an identical screen) produces no frame, so a marker must differ between states.
- **Isolation held.** Each trial records its `--db`, its `T3CODE_HOME` and what that folder
  held at the end. A non-empty folder at the end invalidates the run.
- A run with no accepted cold open, too few accepted during-read keys, or a return that does not
  reach the Live board is `valid: false` and exits 2. A marker that never appears, a missed key, a
  missed event, a product that exits, or a lock already held is `FAIL (fail closed)` and exits 3.
- A `claim` block says whether numbers may be quoted: at least five alternating rounds, 1-minute
  load average under `--load-limit` (2.0) at every trial start and end, binaries unchanged during
  the run, no control flag, a calibrated product profile, evidence complete. `--lock FILE` takes an
  exclusive `flock` so two runs that share the file cannot overlap. Do not time while other heavy jobs
  run.

## Escape: program time apart from the parser's wait

The parser holds a lone Escape for `ESC_WAIT` (`crates/term/src/keys.rs`) before reporting it. So
a close by Escape includes that wait and a close by another key does not. Both are measured on the
same screen, alternating which goes first, and reported three ways: Escape minus the other key (the
better figure), Escape minus the source constant (`--source-root` re-reads `ESC_WAIT`), and the
other-key latency as the program time. The same pairing runs on the Live board's help overlay (`?`
against Escape). `i` returns from Insights and is the profile's `alt_key`; the help steps use `?`
for both directions so no timed key carries the parser's wait.

## Commands and replay

Build the programs first, with the target on a real disk, and keep the binaries that are timed. Every scratch
folder must be new, on a real disk, and outside `/tmp` and `~/.t3`. Build the fixture **just before** the run:
its data is relative to the instant it was built, and a real-clock run refuses a fixture older than 90 minutes.

```sh
S=$HOME/tmp/ux-scratch && mkdir -p $S/tmp && export TMPDIR=$S/tmp CARGO_TARGET_DIR=$S/target
cargo build --release --locked -p loupe-term && cp $S/target/release/loupe-term $S/new   # and $S/old from the base commit
cd checks/bench
python3 -I terminal_ux.py selftest --scratch $S/selftest       # the harness's own controls; about 15 minutes on a busy machine
python3 -I terminal_ux.py fixture --root $S/fixture            # dated, relative to now; only its database is used
python3 -I terminal_ux.py dates-check --binary $S/new --fixture $S/fixture --scratch $S/dates   # period labels against the product
python3 -I terminal_ux.py floor --fixture $S/fixture --scratch $S/floor --rounds 6 --keys 100
python3 -I terminal_ux.py snap --binary $S/new --fixture $S/fixture --scratch $S/snap --keys i wait:1 tab wait:1   # look at frames
python3 -I terminal_ux.py snap --binary $S/new --demo --scratch $S/snap-demo --keys i wait:1                       # or the demo
```

Alternating old and new on the Live board, at the board's normal 2 s refresh and the real clock (one short
qualification round; the scored run is `--rounds 5` or more with `--lock` and a quiet machine):

```sh
python3 -I terminal_ux.py live --old $S/old --new $S/new --fixture $S/fixture --scratch $S/live \
    --real-clock --rounds 1 --lock $S/timing.lock --source-root ../..
```

Candidate alone, Insights open, report and period switches, refresh, scrolling, exit, same conditions:

```sh
python3 -I terminal_ux.py insights --new $S/new --fixture $S/fixture --scratch $S/insights \
    --real-clock --rounds 1 --lock $S/timing.lock --source-root ../..   # --rounds 5 or more to be claimable
```

Leave out `--real-clock` to pin the clock to the fixture's (reproducible, no header ticks, no headroom
refresh). Pass `--product-arg=--interval=0.5` for more event resolution; the default is the product's own 2 s.
A round is about 25 s for `insights` and a minute for `live`. Both need a calibrated product profile and five
rounds to be claimable.

Storage-only, outside the `--db` rule (see Isolation; needs the flag):

```sh
python3 -I terminal_ux.py fixture --root $S/fixture-storage --worktrees 75 --files 12000 --history-threads 4 --runs-per-thread 2 --items-per-run 3
python3 -I terminal_ux.py storage --new $S/new --fixture $S/fixture-storage --scratch $S/storage --rounds 5 --accept-discovery-outside-db-rule
```

Each trial copies the fixture's database and checks its hash, so every arm starts from identical bytes.

`live`: one seeded script, the same offsets, events and keys for both arms. An event is a title
change committed as T3 commits one (projection row, an `orchestration_events` row of application
version 2, the `thread-projections` cursor). Keys are `j`/`k` with a minimum gap; a key written within
400 ms after a commit is reported separately as near an event. Arms alternate order each round;
equal script, initial database and final state are checked and recorded in `matched`.

The selftest runs the stand-ins only. Besides the fault modes it has negative controls for each
isolation rule above (every one must be refused before a product is started), controls that run the real
product profile against the product-shaped stand-in (a good one passes every step; a wrong report, a wrong
period, a snapshot that reads again, a switch with no loading frame, a deferred key response and a read with no
read activity are each refused), controls for the dated fixture and the clock, and a check that the fast
virtual screen equals the byte-at-a-time reference.

## Work per event

- **Per frame** (one `ESC[?2026l` found, the frame applied to the virtual screen, the 40-row text
  joined once), and, in `insights` only, two small `/proc/PID` reads for the read evidence. `live` and
  `storage` read no `/proc` counters per frame. The virtual screen applies a frame a token at a time (escape
  sequences by regular expression, text runs by slice or character, OSC 66 parses cached). A full Insights frame
  cost 5.6 ms through the original byte-at-a-time machine and about 0.9 ms through this one, which matters
  because handling time delays the next read. The byte machine is kept as `ReferenceScreen`; anything not
  well formed goes through it for the whole call, and the selftest replays random and malformed input through
  both and compares the whole state.
- **Per key:** one `perf_counter_ns` and one `write`. Waiting uses `select`, not a spin. No timer runs
  in the harness; idle windows sample the process's CPU twice. Before a state-driven step the screen is read
  once for the state check (a regular expression on the 40-row text, per key, not per frame).
- **Per trial:** the fixture database is read once (opened without following links), hashed as it is
  copied, and never read back. A fixture's `-wal` and `-shm` files are not copied.
- The harness's own cost is measured, not assumed: `floor` times a stand-in that redraws one number
  per key with and without the `/proc` samples, and every result reports `harness_overhead`
  (per-frame handling). Latency stops at the `read(2)`, so handling time is outside every figure,
  but it delays the next read: do not subtract the floor from a product figure, it is a lower bound.
- The product is observed from outside. No production hook, flag, environment variable or build is used.

## Evidence gaps and known pending validation

1. **Calibration covers one screen size.** The product profile was calibrated on the built terminal
   at 120×40 with a synthetic dated fixture. Coverage excludes other widths, different answers to startup queries, ARM,
   macOS, a real database or a real terminal emulator. A change to the screen's text needs `snap` and
   `dates-check` again.
2. **Periods are scored, by label.** The period row's dates are tied to the display clock and checked against
   the product at seven pinned clocks. An empty previous period can fall forward to the current period.
   The timed fixture has records in all four periods so each navigation target has a distinct date label.
3. **Read evidence is `rchar`.** It sees reads through `read(2)` and so a SQLite file read without mmap. The
   candidate's reader raised it by about 19 MB for a this-week read on the default fixture, so it works here.
   If a later build reads through mmap, or from a pre-built cache, `rchar` can stay flat during a real read and
   every sample would be rejected (a false negative, not a false pass). The cache is opt-in and the harness sets
   `LOUPE_NO_CACHE=1`. If mmap is used, replace the evidence with per-thread CPU. The harness cannot tell
   a read thread from the main thread by `rchar` alone, so it proves that reading continued, not which thread read.
4. **A key is "during a read" by the screen and the counter, not by the program's own state.** The
   ready frame could be drawn later than the read ends; the counter bounds that to reads that were
   still asking for bytes after the key. The default fixture's this-week read lasts about 15 to 30 ms, so
   a during-read key has room for two keys but little for more; `during_key_share_of_read_still_to_come`
   says how much of the read was left. Month reads are longer (about 55 to 85 ms) and are not used for
   during-read keys, because the open is the first read and scrolling needs a snapshot.
5. **Fixture size follows the date**: history reaches back to the start of last month at 10.7 threads a day,
   so the database is 31 to 62 days and about 250 to 500 MB, and the month reads change length with it.
   Compare arms on the same fixture, and do not compare absolute read times across days.
6. **Event-to-frame includes the product's poll phase** (default period 2 s), so it is a comparison
   between arms over matched offsets, not an absolute figure. Use `--product-arg=--interval=0.5` for
   more resolution.
7. **A real clock moves between steps and trials.** Period labels are taken at the moment of use and a trial
   that crosses a week or month boundary is refused, but the board's ages, sparklines and header clock differ
   between the two arms of a pair (`matched.initial_screen_compare` says the thread rows only were compared).
8. **No claim-grade run exists.** The virtual screen implements only the sequences 1.3.0 and the
   Insights screen emit. Terminal queries are answered like foot.
9. **The Storage-only scenario is outside the `--db` rule**; it was not run in this repair, and it still uses
   the fixture as a populated `T3CODE_HOME`.
