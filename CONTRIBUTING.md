# Contributing to Loupe

Loupe is a terminal dashboard for T3 Code agents, with a bar widget for Omarchy. The dashboard is a Rust program. It began as a TypeScript app, which is kept in `oracle/` as the reference: the Rust program must put the same thing on the screen and answer the same commands as that app, and the checks in `checks/` compare the two.

That is the rule that shapes most contributions. A change that makes the Rust program behave differently from `oracle/` is either a bug or a decision. If it is a decision, say so in the pull request and change the reference or the check with it.

## Before you start

- **A bug:** open an issue with what you ran, what you expected and what you got, and the release you installed or the commit you built. Reproduce it with `--demo` if you can. Never attach T3 Code's database or paste real thread titles; describe them instead.
- **A small fix** (a typo, a crash, a wrong number): send the pull request.
- **A feature, a new read from T3's database, or a new dependency:** open an issue first and say what it's for. A new read has to fit the privacy rules below, and some ideas don't.

## Working with an agent

Agent-written pull requests are welcome, under the same rules as any other. [AGENTS.md](AGENTS.md) is the agent's brief: the rules in order, the commands, and the definition of done. Point your agent at it before it starts.

- Say in the pull request that an agent wrote the change, and which model.
- You are the author. Read the diff before you open the pull request, and make sure the checks it claims were run on the final commit.
- Results an agent reports without the command that produced them don't count as checks. Paste the command and its outcome, or say it wasn't run.

## Layout

```text
crates/core/          loupe-core: the model, the read-only T3 reader (full and
                      incremental), the demo, the status line, the command line, and the
                      composition of the dashboard into a grid of cells. No screen.
crates/term/          loupe-term: the terminal presenter, and the program that is
                      installed. It turns the grid into terminal output and keys into actions.
oracle/               The original TypeScript app. The reference. Do not edit it to make a
                      check pass.
checks/scripts/       The comparisons, and the generators for their inputs
checks/bench/         Measurement harnesses
checks/asks-count/    The asks-count harness: the corpus against the product
checks/fixtures/      Synthetic inputs and recorded results, tracked
checks/generated/     Scratch output, ignored
themes/               The built-in color themes, compiled into the program
install.sh            Installs, updates and removes the program for the current user
loupe.svg             Desktop launcher icon installed alongside the program
dashboard/install.sh  Forwards to install.sh; the path older installs update from
manifest.json, Service.qml, BarWidget.qml, Status.js, ui/, i18n/, test/
                      The Omarchy bar widget
```

The core never depends on a presenter. Another front end gets its own crate under `crates/` and depends on `loupe-core`.

## Build and run

You need a Rust toolchain. The first build takes about a minute.

```sh
cargo build --release --locked
cargo run --release -p loupe-term -- --demo     # the dashboard on made-up data
target/release/loupe-term --help
```

If you add a dependency, build once without `--locked`, commit `Cargo.lock`, and update `THIRD-PARTY-NOTICES.md` (`checks/scripts/list-notice-crates.sh` prints the list to compare).

## Checks

```sh
checks/scripts/ci.sh
```

This is the Linux workspace and parity suite. It installs the reference app’s packages, builds the workspace, generates synthetic inputs and runs the comparisons. It also checks the report readers, the two reports sharing one cache, ask counts and privacy, and the Linux storage inventory. It needs Node.js 26.4 or later with `npm`, Python 3, and `tmux`.

What it compares, each on the same input for both programs:

- every cell of the screen, with color and weight, across several hundred states and every width from 60 to 300 columns;
- behavior over time, key sequences, the status line and the command line, byte for byte;
- both programs inside tmux, and against stand-in terminals on a pty that answer the way real terminals do;
- the grapheme-cluster layout against the reference renderer's own buffer;
- the reader against the reference on synthetic databases, at several versions of T3 Code's schema;
- Recap command-line behavior on the built program;
- the installer in a sandbox.

Faster loops while you work:

```sh
cargo test --release --locked -p loupe-core            # the core, about two minutes
cargo test --release --locked -p loupe-core -p loupe-term \
  --test golden --test golden_status --test golden_cli     # recorded results, no Node.js
node --test test/                                           # the bar widget
```

A plain `cargo test` of the whole workspace also runs one test that reads `checks/generated/edge.json`. Run `checks/scripts/ci.sh` once, or `node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/fixtures.ts` after `npm ci` in `oracle/`, to generate it.

Two checks are not part of `ci.sh` because they need more than a build machine:

- `checks/scripts/window-parity.sh` runs both programs in a real terminal emulator inside an isolated desktop ([omabox](https://github.com/diogochaves/omabox)) and compares the windows pixel by pixel. Its header has the options.
- `checks/bench/` holds the measurement harnesses. Report a measurement as the median and range of at least five alternating trials, on built binaries that do not change during the run.

The scale generator's legacy `--tier real` name selects a synthetic stress workload.
It combines a few large histories with many small ones, large item payloads, recent
and older activity, remote delegation, and a pruned event log. Regenerate cached
bases before measuring this workload, and rebuild the dense copy with
`checks/bench/full-read.py build`. Pass `real` to `checks/scripts/status-live-parity.ts`
to check that tier's watcher and cross-machine writes.

The asks count has its own harness. `checks/scripts/asks-count.sh` runs
`loupe_core::asks::count_asks` over the made-up messages in
`checks/fixtures/asks-counts.json`, whose expected numbers came from the
`asks/questions.py` prototype. `python3 checks/scripts/asks-corpus.py --check` holds
the corpus together with no prototype and no Node; naming the prototype with
`--reference PATH` re-runs it, or compares it with the harness's `--json` output. CI checks the corpus and runs the synthetic privacy gate.

## Themes

The built-in themes are the files in `themes/`, compiled in by `crates/core/src/theme.rs`;
[docs/themes.md](docs/themes.md) describes the format. Omarchy's are its own `colors.toml`
files, copied unchanged from the commit their header names, with the dashboard's role keys
added at the end. To add one, put the file in `themes/`, add a line to `BUILTINS` in
`theme.rs`, and run the core's tests: `every_builtin_reads_and_keeps_what_means_different_things_apart`
holds every built-in to the contrast and separation floors. `loupe --theme-check NAME`
shows where a theme stands.

The theme options, the `c` key and the light and dark detection are the product's, not the
original's. `LOUPE_NO_THEMES=1` turns them off, and the checks that compare the two
programs set it; `crates/term/tests/pty.rs` and the tests in `theme.rs` cover the rest.

## The bar widget

The widget lives at the repository root (`manifest.json`, `Service.qml`, `BarWidget.qml`, `Status.js`, and its parts in `ui/`) so `omarchy plugin add` can install it straight from git. `node --test test/` checks its logic without a desktop, and `omarchy plugin validate .` checks the folder. Omarchy rejects symlinks in a plugin folder, and `npm ci` creates some under `oracle/node_modules`, so validate a clean clone and never install the checks' dependencies inside an installed plugin.

## Documentation

`README.md` stays short: what it is, where it runs, the quickest install, and a feature list. Instructions and detail go in `docs/`: `install.md`, `usage.md`, `features.md`, `how-it-works.md`, and `agents.md` for programs that read Loupe's output. A change to a command's output, its fields or its exit codes goes in `agents.md` too. When a release changes the version, the `v=` lines are in `README.md`, `docs/install.md` and `AGENTS.md`.

## Rules

**Synthetic data only.** No check, test or benchmark may open a real T3 Code database. Pass `--demo` or `--db` with a database the check built, and set `T3CODE_HOME` to a new empty folder as well, so a default path can never reach a real one. The scripts here already do both.

**The T3 reader is read-only and field-scoped.** It opens the database with `query_only`. Dashboard queries project the fields used for status, activity and thread navigation, including existing labels, file names and todo steps. For command activity on a row or in its details, the dashboard reads at most the first 240 characters of the input and displays the first line with terminal controls stripped. The status JSON’s `now` field carries only the activity category. The live reader also uses routing keys, event headers, projection positions and a migration row count to decide what to refresh. Keep authentication tables, prompts, subagent results, tool output, message bodies and reasoning text out of query results.

- Open asks uses request state and message provenance and timing. Only `t3_ask_count` may inspect eligible assistant message text, and it returns a count.
- Recap projects only inputs to its aggregates, including times, categories, numeric counts and identifiers needed for grouping and deduplication. Trace reads the metadata needed to draw one task, including titles, tool names and timing. Tools reads call metadata and integer command exit codes. Trace and Tools pass command input through the SQLite program classifier and return only allowlisted program names. The report skims may receive item records inside SQLite and return only the fields their report uses.
- The opt-in report cache keeps derived run facts, hashed keys and structural validation data in its own file. Source validation uses schema, run state, file and WAL metadata, and item membership and page locations. Keep raw item content out of the cache. Keep filesystem inventory to directory entries and metadata.
- Telemetry Check (`e` and `--telemetry-check`) reads only local configuration files (`server-runtime.json`, `settings.json`, `service.env`, OS `policy.json`, `/etc/hosts`, `/etc/nsswitch.conf`), allowlisted telemetry env vars (with URL userinfo/path/query stripped), `symlink_metadata()` on `anonymous-id`, and `/proc/<pid>/stat` + `kill(pid, 0)` for liveness/ancestry. Never open `userdata/secrets/`, `~/.codex/auth.json`, `~/.claude.json`, `/proc/*/environ`, `/proc/*/cmdline`, `OTEL_EXPORTER_OTLP_HEADERS`, or `statev2.sqlite`, and never make network calls or writes.

Document the purpose of any new source field in the pull request. Check the executed SQL and its query plan on the supported schema paths when changing item reads.

**Do not run the installer against your own account to test it.** `checks/scripts/install-parity.mjs` runs it in a sandbox and fails if a real install changed while it ran. If you must run it by hand, use `env -i HOME=/some/scratch/folder PATH=/usr/bin:/bin ./install.sh ...`. A stand-in `HOME` alone is not enough: desktop sessions often export `XDG_DATA_HOME` and `XDG_CACHE_HOME` as absolute paths into the real account, and the installer honors them. It refuses that combination, but do not rely on the refusal.

**No windows on a real desktop from a check.** Anything that opens a terminal emulator goes through `window-parity.sh` and its isolated desktop.

**Match the reference from its source, not from a guess.** Where the terminal behavior comes from OpenTUI, read OpenTUI's source for the rule and cite the file in the code comment. Rules inferred from a few screens have been wrong before.

**Do not edit generated files by hand.** `crates/core/src/cluster_tables.rs` comes from `checks/scripts/generate-clusters.ts`; the recorded results under `checks/fixtures/golden/` come from the `export-*` scripts; `checks/fixtures/asks-counts.json` comes from `checks/scripts/asks-corpus.py --write`; the schema fixtures come from `checks/scripts/schema-extract.py`.

## What to run for what you changed

`checks/scripts/ci.sh` runs most of what's below. It doesn't run the bar widget's tests or its plugin check, `asks-corpus.py --check`, the notices comparison, the window comparison or the benchmarks, so run those yourself when the row names them. While you work, the narrower commands are enough. Before the pull request, run `ci.sh` unless the change is documentation only.

| You changed | Run at least |
| --- | --- |
| Documentation only (`*.md`, `docs/`) | `git diff --check`, and follow every link you added. A version number changes in all three `v=` places together. |
| The screen, keys or layout (`crates/core/src/ui.rs`, `*_view.rs`, `terminal.rs`, `crates/term/`) | The core's tests and the recorded fixtures (see [Checks](#checks)), then `ci.sh` for the grid, behavior and terminal comparisons. |
| The T3 reader (`crates/core/src/source*`, `live*`, and the readers in `recap.rs`, `tools.rs`, `trace.rs`) | `ci.sh`, which compares readers on synthetic databases at every supported schema. List each new field in the pull request. |
| The status line (`crates/core/src/status.rs`) | `ci.sh` (status parity and the status watcher), and `node --test test/`, since the widget reads it. |
| The ask count (`crates/core/src/asks.rs`) | `checks/scripts/asks-count.sh`, `checks/scripts/asks-privacy.sh` and `python3 checks/scripts/asks-corpus.py --check`. |
| Storage (`crates/core/src/storage*`) | The core's tests and `checks/scripts/storage-walker.sh`, on Linux. |
| Telemetry Check (`crates/core/src/telemetry*`) | The core's tests and `loupe --telemetry-check --demo`. Nothing in it may open a file outside the list under [Rules](#rules). |
| A theme (`themes/`) | The core's tests and `loupe --theme-check NAME`. |
| The installer (`install.sh`, `dashboard/install.sh`, `loupe.svg`) | `node --experimental-ffi checks/scripts/install-parity.mjs` after `npm ci` in `oracle/`, or `ci.sh`. Never the real account. |
| The bar widget (`manifest.json`, `*.qml`, `Status.js`, `ui/`, `i18n/`) | `node --test test/`, and `omarchy plugin validate .` on a clean clone. |
| A dependency (`Cargo.toml`, `Cargo.lock`) | `ci.sh`, and compare `THIRD-PARTY-NOTICES.md` with `checks/scripts/list-notice-crates.sh`. |
| A generated file | Its generator, named under [Rules](#rules), never a hand edit. |

## Pull requests

- Keep a pull request to one change.
- Run `checks/scripts/ci.sh` on the final commit and say that it passed, or which step didn't and why.
- Say what you checked beyond it, and what you couldn't check.
- State any deliberate difference from `oracle/`, and the check that now covers it.
- List each new field read from T3's database, and what it's for.
- For a performance claim, give the median and range of at least five alternating trials on built binaries, and say what wasn't measured.

The pull request template asks for these.

## How a pull request is evaluated

A reviewer, a person or an agent, works through these in order and stops at the first that fails. Each one says what counts as evidence.

1. **Privacy.** The change opens no real T3 database in a test, check or benchmark, and adds no read of the data the reader must never return: authentication tables, prompts, message text, tool output, subagent results or reasoning text. Evidence: the diff, and the fields the pull request lists.
2. **Read-only.** The reader still opens T3's database with `query_only`, and nothing writes to it or to the person's files outside what the installer documents. Evidence: the diff.
3. **Matches the reference.** `ci.sh` passed on the final commit, or the pull request names the step that didn't and why that's right. A difference from `oracle/` is stated and covered by a check. Evidence: the CI run on that commit, or the pasted output.
4. **Safe to run.** The change opens no window on a real desktop, runs no installer against a real account, and stops no process it didn't start. Evidence: the diff of any script or test.
5. **Rules from source.** A terminal rule cites the OpenTUI file it comes from. Evidence: a comment with the path.
6. **Generated files regenerated.** No hand edits to the files under [Rules](#rules). Evidence: the generator's output matches the committed file.
7. **Claims backed.** Every "passed", "faster" or "fixed" in the description has a command or a measurement behind it. Evidence: the description.
8. **Documented.** A change people can see is in `docs/`. A version change touches every place that names the version. Evidence: the diff.
9. **One change.** Evidence: the diff tells one story.

The first eight block a merge. The last is a request to split the pull request. Taste, naming and structure come after all nine, as suggestions.

Contributions are under the repository's license, MIT.

## Releases

`.github/workflows/linux-release.yml` is started by hand. It builds an archive for x86-64 and ARM Linux (the program, `install.sh`, `loupe.svg`, the license, the third-party notices and a build record) and keeps it as the run's artifact. `checks/scripts/package-linux.sh` does the same for the machine it runs on.

`.github/workflows/macos-release.yml` is started by hand too. It runs `checks/scripts/package-macos.sh`, which builds one universal macOS program (minimum macOS 11.0, ad hoc signed) and archives it without `install.sh`; `--native-only` builds just the Mac it runs on.

A release is the tag `vVERSION` on a commit of `main`, with the archives from that commit's runs (two for Linux, one for macOS) and one `SHA256SUMS` file listing them all. The version is the one in `Cargo.toml`. The badge in `README.md` and the `v=` lines in `README.md`, `docs/install.md` and `AGENTS.md` name it too, so change them in the same commit. Publishing a release is the maintainer's step.
