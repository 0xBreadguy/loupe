# Contributing to T3 Agents

T3 Agents is a terminal dashboard for T3 Code agents, with a bar widget for Omarchy. The dashboard is a Rust program. It began as a TypeScript app, which is kept in `oracle/` as the reference: the Rust program must put the same thing on the screen and answer the same commands as that app, and the checks in `checks/` compare the two.

That is the rule that shapes most contributions. A change that makes the Rust program behave differently from `oracle/` is either a bug or a decision. If it is a decision, say so in the pull request and change the reference or the check with it.

## Layout

```text
crates/core/          t3-agents-core: the model, the read-only T3 reader (full and
                      incremental), the demo, the status line, the command line, and the
                      composition of the dashboard into a grid of cells. No screen.
crates/term/          t3-agents-term: the terminal presenter, and the program that is
                      installed. It turns the grid into terminal output and keys into actions.
oracle/               The original TypeScript app. The reference. Do not edit it to make a
                      check pass.
checks/scripts/       The comparisons, and the generators for their inputs
checks/bench/         Measurement harnesses
checks/fixtures/      Synthetic inputs and recorded results, tracked
checks/generated/     Scratch output, ignored
themes/               The built-in color themes, compiled into the program
install.sh            Installs, updates and removes the program for the current user
dashboard/install.sh  Forwards to install.sh; the path older installs update from
manifest.json, Service.qml, BarWidget.qml, Status.js, ui/, i18n/, test/
                      The Omarchy bar widget
```

The core never depends on a presenter. Another front end gets its own crate under `crates/` and depends on `t3-agents-core`.

## Build and run

You need a Rust toolchain. The first build takes about a minute.

```sh
cargo build --release --locked
cargo run --release -p t3-agents-term -- --demo     # the dashboard on made-up data
target/release/t3-agents-term --help
```

If you add a dependency, build once without `--locked`, commit `Cargo.lock`, and update `THIRD-PARTY-NOTICES.md` (`checks/scripts/list-notice-crates.sh` prints the list to compare).

## Checks

```sh
checks/scripts/ci.sh
```

This is everything CI runs, in 30 steps, and takes about 25 minutes. It installs the reference app's packages, builds the workspace, generates the inputs and runs every comparison. It needs Node.js 26.4 or later with `npm`, Python 3, and `tmux`.

What it compares, each on the same input for both programs:

- every cell of the screen, with color and weight, across several hundred states and every width from 60 to 300 columns;
- behavior over time, key sequences, the status line and the command line, byte for byte;
- both programs inside tmux, and against stand-in terminals on a pty that answer the way real terminals do;
- the grapheme-cluster layout against the reference renderer's own buffer;
- the reader against the reference on synthetic databases, at several versions of T3 Code's schema;
- the installer in a sandbox.

Faster loops while you work:

```sh
cargo test --release --locked -p t3-agents-core            # the core, about two minutes
cargo test --release --locked -p t3-agents-core -p t3-agents-term \
  --test golden --test golden_status --test golden_cli     # recorded results, no Node.js
node --test test/                                           # the bar widget
```

A plain `cargo test` of the whole workspace also runs one test that reads `checks/generated/edge.json`. Run `checks/scripts/ci.sh` once, or `node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/fixtures.ts` after `npm ci` in `oracle/`, to generate it.

Two checks are not part of `ci.sh` because they need more than a build machine:

- `checks/scripts/window-parity.sh` runs both programs in a real terminal emulator inside an isolated desktop ([omabox](https://github.com/diogochaves/omabox)) and compares the windows pixel by pixel. Its header has the options.
- `checks/bench/` holds the measurement harnesses. Report a measurement as the median and range of at least five alternating trials, on built binaries that do not change during the run.

## Themes

The built-in themes are the files in `themes/`, compiled in by `crates/core/src/theme.rs`;
[docs/themes.md](docs/themes.md) describes the format. Omarchy's are its own `colors.toml`
files, copied unchanged from the commit their header names, with the dashboard's role keys
added at the end. To add one, put the file in `themes/`, add a line to `BUILTINS` in
`theme.rs`, and run the core's tests: `every_builtin_reads_and_keeps_what_means_different_things_apart`
holds every built-in to the contrast and separation floors. `t3-agents --theme-check NAME`
shows where a theme stands.

The theme options, the `c` key and the light and dark detection are the product's, not the
original's. `T3_AGENTS_NO_THEMES=1` turns them off, and the checks that compare the two
programs set it; `crates/term/tests/pty.rs` and the tests in `theme.rs` cover the rest.

## The bar widget

The widget lives at the repository root (`manifest.json`, `Service.qml`, `BarWidget.qml`, `Status.js`, and its parts in `ui/`) so `omarchy plugin add` can install it straight from git. `node --test test/` checks its logic without a desktop, and `omarchy plugin validate .` checks the folder. Omarchy rejects symlinks in a plugin folder, and `npm ci` creates some under `oracle/node_modules`, so validate a clean clone and never install the checks' dependencies inside an installed plugin.

## Documentation

`README.md` stays short: what it is, where it runs, the quickest install, and a feature list. Instructions and detail go in `docs/`: `install.md`, `usage.md`, `features.md` and `how-it-works.md`. When a release changes the version, the `v=` lines are in `README.md`, `docs/install.md` and `AGENTS.md`.

## Rules

**Synthetic data only.** No check, test or benchmark may open a real T3 Code database. Pass `--demo` or `--db` with a database the check built, and set `T3CODE_HOME` to a new empty folder as well, so a default path can never reach a real one. The scripts here already do both.

**The program reads, and reads little.** It opens T3's database read-only and takes only the fields the dashboard shows. It never reads authentication tables, prompts, message bodies, results or reasoning. A change that reads a new field needs to say why in the pull request.

**Do not run the installer against your own account to test it.** `checks/scripts/install-parity.mjs` runs it in a sandbox and fails if a real install changed while it ran. If you must run it by hand, use `env -i HOME=/some/scratch/folder PATH=/usr/bin:/bin ./install.sh ...`. A stand-in `HOME` alone is not enough: desktop sessions often export `XDG_DATA_HOME` and `XDG_CACHE_HOME` as absolute paths into the real account, and the installer honors them. It refuses that combination, but do not rely on the refusal.

**No windows on a real desktop from a check.** Anything that opens a terminal emulator goes through `window-parity.sh` and its isolated desktop.

**Match the reference from its source, not from a guess.** Where the terminal behavior comes from OpenTUI, read OpenTUI's source for the rule and cite the file in the code comment. Rules inferred from a few screens have been wrong before.

**Do not edit generated files by hand.** `crates/core/src/cluster_tables.rs` comes from `checks/scripts/generate-clusters.ts`; the recorded results under `checks/fixtures/golden/` come from the `export-*` scripts; the schema fixtures come from `checks/scripts/schema-extract.py`.

## Pull requests

- Run `checks/scripts/ci.sh` and say that it passed, or which step did not and why.
- Say what you checked beyond it, and what you could not check.
- Keep a pull request to one change.

Contributions are under the repository's license, MIT.

## Releases

`.github/workflows/linux-release.yml` is started by hand. It builds an archive for x86-64 and ARM Linux (the program, `install.sh`, the license, the third-party notices and a build record) and keeps it as the run's artifact. `checks/scripts/package-linux.sh` does the same for the machine it runs on.

`.github/workflows/macos-release.yml` is started by hand too. It runs `checks/scripts/package-macos.sh`, which builds one universal macOS program (minimum macOS 11.0, ad hoc signed) and archives it without `install.sh`; `--native-only` builds just the Mac it runs on.

A release is the tag `vVERSION` on a commit of `main`, with the archives from that commit's runs (two for Linux, one for macOS) and one `SHA256SUMS` file listing them all. The version is the one in `Cargo.toml`. The badge in `README.md` and the `v=` lines in `README.md`, `docs/install.md` and `AGENTS.md` name it too, so change them in the same commit. Publishing a release is the maintainer's step.
