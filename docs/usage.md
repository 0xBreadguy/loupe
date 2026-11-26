# Using Loupe

Run `loupe` in any terminal. `loupe --demo` runs it on made-up agents,
so you can try every key without touching your own data.

## A window of its own

`loupe --window` opens the dashboard in a dedicated window (Linux only): [foot](https://codeberg.org/dnkl/foot) when it's installed,
otherwise your default terminal through `xdg-terminal-exec`, otherwise
`$TERMINAL -e`. Under Hyprland, `--window` focuses the dashboard instead when
it's already open with its app id, which foot always sets and
`xdg-terminal-exec` sets when your terminal supports it.

## Keys, marks and options

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k`, click, wheel | select an agent |
| `g` `G` / `Home` `End` | first / last agent |
| `Enter` / `o` | open the selected agent's thread in T3 Code |
| `M` | send a quick reply or contextual nudge to the selected local thread (`t3 thread send`, asks first) |
| `Space` / `←` `→` | fold or unfold subagents; `→` again lists the earlier ones too, `←` steps back (on a leaf it jumps to the parent) |
| `n` | jump to the next agent waiting on you (or click the NEEDS YOU tile) |
| `a` | scope (starts on T3's sidebar by default; `--db` keeps all for parity): T3's sidebar (not settled or snoozed) → all threads → active only → T3's sidebar |
| `f` | follow the thread open in T3 (on by default) |
| `t` | lane window: 15 min, 30 min, 1 h, 3 h |
| `J` `K` / `PgDn` `PgUp`, wheel over details | scroll the details |
| `p` / `r` | pause updates / refresh now |
| `i` | open [Insights](features.md#insights) on Recap for this week |
| `s` | Storage: disk use by project and thread |
| `e` | [Telemetry](features.md#telemetry-check): check what T3 Code sends and whether opt-outs work |
| `m` | calm mode: nothing animates (on by default when Omarchy's animations are off) |
| `x` | CRT effects, purely for fun (off in calm mode) |
| `c` / `C` | next / previous theme, saved for next time ([themes](themes.md)) |
| `w` | [trace](features.md#trace) the selected agent's task: every step on one clock; `w` or `Esc` goes back |
| `?` / `h` | help and legend (scroll it with `↑` `↓` / `j` `k` or the wheel) |
| `Esc` | close help, return from Insights, Storage, Telemetry or Trace, or quit |
| `q` | quit |

In Insights, press `Tab` to switch between Recap and Tools. The left and right
arrows step through this week, last week, this month and last month. Use the
wheel, `↑` `↓` / `j` `k`, `PgUp` `PgDn` or `Home` `End` to scroll. Press `f` to
start a T3 Code thread to audit slow/failing tools (Tools) or turn period
findings into workflow optimizations (Recap); Loupe asks first and defaults to
`"No project"`. Press `r` to read the report again, `?` for help, and `i` or
Esc to return to Live. Esc closes help first.

In Storage, use the arrows or wheel to select a row, Space to fold a project,
Enter to open an eligible thread in T3 Code, `f` to start a T3 Code thread to
inspect and clean up the selected worktrees or build caches (asks first, with
two-step confirmation), and `r` to measure again. Press `s` or Esc to return to
the dashboard. Disk measurements are available with the live dashboard on Linux. The
demo uses sample sizes, and an explicit `--db` leaves filesystem measurements
unavailable.

In Trace, press `f` to start a T3 Code thread to diagnose why the task was slow,
stalled or failed (with only the timings, statuses and listed program names
shown on screen; asks first).

In Telemetry, press `Tab` to switch between Telemetry and Sources. Use the
arrows, `j`/`k`, click or wheel to select a row and inspect its decision ladder
or notes, `Enter` or `Space` to show or hide the suggested fix commands
(descriptive by default), `f` to start a T3 Code thread with the findings and
fix instructions when telemetry isn't already off (asks first, with two-step
confirmation, defaulting to `"No project"`), `r` to check again now, `?` for
help, and `e` or Esc to return to Live (Esc closes help or expanded commands
first). Telemetry reads local files only (never secrets, agent logins or
`/proc/*/environ`), sends nothing and changes nothing. The demo uses a made-up
machine, and an explicit `--db` leaves local telemetry inspection unavailable.

Across Insights, Storage, Trace and Telemetry, pressing `f` opens the same
handoff popup before starting anything: an exact payload preview (`p` to hide
or show, `j`/`k` to scroll), a project picker (`←`/`→`, including `"No project"`
— the default for machine-wide Telemetry and Insights handoffs), and a model
picker (`↑`/`↓`). Config and file actions (Telemetry and Storage) require
two-step confirmation (`Enter` to arm, `Enter` again to start, `Esc` to disarm).

The numpad works too: its Enter opens threads, and with Num Lock off its arrows,
Home, End and Page Up/Down act like the main keys.

| Mark | Meaning |
| --- | --- |
| `⠹` | running (`◉` in calm mode) |
| `◔` | queued or starting |
| `◆` | needs you; blinks until you've seen it (selected it, or opened it in T3), also on a folded parent |
| `◇1` | one recorded ask that can stay open while the thread works |
| `~2` | two estimated asks in an agent reply since you last wrote |
| `◌` | running, but nothing new for 2 minutes; `◌ stalled` and warm after 8 |
| `△` | a tool call running over 5 minutes |
| `—` | no value, with the reason beside it |
| `▼` `▲` N more | more out of view, below or above |
| `14:02╷` `╷14:12` | on the lane axis: when the selected agent's latest run started and ended |
| `┄` N earlier | subagents folded away: finished over 3 hours ago, or before the latest 3. `→`, `Enter` or a click lists them |
| `✓` `✗` `⊘` | done, failed, interrupted |
| `●` | provider: each theme gives Claude, GPT / Codex, Grok, Gemini and the rest their own colors |
| `▎` and a band | the thread open in T3 and its subagents |
| `×` in a lane | a failed tool call |

Options:

| Option | What it does |
| --- | --- |
| `--once` | print a plain-text snapshot and exit |
| `--status` | print the one-line JSON the bar widget reads; `--watch` prints a new line whenever it changes |
| `--window` | open the dashboard in its own terminal window, or focus it (the installed command only) |
| `--db PATH` | read T3's database from `PATH` |
| `--demo` | made-up agents and usage instead of yours |
| `--interval SECONDS` | how often the dashboard rereads T3, 0.5 or more (default 2) |
| `--calm` | start in calm mode; on by default when Omarchy's animations are off |
| `--theme NAME` | a [theme](themes.md): a built-in, a file, `omarchy`, or `dark:NAME,light:NAME` |
| `--themes` | list the themes and say which one is in use |
| `--theme-check NAME` | a theme's colors, contrast and colorblind separation |
| `--recap` | [last week on one card](features.md#recap); with `--this-week`, the current week so far; with `--month` or `--this-month`, [a month](features.md#a-month); with `--json`, the numbers; `--wide`, `--compact` or `--narrow` picks [the card's size](features.md#the-wide-card) |
| `--tools` | [what agents ran last week and what failed](features.md#tools); with `--this-week`, `--month`, `--this-month` or `--json`, as `--recap`; `--wide` or `--compact` picks the report's size |
| `--trace [ID]` | [one task on a time axis](features.md#trace): the latest, a thread's latest (its id, or the end of it), or the task that holds a run; with `--json`, the spans |
| `--telemetry-check` | [check T3 Code telemetry](features.md#telemetry-check) on this machine (local files only, never opens secrets); with `--json`, `--wide`, `--compact` or `--narrow` |
| `--help` | list the options |

Recap and Tools read directly by default. Set `LOUPE_CACHE=1` to reuse
facts for finished runs on supported Linux filesystems.
`LOUPE_NO_CACHE=1` takes precedence.

By default it reads `~/.t3/userdata/statev2.sqlite`. If your T3 data lives
somewhere else, set `T3CODE_HOME` to that folder (it reads
`$T3CODE_HOME/userdata/statev2.sqlite`) or pass `--db`. When the database
isn't there, the dashboard and `--once` say which path they tried and what to
check, and `--status` puts a short version in its `error` field for the bar.
