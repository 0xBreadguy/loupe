# Golden fixtures

Expected output saved from the TypeScript original at main `2f1d553`, so the Rust tests keep working after the original is retired.

Every fixture is one gzip-compressed JSON object written by an exporter under `checks/scripts/`. Two of them, `text.json.gz` and `terminal.json.gz`, record their gate differently from the rest (below), but have the same envelope:

- `dashboard`: the frozen oracle's revision, recorded by the exporter: the last commit that touched `oracle/`, the `oracle/` tree hash, a SHA-256 over `oracle/src`, and whether `oracle/` was dirty.
- `generator`: the parity gate that produced the expected values.
- `note`: what the file holds, in one sentence.
- `cases`: one entry per recorded case, each holding the oracle's expected output.
- `assets`: inputs the oracle built with its own code, so the Rust test can rebuild them without Node. SQLite databases are SQL text (rebuildable row for row, row ids included); everything else is kept verbatim.

Two runs over the same tree produce identical bytes (`gzip`, level 9, mtime 0), and every exporter runs its gate twice and refuses to install a fixture whose two runs differ.

`states.jsonl.gz`, `hidden.jsonl.gz`, `labels.jsonl.gz` and their snapshot files are the older grid export from `export-golden.ts`, described at the end.

## Files

| File | What it holds |
| --- | --- |
| `demo.json.gz` | 18 demo clock phases: the original's whole snapshot at each offset, normalized as the live gate compares it (null object members absent, root path ignored). |
| `status.json.gz` | 347 cases of the bar's status line: 342 fixture snapshots (including a 300-case seeded fuzz), four database error lines whose path is `{path}`, and the demo watcher's 1,206-second timer replay. |
| `cli.json.gz` | 75 cases of the product command line: exit code, stdout and stderr bytes as hex, plus the synthetic databases and T3 homes the cases read as SQL text and file trees. |
| `behavior.json.gz` | 104 timed frames: eight event scripts replayed over the demo snapshot, each frame's state, motion tweens, notices and FNV-1a digests of the four cell planes. The scripts travel as the exact bytes the oracle read, because the original's `mix` maps keep JSON insertion order for tied tool counts. |
| `full-read.json.gz` | 39 full-reader cases: five schema and plan variants (T3's schema, table scans, covering indexes, nodes without a primary key, a malformed payload) across three rounds and three clocks. Each variant's database travels once as SQL text and the deletes, retimes and inserts between rounds travel as the statements the original ran. Each case records the digest of the whole normalized snapshot plus its counts, or the failure the original reported. |
| `cross.json.gz` | The cross-machine case: a synthetic database (remote parents, a retry through an earlier provider turn, a delegation cycle) with its machine id as a tree, the original's whole snapshot, the 78 full-grid states over it and every agent's focused state, as plane digests. |
| `fuzz.json.gz` | 945 action prefixes: 45 seeded cases of 10 to 32 agents (malformed topology, cycles and orphans included), 21 prefixes each. The generated snapshots and action lists travel with it. |
| `semantic.json.gz` | 124 semantic frames: selected row, scope, sorted fold/expand sets and the complete row list after each prefix of the 19 standard actions, for demo, empty, error, edge, large, rapid and four burst snapshots. The input snapshots travel with it. |
| `text.json.gz` | The text and number helpers over 5,108,760 generated inputs, as digests: 259 groups plus the all-code-points `clean` group, in UTC, Asia/Kolkata and America/Los_Angeles. See [Text and number helpers](#text-and-number-helpers-digests-not-data). 0.7 MB. |
| `labels.jsonl.gz` | Grid states for `checks/fixtures/labels.json`: `itemLabel`'s home-folder shortening over 25 action sequences, six grids and three `HOME` values (one plain, one containing regular-expression characters, one unset). 450 states. See [Grid golden fixtures](#grid-golden-fixtures). |
| `terminal.json.gz` | The real-terminal gate's 551 cases, plus four odd-size and `TERM` cases: what the original's tmux panes showed (544 captures) and left behind, its database, and what it prints off a terminal. See [The real terminal](#the-real-terminal). 0.3 MB. |

## Regenerate

```sh
N="node --experimental-ffi --disable-warning=ExperimentalWarning"
$N checks/scripts/fixtures.ts
$N checks/scripts/export-status.ts
$N checks/scripts/export-cli.ts
$N checks/scripts/export-semantic.ts
$N checks/scripts/export-behavior.ts
$N checks/scripts/export-fuzz.ts
$N checks/scripts/export-demo.ts
$N checks/scripts/export-cross.ts
$N checks/scripts/export-full-read.ts
$N checks/scripts/export-text.ts       # about 4 minutes: three timezones, two runs each
$N checks/scripts/export-terminal.ts   # about 20 minutes: two runs of the tmux gate; needs tmux and a release t3-agents-term
```

`canonicalDigest` gives a whole JSON value one order-independent digest (object keys sorted, strings as UTF-8 bytes, numbers as integers or IEEE-754 bits). The full-reader fixture uses it, so 39 snapshots of about 1.3 MB each cost a few hundred bytes; `golden_full_read.rs` checks both implementations against the same two vectors.

Every exporter runs its gate twice and refuses to install a fixture whose two runs differ, so the "regenerate" line above is also the determinism check.

Each exporter only makes sense while `oracle/` is the frozen oracle. It records the revision it ran against, so a fixture taken from a modified `oracle/` says so.

## Check

```sh
cargo test --release --locked -p t3-agents-core -p t3-agents-term
```

`T3_GOLDEN_DIR` points a test at another export, which is how the fail-on-purpose check is done: change one expected value in a copy and watch the test name the case.

## Text and number helpers: digests, not data

`text-parity.ts` feeds about 5.1 million generated inputs (text cleaning and width, fit and wrap, number and duration formatting, link encoding, colours, and so on) to the small helpers, once in each of three timezones. Storing the inputs or the outputs would be far too large, so `text.json.gz` keeps digests, and `crates/core/tests/golden_text.rs` rebuilds the inputs itself with `golden_text/generators.rs`, a Rust port of the gate's generators: its linear congruential generator, its character pool, and each group's construction in the order it draws random numbers. The helpers run in the same binary and by the same `--probe-helper` interface as the live gate.

One entry per `compare(...)` call of the gate (a *group*), in the gate's order, each holding:

- `inputs`: FNV-1a over the exact bytes piped to the probe. **This is what proves the port.** If the Rust generators build anything else, the test fails and names the group and blames the port, not the helper.
- `head`: the oracle's first 64 outputs, verbatim. A failure among them names the exact input, the recorded value and the Rust value.
- `blocks`: one FNV-1a digest per further 64 outputs, each over the key-order-independent digests of the outputs (`canonicalDigest`). A failure there names the block of inputs (and shows what Rust gives for its first few) but not the one input, because the recording does not keep it. To find it, run `checks/scripts/text-parity.ts` live, which prints the first five differences per group, or re-record.
- `zones`: only on the groups whose outputs depend on the timezone (`clock` and `when`): the same `head` and `blocks` for each zone that differs from UTC. Every other group is recorded once, and `export-text.ts` checks that the three zones agreed on it.

Two more kinds of entry:

- `hidden`: the `clean` group enumerates every code point (1,114,112); the oracle's answer is a list of ranges, so a failure names the code point.
- `kept`: the gate skips inputs on which the original's `wrap` never terminates. That depends on the original's character widths, so its decision is recorded (one flag per candidate) and the port applies it instead of re-deciding it.

Two details the port had to match, and the digests proved: V8 prints a double that falls exactly between two shortest decimal strings with the even one (`0.009660720825195312`), which Rust's shortest printer does not; and V8's `10 ** 23` is `1.0000000000000001e+23`, not `1e23`.

Each zone is run with `TZ` set explicitly and the rest of the environment cleared, so the result does not depend on the machine's timezone, locale or clock. The test needs the machine's timezone database to know the three zones, and says so if it does not. The record is 0.7 MB for 5.1 million inputs: 24 thousand block digests, the 64 verbatim outputs of each group, and the `wrap` filter.

## The real terminal

`terminal-parity.ts` runs the original and `t3-agents-term` side by side in a private tmux server and compares every captured cell. `terminal.json.gz` keeps what the original did, and `crates/term/tests/golden_terminal.rs` drives only `t3-agents-term` through the same steps and compares with it. It needs tmux, not Node: where tmux is missing each tmux case says `SKIPPED ... tmux is not installed` on stderr and passes.

- `screen`: 14 cases (demo and database sources at four sizes, and other `TERM` and `COLORTERM` values) of 38 captures each: the initial frame and 37 steps. Every entry holds the step's name, **the key that was sent** (a tmux key name, a character, or SGR mouse bytes as `hex:...`, or a resize), the terminal size at that point, the original's capture verbatim (`tmux capture-pane -p -e -N`, content-addressed in `assets`), and a digest of the cells the gate's parser reads from it. The replay sends the recorded key, waits for the screen to settle as the gate does, parses both captures with a Rust port of the gate's parser, and reports every differing cell (glyph, colour, background, weight, attributes) with its position. The recorded digest proves that the port reads the original's capture as the original's parser did, so a parser fault is told apart from a program difference.
- `screen`, bracketed paste: a fifteenth case, `paste demo 80x24 tmux-256color truecolor`, of the initial frame and 11 steps: pastes of quit keys, newlines, escape sequences and 100 KB, a paste in three reads and keys right after one, each paste followed by a `j` so state a paste changed without showing it comes out on the next screen. The key is `paste:<hex>[*<n>]` (the text, n times, given to tmux's `paste-buffer -p`, which wraps it in markers only for a program that switched mode 2004 on) or `pieces:<hex>|<ms>ms|<hex>...` (raw writes with pauses). The test `recording_holds_exactly_the_screen_cases_replayed_here` holds it to 12 captures.
- `exit`: for `q`, Ctrl-C, SIGTERM and SIGHUP, and for `paste+q` and `paste+SIGTERM` (a bracketed paste of quit keys first, which the program must survive, and which must leave mode 2004 switched on and then off), the original's exit code, the terminal modes it left on or off, whether `stty` was restored, and whether the pane was back on the main screen. The Rust program must leave the same modes, with one deliberate difference held to exactly "original on, Rust off": mode 2027, which the original leaves switched on.
- `survival`: four runs at 10×3, 1×1, `TERM=dumb` and inside another tmux, reduced to the gate's verdict and the class of outcome (the pane text carries a timestamp and is not kept).
- `not-a-terminal`: exit status, stderr and stdout of the original with its input and output not on a terminal.
- `database`: the synthetic SQLite file the `db` cases read, base64, byte for byte, with its `environment-id`.

The replay starts tmux and the program under the environment the exporter used (everything cleared but `PATH`, `HOME`, `LANG`, `TZ` and `T3CODE_HOME`, then the exports the gate's scripts make) and pins the clock with `T3_FIXED_NOW`, so the screens do not depend on the machine's desktop, timezone or time. Each case has its own tmux server; four run at once (`T3_GOLDEN_TMUX_JOBS`), since each waits on real time. The whole file takes about two minutes. The recording was taken with tmux 3.8; the captures are cell-exact, so a different tmux that draws the same screen differently would show up as differences.

## Grid golden fixtures

`states.jsonl.gz` is gzip-compressed JSON lines, one record per state: three inputs (demo, edge, empty) × six grids × thirteen action sequences, 234 in all. They are the same states `checks/scripts/grid-parity.ts` compares live.

`hidden.jsonl.gz` is the same recording for `checks/fixtures/hidden.json`, the fixture whose strings carry the characters `clean()` replaces with a space (`U+200B` to `U+200F`, soft hyphen, word joiner, `U+FEFF`, C0 and C1 controls, private use, an unassigned code point, a variation selector, a combining mark, regional indicators, a skin-tone modifier, and emoji sequences built from ZWJ), at the start, middle and end of titles, projects, models, tool labels, commands, files, plan steps, requests and remote labels: 78 states. It is a separate file so the recording above is never rewritten, and `crates/core/tests/golden.rs` checks both.

`labels.jsonl.gz` records `checks/fixtures/labels.json`, the fixture for `itemLabel` (`view.ts` line 110): the home folder at the start, the middle, twice and nowhere in commands, file names and labels; a path that only begins with the same characters; a home folder containing regular-expression characters; first lines that are empty, blank or followed by more lines; every item type with and without a label; every agent status the original names, several it does not (`unknown`, `settled`, `cancelled`, `ßeta`, `ıdentity`), mixed case and an empty one; and token counts, a per-turn delta and a context percentage that sit exactly on a rounding tie. The fixture names its own action list (a pick for each status agent, so its details pane is drawn) and its own home folders: `/home/user`, `/home/user.name[x]` and unset. Each state therefore records the `home` it was drawn under, and the replay sets or removes `HOME` accordingly instead of reading anyone's real home folder. States recorded before this field existed replay under a fixed `/synthetic/home`, which no recorded string contains.

Each record holds:

- `input`, `grid`, `actions`: which state it is.
- `home` (only in `labels.jsonl.gz`): the `HOME` the oracle drew it under, or `null` for unset.
- `w`, `h`, and the semantic fields the oracle reports: `selected`, `list`, `detail`, `detailMax`.
- `text`: the screen as readable rows.
- `ch`, `fg`, `bg`, `at`: one entry per cell in row-major order. Unicode code point, foreground and background as `0xRRGGBB` integers, and the attribute bits (1 is bold).
- `sha256`: SHA-256 over the UTF-8 text `codepoint,fg,bg,attribute\n` for every cell in row-major order.

`demo.json`, `edge.json`, `empty.json` and `hidden.json` are the input snapshots those states were drawn from, so the test needs nothing generated.

Plain arrays inside gzip were chosen over run-length or palette-indexed encodings: the records stay readable with `zcat`, the Rust test needs no decoder beyond `flate2`, and the compressed file is about 1.2 MB.

Regenerate with `TZ=UTC $N checks/scripts/export-golden.ts` after `fixtures.ts`; a third argument records one file only (`states`, `hidden` or `labels`), which is how a new fixture is added without rewriting the older recordings. Check with `cargo test ... -p t3-agents-core --test golden`.

