//! Independent equality gate for the typed data the Insights viewport draws.
//!
//! The gate drives only the public `insights_data` API: `open`, `select`, `dispatch`,
//! `poll`, then the borrowed `visible()` snapshot the renderer receives. It reads the
//! typed `Recap` or `Tools` value and `Loaded::returned`, serializes that value with the
//! CLI's own serializer, and compares it field for field with a separately spawned
//! `t3-agents-term --recap/--tools --json` on the same synthetic database and the same
//! pinned clock. There is no product diagnostic switch: both arms are the shipped paths.
//!
//! Independence: every expected number comes from the fixture's generator parameters
//! (runs written per period), and every expected period label and date is written down
//! here, not read back from the product. A misrouted tab or period, a suppressed or
//! invented fallback, or a changed value therefore fails twice: against the typed
//! value's own metadata and against the independent fixture count.
//!
//! Runs one test sequentially so the isolated worker process and its fixed zone
//! cannot race another test, and because that is the whole gate:
//!
//! ```sh
//! taskset -c 0-3 cargo test --release --locked -p t3-agents-term \
//!   --test insights_data_gate -- --test-threads=1
//! ```
//!
//! CI's `cargo test --release --locked` also picks it up as a standard integration test.
//! It builds its own SQLite files under `CARGO_TARGET_TMPDIR` and runs the compiled
//! binary with a cleared environment and a fresh empty `T3CODE_HOME`; it never opens a
//! real T3 database.
#![cfg(unix)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::mpsc,
    time::Duration,
};

use serde_json::{json, Value as Json};
use t3_agents_core::{
    platform,
    product::{self, Database, Origin},
    recap::{self, Span, Which},
    recap_view,
};
use t3_agents_term::insights_data::{
    InsightsData, Loaded, ReadError, Report, Selection, Value,
};

/// 2026-11-18 10:20:00 America/Los_Angeles, the clock the screen's design fixtures use.
const NOW_MS: f64 = 1_795_026_000_000.0;
const NOW_TEXT: &str = "1795026000000";
const ZONE: &str = "America/Los_Angeles";
const TIMEOUT: Duration = Duration::from_secs(30);
const PERIODS: [Which; 4] = [Which::LastWeek, Which::ThisWeek, Which::LastMonth, Which::ThisMonth];

const P: &str = "orchestration_v2_projection_";

/// The synthetic projection schema Recap and Tools read. This is the same minimal shape
/// the report checks build: identifiers, times, statuses and item types only.
const SCHEMA: &str = "
CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,payload_json TEXT,deleted_at TEXT,archived_at TEXT);
CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_subagents(subagent_id TEXT PRIMARY KEY,thread_id TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_nodes(node_id TEXT PRIMARY KEY,thread_id TEXT,parent_node_id TEXT,run_id TEXT);
CREATE TABLE orchestration_v2_projection_provider_turns(thread_id TEXT,provider_thread_id TEXT,ordinal INTEGER,payload_json TEXT,started_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_turn_items(item_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,node_id TEXT,provider_thread_id TEXT,type TEXT,status TEXT,updated_at TEXT,ordinal INTEGER,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_runtime_requests(request_id TEXT PRIMARY KEY,thread_id TEXT,node_id TEXT,kind TEXT,status TEXT,created_at TEXT,resolved_at TEXT);";

/// One group of finished runs, three hours apart, each lasting `minutes`.
struct Group {
    start: &'static str,
    count: u32,
    minutes: i64,
}

/// A scratch folder under the cargo target directory, so no check writes to `/tmp`.
struct Scratch {
    root: PathBuf,
    home: PathBuf,
    t3: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("insights-data-gate-{}", std::process::id()));
        fs::create_dir_all(root.parent().unwrap()).expect("cargo test scratch");
        fs::create_dir(&root).expect("fresh gate directory");
        let home = root.join("home");
        let t3 = root.join("empty-t3code-home");
        fs::create_dir_all(&home).expect("scratch home");
        fs::create_dir_all(&t3).expect("scratch T3CODE_HOME");
        Self { root, home, t3 }
    }

    fn database(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.sqlite"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn isolate(command: &mut Command, scratch: &Scratch) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &scratch.home)
        .env("T3CODE_HOME", &scratch.t3)
        .env("XDG_CONFIG_HOME", scratch.home.join(".config"))
        .env("XDG_DATA_HOME", scratch.home.join(".local/share"))
        .env("XDG_CACHE_HOME", scratch.home.join(".cache"))
        .env("XDG_STATE_HOME", scratch.home.join(".local/state"))
        .env("TZ", ZONE)
        .env("T3_AGENTS_NO_CACHE", "1")
        .env("TMPDIR", &scratch.root);
}

fn check_clock() {
    let clock = platform::local_time((NOW_MS / 1000.0) as i64).expect("a local clock");
    assert_eq!(
        clock,
        platform::LocalTime { year: 2026, month0: 10, day: 18, hour: 10, minute: 20, second: 0 },
        "the fixed clock must be 2026-11-18 10:20 in {ZONE}"
    );
}

/// The period metadata this gate expects, written from the calendar, not from the product.
#[derive(Clone, Copy, Debug)]
struct Meta {
    span: Span,
    from: &'static str,
    to: &'static str,
    so_far: bool,
    label: &'static str,
}

fn meta(which: Which) -> Meta {
    match which {
        Which::LastWeek => Meta { span: Span::Week, from: "2026-11-09", to: "2026-11-15", so_far: false, label: "Nov 9 – 15" },
        Which::ThisWeek => Meta { span: Span::Week, from: "2026-11-16", to: "2026-11-22", so_far: true, label: "Nov 16 – 22" },
        Which::LastMonth => Meta { span: Span::Month, from: "2026-10-01", to: "2026-10-31", so_far: false, label: "October 2026" },
        Which::ThisMonth => Meta { span: Span::Month, from: "2026-11-01", to: "2026-11-30", so_far: true, label: "November 2026" },
    }
}

fn which_text(which: Which) -> &'static str {
    match which {
        Which::LastWeek => "last week",
        Which::ThisWeek => "this week",
        Which::LastMonth => "last month",
        Which::ThisMonth => "this month",
    }
}

fn report_text(report: Report) -> &'static str {
    match report {
        Report::Recap => "recap",
        Report::Tools => "tools",
    }
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    report: Report,
    requested: Which,
    /// The exact period the fixture's data and the CLI fallback rule must produce.
    returned: Which,
    /// Finished runs (Recap) and calls (Tools) the fixture wrote for that period.
    /// `None` for the demo, whose shape is checked but whose made-up numbers are not
    /// this gate's fixture.
    count: Option<u32>,
}

/// Build one synthetic database from the groups, returning its path.
fn build_fixture(scratch: &Scratch, name: &str, groups: &[Group]) -> PathBuf {
    let path = scratch.database(name);
    let db = rusqlite::Connection::open(&path).expect("synthetic database");
    db.execute_batch(SCHEMA).expect("synthetic schema");
    if groups.is_empty() {
        return path;
    }
    db.execute(
        &format!("INSERT INTO {P}threads VALUES('t1',?,NULL,NULL)"),
        [json!({
            "projectId": "p",
            "createdAt": "2026-09-01T10:00:00.000Z",
            "lineage": {"relationshipToParent": null},
            "pullRequests": []
        })
        .to_string()],
    )
    .expect("thread");

    let mut ordinal: i64 = 0;
    for group in groups {
        let base = recap::ms(group.start).expect("fixture start is ISO");
        for index in 0..group.count {
            ordinal += 1;
            let start = base + f64::from(index) * 3.0 * recap::HOUR;
            let end = start + group.minutes as f64 * 60_000.0;
            let (from, to) = (recap::iso(start), recap::iso(end));
            let run = format!("{name}-r{ordinal}");
            db.execute(
                &format!("INSERT INTO {P}runs VALUES(?,?,?,?,?,?)"),
                rusqlite::params![
                    run,
                    "t1",
                    ordinal,
                    json!({
                        "status": "completed",
                        "providerInstanceId": "claudeAgent",
                        "startedAt": from,
                        "modelSelection": {"model": "claude-sonnet-5-5"}
                    })
                    .to_string(),
                    from,
                    to
                ],
            )
            .expect("run");
            let items = [
                (
                    "user_message",
                    json!({"createdBy": "user", "creationSource": "web"}),
                ),
                (
                    "command_execution",
                    json!({"startedAt": from, "completedAt": to, "input": "git status"}),
                ),
                (
                    "checkpoint",
                    json!({"files": [{"kind": "modified", "additions": ordinal, "deletions": index}]}),
                ),
            ];
            for (step, (kind, payload)) in items.iter().enumerate() {
                db.execute(
                    &format!("INSERT INTO {P}turn_items VALUES(?,'t1',?,'n',NULL,?,'completed',?,?,?)"),
                    rusqlite::params![
                        format!("{run}-{kind}"),
                        run,
                        kind,
                        to,
                        step as i64 + 1,
                        payload.to_string()
                    ],
                )
                .expect("item");
            }
            db.execute(
                &format!("INSERT INTO {P}provider_turns VALUES('t1',NULL,?,?,?,?)"),
                rusqlite::params![
                    ordinal,
                    json!({
                        "turnTokenUsage": {
                            "usageStatus": "complete",
                            "inputTokens": 1000,
                            "outputTokens": 100,
                            "cachedInputTokens": 0
                        },
                        "tokenUsage": {"usedTokens": 1000, "maxTokens": 1000000}
                    })
                    .to_string(),
                    from,
                    to
                ],
            )
            .expect("provider turn");
        }
    }
    path
}

fn period_argument(which: Which) -> &'static str {
    match which {
        Which::LastWeek => "",
        Which::ThisWeek => "--this-week",
        Which::LastMonth => "--month",
        Which::ThisMonth => "--this-month",
    }
}

/// Run the shipped binary on the same database, clock and reader settings.
fn cli(scratch: &Scratch, database: &Database, report: Report, which: Which) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_t3-agents-term"));
    command.arg(match report {
        Report::Recap => "--recap",
        Report::Tools => "--tools",
    });
    command.arg("--json");
    let period = period_argument(which);
    if !period.is_empty() {
        command.arg(period);
    }
    command.args(["--now-ms", NOW_TEXT]);
    match database {
        Database::Demo => {
            command.arg("--demo");
        }
        Database::Path { path, .. } => {
            command.args(["--db", path.to_str().expect("utf-8 synthetic path")]);
        }
    }
    isolate(&mut command, scratch);
    command.output().expect("the compiled t3-agents-term must run")
}

/// The first field that differs, so a failure names a path instead of dumping two reports.
fn difference(view: &Json, cli: &Json, path: &str) -> Option<String> {
    match (view, cli) {
        (Json::Object(view), Json::Object(cli)) => {
            for (key, value) in view {
                match cli.get(key) {
                    Some(other) => {
                        if let Some(found) = difference(value, other, &format!("{path}.{key}")) {
                            return Some(found);
                        }
                    }
                    None => return Some(format!("{path}.{key}: only in the value given to the view")),
                }
            }
            for key in cli.keys() {
                if !view.contains_key(key) {
                    return Some(format!("{path}.{key}: only in the CLI JSON"));
                }
            }
            None
        }
        (Json::Array(view), Json::Array(cli)) => {
            if view.len() != cli.len() {
                return Some(format!("{path}: {} items for the view, {} from the CLI", view.len(), cli.len()));
            }
            for (index, (left, right)) in view.iter().zip(cli).enumerate() {
                if let Some(found) = difference(left, right, &format!("{path}[{index}]")) {
                    return Some(found);
                }
            }
            None
        }
        _ if view != cli => Some(format!("{path}: view {view} != CLI {cli}")),
        _ => None,
    }
}

/// Drive a fresh visit through open/dispatch/poll and hold the borrowed snapshot the
/// renderer gets. Returns the pretty JSON of the value the view was handed, for the
/// demo's cross-period distinctness check.
fn run_case(scratch: &Scratch, database: &Database, case: Case) -> String {
    let selection = Selection { report: case.report, period: case.requested };
    let label = format!("{} / {} / {}", case.name, report_text(case.report), which_text(case.requested));

    let mut data = InsightsData::default();
    data.open(NOW_MS);
    assert_eq!(data.selection(), Some(Selection::ENTRY), "{label}: opening starts at Recap this week");
    assert_eq!(data.loading_since(), Some(NOW_MS), "{label}: the visit pinned the clock");
    assert!(data.visible().is_none(), "{label}: nothing is drawn before a read finishes");

    if selection != Selection::ENTRY {
        assert!(data.select(selection, NOW_MS), "{label}: the unread selection stages a read");
    }
    assert_eq!(data.selection(), Some(selection), "{label}: the requested selection is kept");

    let (wake, ready) = mpsc::channel();
    assert!(data.dispatch(database, move || {
        let _ = wake.send(());
    }), "{label}: dispatch starts the read");
    assert!(!data.dispatch(database, || unreachable!("already dispatched")), "{label}: a staged read is dispatched once");
    ready.recv_timeout(TIMEOUT).unwrap_or_else(|_| panic!("{label}: the reader never woke the loop"));
    assert!(data.poll(), "{label}: poll takes the finished read");
    assert!(!data.poll(), "{label}: the mailbox is empty after poll");
    assert_eq!(data.loading_since(), None, "{label}: the read is no longer pending");

    let snapshot = data.visible().unwrap_or_else(|| panic!("{label}: a finished read must be visible"));
    assert_eq!(snapshot.selected, selection, "{label}: the snapshot names the requested cache key");
    assert_eq!(snapshot.cutoff, NOW_MS, "{label}: the snapshot kept the pinned cutoff");
    assert!(snapshot.finished_at.is_finite() && snapshot.finished_at > 0.0, "{label}: a finish time is recorded");
    assert!(std::ptr::eq(snapshot, data.outcome().expect("outcome")), "{label}: outcome and visible agree before a refresh");

    let loaded: &Loaded = match &snapshot.outcome {
        Ok(loaded) => loaded,
        Err(error) => panic!("{label}: expected a report, got {error}"),
    };
    assert_eq!(loaded.returned, case.returned, "{label}: the actual period is reported independently");

    let expected = meta(case.returned);
    match (&case.report, &loaded.value) {
        (Report::Recap, Value::Recap(value)) => {
            assert_eq!(value.period, expected.span, "{label}: the report's span");
            assert_eq!(
                (value.from.as_str(), value.to.as_str(), value.so_far),
                (expected.from, expected.to, expected.so_far),
                "{label}: the report's own dates and partial-period flag"
            );
            assert_eq!(
                recap_view::period_text(value.period, &value.from, &value.to),
                expected.label,
                "{label}: the period label the screen draws"
            );
            assert_eq!(value.by_day.len(), value.days as usize, "{label}: one entry a day");
            assert_eq!(value.by_hour.len(), value.days as usize, "{label}: one entry a day");
            match case.count {
                Some(seed) => {
                    assert_eq!(value.runs, seed, "{label}: runs from the fixture's own count");
                    assert_eq!(value.tasks.total, seed, "{label}: every fixture run is a task in the period");
                    assert_eq!(value.tasks.finished, seed, "{label}: every fixture run finished");
                    assert_eq!(value.commands, seed, "{label}: one command per fixture run");
                    assert_eq!(value.threads, u32::from(seed > 0), "{label}: one fixture thread");
                }
                None => {
                    assert!(value.runs > 0, "{label}: the demo period has runs");
                    assert_eq!(value.tasks.total, value.runs, "{label}: the demo's tasks add up");
                    assert!(!value.models.is_empty(), "{label}: the demo names models");
                }
            }
        }
        (Report::Tools, Value::Tools(value)) => {
            assert_eq!(value.period, expected.span, "{label}: the report's span");
            assert_eq!(
                (value.from.as_str(), value.to.as_str(), value.so_far),
                (expected.from, expected.to, expected.so_far),
                "{label}: the report's own dates and partial-period flag"
            );
            assert_eq!(
                recap_view::period_text(value.period, &value.from, &value.to),
                expected.label,
                "{label}: the period label the screen draws"
            );
            assert_eq!(
                value.lengths.iter().map(|bucket| bucket.calls).sum::<u32>(),
                value.calls - value.open - value.closed_together,
                "{label}: the length buckets account for the calls"
            );
            match case.count {
                Some(calls) => {
                    assert_eq!(value.calls, calls, "{label}: calls from the fixture's own count");
                    assert_eq!(value.commands.calls, calls, "{label}: one command per fixture run");
                    assert_eq!(value.commands.named, calls, "{label}: every fixture command is named");
                    assert_eq!(value.kinds.iter().map(|row| row.calls).sum::<u32>(), calls, "{label}: kinds add up");
                    assert_eq!(value.providers.iter().map(|row| row.calls).sum::<u32>(), calls, "{label}: providers add up");
                    assert_eq!(value.threads, u32::from(calls > 0), "{label}: one fixture thread");
                    match value.programs.iter().find(|row| row.name.as_deref() == Some("git")) {
                        Some(git) => assert_eq!(git.calls, calls, "{label}: every fixture call is the same listed program"),
                        None => assert_eq!(calls, 0, "{label}: no calls, no program rows"),
                    }
                }
                None => {
                    assert!(value.calls > 0, "{label}: the demo period has calls");
                    assert_eq!(
                        value.kinds.iter().map(|row| row.calls).sum::<u32>(),
                        value.calls,
                        "{label}: the demo's kinds add up"
                    );
                    assert_eq!(
                        value.providers.iter().map(|row| row.calls).sum::<u32>(),
                        value.calls,
                        "{label}: the demo's providers add up"
                    );
                }
            }
        }
        (Report::Recap, Value::Tools(_)) | (Report::Tools, Value::Recap(_)) => {
            panic!("{label}: the view was handed the other report")
        }
    }

    // Print with the CLI's serializer, then read that text back, so the field-by-field
    // comparison sees the same shortest float spelling the CLI wrote (a value can hold
    // `f32`, whose JSON number is not the exact `f64` expansion).
    let pretty = match &loaded.value {
        Value::Recap(value) => serde_json::to_string_pretty(value).expect("Recap prints"),
        Value::Tools(value) => serde_json::to_string_pretty(value).expect("Tools prints"),
    };
    let view: Json = serde_json::from_str(&pretty).expect("the view's own JSON reads back");
    let output = cli(scratch, database, case.report, case.requested);
    assert!(
        output.status.success(),
        "{label}: the CLI exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "{label}: the CLI JSON wrote no advice: {}", String::from_utf8_lossy(&output.stderr));
    let printed: Json = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{label}: the CLI printed JSON: {error}"));
    if let Some(found) = difference(&view, &printed, "$") {
        panic!("{label}: the value given to the view differs from the CLI: {found}");
    }
    let text = String::from_utf8(output.stdout).expect("utf-8 CLI JSON");
    assert_eq!(text, format!("{pretty}\n"), "{label}: the value is the CLI's report byte for byte");

    data.close();
    assert!(data.visible().is_none(), "{label}: closing drops the snapshot");
    pretty
}

fn demo_cases(scratch: &Scratch) {
    let demo = Database::Demo;
    for report in [Report::Recap, Report::Tools] {
        let mut seen = Vec::new();
        for requested in PERIODS {
            let printed = run_case(scratch, &demo, Case {
                name: "demo",
                report,
                requested,
                returned: requested,
                count: None,
            });
            for (earlier, text) in &seen {
                assert_ne!(
                    *text, printed,
                    "demo / {} / {}: the demo must differ from {} so a misrouted period is visible",
                    report_text(report),
                    which_text(requested),
                    which_text(*earlier)
                );
            }
            seen.push((requested, printed));
        }
    }
}

fn fixture_cases(scratch: &Scratch) {
    // Four periods, four different amounts of work: last month 5, last week 2,
    // this week 4, this month 6 (both November groups).
    let distinct = build_fixture(scratch, "distinct", &[
        Group { start: "2026-10-07T18:00:00Z", count: 5, minutes: 30 },
        Group { start: "2026-11-10T18:00:00Z", count: 2, minutes: 60 },
        Group { start: "2026-11-17T18:00:00Z", count: 4, minutes: 120 },
    ]);
    let database = Database::Path { path: distinct, origin: Origin::Db };
    for report in [Report::Recap, Report::Tools] {
        for (requested, count) in [
            (Which::LastWeek, 2),
            (Which::ThisWeek, 4),
            (Which::LastMonth, 5),
            (Which::ThisMonth, 6),
        ] {
            run_case(scratch, &database, Case {
                name: "distinct",
                report,
                requested,
                returned: requested,
                count: Some(count),
            });
        }
    }

    // Only this week has work: last week is empty and falls back to this week.
    let week = build_fixture(scratch, "week-fallback", &[
        Group { start: "2026-11-17T18:00:00Z", count: 4, minutes: 30 },
    ]);
    let database = Database::Path { path: week, origin: Origin::Db };
    for report in [Report::Recap, Report::Tools] {
        run_case(scratch, &database, Case {
            name: "week-fallback",
            report,
            requested: Which::LastWeek,
            returned: Which::ThisWeek,
            count: Some(4),
        });
        run_case(scratch, &database, Case {
            name: "week-fallback",
            report,
            requested: Which::ThisWeek,
            returned: Which::ThisWeek,
            count: Some(4),
        });
    }

    // Only last week has work: last month is empty and falls back to this month, while
    // this week is empty and, being a period so far, stays empty.
    let month = build_fixture(scratch, "month-fallback", &[
        Group { start: "2026-11-10T18:00:00Z", count: 2, minutes: 45 },
    ]);
    let database = Database::Path { path: month, origin: Origin::Db };
    for report in [Report::Recap, Report::Tools] {
        for (requested, returned, count) in [
            (Which::LastWeek, Which::LastWeek, 2),
            (Which::ThisWeek, Which::ThisWeek, 0),
            (Which::LastMonth, Which::ThisMonth, 2),
            (Which::ThisMonth, Which::ThisMonth, 2),
        ] {
            run_case(scratch, &database, Case {
                name: "month-fallback",
                report,
                requested,
                returned,
                count: Some(count),
            });
        }
    }

    // Both periods empty: the original selection is kept and drawn empty.
    let empty = build_fixture(scratch, "both-empty", &[]);
    let database = Database::Path { path: empty, origin: Origin::Db };
    for report in [Report::Recap, Report::Tools] {
        for requested in PERIODS {
            run_case(scratch, &database, Case {
                name: "both-empty",
                report,
                requested,
                returned: requested,
                count: Some(0),
            });
        }
    }
}

fn error_case(scratch: &Scratch, path: &Path, report: Report, kind: &str) {
    let selection = Selection { report, period: Which::ThisWeek };
    let label = format!("{kind} / {}", report_text(report));
    let database = Database::Path { path: path.to_path_buf(), origin: Origin::Db };
    let mut data = InsightsData::default();
    data.open(NOW_MS);
    if selection != Selection::ENTRY {
        assert!(data.select(selection, NOW_MS));
    }
    let (wake, ready) = mpsc::channel();
    assert!(data.dispatch(&database, move || {
        let _ = wake.send(());
    }));
    ready.recv_timeout(TIMEOUT).unwrap_or_else(|_| panic!("{label}: the reader never woke the loop"));
    assert!(data.poll());
    let snapshot = data.visible().unwrap_or_else(|| panic!("{label}: the failed read is still shown as the outcome"));
    assert_eq!(snapshot.selected, selection, "{label}: the failed read names the selection");
    assert_eq!(snapshot.cutoff, NOW_MS, "{label}: the failed read kept the pinned cutoff");
    let source = match &snapshot.outcome {
        Ok(_) => panic!("{label}: expected a source error, got a report"),
        Err(ReadError::Source(error)) => error,
        Err(ReadError::StartWorker(error)) => panic!("{label}: the worker could not start: {error}"),
    };
    assert_eq!(source.kind, kind, "{label}: the typed error kind");
    assert_eq!(source.path, path, "{label}: the typed error names the path");
    if kind == "missing" {
        assert!(source.detail.is_empty(), "{label}: a missing file has no reader detail");
    } else {
        assert!(!source.detail.is_empty(), "{label}: an unreadable file says why");
    }

    let advice = product::advice(source, Origin::Db);
    let first = if kind == "missing" {
        format!("t3-agents: no T3 database at {}", path.display())
    } else {
        format!("t3-agents: can't read the T3 database at {}: ", path.display())
    };
    assert!(advice.starts_with(&first), "{label}: advice names the failure: {advice}");
    assert!(advice.contains("Check the path given to --db."), "{label}: advice points at --db");

    let output = cli(scratch, &database, report, Which::ThisWeek);
    assert_eq!(output.status.code(), Some(1), "{label}: the CLI exits 1");
    assert!(output.stdout.is_empty(), "{label}: the CLI prints no JSON on a source error");
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf-8 advice"),
        advice,
        "{label}: the typed error produces the CLI's advice"
    );
    data.close();
}

fn error_cases(scratch: &Scratch) {
    let missing = scratch.root.join("does-not-exist.sqlite");
    let unreadable = scratch.root.join("not-a-database.sqlite");
    fs::write(&unreadable, b"this is not a SQLite database\n").expect("unreadable fixture");
    for (kind, path) in [("missing", missing.as_path()), ("unreadable", unreadable.as_path())] {
        for report in [Report::Recap, Report::Tools] {
            error_case(scratch, path, report, kind);
        }
    }
}

#[test]
fn insights_visible_data_equals_the_cli_for_every_report_and_period() {
    let scratch = Scratch::new();
    // Start the worker arm in the same clean environment as the CLI, before
    // any of its threads start. Never mutate the test runner's environment.
    if std::env::var_os("INSIGHTS_GATE_CHILD").is_none() {
        let mut child = Command::new(std::env::current_exe().unwrap());
        isolate(&mut child, &scratch);
        let status = child.env("INSIGHTS_GATE_CHILD", "1")
            .args(["--exact", "insights_visible_data_equals_the_cli_for_every_report_and_period", "--nocapture"])
            .status().expect("isolated worker arm");
        assert!(status.success());
        return;
    }
    check_clock();
    demo_cases(&scratch);
    fixture_cases(&scratch);
    error_cases(&scratch);
}
