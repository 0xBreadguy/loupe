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

One set of keys works on every screen: the dashboard, Insights, Storage,
Telemetry and Trace.

| Key | On every screen |
| --- | --- |
| `↑` `↓` / `j` `k`, click, wheel | move the selection (or scroll, where there's nothing to select) |
| `g` `G` / `Home` `End` | first / last |
| `J` `K` / `PgDn` `PgUp` | a page (on the dashboard: scroll the details) |
| `Tab` / `Shift-Tab` | next / previous view in this screen: Storage's folders ↔ cleanup settings, Recap ↔ Tools, Telemetry ↔ Sources |
| `s` `i` `e` `w` | go to [Storage](features.md), [Insights](features.md#insights), [Telemetry](features.md#telemetry-check) or [Trace](features.md#trace) from any screen; a screen's own key goes back to the dashboard |
| `Enter` / `o` | open the selected thread in T3 Code |
| `f` | hand this to a T3 Code agent: Loupe shows what it will send and asks first |
| `r` | read it again: refresh, rescan, re-check, re-read |
| `c` `C` / `m` | next / previous theme ([themes](themes.md)) / calm mode: nothing animates |
| `?` / `h` | help; on Insights, Storage, Telemetry and Trace every key but the theme and calm keys goes to it while it's open |
| `Esc` / `q` | back one level / quit |

One exception: on the dashboard `f` means **follow**, and keeps the thread
open in T3 selected. Everywhere else it hands work to a T3 agent.

The dashboard also has these:

| Key | Action |
| --- | --- |
| `n` | jump to the next agent waiting on you (or click the NEEDS YOU tile) |
| `Space` / `←` `→` | fold or unfold subagents; `→` again lists the earlier ones too, `←` steps back (on a leaf it jumps to the parent) |
| `a` | scope: all threads → active only → T3's sidebar (not settled or snoozed) → all |
| `t` | lane window: 15 min, 30 min, 1 h, 3 h |
| `p` | pause updates |
| `x` | CRT effects, purely for fun (off in calm mode) |
| `M` | send a quick reply or a nudge to the selected local thread (`t3 thread send`, asks first) |

On the dashboard `Esc` closes help, then quits; with help open, the arrows
and the wheel scroll it. Calm mode is on by default when Omarchy's animations
are off. The numpad works too: its Enter opens threads, and with Num Lock off
its arrows, Home, End and Page Up/Down act like the main keys.

**Insights** opens on Recap for this week. `←` `→` step through this week,
last week, this month and last month, and a click on a period or on RECAP /
TOOLS picks it. `f` starts a T3 Code thread to audit slow or failing tools
(Tools) or turn the week's findings into workflow changes (Recap). `r` reads
the report again and keeps the old one on screen until it's done.

**Storage** lists disk use by project and thread. `Space` folds a project
(so does `Enter` on its row), `f` has a T3 Code thread inspect and clean up
the selected worktrees or build caches, and `r` measures again. The T3
CLEANUP band above the list names T3's auto-delete settings that are off and
what they would cover. When T3's settings could be read, `Tab` switches to
the cleanup settings panel: select a setting to have it explained, `f` starts
a T3 thread to set them up with you, `r` reads them again, and `Tab` or `Esc`
goes back to the folders. Change settings in T3 Settings › Worktrees; Loupe
only reads them. Disk measurements need the live dashboard on Linux. The demo
uses sample sizes, and an explicit `--db` leaves them unavailable.

**Trace** shows the selected agent's task on one clock. `←` `→` step to the
thread's previous or next task, `Enter` on a delegate opens its own trace
(on anything else it opens the thread in T3), `f` starts a T3 Code thread to
diagnose why the task was slow, stalled or failed (with only the timings,
statuses and program names on screen), `r` reads the trace again and `p`
pauses updates. `Esc` goes back one trace, then to the dashboard; `w` goes
straight back.

**Telemetry** checks what T3 Code sends and whether opt-outs work. On the
Telemetry view the selection picks a stream and shows its decision ladder or
notes; on Sources the arrows and wheel scroll, and a click on TELEMETRY or
SOURCES in the header switches views. `f` starts a T3 Code thread with the
findings and fix instructions when telemetry isn't already off, and `r`
checks again (it also does when a file changes). It reads local files only
(never secrets, agent logins or `/proc/*/environ`), sends nothing and changes
nothing. The demo uses a made-up machine, and an explicit `--db` leaves it
unavailable.

### The confirmation bar

`f` (and `M`) never start anything straight away. A bar at the bottom says
what will happen, with a preview of exactly what goes to T3: the title,
project, model, base folder and prompt.

| Key | In the confirmation bar |
| --- | --- |
| `Enter` | start the T3 thread; for changes to files or settings, the first `Enter` arms and the second starts |
| `←` `→` / `Tab` | pick the project |
| `↑` `↓` | pick the model (`j` `k` too while the preview is hidden) |
| `p` | show or hide the preview |
| `j` `k` / `PgDn` `PgUp` | scroll the preview |
| `Esc` | back: disarm, then cancel |
| `q` | quit |

The preview needs a terminal of at least 28 × 7, and `Enter` waits until it
fits. Insights and Telemetry draw the bar from 52 × 14; below that `f` does
nothing, and shrinking the window cancels a bar it can no longer show. After a
start, `Esc` dismisses the result. While a thread is being started, `Esc` on
the dashboard waits instead of quitting.

### The reply prompt

`M` on the dashboard offers replies that fit the selected thread.

| Key | In the reply prompt |
| --- | --- |
| `←` `→` | pick a reply |
| `Enter` | send it (on "custom reply…", start typing) |
| `Tab` | type your own |
| `Esc` | cancel |

While typing, `Enter` sends, `Backspace` deletes, `Ctrl-U` clears and `Esc`
goes back to the replies. Pasting works; line breaks become spaces, and a
reply holds up to 400 characters.

### Marks and options

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
