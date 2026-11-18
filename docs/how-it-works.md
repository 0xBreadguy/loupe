# How it works, and what it touches

T3 Agents reads T3 Code's local database and draws what it finds. It never
writes to it, and it makes no network connections.

## What it reads and touches

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
- **Themes:** it reads the theme it's told to use, once a second so edits
  show: its own files in `~/.config/t3-agents/themes/`, and on Omarchy the live
  theme in `~/.local/state/omarchy/current/theme/` (`colors.toml`,
  `t3-agents.toml` and `theme.name` beside it) and installed theme folders. The
  one file it writes is its own `~/.config/t3-agents/config.toml`, and only
  its `theme` line, when you press `c`. With a dark and light pair away from
  Omarchy it asks the terminal for its background color and for color scheme
  reports, and switches those reports off when it quits.
- **Network:** none. The bar widget runs one `t3-agents --status --watch`
  process that checks the database's modification time every two seconds, reads
  what changed after T3 writes and again every 30 seconds (so the failed-call
  count ages out), and prints only when the status changes. If that process
  can't start or stops, the widget tries again every 15 seconds.
- **The window:** `--window` opens the dashboard with the app id
  `TUI.t3-agents`. Omarchy treats `TUI.*` windows as terminal apps (Super+C
  copies instead of sending Ctrl+C, terminal opacity) and still tiles them; you
  can match the id in your own window rules.

## Limits worth knowing

Only agents in your local T3 database show up;
delegates on linked machines show the status recorded here. Cross-machine
children nest under their actual parents using both the machine and thread
IDs, including chains that return to this machine. When a parent's state is
absent locally, its row says **UNKNOWN** and identifies it as a parent reference.
Nesting does not change settlement values or infer a remote parent's settlement.
Context figures are
whatever the provider reports, sometimes only between turns. Stall thresholds
are judgment calls: a model can think for a while, and builds take minutes, so
read `◌` as "take a look", not "broken".

## How light it is

It's meant to stay open all day. Sitting on the demo it uses about 0.6% of one
CPU core and 14 MB of memory. On a busy database shaped like a real one (about
270 agents, a write every second) it uses about 1% of a core and 50 MB. The bar
widget's watcher uses less than 1% and about 30 MB there. Each figure is the
median of five alternating trials on one Linux laptop, taken with the harnesses
in `checks/bench/`.

## Requirements and compatibility

- Linux, on x86-64 or ARM. The released program needs glibc 2.34 or later and
  nothing else installed: SQLite is compiled in. The macOS preview needs
  macOS 11 or later, on Apple silicon or Intel. Building it yourself needs a
  Rust toolchain (developed with Rust 1.99, released with 1.100).
- T3 Code's desktop app with its v2 local database. This is an unofficial
  adapter to internal storage: a T3 update can change the tables or the window
  title format. Built against T3 Code Nightly, November 2026.
- A terminal with full color or 256 colors. It has been checked against the
  original, pixel for pixel, in foot, kitty, Alacritty, Ghostty, WezTerm and a
  VTE terminal, and inside tmux. Other terminals and real SSH sessions should
  work and haven't been tried yet.
- Optional: Omarchy for the bar widget, its usage records and the live theme
  (the built-in themes work anywhere);
  Hyprland to follow the open T3 thread and to focus an open dashboard; foot,
  `xdg-terminal-exec` or `$TERMINAL` for `--window`.
- Tested on one Omarchy laptop (Hyprland 0.56). Other setups are untested so
  far, and reports are welcome.
