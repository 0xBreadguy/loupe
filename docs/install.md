# Install, update and remove

T3 Agents needs T3 Code's desktop app on the same machine, because it reads T3's
local database.

| System | Status | How |
| --- | --- | --- |
| Linux, x86-64 or ARM | Supported | [release archive](#linux-from-a-release) or [source](#linux-from-source) |
| macOS 11 or later, Apple silicon or Intel | Preview | [release archive](#macos-preview), no installer yet |
| Windows | Not supported | |
| Omarchy bar widget | Omarchy only | [plugin](#the-omarchy-bar-widget) |

To look before you install, unpack a release archive and run
`./t3-agents-term --demo` from its folder. It runs on made-up agents and reads
nothing of yours.

## Linux, from a release

```sh
v=1.1.0 t=$(uname -m)-unknown-linux-gnu
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/t3-agents-$v-$t.tar.gz"
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf "t3-agents-$v-$t.tar.gz"
cd "t3-agents-$v-$t" && ./install.sh --binary ./t3-agents-term
```

There's an archive for x86-64 and one for ARM, and `uname -m` picks yours. The
program needs glibc 2.34 or later and nothing else: SQLite is compiled in.
`BUILD-INFO` in the archive says exactly what it was built from.

On Linux, every route installs the same three things, for your user only: one
program in `~/.local/share/t3-agents/`, a `t3-agents` command in `~/.local/bin`
and a **T3 Agents** entry in the app launcher. No root, no autostart, no
keybindings, no window rules, and it won't overwrite a launcher it didn't
create.

## Linux, from source

```sh
git clone https://github.com/0xBreadguy/t3-agents
./t3-agents/install.sh
```

This needs a Rust toolchain with `cargo` ([rustup.rs](https://rustup.rs) or
your distribution's package). The installer checks for it before it copies
anything, and builds in `~/.cache/t3-agents`, not inside the clone.

## The Omarchy bar widget

```sh
omarchy plugin add https://github.com/0xBreadguy/t3-agents --enable
```

The widget runs the `t3-agents` command, so it works with the dashboard
installed either way above. Until the dashboard is installed, the widget shows
its alert icon and says so. It checks again every 15 seconds, so it picks the dashboard up
by itself once the installer finishes, with no shell restart.

The plugin folder is a copy of this repository, so if you have `cargo` you can
also build and install straight from it:

```sh
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh
```

Hide the widget while nothing runs with
`omarchy bar set 0xbreadguy.t3-agents hideWhenIdle true --json`. The alert
icon stays visible either way.

## macOS (preview)

```sh
v=1.1.0 f=t3-agents-$v-universal-apple-darwin
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/$f.tar.gz"
tar -xzf "$f.tar.gz"
"$f/t3-agents-term" --demo
```

One archive runs on Apple silicon and Intel Macs, on macOS 11 or later, with
nothing else to install. Drop `--demo` to see your own agents. To keep it, copy
the program somewhere on your `PATH`:

```sh
mkdir -p ~/.local/bin && cp "$f/t3-agents-term" ~/.local/bin/t3-agents
```

It's a preview. The program builds and passes its tests on a Mac, but it hasn't
been checked window by window in Mac terminals the way the Linux build has.
There's no installer, no `--window`, no menu bar item and no usage-limit meter
on macOS yet, and it doesn't follow the thread you have open in T3. It also
isn't signed by an Apple developer account, so download it with `curl` as
above: macOS may block a copy downloaded with a browser.

## Installer options

The installer tells you if `~/.local/bin` isn't on your `PATH`. A few options,
for the cases that need them:

| Option | What it does |
| --- | --- |
| `--name NAME` | install under another name: its own command, folder, launcher entry and window id. Two names never touch each other, so you can try a new build beside the one you use |
| `--binary PATH` | install a program that's already built, from a release or your own build, without `cargo` |
| `--uninstall` | remove what an install with that name wrote, and nothing else |

## Coming from the Node.js version

Install any of the ways above, over the
top of it. `dashboard/install.sh` still works too, and builds from source like
`install.sh`. Either way it replaces the old program in place, leaves no
`node_modules` behind, and the bar widget moves to the new program by itself
within a few seconds.

## Update

The dashboard and the bar widget update separately.

**The dashboard.** Install again the way you installed. From a release, that's
the same commands with the new version number. From source or the plugin
folder, refresh the source and rerun the installer:

```sh
omarchy plugin update 0xbreadguy.t3-agents
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh
```

The bar widget moves to the new program by itself: the installer ends the old
watcher and the widget starts a new one within a few seconds. Reopen the
dashboard to get the new version of that.

**The bar widget.**

```sh
omarchy plugin update 0xbreadguy.t3-agents
omarchy restart shell
```

The restart matters. Omarchy keeps a widget's code in memory, so until the
shell restarts, the bar still runs the old widget against the new files, and
its hover text can show placeholder names where words should be.

## Remove

To remove the dashboard, run `./install.sh --uninstall` from any copy of the
installer: the unpacked release folder, a clone, or the plugin folder. To
remove everything on Omarchy:

```sh
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh --uninstall
omarchy plugin remove 0xbreadguy.t3-agents
rm -rf "${XDG_CACHE_HOME:-$HOME/.cache}/t3-agents"   # the build folder, if you built from source
```
