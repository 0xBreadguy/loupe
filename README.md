```
▀█▀ ▀▀▀▄    ▄▀▀▄ ▄▀▀▀ █▀▀▀ █▄ █ ▀█▀ ▄▀▀▀
 █   ▀▀▄    █▀▀█ █ ▀█ █▀▀  █ ▀█  █   ▀▀▄
 ▀  ▀▀▀     ▀  ▀  ▀▀▀ ▀▀▀▀ ▀  ▀  ▀  ▀▀▀
```

# T3 Agents

[![test](https://github.com/0xBreadguy/t3-agents/actions/workflows/test.yml/badge.svg)](https://github.com/0xBreadguy/t3-agents/actions/workflows/test.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Rust](https://img.shields.io/badge/rust-2024%20edition-b7410e?logo=rust&logoColor=white)
![Omarchy bar widget](https://img.shields.io/badge/Omarchy-bar%20widget-5c6b73)

A live dashboard for everything your [T3 Code](https://t3.codes) agents are
doing, built to sit next to T3 and be read at a glance. Which agents are
running, which one needs you, how full each context window is, what every
subagent is up to right now, and which one has gone quiet. It comes with an
[Omarchy](https://omarchy.org) bar widget, so you see the short version without
opening anything.

> Unofficial. T3 Agents isn't made by or affiliated with the T3 Code team or
> Omarchy, and it isn't part of either. It reads T3's local database, which can
> change in any T3 update.

![The T3 Agents dashboard, running on made-up demo data](docs/dashboard.png)

It's a terminal app written in Rust: one small program, no runtime to install,
and it works anywhere a terminal does. Shape and motion carry the state:
spinners while agents work, a still `◌` when one goes quiet, one hot color for
"needs you", and a subtle hue per model provider. Nothing moves unless an agent
is working, and calm mode (`m`, or `--calm`) stills even that; it starts on by
itself when Omarchy's animations are off. It follows your Omarchy theme live.

It's also light, because it's meant to stay open all day. Sitting on the demo
it uses about 0.6% of one CPU core and 14 MB of memory. On a busy
database shaped like a real one (about 270 agents, a write every second) it
uses about 1% of a core and 50 MB. The bar widget's watcher uses less than 1%
and about 30 MB there. Each figure is the median of five alternating trials
against the original app, taken with `checks/bench/`.

## Try it

You don't need T3 data to see it. `--demo` runs the dashboard on a made-up
workspace: a few projects, subagents across Claude, GPT and Grok, one agent
waiting on you, one gone quiet, one slow test run.

```sh
git clone https://github.com/0xBreadguy/t3-agents
cd t3-agents
cargo run --release -p t3-agents-term -- --demo
```

That needs a Rust toolchain, and the first build takes about a minute.
`--help` lists every option.

## What you get

**The dashboard**

- **Big numbers up top:** active agents, agents waiting on you, tool calls per
  minute, peak context, tokens today, failed calls. Only numbers worth a look
  are big: zeros stay small and dim, and a missing value is a small `—` with the
  reason. If T3 can't be read, the tiles say so instead of showing zeros. In a
  window at least 44 rows tall and about 190 columns wide, every big number is
  drawn in a shadowed block font; smaller windows get compact half-block digits.
  One screen never mixes the two.
- **Needs you, one key away:** the tile shows what's being asked and how long
  it has waited. `n` (or a click on the tile) jumps to the next agent waiting
  on you, and `Enter` opens it in T3. Its `◆` blinks until you've seen it.
- **Limits:** a meter per subscription window from Omarchy's usage records,
  with a tick for how much of the window has passed, so you can tell when
  you're burning ahead of pace. In short windows it folds into one line with
  each provider's fullest window.
- **Every agent and subagent as a tree:** status, a context meter that warms
  past 75% and goes hot past 90%, what it's doing right now, model, how long
  its latest run took (counting up while it runs), and an activity lane on a
  shared time axis, where a failed call shows as `×`. Select an agent and the
  axis ticks the clock time its run started and ended. A new tool call sweeps
  once across the agent's name.
- **Long threads stay short:** under each agent the list keeps the subagents
  that are running, any waiting on you, and the three latest finishes from the
  last 3 hours. The rest fold into one `┄ 27 earlier ▸ · 25m–7h ago · 2 ✗`
  row, and its lane still shows their activity. Select it to see them listed in
  the details; `→`, `Enter` or a click puts them all back in the list. Top-level
  threads never fold.
- **Three ways to scope the list:** `a` steps from every unarchived thread to
  active only (running or waiting on you), then to T3's sidebar: the threads
  you haven't settled or snoozed, with their subagents. That last one keeps up
  with T3, so settling or snoozing a thread there takes it off here. Anything
  running or waiting on you stays listed in every scope, and the T3 scope also
  keeps the thread open in T3, settled or not.
- **Stall detection:** a running agent with no tool call in flight and nothing
  new for 2 minutes stops spinning and says `◌ quiet 4m · last ❯ npm test`;
  after 8 it says `◌ stalled` and turns warm. A tool call shows its running
  time, and past 5 minutes gets a `△` and turns warm. An agent waiting on its
  own subagents is delegating, not stuck.
- **Nothing hidden without a cue:** a list or panel with more than fits says
  `▼ N more`, and the list's cue sums up what's out of view (`◆1 · 1 stalled ·
  1 ✗`), turning hot when someone waiting on you is scrolled away.
- **Linked to T3, both ways:** open a thread in T3 and the dashboard selects it
  and marks it and its subagents. Press `Enter` on any agent and T3 jumps to
  that thread.
- **Details for the selected agent:** how long its latest run took and when
  (`10m 00s  14:02 → 14:12 · 31m ago`), context as a big number, its growth per
  turn, the last call's token mix, a prompt-cache meter that drains in real
  time, the plan's progress, tool mix, and recent activity. Native subagents
  also show activity when T3 records no application run for them.
- **Failures you can inspect:** recent failed calls stay at the top of the
  details, with the command or tool, age, and exit code when reported. They
  cover the selected lane window across runs, so a later run or successful
  call won't bury them. Scroll the details with `J` / `K` or the wheel;
  `Enter` opens the thread in T3 for full output. A failed call does not by
  itself mean the agent's task failed; even a search with no matches can
  return a nonzero exit code.

**The bar widget**

- A dim `⠿` when nothing's running, a spinner and a count while agents work,
  and a `◆` with a count in the bar's alert color when one needs you.
- A `⚠` at full strength when it can't see T3: no dashboard installed yet, no
  T3 database, or a watcher that stopped. Hover says which, in plain words. It
  keeps retrying on its own, and it's never hidden, so a broken monitor never
  passes for a quiet one.
- Hover for the list with models and context, click to open the dashboard,
  middle-click to restart its watcher.

![The bar widget's tooltip on stock Omarchy, with demo data](docs/bar-widget.png)

## Install

You need a Rust toolchain with `cargo` ([rustup.rs](https://rustup.rs) or your
distribution's package) and T3 Code's desktop app on the same machine. There
are no prebuilt binaries yet, so the installer compiles the program. It runs on
Linux; macOS and Windows aren't supported yet.

**On Omarchy, with the bar widget:**

```sh
omarchy plugin add https://github.com/0xBreadguy/t3-agents --enable
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh
```

The first line installs the bar widget from this repository. The second builds
the dashboard from that same copy and installs it: one program in
`~/.local/share/t3-agents/`, a `t3-agents` command in `~/.local/bin` and a
**T3 Agents** entry in the app launcher. The build happens in
`~/.cache/t3-agents`, not inside the plugin folder. It adds no autostart, no
keybindings and no window rules, and it won't overwrite a launcher it didn't
create.

Until the second step runs, the widget shows `⚠` and says the dashboard isn't
installed. It checks again every 15 seconds, so once the installer finishes
it picks the dashboard up by itself, with no shell restart.

Hide the widget while nothing runs with
`omarchy bar set 0xbreadguy.t3-agents hideWhenIdle true --json`. The `⚠` stays
visible either way.

**Dashboard only, any Linux terminal:**

```sh
git clone https://github.com/0xBreadguy/t3-agents
./t3-agents/install.sh
```

Then run `t3-agents` in any terminal, or `t3-agents --window` for a
dedicated window: [foot](https://codeberg.org/dnkl/foot) when it's installed,
otherwise your default terminal through `xdg-terminal-exec`, otherwise
`$TERMINAL -e`. Under Hyprland, `--window` focuses the dashboard instead when
it's already open with its app id, which foot always sets and
`xdg-terminal-exec` sets when your terminal supports it.

The installer checks for `cargo` before it copies anything, and it tells you if
`~/.local/bin` isn't on your `PATH`. A few options, for the cases that need
them:

| Option | What it does |
| --- | --- |
| `--name NAME` | install under another name: its own command, folder, launcher entry and window id. Two names never touch each other, so you can try a new build beside the one you use |
| `--binary PATH` | install a program you already built, without `cargo` |
| `--uninstall` | remove what an install with that name wrote, and nothing else |

**Coming from the Node.js version:** run the installer the same way you always
have. `dashboard/install.sh` still works and does the same thing. It replaces
the old program in place, leaves no `node_modules` behind, and the bar widget
moves to the new program by itself within a few seconds.

## Use

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k`, click, wheel | select an agent |
| `g` `G` / `Home` `End` | first / last agent |
| `Enter` / `o` | open the selected agent's thread in T3 Code |
| `Space` / `←` `→` | fold or unfold subagents; `→` again lists the earlier ones too, `←` steps back (on a leaf it jumps to the parent) |
| `n` | jump to the next agent waiting on you (or click the NEEDS YOU tile) |
| `a` | scope: all threads → active only → T3's sidebar (not settled or snoozed) → all |
| `f` | follow the thread open in T3 (on by default) |
| `t` | lane window: 15 min, 30 min, 1 h, 3 h |
| `J` `K` / `PgDn` `PgUp`, wheel over details | scroll the details |
| `p` / `r` | pause updates / refresh now |
| `m` | calm mode: nothing animates (on by default when Omarchy's animations are off) |
| `x` | CRT effects, purely for fun (off in calm mode) |
| `?` / `h` | help and legend (scroll it with `↑` `↓` / `j` `k` or the wheel) |
| `Esc` | close the help, or quit |
| `q` | quit |

The numpad works too: its Enter opens threads, and with Num Lock off its arrows,
Home, End and Page Up/Down act like the main keys.

| Mark | Meaning |
| --- | --- |
| `⠹` | running (`◉` in calm mode) |
| `◔` | queued or starting |
| `◆` | needs you; blinks until you've seen it (selected it, or opened it in T3), also on a folded parent |
| `◌` | running, but nothing new for 2 minutes; `◌ stalled` and warm after 8 |
| `△` | a tool call running over 5 minutes |
| `—` | no value, with the reason beside it |
| `▼` `▲` N more | more out of view, below or above |
| `14:02╷` `╷14:12` | on the lane axis: when the selected agent's latest run started and ended |
| `┄` N earlier | subagents folded away: finished over 3 hours ago, or before the latest 3. `→`, `Enter` or a click lists them |
| `✓` `✗` `⊘` | done, failed, interrupted |
| `●` | provider. On grayscale themes: plum Claude, teal GPT / Codex, blue Grok, gray other. Colorful themes use their own colors |
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
| `--help` | list the options |

By default it reads `~/.t3/userdata/statev2.sqlite`. If your T3 data lives
somewhere else, set `T3CODE_HOME` to that folder (it reads
`$T3CODE_HOME/userdata/statev2.sqlite`) or pass `--db`. When the database
isn't there, the dashboard and `--once` say which path they tried and what to
check, and `--status` puts a short version in its `error` field for the bar.

## How it works, and what it touches

- **T3 data:** it opens T3's `userdata/statev2.sqlite` read-only (`query_only`)
  and never creates, migrates or writes it. Its queries select only the fields
  it shows, so authentication tables, secrets, prompts, subagent results,
  message bodies, tool output and reasoning text never reach it. Thread titles,
  tool names, file names and command excerpts are shown locally, with terminal
  control characters stripped.
- **Staying current without rereading everything:** every two seconds it asks
  SQLite whether T3 has written anything. If it has, it reads the new entries
  in T3's event log to learn which threads changed, and rereads only those. From
  the log it takes each entry's sequence number, the id of the thread or
  project it names, and its kind and version, plus T3's own note of how far
  its tables have caught up with the log. It also counts the rows in T3's
  list of database migrations, and reads nothing else from that list:
  migrations, which run when T3 starts or updates, are the one way T3 changes
  threads without logging an event, and each one adds a row. It never reads
  what an event contains. Whenever it can't be sure that shortcut gives the
  same answer as reading everything, it reads everything.
- **If you'd rather it didn't read the event log:** start it with
  `T3_AGENTS_SYNC_READ=1` in its environment. It then rereads the tables it
  shows in full, the way the Node.js version did, and reads nothing else. That
  costs more CPU on a large database and changes nothing you see.
- **Following T3:** under Hyprland it listens on Hyprland's event socket for
  the T3 Code window's title, which T3 sets to `project / thread`. It keeps only
  T3 windows' titles and never sends Hyprland a command that changes anything.
- **Opening threads:** `Enter` runs `xdg-open` with a `t3code://threads/…` link,
  built the way T3 builds its own, using the environment id T3 keeps beside its
  database for local threads and the recorded destination for remote threads.
  That's the only process it starts, and it only asks T3 to show a thread.
- **Usage limits:** read from Omarchy's agent usage records in
  `~/.local/state/omarchy/agents/usage/`, if they exist.
- **Network:** none. The bar widget runs one `t3-agents --status --watch`
  process that checks the database's modification time every two seconds, reads
  what changed after T3 writes and again every 30 seconds (so the failed-call
  count ages out), and prints only when the status changes. If that process
  can't start or stops, the widget tries again every 15 seconds.
- **The window:** `--window` opens the dashboard with the app id
  `TUI.t3-agents`. Omarchy treats `TUI.*` windows as terminal apps (Super+C
  copies instead of sending Ctrl+C, terminal opacity) and still tiles them; you
  can match the id in your own window rules.

Some limits worth knowing. Only agents in your local T3 database show up;
delegates on linked machines show the status recorded here. Cross-machine
children nest under their actual parents using both the machine and thread
IDs, including chains that return to this machine. When a parent's state is
absent locally, its row says **UNKNOWN** and identifies it as a parent reference.
Nesting does not change settlement values or infer a remote parent's settlement.
Context figures are
whatever the provider reports, sometimes only between turns. Stall thresholds
are judgment calls: a model can think for a while, and builds take minutes, so
read `◌` as "take a look", not "broken".

## Requirements and compatibility

- Linux, and a Rust toolchain to build it (built and tested with Rust 1.99).
  The program itself needs nothing installed: SQLite is compiled in.
- T3 Code's desktop app with its v2 local database. This is an unofficial
  adapter to internal storage: a T3 update can change the tables or the window
  title format. Built against T3 Code Nightly, November 2026.
- A terminal with full color or 256 colors. It has been checked against the
  original, pixel for pixel, in foot, kitty, Alacritty, Ghostty, WezTerm and a
  VTE terminal, and inside tmux. Other terminals and real SSH sessions should
  work and haven't been tried yet.
- Optional: Omarchy for the bar widget, its usage records and the live theme;
  Hyprland to follow the open T3 thread and to focus an open dashboard; foot,
  `xdg-terminal-exec` or `$TERMINAL` for `--window`.
- Tested on one Omarchy laptop (Hyprland 0.56). Other setups are untested so
  far, and reports are welcome.

## Update and remove

Pulling or `omarchy plugin update` refreshes the source. To apply it, rerun the
installer:

```sh
omarchy plugin update 0xbreadguy.t3-agents
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh
```

The bar widget moves to the new program by itself: the installer ends the old
watcher and the widget starts a new one within a few seconds. Reopen the
dashboard to get the new version of that. If the widget itself still looks like
the old version, run `omarchy restart shell`.

To remove everything:

```sh
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh --uninstall
omarchy plugin remove 0xbreadguy.t3-agents
rm -r "${XDG_CACHE_HOME:-$HOME/.cache}/t3-agents"
```

## Develop

```sh
cargo build --release --locked
cargo test --release --locked -p t3-agents-core   # the core, with the recorded checks
node --test test/                 # bar widget
checks/scripts/ci.sh              # everything CI runs
```

The code is in two crates. `crates/core` is the whole product without a
screen: the model, the read-only T3 reader, the demo, the status line, and the
composition of the dashboard into a grid of cells. `crates/term` puts that grid
in a terminal and feeds keys and mouse back, and is the program that gets
installed. Core never depends on a presenter, so another front end can sit on
the same core.

This is a rewrite of a TypeScript app, and that original is kept in `oracle/`
as the reference. Most checks in `checks/scripts/` run both and compare them:
every cell of the screen across 234 states, the status line and command line
byte for byte, and both programs inside a real terminal under tmux. Those
checks need Node.js 26.4 or later and `npm ci` in `oracle/`. Eleven of them are
also recorded under `checks/fixtures/golden/`, which is what `cargo test`
replays.

[CONTRIBUTING.md](CONTRIBUTING.md) has the layout, the checks and the rules for
working here. [AGENTS.md](AGENTS.md) has the same for coding agents, and how to
install T3 Agents for someone.

The bar widget lives at the repository root (`manifest.json`, `Service.qml`,
`BarWidget.qml`, `Status.js`) so `omarchy plugin add` can install it straight
from git; check it with `omarchy plugin validate .`. Omarchy rejects symlinks in
a plugin folder, and `npm ci` creates some under `oracle/node_modules`, so
validate a clean clone and never install the checks' dependencies inside an
installed plugin.

## License

[MIT](LICENSE)
