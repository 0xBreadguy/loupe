# Using T3 Agents

Run `t3-agents` in any terminal. `t3-agents --demo` runs it on made-up agents,
so you can try every key without touching your own data.

## A window of its own

`t3-agents --window` opens the dashboard in a dedicated window (Linux only): [foot](https://codeberg.org/dnkl/foot) when it's installed,
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
| `Space` / `←` `→` | fold or unfold subagents; `→` again lists the earlier ones too, `←` steps back (on a leaf it jumps to the parent) |
| `n` | jump to the next agent waiting on you (or click the NEEDS YOU tile) |
| `a` | scope: all threads → active only → T3's sidebar (not settled or snoozed) → all |
| `f` | follow the thread open in T3 (on by default) |
| `t` | lane window: 15 min, 30 min, 1 h, 3 h |
| `J` `K` / `PgDn` `PgUp`, wheel over details | scroll the details |
| `p` / `r` | pause updates / refresh now |
| `m` | calm mode: nothing animates (on by default when Omarchy's animations are off) |
| `x` | CRT effects, purely for fun (off in calm mode) |
| `c` / `C` | next / previous theme, saved for next time ([themes](themes.md)) |
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
| `●` | provider: each theme gives Claude, GPT / Codex, Grok and the rest their own colors |
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
| `--help` | list the options |

By default it reads `~/.t3/userdata/statev2.sqlite`. If your T3 data lives
somewhere else, set `T3CODE_HOME` to that folder (it reads
`$T3CODE_HOME/userdata/statev2.sqlite`) or pass `--db`. When the database
isn't there, the dashboard and `--once` say which path they tried and what to
check, and `--status` puts a short version in its `error` field for the bar.
