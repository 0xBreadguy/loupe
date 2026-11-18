```
▀█▀ ▀▀▀▄    ▄▀▀▄ ▄▀▀▀ █▀▀▀ █▄ █ ▀█▀ ▄▀▀▀
 █   ▀▀▄    █▀▀█ █ ▀█ █▀▀  █ ▀█  █   ▀▀▄
 ▀  ▀▀▀     ▀  ▀  ▀▀▀ ▀▀▀▀ ▀  ▀  ▀  ▀▀▀
```

# T3 Agents

[![test](https://github.com/0xBreadguy/t3-agents/actions/workflows/test.yml/badge.svg)](https://github.com/0xBreadguy/t3-agents/actions/workflows/test.yml)
[![Release 1.1.0](https://img.shields.io/badge/release-1.1.0-2ea44f)](https://github.com/0xBreadguy/t3-agents/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Rust](https://img.shields.io/badge/rust-2024%20edition-b7410e?logo=rust&logoColor=white)
![Omarchy bar widget](https://img.shields.io/badge/Omarchy-bar%20widget-5c6b73)

A live terminal dashboard for your [T3 Code](https://t3.codes) agents. It shows
who's running, who needs you, how full each context window is, and who's gone
quiet. On [Omarchy](https://omarchy.org) it also puts the short version in your
bar.

> Unofficial. Not made by or affiliated with the T3 Code team or Omarchy. It
> reads T3's local database, which can change in any T3 update.

![The T3 Agents dashboard, running on made-up demo data](docs/dashboard.png)

## Where it runs

| | |
| --- | --- |
| **Linux**, x86-64 and ARM | Supported. glibc 2.34 or later: Ubuntu 22.04, Debian 12, Fedora 35, or newer |
| **macOS** 11 or later, Apple silicon and Intel | Preview. It runs, but there's no installer or menu bar item yet |
| **Windows** | Not supported |
| **Bar widget** | Omarchy only |
| **Terminals** | Anything with 256 colors or better. Checked in foot, kitty, Alacritty, Ghostty, WezTerm, VTE terminals and tmux |
| **T3 Code** | The desktop app, on the same machine |

## Install

Linux:

```sh
v=1.1.0 t=$(uname -m)-unknown-linux-gnu
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/t3-agents-$v-$t.tar.gz"
tar -xzf "t3-agents-$v-$t.tar.gz"
cd "t3-agents-$v-$t" && ./install.sh --binary ./t3-agents-term
```

Then run `t3-agents`, or `t3-agents --demo` to look around on made-up agents
first.

macOS:

```sh
v=1.1.0 f=t3-agents-$v-universal-apple-darwin
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/$f.tar.gz"
tar -xzf "$f.tar.gz"
"$f/t3-agents-term" --demo
```

Drop `--demo` to see your own agents.

The Omarchy bar widget:

```sh
omarchy plugin add https://github.com/0xBreadguy/t3-agents --enable
```

Building from source, keeping the Mac build on your `PATH`, updating and
removing are all in [docs/install.md](docs/install.md).

## What you get

- **Every agent and subagent in one tree:** status, context, model, run time,
  and what it's doing right now.
- **Needs-you, one key away.** One hot color, `n` jumps to the next agent
  waiting on you, and `Enter` opens it in T3.
- **Big numbers up top:** active, waiting, tool calls a minute, peak context,
  tokens today, failed calls.
- **Stall detection.** Quiet after 2 minutes, stalled after 8, slow tool calls
  flagged.
- **Failed calls you can inspect,** with the command and the exit code.
- **Long threads stay short.** Old subagents fold into one row.
- **Three scopes:** everything, active only, or what's in T3's sidebar.
- **Follows T3.** On Hyprland it selects the thread you have open in T3.
- **Usage-limit meters** from Omarchy's usage records.
- **Every Omarchy theme:** all 23 built in, plus any you've installed, and it
  follows your live theme when you switch.
- **27 themes on any system,** with Dracula and three of its own. It follows
  your terminal between light and dark, `c` steps through them, and your own
  theme is one small file.
- **Calm mode,** for when you want nothing moving.
- **A bar widget** on Omarchy: a count in the bar, and a card of who's doing
  what when you click it.

![The bar widget's card on stock Omarchy, with demo data](docs/bar-widget.png)

It's one small Rust program. It only reads T3's data, never writes it, and
makes no network connections. On Linux it sits at about 0.6% of a CPU core and 14 MB on the
demo.

## Docs

- [Install, update and remove](docs/install.md)
- [Keys, marks and options](docs/usage.md)
- [Themes, and making your own](docs/themes.md)
- [Features, in detail](docs/features.md)
- [How it works, and what it touches](docs/how-it-works.md)
- [Contributing](CONTRIBUTING.md), and [notes for coding agents](AGENTS.md)

## License

[MIT](LICENSE)
