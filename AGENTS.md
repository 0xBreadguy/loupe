# Loupe: notes for coding agents

Loupe is a terminal dashboard for T3 Code agents, and an Omarchy bar widget that shows their status. The dashboard is one Rust program, `loupe-term`, installed as the command `loupe`. It reads T3 Code's local SQLite database read-only and never writes to it.

This file covers two jobs: installing it for someone, and working on the code. [CONTRIBUTING.md](CONTRIBUTING.md) has the detail for the second, [README.md](README.md) describes the product, and [docs/](docs/) has the full install guide, the keys and options, and what the program reads.

## Installing it for someone

It runs on Linux. T3 Code's desktop app must be on the same machine, because the dashboard reads its database. For macOS there is a preview build with no installer: follow [docs/install.md](docs/install.md#macos-preview) (download, unpack, run `./loupe-term --demo`, and copy the program onto `PATH` only if the person wants to keep it), and say that it is a preview. Windows is not supported yet.

**With a release archive.** No toolchain is needed. The [releases page](https://github.com/0xBreadguy/loupe/releases/latest) has one archive for x86-64 and one for ARM, and a `SHA256SUMS` file listing all release archives. Check the page for the newest version and use its number for `v`.

```sh
v=1.6.0 t=$(uname -m)-unknown-linux-gnu
curl -fLO "https://github.com/0xBreadguy/loupe/releases/download/v$v/loupe-$v-$t.tar.gz" &&
  curl -fLO "https://github.com/0xBreadguy/loupe/releases/download/v$v/SHA256SUMS" &&
  sha256sum -c --ignore-missing SHA256SUMS &&
  tar -xzf "loupe-$v-$t.tar.gz"
```

Stop if the checksum does not match. Compare the highest glibc symbol version in `loupe-$v-$t/BUILD-INFO` with the installed version shown by `ldd --version`. If the installed glibc is older, build from source; otherwise run `cd "loupe-$v-$t" && ./install.sh --binary ./loupe-term`.

**From source.** This needs a Rust toolchain with `cargo`. Check for one before starting, and ask before installing one.

```sh
git clone https://github.com/0xBreadguy/loupe
./loupe/install.sh
```

**On Omarchy, with the bar widget.**

```sh
omarchy plugin add https://github.com/0xBreadguy/loupe --enable
~/.config/omarchy/plugins/0xbreadguy.loupe/install.sh
```

What the installer writes, all for the current user: the program in `~/.local/share/loupe/`, the command `~/.local/bin/loupe`, and a launcher entry. It adds no autostart, keybinding or window rule, and needs no root. `--name NAME` installs under another name beside an existing install. `--uninstall` removes what an install wrote and nothing else.

Check the result without touching anyone's data:

```sh
loupe --status --demo        # one line of JSON
loupe --once --demo          # the list, printed once
```

Then `loupe` opens the dashboard on the real data, and `loupe --window` opens it in its own terminal window.

Things not to do while installing:

- Do not run the installer with `sudo`. It installs for one user.
- Do not open, copy or edit T3 Code's database. If the dashboard says it cannot read it, report what it says.
- Do not add autostart entries, keybindings or shell configuration the person did not ask for. If `~/.local/bin` is not on `PATH`, the installer says so; tell the person rather than editing their shell files unasked.
- If an older Node.js install of Loupe is present, run the installer the same way. It upgrades that install in place.

## Working on the code

```text
crates/core/    the product without a screen: model, read-only T3 reader, demo, status line,
                command line, and the dashboard composed into a grid of cells
crates/term/    the terminal presenter; this is the installed program
themes/         the built-in color themes, compiled in (crates/core/src/theme.rs reads them)
oracle/         the original TypeScript app, kept as the reference. Do not edit it to make
                a check pass
checks/         the comparisons against the reference, benchmarks and fixtures
install.sh      the installer
```

```sh
cargo build --release --locked
cargo run --release -p loupe-term -- --demo
cargo test --release --locked -p loupe-core
checks/scripts/ci.sh             # everything CI runs; about 25 minutes; needs Node.js 26.4+, tmux
```

The rules, in the order they matter:

1. **Never open a real T3 Code database** in a test, check, benchmark or experiment. Use `--demo` or a database the check built with `--db`, and set `T3CODE_HOME` to a new empty folder too. Nothing under `~/.t3` is yours to read.
2. **Never run `install.sh` against the real account to try it.** Use `checks/scripts/install-parity.mjs`, or `env -i HOME=/scratch/folder PATH=/usr/bin:/bin ./install.sh ...`. A stand-in `HOME` is not isolation when the session exports `XDG_DATA_HOME` or `XDG_CACHE_HOME`.
3. **Never open a window or a terminal emulator on the real desktop.** `checks/scripts/window-parity.sh` runs them in an isolated desktop.
4. **The Rust program must behave like `oracle/`.** If a check fails, the Rust program is wrong unless you can show the reference is. A deliberate difference is stated in the pull request and in the check.
5. **Read the source for a rule; do not infer it.** Terminal behavior comes from OpenTUI 0.8.3. Find the rule in its source and cite the file in your comment.
6. **Keep the reader read-only.** Follow the field list in [CONTRIBUTING.md](CONTRIBUTING.md) and explain each new read in the pull request. Never read authentication tables or return prompts, message text, tool output, subagent results or reasoning text. Only the ask-count function may inspect eligible assistant prose, and it returns a number. The live dashboard may display up to 240 characters of command input locally in row activity and details, with terminal controls stripped. For tool calls, Trace and Tools may return tool names and allowlisted program names, and Tools may return integer command exit codes. Recap uses counts and timings for commands. Storage measures directory entries and file metadata. Telemetry Check reads local configuration files and allowlisted env vars only, never opens secrets, auth files or `/proc/*/environ`, and never sends data or writes files.
7. **Stop only processes you started**, by process id. Do not kill by name, and do not delete temporary folders by pattern: other work may be running on the machine.
8. **Do not hand-edit generated files**: `crates/core/src/cluster_tables.rs`, `checks/fixtures/golden/`, the schema fixtures. Their generators are named in [CONTRIBUTING.md](CONTRIBUTING.md).
9. **Measure, do not estimate.** A performance claim is the median and range of at least five alternating trials on built binaries. Say what was not measured.

Before you say a change is done: build with `--locked`, run the core's tests, and run `checks/scripts/ci.sh` if you touched anything the screen, the reader or the installer depends on. Report what passed, what failed and what you did not run.
