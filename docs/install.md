# Install, update and remove

Loupe needs T3 Code's desktop app on the same machine, because it reads T3's
local database.

| System | Status | How |
| --- | --- | --- |
| Linux, x86-64 or ARM | Supported | [release archive](#linux-from-a-release) or [source](#linux-from-source) |
| macOS 11 or later, Apple silicon or Intel | Preview | [release archive](#macos-preview), no installer yet |
| Windows | Not supported | |
| Omarchy bar widget | Omarchy only | [plugin](#the-omarchy-bar-widget) |

To look before you install, unpack a release archive and run
`./loupe-term --demo` from its folder. It runs on made-up agents and reads
nothing of yours.

## Linux, from a release

```sh
v=1.8.0 t=$(uname -m)-unknown-linux-gnu
curl -fLO "https://github.com/0xBreadguy/loupe/releases/download/v$v/loupe-$v-$t.tar.gz" &&
  curl -fLO "https://github.com/0xBreadguy/loupe/releases/download/v$v/SHA256SUMS" &&
  sha256sum -c --ignore-missing SHA256SUMS &&
  tar -xzf "loupe-$v-$t.tar.gz"
```

There's an archive for x86-64 and one for ARM, and `uname -m` picks yours. Before
installing, compare the highest glibc symbol version in `loupe-$v-$t/BUILD-INFO`
with your system's `ldd --version`. If yours is older, build from source; otherwise
install the verified binary:

```sh
cd "loupe-$v-$t" && ./install.sh --binary ./loupe-term
```

SQLite is compiled in, and `--binary` needs no toolchain or network access.

On Linux, every route installs the same three things, for your user only: one
program in `${XDG_DATA_HOME:-~/.local/share}/loupe/`, a `loupe` command in `~/.local/bin`
and a **Loupe** entry in the app launcher. No root, no autostart, no
keybindings, no window rules, and it won't overwrite a launcher it didn't
create.

## Linux, from source

```sh
git clone https://github.com/0xBreadguy/loupe
./loupe/install.sh
```

This needs a Rust toolchain with `cargo` ([rustup.rs](https://rustup.rs) or
your distribution's package) and a C compiler for bundled SQLite. The first
build fetches locked crates from crates.io and builds in
`${XDG_CACHE_HOME:-~/.cache}/loupe`, not inside the clone.

## The Omarchy bar widget

```sh
omarchy plugin add https://github.com/0xBreadguy/loupe --enable
```

The widget runs the `loupe` command, so it works with the dashboard
installed either way above. Until the dashboard is installed, the widget shows
its alert icon and says so. It checks again every 15 seconds, so it picks the dashboard up
by itself once the installer finishes, with no shell restart.

The plugin folder is a copy of this repository, so if you have `cargo` you can
also build and install straight from it:

```sh
~/.config/omarchy/plugins/0xbreadguy.loupe/install.sh
```

Hide the widget while nothing runs with
`omarchy bar set 0xbreadguy.loupe hideWhenIdle true --json`. The alert
icon stays visible either way.

## macOS (preview)

```sh
v=1.8.0 f=loupe-$v-universal-apple-darwin
curl -fLO "https://github.com/0xBreadguy/loupe/releases/download/v$v/$f.tar.gz" &&
  curl -fLO "https://github.com/0xBreadguy/loupe/releases/download/v$v/SHA256SUMS" &&
  shasum -a 256 -c --ignore-missing SHA256SUMS &&
  tar -xzf "$f.tar.gz" &&
  "$f/loupe-term" --demo
```

One archive runs on Apple silicon and Intel Macs, on macOS 11 or later, with
nothing else to install. Drop `--demo` to see your own agents. To keep it, copy
the program somewhere on your `PATH`:

```sh
mkdir -p ~/.local/bin && cp "$f/loupe-term" ~/.local/bin/loupe
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
| `--name NAME` | install under another name: its own command, program folder, launcher entry, cache folder and window id. Two names never touch each other's installed files (source builds share `${XDG_CACHE_HOME:-~/.cache}/loupe/target` and themes still read `~/.config/loupe/`), so you can try a new build beside the one you use, or avoid a command-name clash (for example with GNOME's Image Viewer at `/usr/bin/loupe` on Fedora) |
| `--title TITLE` | display name in the app launcher and `--window` title (default: `Loupe`, or `Loupe (NAME)` for another `--name`) |
| `--binary PATH` | install a program that's already built, from a release or your own build, without `cargo` |
| `--uninstall` | remove what an install with that name wrote, and nothing else |

## Coming from T3 Agents

Loupe was previously called T3 Agents. Install Loupe normally using any route
above, then remove an earlier Rust `t3-agents` install if you still have it:

```sh
./install.sh --name t3-agents --uninstall
```

If you still have the older Node.js `t3-agents` install (`--uninstall` leaves
Node.js installs untouched by design), remove its three paths directly:

```sh
rm -rf ~/.local/bin/t3-agents "${XDG_DATA_HOME:-$HOME/.local/share}/t3-agents" "${XDG_DATA_HOME:-$HOME/.local/share}/applications/t3-agents.desktop"
```

On Omarchy, swap the plugin and restart the shell:

```sh
omarchy plugin remove 0xbreadguy.t3-agents
omarchy plugin add https://github.com/0xBreadguy/loupe --enable
omarchy restart shell
```

If you had custom themes or a saved theme choice under `~/.config/t3-agents/`,
move them to `~/.config/loupe/` (and rename any Omarchy `t3-agents.toml`
override to `loupe.toml`).

## Update

The dashboard and the bar widget update separately.

**The dashboard.** Install again the way you installed. From a release, that's
the same commands with the new version number. From source or the plugin
folder, refresh the source and rerun the installer:

```sh
omarchy plugin update 0xbreadguy.loupe
~/.config/omarchy/plugins/0xbreadguy.loupe/install.sh
```

The bar widget moves to the new program by itself: the installer ends the old
watcher and the widget starts a new one within a few seconds. Reopen the
dashboard to get the new version of that.

**The bar widget.**

```sh
omarchy plugin update 0xbreadguy.loupe
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
~/.config/omarchy/plugins/0xbreadguy.loupe/install.sh --uninstall
omarchy plugin remove 0xbreadguy.loupe
rm -rf "${XDG_CACHE_HOME:-$HOME/.cache}/loupe/target"   # optional, for the default source build
```

If you enabled the Insights cache, uninstall checks the named install’s cache
folder. It removes its own idle database, journal files and ownership marker.
It leaves the lock file in place. If a report is using the cache, close the
report and run uninstall again. Uninstall also leaves an unrecognized database
or a symlinked cache folder alone.
