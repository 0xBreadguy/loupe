# T3 Agents: notes for coding agents

T3 Agents is a terminal dashboard for T3 Code agents, and an Omarchy bar widget that shows their status. The dashboard is one Rust program, `t3-agents-term`, installed as the command `t3-agents`. It reads T3 Code's local SQLite database read-only and never writes to it.

This file covers two jobs: installing it for someone, and working on the code. [CONTRIBUTING.md](CONTRIBUTING.md) has the detail for the second, [README.md](README.md) describes the product, and [docs/](docs/) has the full install guide, the keys and options, and what the program reads.

## Installing it for someone

It runs on Linux. T3 Code's desktop app must be on the same machine, because the dashboard reads its database. For macOS there is a preview build with no installer: follow [docs/install.md](docs/install.md#macos-preview) (download, unpack, run `./t3-agents-term --demo`, and copy the program onto `PATH` only if the person wants to keep it), and say that it is a preview. Windows is not supported yet.

**With a release archive.** No toolchain is needed. The [releases page](https://github.com/0xBreadguy/t3-agents/releases/latest) has one archive for x86-64 and one for ARM, and a `SHA256SUMS` file listing both. Check the page for the newest version and use its number for `v`.

```sh
v=1.2.0 t=$(uname -m)-unknown-linux-gnu
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/t3-agents-$v-$t.tar.gz"
curl -fLO "https://github.com/0xBreadguy/t3-agents/releases/download/v$v/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf "t3-agents-$v-$t.tar.gz"
cd "t3-agents-$v-$t" && ./install.sh --binary ./t3-agents-term
```

Stop if the checksum does not match. The binary needs glibc 2.34 or later; `BUILD-INFO` in the archive says so, and `ldd --version` shows what the system has. On an older system, install from source instead.

**From source.** This needs a Rust toolchain with `cargo`. Check for one before starting, and ask before installing one.

```sh
git clone https://github.com/0xBreadguy/t3-agents
./t3-agents/install.sh
```

**On Omarchy, with the bar widget.**

```sh
omarchy plugin add https://github.com/0xBreadguy/t3-agents --enable
~/.config/omarchy/plugins/0xbreadguy.t3-agents/install.sh
```

What the installer writes, all for the current user: the program in `~/.local/share/t3-agents/`, the command `~/.local/bin/t3-agents`, and a launcher entry. It adds no autostart, keybinding or window rule, and needs no root. `--name NAME` installs under another name beside an existing install. `--uninstall` removes what an install wrote and nothing else.

Check the result without touching anyone's data:

```sh
t3-agents --status --demo        # one line of JSON
t3-agents --once --demo          # the list, printed once
```

Then `t3-agents` opens the dashboard on the real data, and `t3-agents --window` opens it in its own terminal window.

Things not to do while installing:

- Do not run the installer with `sudo`. It installs for one user.
- Do not open, copy or edit T3 Code's database. If the dashboard says it cannot read it, report what it says.
- Do not add autostart entries, keybindings or shell configuration the person did not ask for. If `~/.local/bin` is not on `PATH`, the installer says so; tell the person rather than editing their shell files unasked.
- If an older Node.js install of T3 Agents is present, run the installer the same way. It upgrades that install in place.

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
cargo run --release -p t3-agents-term -- --demo
cargo test --release --locked -p t3-agents-core
checks/scripts/ci.sh             # everything CI runs; about 25 minutes; needs Node.js 26.4+, tmux
```

The rules, in the order they matter:

1. **Never open a real T3 Code database** in a test, check, benchmark or experiment. Use `--demo` or a database the check built with `--db`, and set `T3CODE_HOME` to a new empty folder too. Nothing under `~/.t3` is yours to read.
2. **Never run `install.sh` against the real account to try it.** Use `checks/scripts/install-parity.mjs`, or `env -i HOME=/scratch/folder PATH=/usr/bin:/bin ./install.sh ...`. A stand-in `HOME` is not isolation when the session exports `XDG_DATA_HOME` or `XDG_CACHE_HOME`.
3. **Never open a window or a terminal emulator on the real desktop.** `checks/scripts/window-parity.sh` runs them in an isolated desktop.
4. **The Rust program must behave like `oracle/`.** If a check fails, the Rust program is wrong unless you can show the reference is. A deliberate difference is stated in the pull request and in the check.
5. **Read the source for a rule; do not infer it.** Terminal behavior comes from OpenTUI 0.8.3. Find the rule in its source and cite the file in your comment.
6. **The reader stays read-only and narrow.** No new table or field without a stated reason; never authentication tables, prompts, message bodies, results or reasoning.
7. **Stop only processes you started**, by process id. Do not kill by name, and do not delete temporary folders by pattern: other work may be running on the machine.
8. **Do not hand-edit generated files**: `crates/core/src/cluster_tables.rs`, `checks/fixtures/golden/`, the schema fixtures. Their generators are named in [CONTRIBUTING.md](CONTRIBUTING.md).
9. **Measure, do not estimate.** A performance claim is the median and range of at least five alternating trials on built binaries. Say what was not measured.

Before you say a change is done: build with `--locked`, run the core's tests, and run `checks/scripts/ci.sh` if you touched anything the screen, the reader or the installer depends on. Report what passed, what failed and what you did not run.
