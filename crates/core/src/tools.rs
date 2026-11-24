//! Tools by program: timing and status from T3's projections. The command line is
//! consumed by a SQL function and only its listed program names reach the reader.
//!
//! By default, up to four read-only connections read calls, passing over each payload
//! once inside the query through `skim.rs`. The scope lists and coverage share one
//! snapshot, and each worker holds its own, so commits can land between snapshots.
//! Set `LOUPE_READ_THREADS=1` to read in one transaction or
//! `LOUPE_SKIM=off` to use SQLite's JSON parser.

use crate::{
    fanout, program, recap::{self, DAYS, DEMO_SCHEMA, HOUR, Local, Period, Span, iso, local_date, ms},
    skim, source::SourceError, terminal,
};
use rusqlite::{Connection, Row as SqlRow, Statement, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, time::Duration};

const P: &str = "orchestration_v2_projection_";
pub const MIN_FOR_RATE: u32 = 20;
/// Programs for which exit code 1 is an answer and not a breakage: a search that found
/// nothing, a comparison that found a difference. The report words those failures so; it is
/// the report's reading, kept in this one place, and nothing is guessed from a name.
pub const FOUND_NOTHING: [&str; 2] = ["rg", "grep"];
pub const FILES_DIFFER: [&str; 1] = ["diff"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tools {
    pub period: Span,
    pub from: String,
    pub to: String,
    pub so_far: bool,
    pub days: u32,
    pub days_elapsed: u32,
    pub days_recorded: u32,
    pub calls: u32,
    pub ms: f64,
    pub busy_ms: f64,
    pub failed: u32,
    pub open: u32,
    #[serde(default)]
    pub closed_together: u32,
    pub threads: u32,
    pub without_run: u32,
    pub commands: Commands,
    pub kinds: Vec<Row>,
    pub programs: Vec<Row>,
    pub tools: Vec<Row>,
    pub lengths: Vec<Bucket>,
    pub providers: Vec<Provider>,
    pub longest: Vec<Long>,
    pub min_for_rate: u32,
}

impl Tools {
    pub fn is_empty(&self) -> bool { self.calls == 0 }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind { #[default] Shell, Edit, Read, Search, Web, Tool }

/// Commands with a recorded integer exit code, classified by how they ended.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Exits {
    pub known: u32,
    pub failed_known: u32,
    pub zero: u32,
    pub one: u32,
    pub two: u32,
    pub timed_out: u32,
    pub not_runnable: u32,
    pub not_found: u32,
    pub signal: u32,
    pub other: u32,
    pub passed_nonzero: u32,
}

impl Exits {
    fn add(&mut self, code: Option<i64>, failed: bool) {
        if let Some(code) = code {
            self.known += 1;
            if !failed {
                self.passed_nonzero += u32::from(code != 0);
                return;
            }
            self.failed_known += 1;
            *match code {
                0 => &mut self.zero, 1 => &mut self.one, 2 => &mut self.two,
                124 => &mut self.timed_out, 126 => &mut self.not_runnable,
                127 => &mut self.not_found, 129..=255 => &mut self.signal,
                _ => &mut self.other,
            } += 1;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Commands {
    pub calls: u32,
    pub ms: f64,
    pub failed: u32,
    pub named: u32,
    pub unnamed: u32,
    pub unnamed_ms: f64,
    pub chained: u32,
    pub listed: u32,
    #[serde(default)]
    pub exits: Exits,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub kind: Kind,
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub calls: u32,
    pub ms: f64,
    pub failed: u32,
    pub open: u32,
    #[serde(default)]
    pub closed_together: u32,
    pub longest_ms: f64,
    pub median_ms: Option<f64>,
    pub lengths: Vec<Bucket>,
    pub chained: u32,
    pub appears_in: u32,
    pub by_provider: Vec<Share>,
    #[serde(default)]
    pub exits: Exits,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Share { pub provider: String, pub calls: u32, pub ms: f64, pub failed: u32, #[serde(default)] pub closed_together: u32 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bucket { pub label: String, pub calls: u32, pub ms: f64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    pub provider: String,
    pub calls: u32,
    pub ms: f64,
    pub failed: u32,
    #[serde(default)]
    pub closed_together: u32,
    pub commands: u32,
    pub commands_failed: u32,
    #[serde(default)]
    pub exits: Exits,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Long {
    pub kind: Kind,
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub ms: f64,
    pub failed: bool,
    pub provider: String,
    pub thread: String,
    pub run: Option<String>,
    pub started: f64,
    pub when: String,
    #[serde(default)]
    pub exit: Option<i64>,
}

// ── Command names ────────────────────────────────────────────────────────────
// They come from `program.rs`, the one implementation shared with Trace: the SQL function
// `t3_programs`, which a query calls on a command line so that no Tools query returns it, then
// `program::listed` for each word it gives back and `program::label` for the row. This
// file holds no parser, no list and no labelling rule.

/// Made-up command lines, each with the listed programs and the label `program.rs` gives
/// it. The demo uses the rows before the Trace and privacy fixtures. A test holds the
/// parser to every line.
const LINES: &[(&str, &[&str], Option<&str>)] = &[
    // Demo command families: default, chained, and wrapped, in that order.
    ("sed -n '1,4p' notes.txt", &["sed"], Some("sed")),
    ("cd app && sed -n '1,4p' notes.txt | tail -20", &["sed", "tail"], Some("sed")),
    (r#"bash -lc "sed -n '1,4p' notes.txt""#, &["sed"], Some("sed")),
    ("/usr/bin/git log", &["git"], Some("git")),
    ("cd app && git diff | tail -20", &["git", "tail"], Some("git")),
    (r#"bash -lc "git status""#, &["git"], Some("git")),
    ("cat notes.txt", &["cat"], Some("cat")),
    ("cd app && cat notes.txt | tail -20", &["cat", "tail"], Some("cat")),
    (r#"bash -lc "cat notes.txt""#, &["cat"], Some("cat")),
    ("grep test notes.txt", &["grep"], Some("grep")),
    ("cd app && grep test notes.txt | tail -20", &["grep", "tail"], Some("grep")),
    (r#"bash -lc "grep test notes.txt""#, &["grep"], Some("grep")),
    ("rg -n foo | head", &["rg", "head"], Some("rg")),
    ("cd app && rg test | tail -20", &["rg", "tail"], Some("rg")),
    (r#"bash -lc "rg test""#, &["rg"], Some("rg")),
    ("cargo test -p x", &["cargo"], Some("cargo")),
    ("cd app && cargo test | tail -20", &["cargo", "tail"], Some("cargo")),
    (r#"bash -lc "cd /x && cargo test 2>&1 | tail -20""#, &["cargo", "tail"], Some("cargo")),
    ("python3 run.py", &["python3"], Some("python3")),
    ("cd app && python3 run.py | tail -20", &["python3", "tail"], Some("python3")),
    (r#"bash -lc "python3 run.py""#, &["python3"], Some("python3")),
    ("./run-checks.sh", &[], None),
    ("bash scripts/build.sh", &[], None),
    ("omabox run demo", &[], None),
    (r#"bash -lc "./run-checks.sh""#, &[], None),
    ("ls -la crates", &["ls"], Some("ls")),
    ("cd app && ls -1 snapshots | head -20", &["ls", "head"], Some("ls")),
    ("cargo test -p app", &["cargo"], Some("cargo")),
    ("python3 - <<'PY'\nfrom pathlib import Path\nprint(len(list(Path('tests').glob('*.py'))))\nPY", &["python3"], Some("python3")),
    ("cd app && python3 - <<'PY'\nfrom pathlib import Path\nprint(Path('pyproject.toml').is_file())\nPY", &["python3"], Some("python3")),
    ("npm run build", &["npm"], Some("npm")),
    ("cd app && npm test 2>&1 | tail -20", &["npm", "tail"], Some("npm")),
    ("pytest -q tests/integration", &["pytest"], Some("pytest")),
    (r#"bash -lc "pytest -q tests/unit""#, &["pytest"], Some("pytest")),
    ("node scripts/check-bundle.mjs", &["node"], Some("node")),
    ("cd app && node scripts/verify.mjs | head -30", &["node", "head"], Some("node")),
    ("make test", &["make"], Some("make")),
    ("cd docs && make html 2>&1 | tail -20", &["make", "tail"], Some("make")),
    ("docker build -t app-test .", &["docker"], Some("docker")),
    ("docker compose run --rm test", &["docker"], Some("docker")),
    ("gh pr checks --watch", &["gh"], Some("gh")),
    ("curl -fsS https://example.com/manifest.json | jq .version", &["curl", "jq"], Some("curl")),
    ("ssh buildbox 'uptime'", &["ssh"], Some("ssh")),
    ("jq -r '.scripts | keys[]' package.json", &["jq"], Some("jq")),
    ("find tests -type f -name '*.py' | head -30", &["find", "head"], Some("find")),
    ("awk -F, 'NR > 1 {print $2}' timings.csv | head -20", &["awk", "head"], Some("awk")),
    ("diff -u expected.txt actual.txt", &["diff"], Some("diff")),
    ("uv sync --locked", &["uv"], Some("uv")),
    ("go test ./...", &["go"], Some("go")),
    ("tsc --noEmit -p tsconfig.json", &["tsc"], Some("tsc")),
    ("sleep 5", &["sleep"], Some("sleep")),
    ("mkdir -p tmp/fixtures", &["mkdir"], Some("mkdir")),
    ("cp fixtures/sample.json tmp/fixtures/", &["cp"], Some("cp")),
    ("rm -f tmp/fixtures/sample.json", &["rm"], Some("rm")),
    ("echo 'fixture ready'", &["echo"], Some("echo")),
    ("head -40 CHANGELOG.md", &["head"], Some("head")),
    ("tail -60 tmp/build.log", &["tail"], Some("tail")),
    ("wc -l src/*.rs", &["wc"], Some("wc")),
    // The rest of the Trace test table, followed by the reader's privacy fixture.
    ("cd x && FOO=1 python3 run.py", &["python3"], Some("python3")),
    ("mkdir -p out && cargo build --release", &["mkdir", "cargo"], Some("cargo")),
    ("export A=1; npm test", &["npm"], Some("npm")),
    (r#"echo "a && cargo test""#, &["echo"], Some("echo")),
    ("echo 'x | rg y'", &["echo"], Some("echo")),
    ("cat <<EOF > f.sh\ncargo test\nEOF\ngit status", &["cat", "git"], Some("git")),
    ("x=$(git rev-parse HEAD); echo $x", &["echo"], Some("echo")),
    ("echo `cargo --version`", &["echo"], Some("echo")),
    ("for f in *.rs; do rustc $f; done", &["rustc"], Some("rustc")),
    ("if cargo check; then git commit; fi", &["cargo", "git"], Some("cargo")),
    ("sudo -E env A=1 timeout 60 make test", &["make"], Some("make")),
    ("(cd x && go test ./...) && git diff", &["go", "git"], Some("go")),
    (r#"sh -c 'bash -c "sh -c cargo"'"#, &[], None),
    ("bash build.sh", &[], None),
    ("./never-read --flag", &[], None),
    ("never-read-program arg", &[], None),
    ("# cargo test", &[], None),
    ("", &[], None),
    ("cargo test -p x # never read", &["cargo"], Some("cargo")),
];

// ── The read-only source ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Facts {
    calls: Vec<Call>,
    first_record: Option<f64>,
}

#[derive(Debug)]
struct Call {
    kind: Kind,
    name: Option<String>,
    server: Option<String>,
    programs: Vec<&'static str>,
    label: Option<&'static str>,
    start: f64,
    end: Option<f64>,
    failed: bool,
    provider: String,
    thread: String,
    run: Option<String>,
    exit: Option<i64>,
}

impl Call {
    fn length(&self) -> Option<f64> { self.end.filter(|&end| end >= self.start).map(|end| end - self.start) }

    fn name(&self) -> Option<&str> { self.label.or(self.name.as_deref()) }
}

// Whether this report uses the Insights cache. The store itself honours LOUPE_NO_CACHE=1
// for every reader (`cache_path::insights_file`); turning the cache on is each reader's own
// decision, and Tools' is: only when asked with LOUPE_CACHE=1. Measured on an aged
// database, the cached report is slower than the plain one after any write to T3's
// database, so it stays opt-in here whatever another report does.
fn cache_enabled(enabled: Option<&str>, disabled: Option<&str>) -> bool {
    enabled == Some("1") && disabled != Some("1")
}

#[cfg(unix)]
// This is the only set of tool names an entry may hold. Match byte for byte
// and only with no server. It stays empty until the store's owner agrees to it.
const KNOWN_TOOLS: &[&str] = &[];

#[cfg(unix)]
mod run_entry {
    use super::{Call, Kind, kind_of, program, skim};

    pub(super) const KIND: &str = "tools.calls";
    // Raise this when the encoding, kinds, listed programs, or skim's facts change.
    pub(super) const VERSION: i64 = 1;
    const FORMAT: u8 = 1;
    const MAX_BYTES: usize = 1024 * 1024;

    pub(super) enum Row {
        Named(u32),
        Stored { kind: Kind, start: f64, end: f64, failed: bool,
            exit: Option<i64>, programs: Vec<&'static str>, name: Option<u8> },
    }

    fn mode_tag(skim: Option<skim::Mode>) -> u8 {
        // The modes can differ on unusual payloads, so switching modes is a miss.
        match skim {
            None => 0,
            Some(skim::Mode::Full) => 1,
            Some(skim::Mode::Early) => 2,
            Some(skim::Mode::Head) => 3,
        }
    }

    pub(super) fn encode<'a>(
        rows: impl IntoIterator<Item = (i64, Option<u32>, &'a Call)>,
        skim: Option<skim::Mode>,
        known: &[&str],
    ) -> Option<Vec<u8>> {
        let mut bytes = vec![FORMAT, mode_tag(skim), 0, 0, 0, 0];
        let mut previous = None;
        let mut count = 0u32;
        for (ordinal, slot, call) in rows {
            if previous.is_some_and(|last| last > ordinal) { return None; }
            previous = Some(ordinal);
            let end = call.end?;
            if !call.start.is_finite() || !end.is_finite() || end < call.start { return None; }
            let name = if slot.is_some() && call.server.is_none() {
                call.name.as_deref().and_then(|name| known.iter().position(|known| *known == name))
                    .and_then(|index| u8::try_from(index).ok())
            } else { None };
            let tag = if slot.is_some() && name.is_none() { 0 } else {
                match call.kind {
                    Kind::Shell => 1,
                    Kind::Edit => 2,
                    Kind::Search if name.is_none() => 3,
                    Kind::Web => 4,
                    Kind::Read if name.is_some() => 5,
                    Kind::Search if name.is_some() => 6,
                    Kind::Tool if name.is_some() => 7,
                    _ => return None,
                }
            };
            bytes.push(tag);
            bytes.extend(ordinal.to_le_bytes());
            if tag == 0 {
                bytes.extend(slot?.to_le_bytes());
            } else {
                bytes.extend(call.start.to_le_bytes());
                bytes.extend(end.to_le_bytes());
                bytes.push(u8::from(call.failed) | (u8::from(call.exit.is_some()) << 1));
                if let Some(exit) = call.exit { bytes.extend(exit.to_le_bytes()); }
                bytes.push(u8::try_from(call.programs.len()).ok()?);
                for name in &call.programs {
                    let index = program::PROGRAMS.iter().position(|(listed, _)| listed == name)?;
                    bytes.push(u8::try_from(index).ok()?);
                }
                if let Some(index) = name { bytes.push(index); }
            }
            if bytes.len() > MAX_BYTES { return None; }
            count += 1;
        }
        bytes[2..6].copy_from_slice(&count.to_le_bytes());
        Some(bytes)
    }

    pub(super) fn decode(mut bytes: &[u8], skim: Option<skim::Mode>, known: &[&str]) -> Option<Vec<(i64, Row)>> {
        fn take<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
            let (head, tail) = bytes.split_first_chunk::<N>()?;
            *bytes = tail;
            Some(*head)
        }

        if bytes.len() > MAX_BYTES || take(&mut bytes)? != [FORMAT, mode_tag(skim)] { return None; }
        let count = u32::from_le_bytes(take(&mut bytes)?) as usize;
        if count > bytes.len() / 9 { return None; }
        let mut rows = Vec::new();
        let mut previous = None;
        let mut previous_slot = None;
        for _ in 0..count {
            let [tag] = take(&mut bytes)?;
            let ordinal = i64::from_le_bytes(take(&mut bytes)?);
            if previous.is_some_and(|last| last > ordinal) { return None; }
            if previous != Some(ordinal) { previous_slot = None; }
            previous = Some(ordinal);
            let kind = match tag {
                0 => {
                    let slot = u32::from_le_bytes(take(&mut bytes)?);
                    if previous_slot.is_some_and(|last| last >= slot) { return None; }
                    previous_slot = Some(slot);
                    rows.push((ordinal, Row::Named(slot)));
                    continue;
                }
                1 => Kind::Shell,
                2 => Kind::Edit,
                3 => Kind::Search,
                4 => Kind::Web,
                5 => Kind::Read,
                6 => Kind::Search,
                7 => Kind::Tool,
                _ => return None,
            };
            let start = f64::from_le_bytes(take(&mut bytes)?);
            let end = f64::from_le_bytes(take(&mut bytes)?);
            if !start.is_finite() || !end.is_finite() || end < start { return None; }
            let [flags] = take(&mut bytes)?;
            if flags > 3 || (kind != Kind::Shell && flags & 2 != 0) { return None; }
            let exit = if flags & 2 != 0 { Some(i64::from_le_bytes(take(&mut bytes)?)) } else { None };
            let [count] = take(&mut bytes)?;
            if usize::from(count) > program::PROGRAMS.len() || (kind != Kind::Shell && count != 0) { return None; }
            let mut programs = Vec::with_capacity(usize::from(count));
            let mut seen = [false; 256];
            for _ in 0..count {
                let [index] = take(&mut bytes)?;
                let index = usize::from(index);
                let &(name, _) = program::PROGRAMS.get(index)?;
                if std::mem::replace(&mut seen[index], true) { return None; }
                programs.push(name);
            }
            let name = if tag >= 5 {
                let [index] = take(&mut bytes)?;
                let name = known.get(usize::from(index))?;
                if kind_of("dynamic_tool", None, Some(name)) != kind { return None; }
                Some(index)
            } else { None };
            rows.push((ordinal, Row::Stored { kind, start, end, failed: flags & 1 != 0, exit, programs, name }));
        }
        bytes.is_empty().then_some(rows)
    }

    #[test]
    fn cache_entry_version_pins_format_and_name_lists() {
        let hash = |hash: u64, byte: u8| (hash ^ u64::from(byte)).wrapping_mul(1099511628211);
        let offset = 14695981039346656037u64;
        let programs = program::PROGRAMS.iter().flat_map(|(name, tier)| name.bytes().chain([0, *tier as u8]))
            .fold(offset, hash);
        let known = super::KNOWN_TOOLS.iter().flat_map(|name| name.bytes().chain([0])).fold(offset, hash);
        assert_eq!((VERSION, FORMAT, programs, known), (1, 1, 0xa996a2e55041d0e9, offset),
                   "raise the version and update this pin when the format or a name list changes");
    }
}

// Copies recap::source_error. SQLite's own message is kept, as Recap keeps it: it names a
// table, a column or a JSON fault ("no such table: …", "malformed JSON") and never a row's
// content, and the names function raises no error at all. Any other error (a conversion in
// this file) gets a fixed text, so nothing it might carry can surface.
fn source_error(path: &Path, error: rusqlite::Error) -> SourceError {
    let (kind, detail) = match error {
        rusqlite::Error::SqliteFailure(code, _) if code.extended_code == 26 => ("unreadable", "not a SQLite database".into()),
        rusqlite::Error::SqliteFailure(code, message) => ("schema", message.unwrap_or_else(|| format!("SQLite error {}", code.extended_code))),
        rusqlite::Error::SqlInputError { msg, .. } => ("schema", msg),
        _ => ("schema", "could not read Tools records".into()),
    };
    SourceError { kind, path: path.to_owned(), detail }
}

// Copies recap::has_column. Reads only column existence to choose the schema's
// representation of subagent metadata. It never reads any row or payload.
const HAS_COLUMN: &str = "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2";
fn has_column(q: &mut Statement<'_>, table: &str, column: &str) -> rusqlite::Result<bool> {
    q.query_row(params![format!("{P}{table}"), column], |r| r.get::<_, i64>(0)).map(|n| n > 0)
}

// Copies recap::provider_label; a provider that is not recorded is "unknown".
fn provider_label(provider: &str) -> String {
    match provider {
        "claudeAgent" => "claude".into(),
        other => Some(terminal::clean(other).to_lowercase()).filter(|label| !label.is_empty()).unwrap_or_else(|| "unknown".into()),
    }
}

// Keep the executed statements here so the privacy test inspects the live SQL.
struct Queries {
    runs: String,
    run_calls: String,
    child_calls: String,
    children: String,
    first_record: String,
}

fn queries(child: &str, started: &str, completed: &str, skim: Option<skim::Mode>) -> Queries {
    let (run_calls, child_calls) = (
        call_sql(skim, "run_id=?1", " ORDER BY ordinal, rowid", ""),
        call_sql(skim, "thread_id=?1 AND run_id IS NULL", "", ""),
    );
    Queries {
        runs: runs_sql(""),
        run_calls,
        child_calls,
        // Either lineage or an overlapping subagent window identifies a child. Return its ID
        // once and its provider, for attribution. Never a title, prompt, result, or message.
        children: format!("WITH children(thread_id) AS (
             SELECT thread_id FROM {P}threads
              WHERE json_extract(payload_json,'$.lineage.relationshipToParent')='subagent'
                AND json_extract(payload_json,'$.createdAt') < ?2 AND json_extract(payload_json,'$.updatedAt') >= ?1
             UNION
             SELECT {child} FROM {P}subagents
              WHERE {child} IS NOT NULL AND {started} < ?2 AND ({completed} IS NULL OR {completed} >= ?1)
         )
         SELECT children.thread_id, json_extract(t.payload_json,'$.providerInstanceId')
           FROM children LEFT JOIN {P}threads t USING (thread_id)"),
        // Thread creation times establish coverage. Never titles, messages, or other content.
        first_record: format!("SELECT json_extract(payload_json,'$.createdAt') FROM {P}threads"),
    }
}

fn runs_sql(extra: &str) -> String {
    // Overlapping runs: IDs to find and group calls, provider for attribution.
    // Never the run's input, output, messages, or other content.
    format!("SELECT run_id, thread_id, json_extract(payload_json,'$.providerInstanceId'){extra}
                       FROM {P}runs WHERE requested_at < ?2 AND (completed_at IS NULL OR completed_at >= ?1)")
}

fn call_sql(skim: Option<skim::Mode>, scope: &str, order: &str, extra: &str) -> String {
    if skim == Some(skim::Mode::Head) { return head_calls_with(scope, order, extra); }
    if skim.is_some() { return skim_calls_with(scope, order, extra); }
    // Calls: type and status for counts, timestamps for timing, and safe names for rows.
    // A command input goes straight into t3_programs, which yields only listed names.
    // The exit code is read for commands only, only as an integer, to tell how a command ended.
    // Nothing else of a result is read.
    // Neither statement returns command text or reads outputs, prompts, messages, or paths.
    format!(
        "SELECT type, status, json_extract(payload_json,'$.startedAt'), json_extract(payload_json,'$.completedAt'),
                CASE WHEN type='dynamic_tool' THEN CASE
                    WHEN typeof(json_extract(payload_json,'$.toolName'))='text'
                     AND length(json_extract(payload_json,'$.toolName')) BETWEEN 1 AND 80
                     AND instr(json_extract(payload_json,'$.toolName'),char(0))=0
                     AND json_extract(payload_json,'$.toolName') NOT GLOB '*[^A-Za-z0-9_.:/-]*'
                    THEN json_extract(payload_json,'$.toolName') END END,
                CASE WHEN type='command_execution' THEN t3_programs(json_extract(payload_json,'$.input')) END,
                CASE WHEN type='command_execution' AND json_type(payload_json,'$.exitCode')='integer' THEN json_extract(payload_json,'$.exitCode') END{extra}
         FROM {P}turn_items WHERE {scope}
           AND type IN ('command_execution','dynamic_tool','file_change','file_search','web_search'){order}"
    )
}

// The skim's call query takes period bounds as epoch milliseconds. The payload goes only into t3_call,
// which gives back the start and end, a safe tool name, an integer exit code and listed
// programs. No json_extract, and no WHERE on the payload: t3_call is NULL for a call
// outside the period, and the reader skips it.
fn skim_calls_with(scope: &str, order: &str, extra: &str) -> String {
    format!("SELECT type, status, t3_call(payload_json, type, ?2, ?3){extra}
         FROM {P}turn_items WHERE {scope}
           AND type IN ('command_execution','dynamic_tool','file_change','file_search','web_search'){order}")
}

// LOUPE_SKIM=head (`skim.rs`, "Head reads"): a command's payload goes whole into t3_call,
// since its exit code follows its output; every other call's row goes to t3_call_head, which
// reads only as far into its payload as T3's key order says it must. `typeof` of the payload
// tells it text from a BLOB or NULL without loading the value. Nothing else of the payload is
// selected, and t3_call_head gives back what t3_call would.
#[cfg(test)]
fn head_calls(scope: &str, order: &str) -> String { head_calls_with(scope, order, "") }

fn head_calls_with(scope: &str, order: &str, extra: &str) -> String {
    format!("SELECT type, status, CASE WHEN type='command_execution' THEN t3_call(payload_json, type, ?2, ?3)
                ELSE t3_call_head(rowid, typeof(payload_json), type, ?2, ?3) END{extra}
         FROM {P}turn_items WHERE {scope}
           AND type IN ('command_execution','dynamic_tool','file_change','file_search','web_search'){order}")
}

/// Read a period in one transaction on the supplied connection, using the same skim
/// setting as `load`.
pub fn read_from(db: &Connection, path: &Path, period: &Period) -> Result<Facts, SourceError> {
    read_with(db, path, period, configured_skim())
}

/// `read_from`, with the calls read through `skim.rs` when a mode is given.
pub fn read_with(db: &Connection, path: &Path, period: &Period, skim: Option<skim::Mode>) -> Result<Facts, SourceError> {
    let fail = |e| source_error(path, e);
    db.pragma_update(None, "query_only", true).map_err(fail)?;
    register_call_functions(db, skim).map_err(fail)?;
    db.execute_batch("BEGIN").map_err(fail)?;
    let result = read(db, period, skim);
    let ended = db.execute_batch("ROLLBACK");
    let facts = result.map_err(fail)?;
    ended.map_err(fail)?;
    Ok(facts)
}

fn register_call_functions(db: &Connection, skim: Option<skim::Mode>) -> rusqlite::Result<()> {
    program::register(db)?;
    if let Some(mode) = skim { skim::register(db, mode)?; }
    Ok(())
}

fn source_queries(db: &Connection, skim: Option<skim::Mode>) -> rusqlite::Result<Queries> {
    let mut schema = db.prepare(HAS_COLUMN)?;
    let mut column = |name: &'static str, fallback: &'static str| -> rusqlite::Result<&'static str> {
        Ok(if has_column(&mut schema, "subagents", name)? { name } else { fallback })
    };
    Ok(queries(
        column("child_thread_id", "json_extract(payload_json,'$.childThreadId')")?,
        column("started_at", "json_extract(payload_json,'$.startedAt')")?,
        column("completed_at", "json_extract(payload_json,'$.completedAt')")?,
        skim,
    ))
}

fn read(db: &Connection, period: &Period, skim: Option<skim::Mode>) -> rusqlite::Result<Facts> {
    let sql = source_queries(db, skim)?;
    let (from, to) = (iso(period.start()), iso(period.until()));
    let mut facts = Facts::default();
    let read_calls = call_reader(skim);

    let mut runs = db.prepare(&sql.runs)?;
    let mut calls = db.prepare(&sql.run_calls)?;
    let mut rows = runs.query(params![from, to])?;
    while let Some(r) = rows.next()? {
        let (run, thread): (String, String) = (r.get(0)?, r.get(1)?);
        let provider = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        read_calls(&mut calls, &mut facts.calls, &thread, Some(&run), &provider, &from, &to)?;
    }

    let mut children = db.prepare(&sql.children)?;
    let mut calls = db.prepare(&sql.child_calls)?;
    let mut rows = children.query(params![from, to])?;
    while let Some(r) = rows.next()? {
        let thread: String = r.get(0)?;
        let provider = r.get::<_, Option<String>>(1)?.unwrap_or_default();
        read_calls(&mut calls, &mut facts.calls, &thread, None, &provider, &from, &to)?;
    }

    facts.first_record = first_record(db, &sql.first_record)?;
    Ok(facts)
}

fn first_record(db: &Connection, sql: &str) -> rusqlite::Result<Option<f64>> {
    let mut first_record: Option<f64> = None;
    let mut records = db.prepare(sql)?;
    let mut rows = records.query([])?;
    while let Some(r) = rows.next()? {
        if let Some(created) = stamp(r.get_ref(0)?) {
            first_record = Some(first_record.map_or(created, |first| first.min(created)));
        }
    }
    Ok(first_record)
}

fn stamp(value: ValueRef<'_>) -> Option<f64> {
    let ValueRef::Text(bytes) = value else { return None };
    std::str::from_utf8(bytes).ok().and_then(ms)
}

// The period's bounds as instants: `from` and `to` are `recap::iso` of them, which `ms` reads back.
fn instants(from: &str, to: &str) -> rusqlite::Result<(f64, f64)> {
    ms(from).zip(ms(to)).ok_or_else(|| rusqlite::Error::InvalidParameterName("period".into()))
}

fn start_in_bounds(start: f64, (lo, hi): (f64, f64)) -> bool { lo <= start && start < hi }

fn read_calls(q: &mut Statement<'_>, calls: &mut Vec<Call>, thread: &str, run: Option<&str>, provider: &str, from: &str, to: &str) -> rusqlite::Result<usize> {
    let provider = provider_label(provider);
    let (lo, hi) = instants(from, to)?;
    let mut rows = q.query(params![run.unwrap_or(thread)])?;
    let mut read = 0;
    while let Some(r) = rows.next()? {
        read += 1;
        if let Some(call) = decode_sqlite(r, thread, run, &provider, (lo, hi))? { calls.push(call); }
    }
    Ok(read)
}

fn decode_sqlite(r: &SqlRow<'_>, thread: &str, run: Option<&str>, provider: &str, bounds: (f64, f64)) -> rusqlite::Result<Option<Call>> {
    let Some(start) = stamp(r.get_ref(2)?).filter(|&start| start_in_bounds(start, bounds)) else { return Ok(None) };
    let item_type: String = r.get(0)?;
    let recorded_name: Option<String> = r.get(4)?;
    let (server, name) = recorded_name.map_or((None, None), split_name);
    let kind = kind_of(&item_type, server.as_deref(), name.as_deref());
    let words: Option<String> = r.get(5)?;
    let programs: Vec<_> = words.as_deref().unwrap_or("").split_whitespace().filter_map(program::listed).collect();
    Ok(Some(Call {
        kind, name, server, label: program::label(&programs), programs, start, end: stamp(r.get_ref(3)?),
        failed: r.get::<_, Option<String>>(1)?.as_deref() == Some("failed"),
        provider: provider.into(), thread: thread.into(), run: run.map(str::to_owned),
        exit: match r.get_ref(6)? { ValueRef::Integer(n) => Some(n), _ => None },
    }))
}

type ReadCalls = fn(&mut Statement<'_>, &mut Vec<Call>, &str, Option<&str>, &str, &str, &str) -> rusqlite::Result<usize>;

fn call_reader(skim: Option<skim::Mode>) -> ReadCalls {
    if skim.is_some() { read_skim_calls } else { read_calls }
}

// `read_calls` for the skim's query: the same Call from t3_call's five fields.
fn read_skim_calls(q: &mut Statement<'_>, calls: &mut Vec<Call>, thread: &str, run: Option<&str>, provider: &str, from: &str, to: &str) -> rusqlite::Result<usize> {
    let provider = provider_label(provider);
    let (lo, hi) = instants(from, to)?;
    let mut rows = q.query(params![run.unwrap_or(thread), lo, hi])?;
    let mut read = 0;
    while let Some(r) = rows.next()? {
        read += 1;
        if let Some(call) = decode_skim(r, thread, run, &provider)?.filter(|call| start_in_bounds(call.start, (lo, hi))) {
            calls.push(call);
        }
    }
    Ok(read)
}

fn decode_skim(r: &SqlRow<'_>, thread: &str, run: Option<&str>, provider: &str) -> rusqlite::Result<Option<Call>> {
    let garbled = || rusqlite::Error::InvalidColumnType(2, "t3_call".into(), rusqlite::types::Type::Text);
    let ValueRef::Text(fields) = r.get_ref(2)? else { return Ok(None) };
    let mut fields = std::str::from_utf8(fields).map_err(|_| garbled())?.split(skim::SEPARATOR);
    let mut next = || fields.next().filter(|field| !field.is_empty());
    let start: f64 = next().and_then(|field| field.parse().ok()).ok_or_else(garbled)?;
    let end = next().map(|field| field.parse::<f64>().map_err(|_| garbled())).transpose()?;
    let recorded_name = next().map(str::to_owned);
    let exit = next().map(|field| field.parse::<i64>().map_err(|_| garbled())).transpose()?;
    let words = next();
    let item_type: String = r.get(0)?;
    let (server, name) = recorded_name.map_or((None, None), split_name);
    let kind = kind_of(&item_type, server.as_deref(), name.as_deref());
    let programs: Vec<_> = words.unwrap_or("").split_whitespace().filter_map(program::listed).collect();
    Ok(Some(Call {
        kind, name, server, label: program::label(&programs), programs, start, end,
        failed: r.get::<_, Option<String>>(1)?.as_deref() == Some("failed"),
        provider: provider.into(), thread: thread.into(), run: run.map(str::to_owned), exit,
    }))
}

/// The kind of a call. T3's own item type decides it for four of the five types. A named tool
/// is a read or a search only when a provider's own tool has exactly one of these names
/// (the dashboard's list in `model::category`, compared the same way); every other named
/// tool is a tool, whatever its name contains. The dashboard's looser rule draws a glyph and
/// may guess; a report that counts by kind may not (`ToolSearch` is not a web search).
fn kind_of(item_type: &str, server: Option<&str>, name: Option<&str>) -> Kind {
    match item_type {
        "command_execution" => Kind::Shell,
        "file_change" => Kind::Edit,
        "file_search" => Kind::Search,
        "web_search" => Kind::Web,
        _ if server.is_some() => Kind::Tool,
        _ => match name.unwrap_or("").to_lowercase().as_str() {
            "read" | "view" | "view_image" | "notebookread" => Kind::Read,
            "glob" | "grep" | "ls" => Kind::Search,
            _ => Kind::Tool,
        },
    }
}

fn split_name(recorded: String) -> (Option<String>, Option<String>) {
    let parts = if let Some(rest) = recorded.strip_prefix("mcp__") {
        rest.split_once("__")
    } else {
        recorded.rsplit_once('.')
    };
    match parts {
        Some((server, name)) if !server.is_empty() && !name.is_empty() => (Some(server.into()), Some(name.into())),
        _ => (None, Some(recorded)),
    }
}

pub fn load(path: &Path, period: &Period, local: Local) -> Result<Tools, SourceError> {
    let enabled = cache_enabled(std::env::var("LOUPE_CACHE").ok().as_deref(),
                                std::env::var("LOUPE_NO_CACHE").ok().as_deref());
    let measure = std::env::var("LOUPE_CACHE_STATS").ok().as_deref() == Some("1");
    let result = load_file(path, period, enabled, None, measure);
    if measure {
        let stats = result.as_ref().ok().and_then(|(_, stats)| *stats);
        eprintln!("{}", cache_diagnostic(stats));
        if let Some(stats) = stats { eprintln!("{}", cache_read_diagnostic(stats.rows)); }
    }
    let (facts, _) = result?;
    Ok(summarize(&facts, period, local))
}

fn load_file(path: &Path, period: &Period, enabled: bool, store: Option<&Path>, measure: bool) -> Result<(Facts, Option<CacheStats>), SourceError> {
    let db = recap::open(path)?;
    let workers = fanout::configured_workers();
    #[cfg(unix)]
    if enabled {
        return read_cached(&db, path, period, configured_skim(), CacheOptions { store, workers, measure });
    }
    #[cfg(not(unix))]
    let _ = (enabled, store, measure);
    let facts = if workers == 1 {
        read_from(&db, path, period)?
    } else {
        read_parallel(db, path, period, workers, configured_skim())?
    };
    Ok((facts, None))
}

#[derive(Clone, Copy, Default)]
struct CacheStats {
    // All finished runs offered to the store, including those with open calls that
    // can never contribute to either hits or writes.
    finished: usize,
    hits: usize,
    written: usize,
    rows: LiveRows,
    #[cfg(unix)]
    reread: usize,
}

#[derive(Clone, Copy, Default)]
struct LiveRows {
    by_ordinal: usize,
    outside: usize,
    children: usize,
    other: usize,
    ordinal_time: Duration,
}

#[cfg(unix)]
impl LiveRows {
    fn add(&mut self, more: Self) {
        self.by_ordinal += more.by_ordinal;
        self.outside += more.outside;
        self.children += more.children;
        self.other += more.other;
        self.ordinal_time += more.ordinal_time;
    }
}

fn cache_diagnostic(stats: Option<CacheStats>) -> String {
    match stats {
        Some(s) => format!("loupe: cache: {} of {} finished runs from the cache, {} written", s.hits, s.finished, s.written),
        None => "loupe: cache: not used".into(),
    }
}

fn cache_read_diagnostic(rows: LiveRows) -> String {
    format!("loupe: cache: {} named-tool rows by ordinal ({} outside the period), {} no-run rows, {} other run rows, {:.3} ms in ordinal reads (sum across workers)",
        rows.by_ordinal, rows.outside, rows.children, rows.other, rows.ordinal_time.as_secs_f64() * 1000.)
}

#[cfg(unix)]
fn probe_cache<'db>(db: &'db Connection, path: &Path, store: Option<&Path>) -> Option<crate::cache::Snapshot<'db>> {
    use crate::cache::Snapshot;
    match store {
        Some(store) => Snapshot::probe_with_store(db, path, store.to_owned()),
        None => Snapshot::probe(db, path),
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct CacheOptions<'a> {
    store: Option<&'a Path>,
    workers: usize,
    measure: bool,
}

#[cfg(unix)]
fn read_cached(db: &Connection, path: &Path, period: &Period, skim: Option<skim::Mode>, options: CacheOptions<'_>) -> Result<(Facts, Option<CacheStats>), SourceError> {
    read_cached_before_workers(db, path, period, skim, options, || {})
}

#[cfg(unix)]
fn read_cached_before_workers(db: &Connection, path: &Path, period: &Period, skim: Option<skim::Mode>,
                              options: CacheOptions<'_>, before_workers: impl FnOnce()) -> Result<(Facts, Option<CacheStats>), SourceError> {
    let fail = |e| source_error(path, e);
    register_call_functions(db, skim).map_err(fail)?;
    let probe = probe_cache(db, path, options.store);
    let transaction = db.unchecked_transaction().map_err(fail)?;
    let result = match probe.and_then(|probe| probe.open(db)) {
        Some(mut cache) => read_with_cache(db, path, period, skim, &mut cache, options, before_workers)
            .map(|(facts, stats)| (facts, Some(stats))),
        None => read(db, period, skim).map(|facts| (facts, None)),
    };
    let ended = transaction.rollback();
    let result = result.map_err(fail)?;
    ended.map_err(fail)?;
    Ok(result)
}

#[cfg(unix)]
struct CacheScope {
    source: CallScope,
    finished: bool,
    entry: Option<Vec<u8>>,
}

#[cfg(unix)]
struct ScopeRead {
    calls: Vec<Call>,
    entry: Option<Vec<u8>>,
    hit: bool,
    rows: LiveRows,
}

#[cfg(unix)]
struct CachedReader<'a> {
    sql: &'a Queries,
    all_sql: String,
    named_sql: String,
    from: &'a str,
    to: &'a str,
    period: &'a Period,
    skim: Option<skim::Mode>,
    options: CacheOptions<'a>,
    token: crate::cache::SourceToken,
}

#[cfg(unix)]
impl CachedReader<'_> {
    fn read_on(&self, db: &Connection, scope: &CacheScope) -> rusqlite::Result<ScopeRead> {
        let source = &scope.source;
        if let Some(rows) = scope.entry.as_deref().and_then(|entry| run_entry::decode(entry, self.skim, KNOWN_TOOLS)) {
            if let Ok((calls, rows)) = read_cache_hit(db, &self.named_sql, rows, source, self.period,
                                                     self.skim, KNOWN_TOOLS, self.options.measure) {
                return Ok(ScopeRead { calls, entry: None, hit: true, rows });
            }
        }
        if scope.finished {
            // Decode the complete run before applying the period. An error from
            // the optional fill must still give the normal live read a chance.
            let ordinal_column = if self.skim.is_some() { 3 } else { 7 };
            if let Ok((calls, entry, read)) = read_cache_miss(db, &self.all_sql, ordinal_column, source,
                                                             self.period, self.skim, KNOWN_TOOLS) {
                let rows = LiveRows { other: read, ..LiveRows::default() };
                return Ok(ScopeRead { calls, entry, hit: false, rows });
            }
        }
        let child = source.run.is_none();
        let mut stmt = db.prepare_cached(if child { &self.sql.child_calls } else { &self.sql.run_calls })?;
        let mut calls = Vec::new();
        let read = call_reader(self.skim)(&mut stmt, &mut calls, &source.thread,
            source.run.as_deref(), &source.provider, self.from, self.to)?;
        let rows = if child { LiveRows { children: read, ..LiveRows::default() } }
            else { LiveRows { other: read, ..LiveRows::default() } };
        Ok(ScopeRead { calls, entry: None, hit: false, rows })
    }
}

#[cfg(unix)]
impl fanout::Reader for CachedReader<'_> {
    type Scope = CacheScope;
    type Output = (bool, rusqlite::Result<ScopeRead>);
    type Prepared<'db> = (&'db Connection, bool);

    fn pin<'db>(&self, db: &'db Connection, path: &Path) -> rusqlite::Result<(rusqlite::Transaction<'db>, bool)> {
        let probe = probe_cache(db, path, self.options.store);
        let tx = db.unchecked_transaction()?;
        tx.query_row(fanout::PIN_SNAPSHOT, [], |_| Ok(()))?;
        let pinned = probe.is_some_and(|probe| probe.matches(db, &self.token));
        Ok((tx, pinned))
    }

    fn prepare<'db>(&self, db: &'db Connection, pinned: bool) -> rusqlite::Result<Self::Prepared<'db>> {
        register_call_functions(db, self.skim)?;
        Ok((db, pinned))
    }

    fn read(&self, prepared: &mut Self::Prepared<'_>, scope: &CacheScope) -> rusqlite::Result<Self::Output> {
        let (db, pinned) = *prepared;
        Ok((pinned, self.read_on(db, scope)))
    }
}

#[cfg(unix)]
fn read_with_cache(db: &Connection, path: &Path, period: &Period, skim: Option<skim::Mode>,
                   cache: &mut crate::cache::Cache<'_>, options: CacheOptions<'_>,
                   before_workers: impl FnOnce()) -> rusqlite::Result<(Facts, CacheStats)> {
    let sql = source_queries(db, skim)?;
    let (from, to) = (iso(period.start()), iso(period.until()));
    let mut runs = db.prepare(&runs_sql(", completed_at IS NOT NULL"))?;
    let mut scopes = runs.query_map(params![from, to], |r| {
        Ok(CacheScope {
            source: CallScope { run: Some(r.get(0)?), thread: r.get(1)?,
                provider: r.get::<_, Option<String>>(2)?.unwrap_or_default() },
            finished: r.get(3)?, entry: None,
        })
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let finished: Vec<_> = scopes.iter().filter(|scope| scope.finished)
        .filter_map(|scope| scope.source.run.as_deref()).collect();
    let mut hits = cache.lookup_many(run_entry::KIND, run_entry::VERSION, &finished);
    let mut stats = CacheStats { finished: finished.len(), ..CacheStats::default() };
    for scope in &mut scopes {
        scope.entry = scope.source.run.as_deref().and_then(|id| hits.remove(id));
    }

    let mut children = db.prepare(&sql.children)?;
    let children = children.query_map(params![from, to], |r| {
        Ok(CacheScope { source: CallScope { thread: r.get(0)?, run: None,
            provider: r.get::<_, Option<String>>(1)?.unwrap_or_default() }, finished: false, entry: None })
    })?;
    for child in children { scopes.push(child?); }

    let reader = CachedReader { sql: &sql, from: &from, to: &to, period, skim, options,
        all_sql: call_sql(skim, "run_id=?1", " ORDER BY ordinal, rowid", ", ordinal"),
        named_sql: named_call_sql(skim), token: cache.source_token() };
    let mut worker_results = if options.workers > 1 && !scopes.is_empty() {
        before_workers();
        fanout::read_scopes(path, options.workers, &scopes, recap::open, &reader)
            .ok().map(Vec::into_iter)
    } else { None };

    let mut entries = Vec::new();
    let mut facts = Facts::default();
    for scope in &scopes {
        let read = match worker_results.as_mut().and_then(Iterator::next) {
            Some((true, Ok(read))) => read,
            _ => {
                // Includes workers that could not prove the main snapshot and
                // any fan-out startup error. Only the main read can contribute.
                if options.workers > 1 { stats.reread += 1; }
                reader.read_on(db, scope)?
            }
        };
        if let (Some(id), Some(entry)) = (scope.source.run.as_deref(), read.entry) {
            entries.push((id, entry));
        }
        facts.calls.extend(read.calls);
        stats.hits += usize::from(read.hit);
        stats.rows.add(read.rows);
    }
    facts.first_record = first_record(db, &sql.first_record)?;
    stats.written = cache.write_many(run_entry::KIND, run_entry::VERSION, &entries);
    Ok((facts, stats))
}

#[cfg(unix)]
fn named_call_sql(skim: Option<skim::Mode>) -> String {
    let scope = if skim.is_some() { "run_id=?1 AND ordinal=?4 AND type='dynamic_tool'" }
        else { "run_id=?1 AND ordinal=?2 AND type='dynamic_tool'" };
    let order = if skim.is_some() { " ORDER BY rowid LIMIT 1 OFFSET ?5" } else { " ORDER BY rowid LIMIT 1 OFFSET ?3" };
    call_sql(skim, scope, order, "")
}

#[cfg(unix)]
fn decode_call(skim: Option<skim::Mode>, row: &SqlRow<'_>, scope: &CallScope, provider: &str,
               bounds: (f64, f64)) -> rusqlite::Result<Option<Call>> {
    if skim.is_some() { decode_skim(row, &scope.thread, scope.run.as_deref(), provider) }
    else { decode_sqlite(row, &scope.thread, scope.run.as_deref(), provider, bounds) }
}

#[cfg(unix)]
fn within_period(start: f64, period: &Period) -> bool {
    start_in_bounds(start, (period.start(), period.until()))
}

#[cfg(unix)]
fn read_cache_miss(db: &Connection, sql: &str, ordinal_column: usize, scope: &CallScope, period: &Period,
                   skim: Option<skim::Mode>, known: &[&str]) -> rusqlite::Result<(Vec<Call>, Option<Vec<u8>>, usize)> {
    let provider = provider_label(&scope.provider);
    let mut stmt = db.prepare_cached(sql)?;
    let bounds = (f64::NEG_INFINITY, f64::INFINITY);
    let mut rows = if skim.is_some() { stmt.query(params![scope.run, bounds.0, bounds.1])? }
        else { stmt.query(params![scope.run])? };
    let mut decoded = Vec::new();
    let mut previous = None;
    let mut next_slot = 0u32;
    let mut read = 0;
    while let Some(row) = rows.next()? {
        read += 1;
        let ordinal = row.get(ordinal_column)?;
        if previous != Some(ordinal) { next_slot = 0; }
        previous = Some(ordinal);
        // Offsets count every dynamic-tool item at this ordinal, even one
        // whose start is unreadable and therefore does not produce a call.
        let slot = if matches!(row.get_ref(0)?, ValueRef::Text(b"dynamic_tool")) {
            let slot = next_slot;
            next_slot = next_slot.checked_add(1).ok_or(rusqlite::Error::InvalidQuery)?;
            Some(slot)
        } else { None };
        if let Some(call) = decode_call(skim, row, scope, &provider, bounds)? {
            decoded.push((ordinal, slot, call));
        }
    }
    let entry = run_entry::encode(decoded.iter().map(|(ordinal, named, call)| (*ordinal, *named, call)), skim, known);
    let calls = decoded.into_iter().map(|(_, _, call)| call).filter(|c| within_period(c.start, period)).collect();
    Ok((calls, entry, read))
}

#[cfg(unix)]
fn read_cache_hit(db: &Connection, sql: &str, rows: Vec<(i64, run_entry::Row)>, scope: &CallScope, period: &Period,
                  skim: Option<skim::Mode>, known: &[&str], measure: bool) -> rusqlite::Result<(Vec<Call>, LiveRows)> {
    let provider = provider_label(&scope.provider);
    let bounds = (f64::NEG_INFINITY, f64::INFINITY);
    let mut live = LiveRows::default();
    let calls = merge_cached_rows(rows, scope, period, &provider, known, |ordinal, slot| {
        let started = measure.then(std::time::Instant::now);
        let mut stmt = db.prepare_cached(sql)?;
        let mut rows = if skim.is_some() { stmt.query(params![scope.run, bounds.0, bounds.1, ordinal, slot])? }
            else { stmt.query(params![scope.run, ordinal, slot])? };
        // Named records came from a usable item of this exact source generation.
        // A missing or unreadable row makes the whole entry a miss.
        let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let call = decode_call(skim, row, scope, &provider, bounds)?.ok_or(rusqlite::Error::InvalidQuery)?;
        live.by_ordinal += 1;
        live.outside += usize::from(!within_period(call.start, period));
        if let Some(started) = started { live.ordinal_time += started.elapsed(); }
        Ok(Some(call))
    })?;
    Ok((calls, live))
}

#[cfg(unix)]
fn merge_cached_rows(rows: Vec<(i64, run_entry::Row)>, scope: &CallScope, period: &Period, provider: &str, known: &[&str],
                     mut read_named: impl FnMut(i64, u32) -> rusqlite::Result<Option<Call>>) -> rusqlite::Result<Vec<Call>> {
    let mut calls = Vec::new();
    for (ordinal, entry) in rows {
        match entry {
            run_entry::Row::Named(slot) => {
                if let Some(call) = read_named(ordinal, slot)?.filter(|call| within_period(call.start, period)) {
                    calls.push(call);
                }
            }
            run_entry::Row::Stored { kind, start, end, failed, exit, programs, name } if within_period(start, period) => {
                let name = name.map(|index| known.get(usize::from(index)).map(|name| (*name).to_owned())
                    .ok_or(rusqlite::Error::InvalidQuery)).transpose()?;
                calls.push(Call { kind, name, server: None, label: program::label(&programs), programs,
                    start, end: Some(end), failed, provider: provider.into(), thread: scope.thread.clone(),
                    run: scope.run.clone(), exit });
            }
            run_entry::Row::Stored { .. } => {}
        }
    }
    Ok(calls)
}

fn configured_skim() -> Option<skim::Mode> {
    skim::choice(std::env::var(skim::VARIABLE).ok().as_deref())
}

struct CallScope {
    thread: String,
    run: Option<String>,
    provider: String,
}

fn call_scopes(db: &Connection, sql: &Queries, from: &str, to: &str) -> rusqlite::Result<Vec<CallScope>> {
    let mut scopes = Vec::new();
    let mut runs = db.prepare(&sql.runs)?;
    let mut rows = runs.query(params![from, to])?;
    while let Some(r) = rows.next()? {
        scopes.push(CallScope { run: Some(r.get(0)?), thread: r.get(1)?, provider: r.get::<_, Option<String>>(2)?.unwrap_or_default() });
    }
    let mut children = db.prepare(&sql.children)?;
    let mut rows = children.query(params![from, to])?;
    while let Some(r) = rows.next()? {
        scopes.push(CallScope { run: None, thread: r.get(0)?, provider: r.get::<_, Option<String>>(1)?.unwrap_or_default() });
    }
    Ok(scopes)
}

struct CallReader<'a> {
    sql: &'a Queries,
    from: &'a str,
    to: &'a str,
    skim: Option<skim::Mode>,
    read_calls: ReadCalls,
}

impl fanout::Reader for CallReader<'_> {
    type Scope = CallScope;
    type Output = Vec<Call>;
    type Prepared<'db> = (Statement<'db>, Statement<'db>);

    fn prepare<'db>(&self, db: &'db Connection, _pinned: bool) -> rusqlite::Result<Self::Prepared<'db>> {
        register_call_functions(db, self.skim)?;
        Ok((db.prepare(&self.sql.run_calls)?, db.prepare(&self.sql.child_calls)?))
    }

    fn read(&self, (runs, children): &mut Self::Prepared<'_>, scope: &CallScope) -> rusqlite::Result<Vec<Call>> {
        let q = if scope.run.is_some() { runs } else { children };
        let mut calls = Vec::new();
        (self.read_calls)(q, &mut calls, &scope.thread, scope.run.as_deref(), &scope.provider, self.from, self.to)?;
        Ok(calls)
    }
}

// The lists and coverage use one snapshot, separate from the workers' snapshots
// (see fanout). T3 can commit after the list read, so the report can mix states.
fn read_parallel(db: Connection, path: &Path, period: &Period, workers: usize, skim: Option<skim::Mode>) -> Result<Facts, SourceError> {
    let fail = |e| source_error(path, e);
    program::register(&db).map_err(fail)?;
    let (from, to) = (iso(period.start()), iso(period.until()));
    let lists = db.unchecked_transaction().map_err(fail)?;
    let sql = source_queries(&lists, skim).map_err(fail)?;
    let scopes = call_scopes(&lists, &sql, &from, &to).map_err(fail)?;
    let first_record = first_record(&lists, &sql.first_record).map_err(fail)?;
    lists.rollback().map_err(fail)?;
    drop(db);

    let reader = CallReader { sql: &sql, from: &from, to: &to, skim, read_calls: call_reader(skim) };
    let batches = fanout::read_scopes(path, workers, &scopes, recap::open, &reader).map_err(|e| match e {
        fanout::Error::Open(error) => error,
        fanout::Error::Sqlite(error) => fail(error),
        fanout::Error::Spawn(error) => SourceError {
            kind: "schema", path: path.to_owned(), detail: format!("could not start a Tools worker: {error}"),
        },
    })?;
    Ok(Facts { calls: batches.into_iter().flatten().collect(), first_record })
}

// ── The report ──────────────────────────────────────────────────────────────

fn buckets() -> Vec<Bucket> {
    ["under 1s", "1-10s", "10-60s", "1-10min", "over 10min"].into_iter()
        .map(|label| Bucket { label: label.into(), ..Bucket::default() }).collect()
}

fn add_length(buckets: &mut [Bucket], length: f64) {
    let index = [1000., 10_000., 60_000., 600_000.].partition_point(|&edge| edge <= length);
    buckets[index].calls += 1;
    buckets[index].ms += length;
}

#[derive(Default)]
struct RowStats<'a> {
    lengths: Vec<f64>,
    providers: BTreeMap<&'a str, Share>,
    open: u32,
    closed_together: u32,
    failed: u32,
    chained: u32,
    appears_in: u32,
    exits: Exits,
}

impl<'a> RowStats<'a> {
    fn add(&mut self, call: &'a Call, closed: bool) {
        if call.kind == Kind::Shell { self.exits.add(call.exit, call.failed); }
        let length = if closed { None } else { call.length() };
        self.closed_together += u32::from(closed);
        if let Some(length) = length {
            self.lengths.push(length);
        } else if !closed {
            self.open += 1;
        }
        self.failed += u32::from(call.failed);
        let entry = self.providers.entry(&call.provider).or_default();
        entry.calls += 1;
        entry.ms += length.unwrap_or(0.);
        entry.failed += u32::from(call.failed);
        entry.closed_together += u32::from(closed);
    }

    fn finish(mut self, kind: Kind, server: Option<&str>, name: Option<&str>) -> Row {
        self.lengths.sort_by(f64::total_cmp);
        let n = self.lengths.len();
        let median_ms = (n > 0).then(|| (self.lengths[(n - 1) / 2] + self.lengths[n / 2]) / 2.);
        let mut lengths = buckets();
        for &length in &self.lengths {
            add_length(&mut lengths, length);
        }
        let mut by_provider: Vec<_> = self.providers.into_iter().map(|(provider, share)| Share { provider: provider.into(), ..share }).collect();
        by_provider.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.provider.cmp(&b.provider)));
        Row {
            kind, name: name.map(str::to_owned), server: server.map(str::to_owned),
            calls: n as u32 + self.open + self.closed_together, ms: self.lengths.iter().fold(0., |sum, length| sum + length), failed: self.failed, open: self.open, closed_together: self.closed_together,
            longest_ms: self.lengths.last().copied().unwrap_or(0.), median_ms, lengths,
            chained: self.chained, appears_in: self.appears_in, by_provider, exits: self.exits,
        }
    }
}

fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| b.ms.total_cmp(&a.ms).then_with(|| b.calls.cmp(&a.calls))
        .then_with(|| a.name.is_none().cmp(&b.name.is_none()))
        .then_with(|| a.name.cmp(&b.name)).then_with(|| a.kind.cmp(&b.kind)).then_with(|| a.server.cmp(&b.server)));
}

// Copies recap::merged's interval union, without clipping: a call's entire length belongs
// to its starting period. The caller passes intervals from one thread at a time.
fn busy(mut spans: Vec<(f64, f64)>) -> f64 {
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut pieces: Vec<(f64, f64)> = vec![];
    for (start, end) in spans {
        match pieces.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => pieces.push((start, end)),
        }
    }
    pieces.into_iter().fold(0., |sum, (start, end)| sum + (end - start))
}

fn when(local: Local, stamp: f64, span: Span) -> String {
    let seconds = (stamp / 1000.).floor() as i64;
    let (calendar, day, hour, minute) = if let Some(t) = local(seconds) {
        (format!("{:04}-{:02}-{:02}T00:00:00.000Z", t.year, t.month0 + 1, t.day), t.day, t.hour, t.minute)
    } else {
        let text = iso(stamp);
        let n = |r: std::ops::Range<usize>| text[r].parse::<i32>().unwrap_or(0);
        (iso((stamp / (24. * HOUR)).floor() * 24. * HOUR), n(8..10), n(11..13), n(14..16))
    };
    // Epoch day zero is Thursday. Use the local calendar date, even in a non-UTC zone.
    let epoch_day = (ms(&calendar).unwrap_or(0.) / (24. * HOUR)) as i64;
    let weekday = DAYS[(epoch_day + 3).rem_euclid(7) as usize];
    match span {
        Span::Week => format!("{weekday} {hour:02}:{minute:02}"),
        Span::Month => format!("{weekday} {day}, {hour:02}:{minute:02}"),
    }
}

pub fn summarize(facts: &Facts, period: &Period, local: Local) -> Tools {
    let hi = period.until();
    let days_elapsed = period.days.windows(2).filter(|d| d[0] < hi).count() as u32;
    let days_recorded = facts.first_record.map_or(0, |first| {
        period.days.windows(2).filter(|d| d[0] < hi && d[1] > first).count() as u32
    });
    let mut report = Tools {
        period: period.span, from: local_date(local, period.start()), to: local_date(local, period.last_day()),
        so_far: period.so_far, days: period.len() as u32, days_elapsed, days_recorded,
        lengths: buckets(), min_for_rate: MIN_FOR_RATE,
        commands: Commands { listed: program::PROGRAMS.len() as u32, ..Commands::default() },
        ..Tools::default()
    };
    let mut kinds: BTreeMap<_, RowStats<'_>> = BTreeMap::new();
    let mut programs: BTreeMap<_, RowStats<'_>> = BTreeMap::new();
    let mut tools: BTreeMap<_, RowStats<'_>> = BTreeMap::new();
    let mut providers = BTreeMap::new();
    let mut threads: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    let mut ended = vec![];
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, call) in facts.calls.iter().enumerate() {
        if call.length().is_some() { groups.entry(&call.thread).or_default().push(i); }
    }
    let mut closed = vec![false; facts.calls.len()];
    for indices in groups.values_mut() {
        indices.sort_by(|&a, &b| facts.calls[a].end.unwrap().total_cmp(&facts.calls[b].end.unwrap()));
        let mut first = 0;
        while first < indices.len() {
            let mut last = first + 1;
            let end = facts.calls[indices[first]].end.unwrap();
            while last < indices.len() && facts.calls[indices[last]].end.unwrap() == end { last += 1; }
            let starts = &indices[first..last];
            let min = starts.iter().map(|&i| facts.calls[i].start).fold(f64::INFINITY, f64::min);
            let max = starts.iter().map(|&i| facts.calls[i].start).fold(f64::NEG_INFINITY, f64::max);
            if starts.len() >= 2 && max - min > 1000. { for &i in starts { closed[i] = true; } }
            first = last;
        }
    }
    for (i, call) in facts.calls.iter().enumerate() {
        let closed = closed[i];
        let length = if closed { None } else { call.length() };
        let time = length.unwrap_or(0.);
        let failed = u32::from(call.failed);
        report.calls += 1;
        report.ms += time;
        report.failed += failed;
        report.open += u32::from(call.length().is_none());
        report.closed_together += u32::from(closed);
        report.without_run += u32::from(call.run.is_none());
        kinds.entry(call.kind).or_default().add(call, closed);
        let spans = threads.entry(&call.thread).or_default();
        if let Some(length) = length {
            spans.push((call.start, call.start + length));
            add_length(&mut report.lengths, length);
            ended.push((call, length));
        }

        let provider = providers.entry(call.provider.as_str()).or_insert_with(|| Provider { provider: call.provider.clone(), ..Provider::default() });
        provider.calls += 1;
        provider.ms += time;
        provider.failed += failed;
        provider.closed_together += u32::from(closed);
        if call.kind == Kind::Shell {
            let chained = u32::from(call.programs.len() >= 2);
            let commands = &mut report.commands;
            commands.exits.add(call.exit, call.failed);
            provider.exits.add(call.exit, call.failed);
            commands.calls += 1;
            commands.ms += time;
            commands.failed += failed;
            commands.chained += chained;
            if call.programs.is_empty() {
                commands.unnamed += 1;
                commands.unnamed_ms += time;
            } else {
                commands.named += 1;
            }
            provider.commands += 1;
            provider.commands_failed += failed;
            let row = programs.entry(call.label).or_default();
            row.add(call, closed);
            row.chained += chained;
            for &program in &call.programs {
                programs.entry(Some(program)).or_default().appears_in += 1;
            }
        } else {
            tools.entry((call.kind, call.server.as_deref(), call.name.as_deref())).or_default().add(call, closed);
        }
    }

    report.threads = threads.len() as u32;
    report.busy_ms = threads.into_values().fold(0., |sum, spans| sum + busy(spans));
    report.kinds = kinds.into_iter().map(|(kind, stats)| stats.finish(kind, None, None)).collect();
    report.programs = programs.into_iter().map(|(name, stats)| stats.finish(Kind::Shell, None, name)).collect();
    report.tools = tools.into_iter().map(|((kind, server, name), stats)| stats.finish(kind, server, name)).collect();
    sort_rows(&mut report.kinds);
    sort_rows(&mut report.programs);
    sort_rows(&mut report.tools);
    report.providers = providers.into_values().collect();
    report.providers.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.provider.cmp(&b.provider)));
    ended.sort_by(|(a, a_ms), (b, b_ms)| b_ms.total_cmp(a_ms).then_with(|| a.start.total_cmp(&b.start)));
    report.longest = ended.into_iter().take(10).map(|(call, length)| Long {
        kind: call.kind, name: call.name().map(str::to_owned), server: call.server.clone(),
        ms: length, failed: call.failed, provider: call.provider.clone(), thread: call.thread.clone(),
        run: call.run.clone(), started: call.start, when: when(local, call.start, period.span),
        exit: if call.kind == Kind::Shell { call.exit } else { None },
    }).collect();
    report
}

// ── A made-up week or month ──────────────────────────────────────────────────

pub fn demo(period: &Period, local: Local) -> Result<Tools, SourceError> {
    let path = Path::new(":memory:");
    let fail = |e| source_error(path, e);
    let db = Connection::open_in_memory().map_err(fail)?;
    demo_fill(&db, period).map_err(fail)?;
    Ok(summarize(&read_with(&db, path, period, skim::choice(None))?, period, local))
}

// Copies recap::chance for a repeatable draw in [0, 1).
fn chance(seed: u64) -> f64 {
    let mut h = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0xd1b5_4a32_d192_ed03);
    h ^= h >> 31;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 29;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn demo_command(label: Option<&str>, style: f64) -> (&'static str, Option<&'static str>) {
    const DEMO_LINES: usize = 58;
    let mut forms = LINES[..DEMO_LINES].iter().filter(|row| row.2 == label);
    // Give pipelines and chained commands most of the draws where both forms exist.
    let (chained, draw) = if style < 0.64 { (true, style / 0.64) } else { (false, (style - 0.64) / 0.36) };
    let mut preferred = forms.clone().filter(|row| (row.1.len() > 1) == chained);
    let count = preferred.clone().count();
    let &(line, _, label) = if count > 0 {
        preferred.nth((draw * count as f64) as usize).unwrap()
    } else {
        let count = forms.clone().count();
        forms.nth((draw * count as f64) as usize).unwrap()
    };
    (line, label)
}

fn demo_fill(db: &Connection, period: &Period) -> rusqlite::Result<()> {
    use serde_json::json;
    const PER_DAY: [usize; 7] = [800, 950, 1000, 1300, 950, 425, 575];
    const LANES: usize = 24;
    // Long calls occupy different slots in the week. Later days draw a new slot and
    // duration from the same mix of commands and waits.
    const FEATURED: &[(usize, usize, usize, Option<&str>, Option<&str>, f64, Option<i64>)] = &[
        (0, 2, 6, Some("python3"), None, 107., Some(124)),
        (0, 9, 25, None, Some("AskUserQuestion"), 73., None),
        (1, 8, 18, Some("cargo"), None, 87., None),
        (1, 17, 33, Some("npm"), None, 66., None),
        (2, 7, 25, Some("pytest"), None, 55., Some(137)),
        (2, 15, 13, None, None, 62., None),
        (3, 8, 41, None, Some("t3-code.t3_thread_wait"), 36., None),
        (4, 10, 10, Some("node"), None, 31., None),
        (5, 5, 13, Some("make"), None, 20., None),
        (6, 12, 20, Some("docker"), None, 16., None),
    ];
    // Command weights in parts per thousand, including commands without a listed program.
    const COMMANDS: &[(Option<&str>, usize)] = &[
        (None, 40), (Some("sed"), 170), (Some("git"), 151), (Some("cat"), 126),
        (Some("grep"), 77), (Some("rg"), 69), (Some("ls"), 55), (Some("python3"), 98),
        (Some("cargo"), 52), (Some("npm"), 30), (Some("pytest"), 19), (Some("node"), 17),
        (Some("make"), 13), (Some("docker"), 8), (Some("gh"), 7), (Some("curl"), 7),
        (Some("ssh"), 4), (Some("jq"), 6), (Some("find"), 6), (Some("awk"), 4),
        (Some("diff"), 7), (Some("uv"), 4), (Some("go"), 3), (Some("tsc"), 3),
        (Some("sleep"), 3), (Some("mkdir"), 3), (Some("cp"), 2), (Some("rm"), 2),
        (Some("echo"), 3), (Some("head"), 3), (Some("tail"), 3), (Some("wc"), 5),
    ];
    // The draw favors the front of this list. A few common tools account for most calls.
    const NAMES: &[&str] = &[
        "Read", "Edit", "t3-code.delegate_task", "t3-code.t3_thread_read", "apply_patch",
        "Grep", "Glob", "Write", "update_plan", "ToolSearch", "view_image", "WebFetch",
        "TodoWrite", "codex_apps.github.fetch", "Skill", "spawn_agent", "TaskStop",
        "t3-code.t3_thread_wait", "AskUserQuestion", "t3-code.request_secret",
        "t3-code.html_render", "mcp__playwright__browser_navigate",
        "mcp__context7__get-library-docs", "run this: \"never read\"",
    ];
    let salt = (chance(60_029) * f64::from(u32::MAX)) as u32;
    let thread_id = |seed| {
        let word = |offset| (chance(seed + offset) * f64::from(u32::MAX)) as u32;
        // Odd multiplication and a fixed salt give each lane a distinct eight-digit suffix.
        let suffix = (seed as u32).wrapping_mul(0x9e37_79b9) ^ salt;
        format!("{:08x}-{:04x}-{:04x}-{:04x}-{:04x}{:08x}",
            word(10_001), word(20_003) >> 16, 0x4000 | (word(30_007) & 0x0fff),
            0x8000 | (word(40_009) & 0x3fff), word(50_021) >> 16, suffix)
    };
    db.execute_batch(DEMO_SCHEMA)?;
    db.execute_batch(&format!("CREATE INDEX demo_items_by_thread ON {P}turn_items(thread_id, run_id); BEGIN"))?;
    let mut threads = db.prepare(&format!("INSERT INTO {P}threads(thread_id,payload_json) VALUES(?1,?2)"))?;
    let mut runs = db.prepare(&format!("INSERT INTO {P}runs(run_id,thread_id,ordinal,payload_json,requested_at) VALUES(?1,?2,1,?3,?4)"))?;
    let mut items = db.prepare(&format!("INSERT INTO {P}turn_items(item_id,thread_id,run_id,type,status,ordinal,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7)"))?;
    let mut item_no = 0;
    for day in 0..period.len() {
        let morning = period.days[day] + 7.5 * HOUR;
        let count = PER_DAY[day % PER_DAY.len()];
        let later = (day >= PER_DAY.len()).then(|| {
            let day = day - PER_DAY.len();
            let seed = day as u64 * 4 + 70_003;
            let lane = (chance(seed) * LANES as f64) as usize;
            let slot = 4 + (chance(seed + 1) * (count / LANES - 4) as f64) as usize;
            let minutes = (41 + (day * 19 % 59)) as f64;
            let (_, _, _, label, tool, _, exit) = FEATURED[day % FEATURED.len()];
            (lane, slot, label, tool, minutes, exit)
        });
        for lane in 0..LANES {
            let seed = (day * LANES + lane) as u64;
            let native = day == 0 && lane == 0;
            let provider = match chance(80_003 + seed) {
                _ if native => "claudeAgent",
                n if n < 0.50 => "claudeAgent",
                n if n < 0.85 => "codex",
                n if n < 0.97 => "opencode",
                _ => "grok",
            };
            let thread = thread_id(seed);
            let run = (!native).then(|| format!("r{seed}"));
            let first = morning + lane as f64 * 60_000.;
            if first >= period.until() {
                break;
            }
            let created = iso(first);
            let lineage = if native { json!({"relationshipToParent": "subagent"}) } else { json!(null) };
            threads.execute(params![thread, json!({"providerInstanceId": provider, "createdAt": created,
                "updatedAt": iso(period.days[day + 1]), "lineage": lineage}).to_string()])?;
            if let Some(run) = &run {
                runs.execute(params![run, thread, json!({"providerInstanceId": provider}).to_string(), created])?;
            }
            for index in (lane..count).step_by(LANES) {
                let slot = index / LANES;
                // Pairs share a start on each thread, with fixed spacing even for a partial day.
                let start = first + (slot / 2) as f64 / count.div_ceil(LANES * 2) as f64 * 12.5 * HOUR;
                if start >= period.until() {
                    break;
                }
                let seed = (day * 2000 + index) as u64 * 8;
                let group = chance(seed + 1);
                let featured = if let Some((lane_at, slot_at, label, tool, minutes, exit)) = later {
                    (lane == lane_at && slot == slot_at).then_some((label, tool, minutes, exit))
                } else {
                    FEATURED.iter().find_map(|&(day_at, lane_at, slot_at, label, tool, minutes, exit)| {
                        (day == day_at && lane == lane_at && slot == slot_at).then_some((label, tool, minutes, exit))
                    })
                };
                let (kind, tool) = if let Some((_, tool, _, _)) = featured {
                    (if tool.is_some() { "dynamic_tool" } else { "command_execution" }, tool)
                } else if native || group < 0.70 {
                    ("command_execution", None)
                } else if group < 0.86 {
                    let name = NAMES[(chance(seed + 2).powi(2) * NAMES.len() as f64) as usize];
                    let name = match name {
                        "AskUserQuestion" | "t3-code.t3_thread_wait" if chance(seed + 4) < 0.5 => "Read",
                        _ => name,
                    };
                    let name = match (provider, name) {
                        ("opencode" | "grok", "Read") => "read",
                        ("codex", "Read") => "view_image",
                        ("claudeAgent", "t3-code.delegate_task") => "mcp__t3-code__delegate_task",
                        ("claudeAgent", "t3-code.t3_thread_read") => "mcp__t3-code__t3_thread_read",
                        ("claudeAgent", "codex_apps.github.fetch") => "mcp__github__fetch",
                        _ => name,
                    };
                    ("dynamic_tool", Some(name))
                } else if group < 0.94 {
                    ("file_change", None)
                } else if group < 0.97 {
                    ("file_search", None)
                } else {
                    ("web_search", None)
                };
                let (input, label) = if kind == "command_execution" {
                    let label = if let Some((label, _, _, _)) = featured { label } else {
                        let mut ticket = (chance(seed + 3) * 1000.) as usize;
                        COMMANDS.iter().find_map(|&(name, weight)| {
                            if ticket < weight { Some(name) } else { ticket -= weight; None }
                        }).unwrap()
                    };
                    let (line, label) = demo_command(label, chance(seed + 4));
                    (Some(line), label)
                } else {
                    (None, None)
                };
                let draw = chance(seed + 5);
                let spread = chance(seed + 6);
                let length = if let Some((_, _, minutes, _)) = featured {
                    minutes * 60_000.
                } else {
                    // A program that runs a long job often also does quick probes.
                    let brief = match (kind, label, tool) {
                        ("command_execution", None, _) | (_, Some("python3"), _) => 0.37,
                        (_, Some("node"), _) => 0.26,
                        (_, Some("npm"), _) => 0.12,
                        (_, Some("go" | "tsc" | "uv"), _) => 0.20,
                        _ => 0.,
                    };
                    // Usual durations in seconds, followed by a slower range for occasional calls.
                    let (usual, slow, cut) = match (kind, label, tool) {
                        ("command_execution", None, _) => ((1., 4.), (150., 300.), 0.88),
                        (_, Some("python3"), _) => ((1., 4.), (190., 490.), 0.85),
                        (_, Some("cargo"), _) => ((18., 48.), (95., 285.), 0.88),
                        (_, Some("npm"), _) => ((6., 21.), (75., 190.), 0.90),
                        (_, Some("pytest"), _) => ((16., 42.), (70., 160.), 0.88),
                        (_, Some("node"), _) => ((1.4, 6.), (80., 150.), 0.90),
                        (_, Some("make"), _) => ((9., 28.), (65., 140.), 0.90),
                        (_, Some("docker"), _) => ((14., 45.), (65., 135.), 0.86),
                        (_, Some("go" | "tsc" | "uv"), _) => ((4., 23.), (30., 75.), 0.93),
                        (_, Some("sleep"), _) => ((5., 5.), (5., 5.), 1.),
                        (_, _, Some("AskUserQuestion")) => ((120., 330.), (330., 590.), 0.94),
                        (_, _, Some("t3-code.t3_thread_wait")) => ((61., 170.), (170., 330.), 0.92),
                        (_, _, Some("t3-code.request_secret")) => ((20., 58.), (60., 150.), 0.85),
                        (_, _, Some("t3-code.delegate_task" | "mcp__t3-code__delegate_task")) => ((0.7, 4.), (4., 12.), 0.87),
                        (_, _, Some("WebFetch")) | ("web_search", _, _) => ((0.3, 2.8), (8., 30.), 0.95),
                        (_, _, Some("view_image" | "mcp__playwright__browser_navigate")) => ((0.08, 1.2), (1.5, 8.), 0.91),
                        ("file_change", _, _) | (_, _, Some("Edit" | "Write" | "apply_patch")) => ((0.04, 0.85), (1., 5.), 0.93),
                        _ => ((0.012, 0.75), (1., 4.), 0.99),
                    };
                    let (low, high) = if draw < brief { (0.04, 0.85) }
                        else if draw >= cut { slow }
                        else { usual };
                    1000. * (low + spread * (high - low))
                };
                let end = (!(day == 0 && index < 2) && start + length.floor() <= period.until()).then(|| iso(start + length.floor()));
                let failure_rate = match label {
                    Some("rg" | "grep") => 1. / 9.,
                    Some("diff") => 0.30,
                    Some("cargo" | "python3" | "pytest" | "npm") => 0.033,
                    _ => 0.020,
                };
                let failed = end.is_some() && match featured {
                    Some((_, _, _, exit)) => exit.is_some(),
                    None => chance(seed + 7) < failure_rate,
                };
                let status = if failed { "failed" } else if end.is_some() { "completed" } else { "running" };
                let has_exit = match provider {
                    "codex" | "opencode" => true,
                    "grok" => chance(seed + 100_001) < 0.9,
                    _ => chance(seed + 100_001) < 0.005,
                };
                let exit = (kind == "command_execution" && has_exit && end.is_some()).then(|| {
                    if !failed { return 0; }
                    if let Some((_, _, _, Some(exit))) = featured { return exit; }
                    let draw = chance(seed.wrapping_mul(37) + 201_959);
                    match label {
                        Some("rg" | "grep") => if draw < 0.98 { 1 } else { 2 },
                        Some("diff") => 1,
                        _ => match draw {
                            n if n < 0.60 => 1,
                            n if n < 0.85 => 2,
                            n if n < 0.875 => 124,
                            n if n < 0.90 => 126,
                            n if n < 0.925 => 127,
                            n if n < 0.95 => 130,
                            n if n < 0.975 => 137,
                            _ => 101,
                        },
                    }
                });
                item_no += 1;
                items.execute(params![format!("i{item_no}"), thread, run, kind, status, item_no,
                    json!({"startedAt": iso(start), "completedAt": end, "input": input, "toolName": tool, "exitCode": exit}).to_string()])?;
            }
        }
    }
    let first = period.start() + 8. * HOUR;
    if first + 4. * 60_000. < period.until() {
        let thread = thread_id((PER_DAY.len() * LANES * 5) as u64);
        threads.execute(params![thread, json!({"providerInstanceId":"grok", "createdAt":iso(first), "updatedAt":iso(first + HOUR)}).to_string()])?;
        runs.execute(params!["closed-run", thread, json!({"providerInstanceId":"grok"}).to_string(), iso(first)])?;
        let end = (first + HOUR <= period.until()).then(|| iso(first + HOUR));
        let status = if end.is_some() { "completed" } else { "running" };
        for i in 0..5 {
            items.execute(params![format!("closed-{i}"), thread, "closed-run", "command_execution", status, i,
                json!({"startedAt":iso(first + i as f64 * 60_000.), "completedAt":end, "input":LINES[15].0, "exitCode":end.as_ref().map(|_| 0)}).to_string()])?;
        }
    }
    db.execute_batch("COMMIT")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fanout::PIN_SNAPSHOT, recap::Which};
    use serde_json::{Value, json};
    use std::{fs, path::PathBuf, sync::atomic::{AtomicUsize, Ordering}};

    #[test]
    fn cache_opt_in_and_opt_out() {
        for enabled in [None, Some(""), Some("0"), Some("1"), Some("yes")] {
            for disabled in [None, Some(""), Some("0"), Some("1"), Some("yes")] {
                assert_eq!(cache_enabled(enabled, disabled), enabled == Some("1") && disabled != Some("1"),
                           "enable={enabled:?} disable={disabled:?}");
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod cached {
        use super::*;
        use crate::cache::Cache;

        const RUN: &str = "private-run-for-cache-check";
        const THREAD: &str = "private-thread-for-cache-check";
        const SECRET: &str = "mcp__secret-server__secret_tool";
        const FULL: Option<skim::Mode> = Some(skim::Mode::Full);
        const WORKERS: [usize; 4] = [1, 2, 4, 8];

        struct Files {
            source: PathBuf,
            store: PathBuf,
            _dir: Scratch,
        }

        impl Files {
            fn new() -> Self {
                let dir = Scratch::new();
                Self { source: dir.0.join("tools.sqlite"), store: dir.0.join("cache/insights.sqlite3"), _dir: dir }
            }

            fn fixture(db: &Connection) -> Self {
                let files = Self::new();
                db.execute_batch(&format!("ALTER TABLE {P}runs ADD COLUMN status TEXT;
                    UPDATE {P}runs SET status = CASE WHEN completed_at IS NULL THEN 'running' ELSE 'completed' END;
                    UPDATE {P}runs SET payload_json = json_set(payload_json, '$.status', status);")).unwrap();
                db.execute("VACUUM INTO ?1", [files.source.to_str().unwrap()]).unwrap();
                files
            }

            fn source_db(&self) -> Connection { recap::open(&self.source).unwrap() }

            fn settle(&self, db: &Connection) {
                for _ in 0..200 {
                    if probe_cache(db, &self.source, Some(&self.store)).is_some() { return; }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                panic!("synthetic database did not settle");
            }

            fn with_store<T>(&self, db: &Connection, action: impl FnOnce(&mut Cache<'_>) -> T) -> T {
                self.settle(db);
                let probe = probe_cache(db, &self.source, Some(&self.store)).unwrap();
                db.execute_batch("BEGIN").unwrap();
                let output = action(&mut probe.open(db).unwrap());
                db.execute_batch("ROLLBACK").unwrap();
                output
            }

            fn direct(&self, period: &Period, mode: Option<skim::Mode>) -> Tools {
                summarize(&read_with(&self.source_db(), &self.source, period, mode).unwrap(), period, &utc)
            }

            fn cached(&self, period: &Period, mode: Option<skim::Mode>, workers: usize) -> (Tools, Option<CacheStats>) {
                let db = self.source_db();
                self.settle(&db);
                let (facts, stats) = read_cached(&db, &self.source, period, mode, CacheOptions { store: Some(&self.store), workers, measure: true }).unwrap();
                assert!(db.is_autocommit());
                (summarize(&facts, period, &utc), stats)
            }
        }

        fn check_equal(expected: &Tools, got: &Tools) {
            assert_eq!(got, expected);
            assert_eq!(serde_json::to_vec(got).unwrap(), serde_json::to_vec(expected).unwrap());
        }

        fn single(period: &Period) -> Connection {
            let db = fixture();
            let t = period.start() + HOUR;
            thread(&db, THREAD, "codex", t, t + HOUR, false);
            run(&db, RUN, THREAD, "codex", t, Some(t + HOUR));
            db
        }

        fn assert_absent(bytes: &[u8], contents: &[&str]) {
            for text in contents {
                assert!(!bytes.windows(text.len()).any(|part| part == text.as_bytes()), "cache retained {text}");
            }
        }

        fn varied(period: &Period, seed: Option<u64>) -> Files {
            let db = fixture();
            let start = period.start();
            let mut ordinal = 0;
            for n in 0..48 {
                let thread_id = format!("cache-test-thread-{n}");
                let run_id = format!("cache-test-run-{n}");
                let at = start + HOUR * n as f64;
                thread(&db, &thread_id, "codex", at, at + HOUR, false);
                let (requested, completed) = match n {
                    1 => (start - 2. * HOUR, Some(start + HOUR)),
                    2 => (period.until() - HOUR, Some(period.until() + HOUR)),
                    _ => (at, (n % 9 != 4).then_some(at + 4. * HOUR)),
                };
                run(&db, &run_id, &thread_id, "codex", requested, completed);
                let count = seed.map_or(5, |seed| 1 + (chance(seed + n) * 83.) as i64);
                for i in 0..count {
                    ordinal += 1;
                    let stamp = if n == 1 { start + (i as f64 - 1.) * HOUR }
                        else if n == 2 { period.until() + (i as f64 - 1.) * HOUR }
                        else if let Some(seed) = seed { at + (chance(seed + ordinal as u64) * HOUR).floor() }
                        else { at };
                    let length = (n % 7 != 3 || i != 0).then_some(30_000.);
                    let kinds = ["command_execution", "dynamic_tool", "command_execution", "dynamic_tool", "file_change"];
                    let kind = kinds[i as usize % kinds.len()];
                    let mut payload = if kind == "dynamic_tool" { timing(stamp, length, "toolName", SECRET) }
                        else { timing(stamp, length, "input", "git diff # never read") };
                    if kind == "command_execution" { payload["exitCode"] = json!(i % 2); }
                    item(&db, ordinal, (&thread_id, Some(&run_id)), kind,
                        if length.is_none() { "running" } else { "failed" }, payload);
                }
            }
            thread(&db, "cache-test-child", "claudeAgent", start, start + HOUR, true);
            item(&db, ordinal + 1, ("cache-test-child", None), "command_execution", "completed",
                 timing(start, Some(500.), "input", "cargo test"));
            Files::fixture(&db)
        }

        fn crossing(period: &Period, open: bool) -> Files {
            let db = fixture();
            let edge = period.until();
            thread(&db, THREAD, "codex", edge - HOUR, edge + HOUR, false);
            run(&db, RUN, THREAD, "codex", edge - HOUR, Some(edge + HOUR));
            item(&db, 1, (THREAD, Some(RUN)), "command_execution", "completed",
                 timing(edge - 1000., Some(2000.), "input", "git diff"));
            item(&db, 2, (THREAD, Some(RUN)), "dynamic_tool", "completed",
                 timing(edge - 1000., Some(2000.), "toolName", SECRET));
            item(&db, 3, (THREAD, Some(RUN)), "command_execution", "completed",
                 timing(edge, Some(2000.), "input", "cargo test"));
            item(&db, 4, (THREAD, Some(RUN)), "dynamic_tool", if open { "running" } else { "completed" },
                 timing(edge, (!open).then_some(2000.), "toolName", SECRET));
            Files::fixture(&db)
        }

        fn adjacent_periods() -> [Period; 3] {
            let first = week();
            let second = Period::new(first.until() + 8. * 24. * HOUR, Which::LastWeek, &utc);
            let month = Period::new(second.until() + HOUR, Which::ThisMonth, &utc);
            assert_eq!(first.until(), second.start());
            [first, second, month]
        }

        #[test]
        fn cache_crossing_run_agrees_for_all_period_orders() {
            for workers in WORKERS {
                let periods = adjacent_periods();
                for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
                    let files = crossing(&periods[0], false);
                    for (position, index) in order.into_iter().enumerate() {
                        let period = &periods[index];
                        let (actual, stats) = files.cached(period, FULL, workers);
                        let expected = files.direct(period, FULL);
                        assert_eq!(expected.calls, if index == 2 { 4 } else { 2 });
                        check_equal(&expected, &actual);
                        let stats = stats.unwrap();
                        assert_eq!((stats.hits, stats.written), if position == 0 { (0, 1) } else { (1, 0) });
                        if position > 0 {
                            assert_eq!(stats.rows.by_ordinal, 2);
                            assert_eq!(stats.rows.outside, usize::from(index != 2));
                            assert_eq!(stats.reread, 0);
                        }
                    }
                }
            }
        }

        #[test]
        fn cache_entry_bytes_are_independent_of_the_filling_period() {
            for workers in WORKERS {
                let periods = adjacent_periods();
                let mut entries = Vec::new();
                for period in &periods {
                    let files = crossing(&periods[0], false);
                    let (report, stats) = files.cached(period, FULL, workers);
                    check_equal(&files.direct(period, FULL), &report);
                    assert_eq!(stats.unwrap().written, 1);
                    files.with_store(&files.source_db(), |store| {
                        entries.push(store.lookup_many(run_entry::KIND, run_entry::VERSION, &[RUN]).remove(RUN).unwrap());
                    });
                }
                assert_eq!(entries[0], entries[1]);
                assert_eq!(entries[0], entries[2]);
            }
        }

        #[test]
        fn cache_open_call_outside_the_period_keeps_the_whole_run_live() {
            for workers in WORKERS {
                let periods = adjacent_periods();
                for period in &periods {
                    let files = crossing(&periods[0], true);
                    for _ in 0..2 {
                        let (report, stats) = files.cached(period, FULL, workers);
                        check_equal(&files.direct(period, FULL), &report);
                        let stats = stats.unwrap();
                        assert_eq!((stats.finished, stats.hits, stats.written), (1, 0, 0));
                    }
                }
            }
        }

        #[test]
        fn cache_matches_direct_for_mixed_runs_and_periods() {
            for workers in WORKERS {
                let period = week();
                for seed in [None, Some(3214)] {
                    let files = varied(&period, seed);
                    let month = Period::new(period.until() + 14. * HOUR, Which::ThisMonth, &utc);
                    let want = files.direct(&period, FULL);
                    let (first, stats) = files.cached(&period, FULL, workers);
                    check_equal(&want, &first);
                    assert!(stats.unwrap().written > 1);
                    let (warm, stats) = files.cached(&period, FULL, workers);
                    check_equal(&want, &warm);
                    assert!(stats.unwrap().hits > 1);
                    Connection::open(&files.store).unwrap().execute("DELETE FROM facts WHERE run=(SELECT run FROM facts LIMIT 1)", []).unwrap();
                    let (part, stats) = files.cached(&period, FULL, workers);
                    check_equal(&want, &part);
                    let stats = stats.unwrap();
                    assert!(stats.hits > 0 && stats.written == 1);
                    let (other, stats) = files.cached(&month, FULL, workers);
                    check_equal(&files.direct(&month, FULL), &other);
                    assert!(stats.unwrap().hits > 0);
                }
            }
        }

        #[test]
        fn cache_read_agrees_after_a_rewrite_an_insert_and_a_delete() {
            for workers in WORKERS {
                let period = week();
                let files = varied(&period, Some(882));
                files.cached(&period, FULL, workers);
                check_equal(&files.direct(&period, FULL), &files.cached(&period, FULL, workers).0);
                let writer = Connection::open(&files.source).unwrap();
                let end = iso(period.start() + 2. * HOUR);
                writer.execute(&format!("UPDATE {P}turn_items SET payload_json=json_set(payload_json,'$.completedAt',?1)
                    WHERE item_id='1'"), [end]).unwrap();
                check_equal(&files.direct(&period, FULL), &files.cached(&period, FULL, workers).0);
                item(&writer, 1_000_000, ("cache-test-thread-0", Some("cache-test-run-0")),
                     "command_execution", "completed", timing(period.start(), Some(1000.), "input", "rg needle"));
                check_equal(&files.direct(&period, FULL), &files.cached(&period, FULL, workers).0);
                writer.execute(&format!("DELETE FROM {P}runs WHERE run_id='cache-test-run-0'"), []).unwrap();
                check_equal(&files.direct(&period, FULL), &files.cached(&period, FULL, workers).0);
            }
        }

        #[test]
        fn cache_diagnostic_counts_only_used_entries_and_skips_open_calls() {
            for workers in WORKERS {
                let period = week();
                let db = single(&period);
                let t = period.start() + HOUR;
                run(&db, "finished-with-open-call", THREAD, "codex", t, Some(t + HOUR));
                item(&db, 1, (THREAD, Some(RUN)), "command_execution", "completed", timing(t, Some(1000.), "input", "cargo test"));
                item(&db, 2, (THREAD, Some("finished-with-open-call")), "dynamic_tool", "running", timing(t, None, "toolName", "Read"));
                let files = Files::fixture(&db);
                let (_, closed) = load_file(&files.source, &period, cache_enabled(Some("1"), Some("1")), Some(&files.store), false).unwrap();
                assert_eq!(cache_diagnostic(closed), "loupe: cache: not used");
                assert!(!files.store.exists());
                let (report, first) = files.cached(&period, FULL, workers);
                let (warm, second) = files.cached(&period, FULL, workers);
                check_equal(&report, &warm);
                assert_eq!(cache_diagnostic(first), "loupe: cache: 0 of 2 finished runs from the cache, 1 written");
                assert_eq!(cache_diagnostic(second), "loupe: cache: 1 of 2 finished runs from the cache, 0 written");
                files.with_store(&files.source_db(), |store| {
                    store.lookup_many(run_entry::KIND, run_entry::VERSION, &[RUN]);
                    assert_eq!(store.write_many(run_entry::KIND, run_entry::VERSION, &[(RUN, vec![0xff])]), 1);
                });
                let (broken, stats) = files.cached(&period, FULL, workers);
                check_equal(&files.direct(&period, FULL), &broken);
                assert_eq!(stats.unwrap().hits, 0);
            }
        }

        #[test]
        fn cache_store_damage_leaves_the_report_unchanged() {
            for workers in WORKERS {
                let period = week();
                for form in ["garbage", "truncated", "future"] {
                    let db = single(&period);
                    item(&db, 1, (THREAD, Some(RUN)), "command_execution", "completed",
                         timing(period.start() + HOUR, Some(1000.), "input", "git status"));
                    let files = Files::fixture(&db);
                    let want = files.direct(&period, FULL);
                    assert_eq!(files.cached(&period, FULL, workers).1.unwrap().written, 1);
                    match form {
                        "garbage" => fs::write(&files.store, b"not SQLite").unwrap(),
                        "truncated" => {
                            let data = fs::read(&files.store).unwrap();
                            fs::write(&files.store, &data[..64]).unwrap();
                        }
                        _ => Connection::open(&files.store).unwrap().pragma_update(None, "user_version", 100).unwrap(),
                    }
                    let (actual, stats) = files.cached(&period, FULL, workers);
                    check_equal(&want, &actual);
                    assert!(stats.is_none(), "{form}");
                }
            }
        }

        #[test]
        fn cache_never_persists_text_ids_or_a_named_tool() {
            for workers in WORKERS {
                let period = week();
                let db = single(&period);
                let t = period.start() + HOUR;
                let mut command = timing(t, Some(2000.), "input", "cargo test # never read");
                for field in ["output", "text", "title", "fileName", "pattern", "patterns", "prompt",
                    "message", "summary", "result", "results", "questions", "changes", "toolName"] {
                    command[field] = json!("never read");
                }
                item(&db, 1, (THREAD, Some(RUN)), "command_execution", "completed", command);
                item(&db, 2, (THREAD, Some(RUN)), "dynamic_tool", "completed", timing(t, Some(2000.), "toolName", SECRET));
                item(&db, 3, (THREAD, Some(RUN)), "dynamic_tool", "completed", timing(t, Some(2000.), "toolName", "Read"));
                let files = Files::fixture(&db);
                assert_eq!(files.cached(&period, FULL, workers).1.unwrap().written, 1);
                assert_absent(&fs::read(&files.store).unwrap(), &["never read", "secret-server", "secret_tool", SECRET, "Read", THREAD, RUN]);
                files.with_store(&files.source_db(), |store| {
                    let hits = store.lookup_many(run_entry::KIND, run_entry::VERSION, &[RUN]);
                    let rows = run_entry::decode(&hits[RUN], FULL, KNOWN_TOOLS).unwrap();
                    assert!(matches!(rows.as_slice(), [
                        (1, run_entry::Row::Stored { name: None, .. }), (2, run_entry::Row::Named(0)), (3, run_entry::Row::Named(0)),
                    ]));
                });
            }
        }

        #[test]
        fn cache_only_reads_named_ordinals_and_child_calls_on_an_all_hit_read() {
            for workers in WORKERS {
                let period = week();
                let db = single(&period);
                item(&db, 1, (THREAD, Some(RUN)), "dynamic_tool", "completed",
                     timing(period.start() + HOUR, Some(10.), "toolName", "Read"));
                thread(&db, "cache-live-child", "claudeAgent", period.start(), period.until(), true);
                item(&db, 2, ("cache-live-child", None), "dynamic_tool", "completed",
                     timing(period.start() + HOUR, Some(20.), "toolName", "Read"));
                let files = Files::fixture(&db);
                let (fill, first) = files.cached(&period, FULL, workers);
                let (hit, stats) = files.cached(&period, FULL, workers);
                check_equal(&fill, &hit);
                assert_eq!(first.unwrap().written, 1);
                let stats = stats.unwrap();
                assert_eq!((stats.hits, stats.finished, stats.written), (1, 1, 0));
                assert_eq!((stats.rows.by_ordinal, stats.rows.children, stats.rows.other, stats.reread), (1, 1, 0, 0));
                let source = files.source_db();
                register_call_functions(&source, FULL).unwrap();
                let named = named_call_sql(FULL);
                assert!(named.contains("run_id=?1 AND ordinal=?4 AND type='dynamic_tool'"));
                let explanation = source.prepare(&format!("EXPLAIN QUERY PLAN {named}")).unwrap()
                    .query_map(params![RUN, period.start(), period.until(), 1, 0], |r| r.get::<_, String>(3)).unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>().unwrap().join(" ");
                assert!(explanation.contains("demo_items_by_run") && explanation.contains("run_id=? AND ordinal=?")
                    && !explanation.contains("TEMP B-TREE"), "{explanation}");
                let sql = source_queries(&source, FULL).unwrap();
                assert!(sql.child_calls.contains("thread_id=?1 AND run_id IS NULL"));
                for (mode, args) in [(FULL, vec![rusqlite::types::Value::Text(RUN.into()),
                    rusqlite::types::Value::Real(period.start()), rusqlite::types::Value::Real(period.until())]),
                    (None, vec![rusqlite::types::Value::Text(RUN.into())])] {
                    let sql = source_queries(&source, mode).unwrap();
                    let mut plan = source.prepare(&format!("EXPLAIN QUERY PLAN {}", sql.run_calls)).unwrap();
                    let plan = plan.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3)).unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>().unwrap().join(" ");
                    assert!(plan.contains("demo_items_by_run") && !plan.contains("TEMP B-TREE"), "{plan}");
                }
            }
        }

        #[test]
        fn cache_known_tool_codec_uses_exact_name_and_no_server() {
            let period = week();
            let db = single(&period);
            let known = ["Read"];
            for (n, name) in ["Read", "unlisted-tool", "mcp__secret-server__Read", "read"].into_iter().enumerate() {
                item(&db, n as i64 + 1, (THREAD, Some(RUN)), "dynamic_tool", "completed",
                     timing(period.start() + HOUR, Some(2000.), "toolName", name));
            }
            let files = Files::fixture(&db);
            let source = files.source_db();
            let scope = CallScope { run: Some(RUN.into()), thread: THREAD.into(), provider: "codex".into() };
            register_call_functions(&source, FULL).unwrap();
            files.with_store(&source, |store| {
                let all = call_sql(FULL, "run_id=?1", " ORDER BY ordinal, rowid", ", ordinal");
                let (source_calls, bytes, _) = read_cache_miss(&source, &all, 3, &scope, &period, FULL, &known).unwrap();
                let mut source_calls: Vec<_> = source_calls.into_iter().map(Some).collect();
                let bytes = bytes.unwrap();
                for len in 0..bytes.len() { assert!(run_entry::decode(&bytes[..len], FULL, &known).is_none()); }
                assert!(run_entry::decode(&bytes, FULL, KNOWN_TOOLS).is_none());
                store.lookup_many(run_entry::KIND, run_entry::VERSION, &[RUN]);
                assert_eq!(store.write_many(run_entry::KIND, run_entry::VERSION, &[(RUN, bytes)]), 1);
                let hits = store.lookup_many(run_entry::KIND, run_entry::VERSION, &[RUN]);
                let rows = run_entry::decode(&hits[RUN], FULL, &known).unwrap();
                let mut read = Vec::new();
                let merged = merge_cached_rows(rows, &scope, &period, "codex", &known, |ordinal, slot| {
                    assert_eq!(slot, 0);
                    read.push(ordinal);
                    Ok(source_calls[(ordinal - 1) as usize].take())
                }).unwrap();
                assert_eq!(read, [2, 3, 4]);
                let actual = summarize(&Facts { calls: merged, first_record: Some(period.start() + HOUR) }, &period, &utc);
                check_equal(&files.direct(&period, FULL), &actual);
            });
            assert_absent(&fs::read(&files.store).unwrap(), &["Read", "unlisted-tool", "read", "secret-server", RUN, THREAD]);
        }

        #[test]
        fn cache_preserves_rowid_order_for_tied_ordinals() {
            for workers in WORKERS {
                let period = week();
                let t = period.start() + HOUR;
                let cmd = |name| ("command_execution", timing(t, Some(7000.), "input", name));
                let tool = |name| ("dynamic_tool", timing(t, Some(7000.), "toolName", name));
                for mode in [None, FULL, Some(skim::Mode::Early)] {
                    for (pair, names) in [
                        ([cmd("git diff"), tool("Read")], ["git", "Read"]),
                        ([tool("Read"), tool("mcp__server__Lookup")], ["Read", "Lookup"]),
                        ([cmd("git diff"), cmd("cargo test")], ["git", "cargo"]),
                    ] {
                        for reverse in [false, true] {
                            let db = single(&period);
                            for i in 0..pair.len() {
                                let index = if reverse { 1 - i } else { i };
                                let payload = &pair[index].1;
                                item(&db, i as i64 + 1, (THREAD, Some(RUN)), pair[index].0, "completed", payload.clone());
                            }
                            db.execute(&format!("UPDATE {P}turn_items SET ordinal=7"), []).unwrap();
                            let files = Files::fixture(&db);
                            let want = files.direct(&period, mode);
                            let actual: Vec<_> = want.longest.iter().map(|call| call.name.as_deref().unwrap()).collect();
                            let expected = if reverse { [names[1], names[0]] } else { names };
                            assert_eq!(actual, expected);
                            let (fill, stats) = files.cached(&period, mode, workers);
                            check_equal(&want, &fill);
                            assert_eq!(stats.unwrap().written, 1);
                            let (warm, stats) = files.cached(&period, mode, workers);
                            check_equal(&want, &warm);
                            assert_eq!(stats.unwrap().hits, 1);
                        }
                    }
                }
            }
        }

        #[test]
        fn cache_rereads_a_scope_after_a_worker_pins_a_newer_snapshot() {
            let period = week();
            let db = single(&period);
            let t = period.start() + HOUR;
            item(&db, 1, (THREAD, Some(RUN)), "command_execution", "completed",
                 timing(t, Some(1000.), "input", "git diff"));
            let files = Files::fixture(&db);
            let writer = Connection::open(&files.source).unwrap();
            writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=1").unwrap();
            let main = files.source_db();
            register_call_functions(&main, FULL).unwrap();
            let (old, cached, stats) = files.with_store(&main, |store| {
                let old = summarize(&read(&main, &period, FULL).unwrap(), &period, &utc);
                let options = CacheOptions { store: Some(&files.store), workers: 2, measure: false };
                let (facts, stats) = read_with_cache(&main, &files.source, &period, FULL, store, options, || {
                    writer.execute(&format!("UPDATE {P}turn_items SET payload_json=json_set(payload_json,'$.completedAt',?1)"),
                        [iso(t + 9000.)]).unwrap();
                }).unwrap();
                (old, summarize(&facts, &period, &utc), stats)
            });
            assert_eq!(old.ms, 1000.);
            assert_eq!(files.direct(&period, FULL).ms, 9000.);
            check_equal(&old, &cached);
            assert_eq!((stats.reread, stats.written), (1, 0));
            let saved: i64 = Connection::open(&files.store).unwrap()
                .query_row("SELECT count(*) FROM facts", [], |r| r.get(0)).unwrap();
            assert_eq!(saved, 0);
        }
    }

    // Use Recap's UTC fallback, with no dependency on the host's local zone.
    fn utc(_: i64) -> Option<crate::platform::LocalTime> { None }

    fn load_uncached_for_test(path: &Path, period: &Period, local: Local) -> Result<Tools, SourceError> {
        let (facts, _) = load_file(path, period, false, None, false)?;
        Ok(summarize(&facts, period, local))
    }

    fn week() -> Period {
        Period::new(ms("2026-11-17T12:00:00.000Z").unwrap(), Which::LastWeek, &utc)
    }

    fn fixture() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(DEMO_SCHEMA).unwrap();
        db
    }

    fn thread(db: &Connection, id: &str, provider: &str, created: f64, updated: f64, child: bool) {
        let lineage = if child { json!({"relationshipToParent": "subagent"}) } else { Value::Null };
        db.execute(&format!("INSERT INTO {P}threads(thread_id,payload_json) VALUES(?1,?2)"), params![id,
            json!({"providerInstanceId": provider, "createdAt": iso(created), "updatedAt": iso(updated), "lineage": lineage}).to_string()]).unwrap();
    }

    fn run(db: &Connection, id: &str, thread: &str, provider: &str, requested: f64, completed: Option<f64>) {
        db.execute(&format!("INSERT INTO {P}runs(run_id,thread_id,ordinal,payload_json,requested_at,completed_at)
                            VALUES(?1,?2,1,?3,?4,?5)"),
            params![id, thread, json!({"providerInstanceId": provider}).to_string(), iso(requested), completed.map(iso)]).unwrap();
    }

    fn item(db: &Connection, id: i64, scope: (&str, Option<&str>), kind: &str, status: &str, payload: Value) {
        db.execute(&format!("INSERT INTO {P}turn_items(item_id,thread_id,run_id,type,status,ordinal,payload_json)
                            VALUES(?1,?2,?3,?4,?5,?1,?6)"),
            params![id, scope.0, scope.1, kind, status, payload.to_string()]).unwrap();
    }

    fn timing(start: f64, length: Option<f64>, field: &str, value: impl Into<Value>) -> Value {
        let mut payload = json!({"startedAt": iso(start), "completedAt": length.map(|n| iso(start + n))});
        payload[field] = value.into();
        payload
    }

    fn read_report(db: &Connection, period: &Period) -> Tools {
        summarize(&read_with(db, Path::new("test"), period, None).unwrap(), period, &utc)
    }

    fn program<'a>(report: &'a Tools, name: Option<&str>) -> &'a Row {
        report.programs.iter().find(|row| row.name.as_deref() == name).unwrap()
    }

    fn by_hand() -> (Connection, Period) {
        let db = fixture();
        let period = week();
        let t = period.start() + 24. * HOUR;
        thread(&db, "t", "unused", t, t + HOUR, false);
        run(&db, "r", "t", "codex", t, None);
        let items = [
            ("command_execution", "completed", Some(999.), "input", "cargo test -p x"),
            ("command_execution", "failed", Some(1000.), "input", "mkdir -p out && cargo build --release"),
            ("command_execution", "completed", Some(10_000.), "input", "./run-checks.sh"),
            ("dynamic_tool", "completed", Some(60_000.), "toolName", "Read"),
            ("file_search", "failed", Some(600_000.), "toolName", "ignored"),
            ("command_execution", "running", None, "input", "./run-checks.sh"),
        ];
        for (id, (kind, status, length, field, value)) in items.into_iter().enumerate() {
            item(&db, id as i64, ("t", Some("r")), kind, status, timing(t, length, field, value));
        }
        (db, period)
    }

    // 1. The independently grouped rows, buckets and providers all add back to the totals.
    #[test]
    fn the_sums() {
        fn check(r: &Tools) {
            let rows = |rows: Vec<&Row>| rows.into_iter().fold((0, 0., 0, 0), |(n, ms, f, o), row| {
                assert_eq!(row.lengths.iter().map(|b| b.calls).sum::<u32>(), row.calls - row.open - row.closed_together);
                assert_eq!(row.lengths.iter().map(|b| b.ms).sum::<f64>(), row.ms);
                assert_eq!(row.by_provider.iter().map(|s| s.calls).sum::<u32>(), row.calls);
                assert_eq!(row.by_provider.iter().map(|s| s.ms).sum::<f64>(), row.ms);
                assert_eq!(row.by_provider.iter().map(|s| s.closed_together).sum::<u32>(), row.closed_together);
                assert_eq!(row.by_provider.iter().map(|s| s.failed).sum::<u32>(), row.failed);
                (n + row.calls, ms + row.ms, f + row.failed, o + row.open)
            });
            let fields = |e: &Exits| [e.known, e.failed_known, e.zero, e.one, e.two, e.timed_out, e.not_runnable, e.not_found, e.signal, e.other, e.passed_nonzero];
            assert_eq!(r.kinds.iter().map(|row| row.closed_together).sum::<u32>(), r.closed_together);
            assert_eq!(r.providers.iter().map(|p| p.closed_together).sum::<u32>(), r.closed_together);
            let shell = r.kinds.iter().find(|row| row.kind == Kind::Shell).unwrap();
            assert_eq!(r.programs.iter().map(|row| row.closed_together).sum::<u32>(), shell.closed_together);
            for e in r.kinds.iter().chain(&r.programs).chain(&r.tools).map(|row| &row.exits).chain(r.providers.iter().map(|p| &p.exits)).chain(std::iter::once(&r.commands.exits)) {
                assert!(e.failed_known <= e.known);
                assert_eq!(e.failed_known, e.zero + e.one + e.two + e.timed_out + e.not_runnable + e.not_found + e.signal + e.other);
            }
            for row in r.kinds.iter().chain(&r.programs).chain(&r.tools) { assert!(row.exits.failed_known <= row.failed); }
            for p in &r.providers { assert!(p.exits.failed_known <= p.commands_failed); }
            for i in 0..11 {
                assert_eq!(r.programs.iter().map(|row| fields(&row.exits)[i]).sum::<u32>(), fields(&shell.exits)[i]);
                assert_eq!(fields(&shell.exits)[i], fields(&r.commands.exits)[i]);
                assert_eq!(r.providers.iter().map(|p| fields(&p.exits)[i]).sum::<u32>(), fields(&r.commands.exits)[i]);
            }
            assert_eq!(rows(r.programs.iter().collect()), (shell.calls, shell.ms, shell.failed, shell.open));
            assert_eq!(rows(r.tools.iter().collect()), rows(r.kinds.iter().filter(|row| row.kind != Kind::Shell).collect()));
            assert_eq!(rows(r.kinds.iter().collect()), (r.calls, r.ms, r.failed, r.open));
            assert_eq!(r.lengths.iter().map(|b| b.calls).sum::<u32>(), r.calls - r.open - r.closed_together);
            assert_eq!(r.lengths.iter().map(|b| b.ms).sum::<f64>(), r.ms);
            assert_eq!(r.providers.iter().map(|p| p.calls).sum::<u32>(), r.calls);
            assert_eq!(r.providers.iter().map(|p| p.ms).sum::<f64>(), r.ms);
            assert_eq!(r.providers.iter().map(|p| p.failed).sum::<u32>(), r.failed);
            assert_eq!(r.providers.iter().map(|p| p.commands).sum::<u32>(), r.commands.calls);
            assert_eq!(r.providers.iter().map(|p| p.commands_failed).sum::<u32>(), r.commands.failed);
            assert!(r.programs.iter().filter(|row| row.name.is_some()).all(|row| row.appears_in >= row.calls));
        }

        let (db, period) = by_hand();
        let r = read_report(&db, &period);
        check(&r);
        assert_eq!((r.calls, r.ms, r.failed, r.open, r.days_recorded), (6, 671_999., 2, 1, 6));
        assert_eq!(r.lengths.iter().map(|b| (b.label.as_str(), b.calls)).collect::<Vec<_>>(),
            vec![("under 1s", 1), ("1-10s", 1), ("10-60s", 1), ("1-10min", 1), ("over 10min", 1)]);
        assert_eq!(program(&r, Some("cargo")).median_ms, Some(999.5));
        assert_eq!((r.longest[0].kind, r.longest[0].when.as_str()), (Kind::Search, "Tue 00:00"));

        let db = fixture();
        let start = period.start();
        let scopes = [("t0", Some("r0")), ("t1", Some("r1")), ("t2", None)];
        for ((id, run_id), provider) in scopes.iter().zip(["codex", "claudeAgent", "grok"]) {
            thread(&db, id, provider, start, start + HOUR, run_id.is_none());
            if let Some(run_id) = run_id { run(&db, run_id, id, provider, start, None); }
        }
        let kinds = ["command_execution", "dynamic_tool", "file_change", "file_search", "web_search"];
        for i in 0..500 {
            let seed = i as u64 * 9;
            let scope = scopes[(chance(seed + 1) * scopes.len() as f64) as usize];
            let kind = kinds[(chance(seed + 2) * kinds.len() as f64) as usize];
            let input = LINES[(chance(seed + 3) * LINES.len() as f64) as usize].0;
            let start = start + (chance(seed + 4) * HOUR).floor();
            let length = (chance(seed + 5) > 0.1).then(|| (chance(seed + 6) * 700_000.).floor());
            let status = if chance(seed + 7) < 0.2 { "failed" } else { "completed" };
            let mut payload = timing(start, length, "input", input);
            payload["exitCode"] = json!((chance(seed + 100) * 260.) as i64);
            payload["toolName"] = json!(if chance(seed + 8) < 0.5 { "Read" } else { "mcp__test__fetch" });
            item(&db, i, scope, kind, status, payload);
        }
        for i in 0..2 {
            item(&db, 500 + i, scopes[0], "command_execution", "completed", timing(start + i as f64 * 2000., Some(10_000. - i as f64 * 2000.), "input", "cargo test -p x"));
        }
        let r = read_report(&db, &period);
        assert!(r.closed_together >= 2);
        assert_eq!(r.calls, 502);
        check(&r);
    }

    #[test]
    fn exit_boundaries_and_types() {
        let db = fixture();
        let period = week();
        let t = period.start();
        run(&db, "r", "t", "codex", t, None);
        let values = [json!(-1), json!(0), json!(1), json!(2), json!(3), json!(124), json!(125),
            json!(126), json!(127), json!(128), json!(129), json!(255), json!(256),
            json!("1"), json!(1.5), json!(null), json!(true), json!({})];
        for (i, value) in values.into_iter().enumerate() {
            let mut payload = timing(t, Some(1.), "input", "cargo test -p x");
            payload["exitCode"] = value;
            item(&db, i as i64, ("t", Some("r")), "command_execution", "failed", payload.clone());
            item(&db, i as i64 + 200, ("t", Some("r")), "command_execution", "completed", payload);
        }
        item(&db, 100, ("t", Some("r")), "command_execution", "completed", timing(t, Some(1.), "input", "cargo test -p x"));
        let e = read_report(&db, &period).commands.exits;
        assert_eq!((e.known, e.failed_known, e.zero, e.one, e.two, e.timed_out, e.not_runnable, e.not_found, e.signal, e.other, e.passed_nonzero), (26, 13, 1, 1, 1, 1, 1, 1, 2, 5, 12));
    }

    #[test]
    fn exit_status_and_non_commands() {
        let db = fixture();
        let period = week();
        let t = period.start();
        run(&db, "r", "t", "codex", t, None);
        for (i, kind, status, code) in [(1, "command_execution", "failed", 0), (2, "command_execution", "completed", 1),
            (3, "dynamic_tool", "completed", 2), (4, "file_change", "failed", 127)] {
            let mut payload = timing(t, Some(1.), "input", "cargo test -p x");
            payload["exitCode"] = json!(code);
            item(&db, i, ("t", Some("r")), kind, status, payload);
        }
        let r = read_report(&db, &period);
        assert_eq!((r.commands.exits.known, r.commands.exits.zero, r.commands.exits.passed_nonzero), (2, 1, 1));
        assert!(r.kinds.iter().filter(|row| row.kind != Kind::Shell).all(|row| row.exits.known == 0));
        assert_eq!(r.longest[0].exit, Some(0));
        assert_eq!(r.providers[0].exits.known, 2);
        assert_eq!(r.programs.iter().map(|row| row.exits.known).sum::<u32>(), 2);
    }

    // 2. A chain times and fails once, even when a different program comes first.
    #[test]
    fn chain_goes_under_its_label() {
        let db = fixture();
        let period = week();
        let t = period.start();
        run(&db, "r", "t", "codex", t, None);
        item(&db, 1, ("t", Some("r")), "command_execution", "failed",
            timing(t, Some(1620.), "input", "mkdir -p out && cargo build --release"));
        let r = read_report(&db, &period);
        assert_eq!((r.commands.calls, r.commands.named, r.commands.chained), (1, 1, 1));
        let (cargo, mkdir) = (program(&r, Some("cargo")), program(&r, Some("mkdir")));
        assert_eq!((cargo.calls, cargo.ms, cargo.failed, cargo.chained, cargo.appears_in), (1, 1620., 1, 1, 1));
        assert_eq!((mkdir.calls, mkdir.ms, mkdir.failed, mkdir.chained, mkdir.appears_in), (0, 0., 0, 0, 1));
        assert_eq!(mkdir.median_ms, None);
    }

    // 3. Unnamed command time and calls are still visible and counted.
    #[test]
    fn commands_without_a_name() {
        let (db, period) = by_hand();
        let r = read_report(&db, &period);
        let unnamed = program(&r, None);
        assert_eq!((unnamed.calls, unnamed.ms, unnamed.open), (2, 10_000., 1));
        assert_eq!((r.commands.unnamed, r.commands.unnamed_ms), (unnamed.calls, unnamed.ms));
    }

    // 4. Half-open start bounds, with a complete duration across the upper bound.
    #[test]
    fn period_filters_by_call_start() {
        let db = fixture();
        let period = week();
        let (lo, hi) = (period.start(), period.until());
        run(&db, "r", "t", "codex", lo - HOUR, None);
        for (id, start, length) in [(1, lo - 1., HOUR), (2, lo, 0.), (3, hi - 1., 2000.), (4, hi, 100.)] {
            item(&db, id, ("t", Some("r")), "file_change", "completed", timing(start, Some(length), "toolName", "ignored"));
        }
        let r = read_report(&db, &period);
        assert_eq!((r.calls, r.ms, r.busy_ms, r.open), (2, 2000., 2000., 0));
        assert_eq!(r.longest.iter().map(|call| call.started).collect::<Vec<_>>(), vec![hi - 1., lo]);
    }

    #[test]
    fn closed_together_excludes_time() {
        let db = fixture(); let period = week(); let t = period.start();
        run(&db, "r", "t", "grok", t, None);
        for (i, offset) in [0., 360_000., 420_000.].into_iter().enumerate() {
            let mut payload = timing(t + offset, Some(HOUR - offset), "input", "cargo test -p x");
            payload["exitCode"] = json!(1);
            item(&db, i as i64, ("t", Some("r")), "command_execution", if i == 0 { "failed" } else { "completed" }, payload);
        }
        item(&db, 3, ("t", Some("r")), "command_execution", "completed", timing(t, Some(500.), "input", "rg foo"));
        let r = read_report(&db, &period);
        assert_eq!((r.calls, r.closed_together, r.ms, r.busy_ms, r.failed, r.commands.exits.known), (4, 3, 500., 500., 1, 3));
        assert_eq!(r.lengths.iter().map(|b| b.calls).sum::<u32>(), 1);
        assert_eq!(r.longest.len(), 1);
        let cargo = program(&r, Some("cargo"));
        assert_eq!((cargo.closed_together, cargo.median_ms, cargo.longest_ms), (3, None, 0.));
        let rg = program(&r, Some("rg"));
        assert_eq!((rg.closed_together, rg.median_ms, rg.longest_ms), (0, Some(500.), 500.));
    }

    #[test]
    fn closed_together_boundary_and_threads() {
        for (offset, different, expected) in [(1000., false, 0), (1001., false, 2), (1001., true, 0), (0., false, 0)] {
            let db = fixture(); let period = week(); let t = period.start();
            run(&db, "r", "a", "grok", t, None);
            if different { run(&db, "s", "b", "grok", t, None); }
            item(&db, 1, ("a", Some("r")), "file_change", "completed", timing(t, Some(3000.), "toolName", ""));
            item(&db, 2, (if different { "b" } else { "a" }, Some(if different { "s" } else { "r" })), "file_change", "completed", timing(t + offset, Some(3000. - offset), "toolName", ""));
            if offset == 0. { item(&db, 3, ("a", Some("r")), "file_change", "completed", timing(t, Some(3000.), "toolName", "")); }
            let r = read_report(&db, &period);
            assert_eq!(r.closed_together, expected);
            if offset == 0. { assert_eq!((r.ms, r.busy_ms), (9000., 3000.)); }
        }
    }

    #[test]
    fn reversed_end_is_open_not_closed() {
        let db = fixture(); let period = week(); let t = period.start();
        run(&db, "r", "t", "grok", t, None);
        item(&db, 1, ("t", Some("r")), "file_change", "completed", timing(t, Some(1000.), "toolName", ""));
        item(&db, 2, ("t", Some("r")), "file_change", "completed", timing(t + 2000., Some(-1000.), "toolName", ""));
        let r = read_report(&db, &period);
        assert_eq!((r.open, r.closed_together, r.ms), (1, 0, 1000.));
    }

    // 5. Open and reversed calls hold no time, even with a failed status.
    #[test]
    fn open_calls_have_no_length() {
        let db = fixture();
        let period = week();
        let t = period.start();
        run(&db, "r", "t", "codex", t, None);
        item(&db, 1, ("t", Some("r")), "command_execution", "failed", timing(t, None, "input", "cargo test -p x"));
        item(&db, 2, ("t", Some("r")), "file_change", "interrupted", timing(t, Some(-1.), "toolName", "ignored"));
        item(&db, 3, ("t", Some("r")), "web_search", "running", json!({"startedAt": iso(t), "completedAt": 42}));
        let r = read_report(&db, &period);
        assert_eq!((r.calls, r.open, r.failed, r.ms, r.busy_ms, r.threads), (3, 3, 1, 0., 0., 1));
        assert!(r.lengths.iter().all(|b| b.calls == 0) && r.longest.is_empty());
        assert!(r.kinds.iter().all(|row| row.longest_ms == 0. && row.median_ms.is_none()));
    }

    // 6. Both child routes, with and without the materialized subagent columns.
    #[test]
    fn calls_without_run_use_both_child_routes() {
        let period = week();
        let (lo, hi) = (period.start(), period.until());
        for columns in [false, true] {
            let db = fixture();
            if columns {
                for name in ["child_thread_id", "started_at", "completed_at"] {
                    db.execute_batch(&format!("ALTER TABLE {P}subagents ADD COLUMN {name} TEXT")).unwrap();
                }
            }
            for (i, (id, provider, lineage, updated, window)) in [
                ("lineage", "claudeAgent", true, lo, None),
                ("row", "CoDeX", true, lo - 1., Some((lo - HOUR, Some(lo)))),
                ("both", "opencode", true, hi, Some((lo, None))),
                ("neither", "grok", false, hi, Some((hi, None))),
                ("stale", "grok", true, lo - 1., Some((lo - HOUR, Some(lo - 1.)))),
                ("future", "grok", true, hi, None),
            ].into_iter().enumerate() {
                let created = if id == "future" { hi } else { lo - HOUR };
                thread(&db, id, provider, created, updated, lineage);
                item(&db, i as i64, (id, None), "command_execution", "completed",
                    timing(lo, Some(42.), "input", "cargo test -p x"));
                if let Some((start, end)) = window {
                    if columns {
                        db.execute(&format!("INSERT INTO {P}subagents(subagent_id,thread_id,payload_json,child_thread_id,started_at,completed_at)
                                             VALUES(?1,'parent',?2,?1,?3,?4)"),
                            params![id, json!({"childThreadId": "wrong", "startedAt": iso(hi)}).to_string(), iso(start), end.map(iso)]).unwrap();
                    } else {
                        db.execute(&format!("INSERT INTO {P}subagents VALUES(?1,'parent',?2)"),
                            params![id, json!({"childThreadId": id, "startedAt": iso(start), "completedAt": end.map(iso)}).to_string()]).unwrap();
                    }
                }
            }
            run(&db, "r", "both", "grok", lo, None);
            item(&db, 7, ("both", Some("r")), "file_change", "completed", timing(lo, Some(100.), "toolName", "ignored"));
            let r = read_report(&db, &period);
            assert_eq!((r.calls, r.without_run, r.threads), (4, 3, 3));
            assert_eq!(r.providers.iter().map(|p| (p.provider.as_str(), p.calls, p.commands)).collect::<Vec<_>>(),
                vec![("claude", 1, 1), ("codex", 1, 1), ("grok", 1, 0), ("opencode", 1, 1)]);
            assert_eq!(r.longest.iter().filter(|call| call.run.is_none()).count(), 3);
        }
    }

    // 7. Coalesce across runs on one thread, but never across threads.
    #[test]
    fn overlapping_calls_are_merged_per_thread() {
        let period = week();
        let t = period.start();
        for (second, busy_ms) in [("a", 1500.), ("b", 2000.)] {
            let db = fixture();
            run(&db, "r1", "a", "codex", t, None);
            run(&db, "r2", second, "codex", t, None);
            item(&db, 1, ("a", Some("r1")), "file_change", "completed", timing(t, Some(1000.), "toolName", ""));
            item(&db, 2, (second, Some("r2")), "file_change", "completed", timing(t + 500., Some(1000.), "toolName", ""));
            let r = read_report(&db, &period);
            assert_eq!((r.ms, r.busy_ms), (2000., busy_ms));
            assert_eq!(r.longest.iter().map(|call| call.started).collect::<Vec<_>>(), vec![t, t + 500.]);
        }
    }

    // 8. Names are filtered before classification and split for tool rows and longest.
    #[test]
    fn only_safe_tool_names_leave_sql() {
        let db = fixture();
        let period = week();
        let t = period.start();
        run(&db, "r", "t", "codex", t, None);
        let inputs = [json!("mcp__t3-code__delegate_task"), json!("t3-code.delegate_task"),
            json!("run this: \"never read\""), json!("x".repeat(81)), json!(""), json!("Read\0never read"),
            json!(123), json!("codex_apps.github.fetch"), json!("mcp____tool"), json!("delegate_task"),
            json!("View_Image"), json!("Grep"), json!("ToolSearch"), json!("mcp__fs__read")];
        for (i, name) in inputs.into_iter().enumerate() {
            item(&db, i as i64, ("t", Some("r")), "dynamic_tool", "completed", timing(t, Some(100.), "toolName", name));
        }
        let r = read_report(&db, &period);
        for mode in [skim::Mode::Full, skim::Mode::Early] {
            assert_eq!(summarize(&read_with(&db, Path::new("test"), &period, Some(mode)).unwrap(), &period, &utc), r, "{mode:?}");
        }
        assert!(r.tools.iter().any(|row| row.kind == Kind::Tool && row.name.as_deref() == Some("delegate_task")
            && row.server.as_deref() == Some("t3-code") && row.calls == 2));
        assert!(r.tools.iter().any(|row| row.kind == Kind::Tool && row.name.is_none() && row.calls == 5));
        // A tool is a tool whatever its name contains: not a web call for "fetch", not an agent for "delegate".
        assert!(r.tools.iter().any(|row| row.kind == Kind::Tool && row.server.as_deref() == Some("codex_apps.github") && row.name.as_deref() == Some("fetch")));
        assert!(r.tools.iter().any(|row| row.kind == Kind::Tool && row.server.is_none() && row.name.as_deref() == Some("delegate_task")));
        assert!(r.tools.iter().any(|row| row.server.is_none() && row.name.as_deref() == Some("mcp____tool")));
        // A provider's own tool is a read or a search by its exact name, in any case; a server's tool never is.
        let kind = |name: &str| r.tools.iter().find(|row| row.name.as_deref() == Some(name)).map(|row| (row.kind, row.server.as_deref()));
        assert_eq!(kind("View_Image"), Some((Kind::Read, None)));
        assert_eq!(kind("Grep"), Some((Kind::Search, None)));
        assert_eq!(kind("ToolSearch"), Some((Kind::Tool, None)));
        assert_eq!(kind("read"), Some((Kind::Tool, Some("fs"))));
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("never") && !json.contains(&"x".repeat(81)) && !json.contains("\"Read\"") && !json.contains("\"123\""));
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!("loupe-tools-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
    }

    // 9. Check the actual SQL as well as Facts, the report, and errors from bad sources.
    #[test]
    fn no_text_leaves() {
        let db = fixture();
        let period = week();
        let t = period.start();
        thread(&db, "t", "claudeAgent", t, t, true);
        run(&db, "r", "t", "codex", t, None);
        for (id, scope, input) in [(1, ("t", Some("r")), "cargo test -p x # never read"), (2, ("t", None), "./script 'never read'")] {
            let mut payload = timing(t, Some(50.), "input", input);
            for field in ["output", "text", "title", "fileName", "pattern", "patterns", "prompt",
                "message", "summary", "result", "results", "questions", "changes", "toolName"] {
                payload[field] = json!("never read");
            }
            item(&db, id, scope, "command_execution", "completed", payload);
        }
        let facts = read_with(&db, Path::new("test"), &period, None).unwrap();
        let r = summarize(&facts, &period, &utc);
        assert_eq!((r.calls, r.commands.named, r.commands.unnamed), (2, 1, 1));
        assert_eq!(program(&r, Some("cargo")).calls, 1);
        assert!(!format!("{facts:?}").contains("never"));
        assert!(!serde_json::to_string(&r).unwrap().contains("never"));
        // The same through the skim, which also never gives the sentinel back from t3_call itself:
        // not from a non-identifier tool name, nor from beside a listed program.
        db.pragma_update(None, "query_only", false).unwrap();
        item(&db, 3, ("t", Some("r")), "dynamic_tool", "completed", timing(t, Some(50.), "toolName", "never read"));
        let with_tool = read_report(&db, &period);
        for mode in [skim::Mode::Full, skim::Mode::Early, skim::Mode::Head] {
            let facts = read_with(&db, Path::new("test"), &period, Some(mode)).unwrap();
            assert!(!format!("{facts:?}").contains("never"), "{mode:?}");
            assert_eq!(summarize(&facts, &period, &utc), with_tool, "{mode:?}");
            let function = if mode == skim::Mode::Head { "t3_call_head(rowid, typeof(payload_json)," } else { "t3_call(payload_json," };
            let values: Vec<Option<String>> = db.prepare(&format!("SELECT {function} type, ?1, ?2) FROM {P}turn_items")).unwrap()
                .query_map(params![period.start(), period.until()], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            assert_eq!(values.iter().flatten().count(), 3);
            assert!(values.iter().flatten().all(|value| !value.contains("never")), "{values:?}");
        }
        let dir = Scratch::new();
        let path = dir.0.join("privacy.sqlite");
        db.pragma_update(None, "query_only", false).unwrap();
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        for mode in [skim::choice(Some("off")), skim::choice(None), skim::choice(Some("early")), skim::choice(Some("head"))] {
            let facts = read_parallel(recap::open(&path).unwrap(), &path, &period, 4, mode).unwrap();
            assert!(!format!("{facts:?}").contains("never"), "{mode:?}");
            let report = summarize(&facts, &period, &utc);
            assert!(!serde_json::to_string(&report).unwrap().contains("never"), "{mode:?}");
            assert_eq!(report, with_tool, "{mode:?}");
        }

        for columns in [["child_thread_id", "started_at", "completed_at"],
            ["json_extract(payload_json,'$.childThreadId')", "json_extract(payload_json,'$.startedAt')", "json_extract(payload_json,'$.completedAt')"]] {
            for mode in [skim::Mode::Full, skim::Mode::Early] {
                let sql = queries(columns[0], columns[1], columns[2], Some(mode));
                for statement in [&sql.run_calls, &sql.child_calls] {
                    assert_eq!(statement.matches("payload_json").count(), 1, "{statement}");
                    assert_eq!(statement.matches("t3_call(payload_json,").count(), 1, "{statement}");
                    assert!(!statement.contains("json_extract") && !statement.contains("$.") && !statement.contains("SELECT *"), "{statement}");
                }
            }
            // The head form: the payload whole only for a command, as t3_call's first argument;
            // for every other call the row, and its value's type, which loads nothing.
            let sql = queries(columns[0], columns[1], columns[2], Some(skim::Mode::Head));
            for statement in [&sql.run_calls, &sql.child_calls] {
                assert_eq!(statement.matches("payload_json").count(), 2, "{statement}");
                assert_eq!(statement.matches("CASE WHEN type='command_execution' THEN t3_call(payload_json, type, ?2, ?3)").count(), 1, "{statement}");
                assert_eq!(statement.matches("ELSE t3_call_head(rowid, typeof(payload_json), type, ?2, ?3) END").count(), 1, "{statement}");
                assert!(!statement.contains("json_extract") && !statement.contains("$.") && !statement.contains("SELECT *"), "{statement}");
            }
            // The off form filters on no payload text: a call is in the period by its instant.
            let off = queries(columns[0], columns[1], columns[2], None);
            for statement in [&off.run_calls, &off.child_calls] {
                let filter = &statement[statement.find("WHERE").unwrap()..];
                assert!(!filter.contains("json_extract") && !filter.contains("payload_json"), "{statement}");
            }
            let sql = queries(columns[0], columns[1], columns[2], None);
            let safe = "t3_programs(json_extract(payload_json,'$.input'))";
            for statement in [&sql.run_calls, &sql.child_calls] {
                assert_eq!(statement.matches(safe).count(), 1);
                assert_eq!(statement.matches("CASE WHEN type='command_execution' AND json_type(payload_json,'$.exitCode')='integer' THEN json_extract(payload_json,'$.exitCode') END").count(), 1);
                assert_eq!(statement.matches("json_type(payload_json,'$.exitCode')").count(), 1);
            }
            for statement in [sql.runs.as_str(), &sql.run_calls, &sql.child_calls, &sql.children, &sql.first_record, HAS_COLUMN, PIN_SNAPSHOT] {
                let rest = statement.replace(safe, "");
                let path_prefix = "$.";
                for field in ["input", "output", "text", "title", "fileName", "pattern", "patterns", "prompt",
                    "message", "summary", "result", "results", "questions", "changes"] {
                    assert!(!rest.contains(&format!("{path_prefix}{field}")), "{statement}");
                }
                assert!(!rest.contains("SELECT *") && !rest.contains("SELECT payload_json"));
            }
        }

        let missing = load_uncached_for_test(&dir.0.join("missing.sqlite"), &period, &utc).unwrap_err();
        let corrupt = dir.0.join("corrupt.sqlite");
        fs::write(&corrupt, "never read").unwrap();
        let unreadable = load_uncached_for_test(&corrupt, &period, &utc).unwrap_err();
        let bad_schema = read_from(&Connection::open_in_memory().unwrap(), Path::new("test"), &period).unwrap_err();
        let db = fixture();
        db.execute(&format!("INSERT INTO {P}threads(thread_id,payload_json) VALUES('t','never read')"), []).unwrap();
        let bad_payload = read_from(&db, Path::new("test"), &period).unwrap_err();
        assert!(db.is_autocommit(), "the failed read left a transaction open");
        let mut errors = vec![(missing, "missing"), (unreadable, "unreadable"), (bad_schema, "schema"), (bad_payload, "schema")];
        // A call's payload the skim cannot read, the sentinel inside it: malformed, JSON5, a BLOB.
        for payload in ["{\"startedAt\":\"2026-11-10T00:00:00.000Z\",\"output\":\"never read", "{\"startedAt\":\"2026-11-10T00:00:00.000Z\",'output':'never read'}"] {
            for (mode, kind) in [(skim::Mode::Full, "command_execution"), (skim::Mode::Early, "command_execution"),
                (skim::Mode::Head, "command_execution"), (skim::Mode::Head, "file_change"), (skim::Mode::Head, "dynamic_tool")] {
                let db = fixture();
                run(&db, "r", "t", "codex", t, None);
                db.execute(&format!("INSERT INTO {P}turn_items(item_id,thread_id,run_id,type,status,ordinal,payload_json) VALUES(1,'t','r',?2,'completed',1,?1)"), [payload, kind]).unwrap();
                let strict = read_with(&db, Path::new("test"), &period, Some(mode)).unwrap_err();
                assert_eq!(strict.detail, skim::Fault::NotStrict.to_string(), "{mode:?} {kind}");
                errors.push((strict, "schema"));
                db.pragma_update(None, "query_only", false).unwrap();
                db.execute(&format!("UPDATE {P}turn_items SET payload_json=CAST(payload_json AS BLOB)"), []).unwrap();
                let text = read_with(&db, Path::new("test"), &period, Some(mode)).unwrap_err();
                assert_eq!(text.detail, skim::Fault::NotText.to_string(), "{mode:?} {kind}");
                errors.push((text, "schema"));
                assert!(db.is_autocommit(), "the failed read left a transaction open");
            }
        }
        for (error, kind) in errors {
            assert_eq!(error.kind, kind);
            assert!(!format!("{error} {error:?}").contains("never"));
        }
    }

    // 10. Both the file connection and a caller-provided writable connection refuse writes.
    #[test]
    fn reader_is_read_only() {
        let (db, period) = by_hand();
        let dir = Scratch::new();
        let path = dir.0.join("statev2.sqlite");
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let before = fs::read(&path).unwrap();
        let disk = recap::open(&path).unwrap();
        let delete = format!("DELETE FROM {P}runs");
        assert!(disk.execute(&delete, []).is_err());
        assert_eq!(load_uncached_for_test(&path, &period, &utc).unwrap().calls, 6);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(read_from(&db, Path::new("test"), &period).unwrap().calls.len(), 6);
        assert!(db.is_autocommit());
        assert!(db.execute(&delete, []).is_err());
    }

    fn report_with(path: &Path, period: &Period, workers: usize, mode: Option<skim::Mode>) -> Tools {
        let db = recap::open(path).unwrap();
        let facts = if workers == 1 {
            read_with(&db, path, period, mode)
        } else {
            read_parallel(db, path, period, workers, mode)
        }.unwrap();
        summarize(&facts, period, &utc)
    }

    fn check_parallel(db: &Connection, period: &Period, repetitions: usize) {
        let dir = Scratch::new();
        let path = dir.0.join("tools.sqlite");
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let before = fs::read(&path).unwrap();
        let expected = read_report(&recap::open(&path).unwrap(), period);
        for workers in [1, 2, 3, 4, 8] {
            for setting in ["off", "full", "early", "head"] {
                for _ in 0..repetitions {
                    assert_eq!(report_with(&path, period, workers, skim::choice(Some(setting))), expected, "{workers} workers, skim={setting}");
                }
            }
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn parallel_keeps_scope_order() {
        let db = fixture();
        let period = week();
        let t = period.start();
        let mut id = 0;
        for n in 0..42 {
            let thread_id = format!("t{n}");
            let run_id = format!("r{n}");
            let provider = if n % 2 == 0 { "codex" } else { "claudeAgent" };
            thread(&db, &thread_id, provider, t, t + HOUR, false);
            run(&db, &run_id, &thread_id, provider, t, None);
            let scope = (thread_id.as_str(), Some(run_id.as_str()));
            // The longest calls tie across scopes. The report keeps the query order
            // for those ties, even though the first run has much more work to do.
            id += 1;
            item(&db, id, scope, "file_change", "completed", timing(t, Some(HOUR), "toolName", ""));
            id += 1;
            item(&db, id, scope, "file_search", "completed", timing(t, Some(HOUR), "toolName", ""));
            let count = if n == 0 { 800 } else { n % 13 };
            for i in 0..count {
                id += 1;
                let end = (i % 3 != 2).then_some(8_000. - (i % 3) as f64 * 2_000.);
                let status = if end.is_none() { "running" } else { "failed" };
                let mut payload = timing(t + 2_000. * (i % 3) as f64, end, "input", "rg needle");
                if end.is_some() { payload["exitCode"] = json!(if i % 2 == 0 { 1 } else { 127 }); }
                item(&db, id, scope, "command_execution", status, payload);
            }
        }
        for (child, lineage) in [("lineage", true), ("subagent", false)] {
            thread(&db, child, "opencode", t, t + HOUR, lineage);
            if !lineage {
                db.execute(&format!("INSERT INTO {P}subagents VALUES('subagent','t0',?1)"),
                    [json!({"childThreadId": child, "startedAt": iso(t)}).to_string()]).unwrap();
            }
            id += 1;
            item(&db, id, (child, None), "command_execution", "completed", timing(t, Some(HOUR), "input", "cargo test"));
        }
        check_parallel(&db, &period, 4);
        let report = read_report(&db, &period);
        assert_eq!(report.without_run, 2);
        assert!(report.open > 0 && report.closed_together > 0 && report.commands.exits.failed_known > 0);
    }

    #[test]
    fn parallel_matches_seeded_calls() {
        let db = fixture();
        let period = week();
        let t = period.start();
        let kinds = ["command_execution", "dynamic_tool", "file_change", "file_search", "web_search"];
        let statuses = ["completed", "failed", "running"];
        let providers = ["codex", "claudeAgent", "grok", "opencode"];
        let runs: Vec<_> = (0..63).map(|n| format!("r{n}")).collect();
        for (n, name) in runs.iter().enumerate() {
            let provider = providers[n % providers.len()];
            thread(&db, name, provider, t, t + HOUR, n % 4 == 0);
            run(&db, name, name, provider, t, None);
        }
        for id in 0..3_600 {
            let seed = 5_123 + id as u64 * 6;
            let draw = |offset, count: usize| (chance(seed + offset) * count as f64) as usize;
            let n = draw(0, runs.len());
            let kind = kinds[draw(1, kinds.len())];
            let status = statuses[draw(2, statuses.len())];
            let start = t + draw(3, 3_600_000) as f64;
            let length = (status != "running").then(|| draw(4, 900_000) as f64);
            let payload = if kind == "command_execution" {
                let (line, _, _) = LINES[draw(5, LINES.len())];
                let mut payload = timing(start, length, "input", line);
                payload["exitCode"] = json!(if status == "failed" { 2 } else { 0 });
                payload
            } else {
                timing(start, length, "toolName", "mcp__fs__read")
            };
            let run = (n % 4 != 0 || id % 2 == 0).then_some(runs[n].as_str());
            item(&db, id, (&runs[n], run), kind, status, payload);
        }
        check_parallel(&db, &period, 4);
    }

    #[test]
    fn parallel_with_one_scope_or_none() {
        let period = week();
        check_parallel(&fixture(), &period, 1);
        let (db, period) = by_hand();
        check_parallel(&db, &period, 1);
    }

    #[test]
    fn parallel_worker_error_and_read_only() {
        let (db, period) = by_hand();
        let dir = Scratch::new();
        let path = dir.0.join("tools.sqlite");
        db.execute(&format!("INSERT INTO {P}turn_items(item_id,thread_id,run_id,type,ordinal,payload_json)
                            VALUES('bad','t','r','command_execution',10,'{{')"), []).unwrap();
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let before = fs::read(&path).unwrap();
        let serial = read_with(&recap::open(&path).unwrap(), &path, &period, None).unwrap_err();
        for mode in [None, Some(skim::Mode::Full), Some(skim::Mode::Early)] {
            let same_mode = read_with(&recap::open(&path).unwrap(), &path, &period, mode).unwrap_err();
            let parallel = read_parallel(recap::open(&path).unwrap(), &path, &period, 4, mode).unwrap_err();
            assert_eq!((parallel.kind, parallel.detail), (serial.kind, same_mode.detail), "{mode:?}");
        }
        let disk = recap::open(&path).unwrap();
        let tx = disk.unchecked_transaction().unwrap();
        assert!(tx.execute(&format!("DELETE FROM {P}runs"), []).is_err());
        assert_eq!(tx.query_row(&format!("SELECT count(*) FROM {P}turn_items"), [], |r| r.get::<_, i64>(0)).unwrap(), 7);
        tx.rollback().unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    // 11. Use the checked-in schemas unchanged. Legacy 054 predates these projections.
    #[test]
    fn schema_fixtures() {
        let fixtures = [
            ("main-096", include_str!("../../../checks/fixtures/t3-schema-main-096.sql")),
            ("replayed-083", include_str!("../../../checks/fixtures/t3-schema-replayed-083.sql")),
            ("older-074", include_str!("../../../checks/fixtures/t3-schema-older-074.sql")),
            ("legacy-054", include_str!("../../../checks/fixtures/t3-schema-legacy-054.sql")),
        ];
        let period = week();
        let dir = Scratch::new();
        let t = iso(period.start());
        let payload = timing(period.start(), Some(500.), "input", "cargo test -p x").to_string();
        for (name, schema) in fixtures {
            let path = dir.0.join(format!("{name}.sqlite"));
            let db = Connection::open(&path).unwrap();
            db.execute_batch(schema).unwrap();
            if name == "legacy-054" {
                assert_eq!(load_uncached_for_test(&path, &period, &utc).unwrap_err().kind, "schema");
                continue;
            }
            for thread in ["t", "child"] {
                db.execute(&format!("INSERT INTO {P}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json)
                                     VALUES(?1,'project','ignored','codex','regular','default',?2,?2,?3)"),
                    params![thread, t, json!({"createdAt": t, "providerInstanceId": "claudeAgent"}).to_string()]).unwrap();
            }
            db.execute(&format!("INSERT INTO {P}runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json)
                                 VALUES('r','t',1,'codex','completed',?1,'{{\"providerInstanceId\":\"codex\"}}')"), [&t]).unwrap();
            db.execute(&format!("INSERT INTO {P}subagents(subagent_id,thread_id,run_id,parent_node_id,provider,child_thread_id,origin,status,started_at,updated_at,payload_json)
                                 VALUES('s','t','r','node','claudeAgent','child','provider_native','running',?1,?1,'{{}}')"), [&t]).unwrap();
            for (id, thread, run) in [("i", "t", Some("r")), ("child-item", "child", None)] {
                db.execute(&format!("INSERT INTO {P}turn_items(turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json)
                                     VALUES(?1,?2,?3,1,'command_execution','completed',?4,?5)"), params![id, thread, run, t, payload]).unwrap();
            }
            drop(db);
            let r = load_uncached_for_test(&path, &period, &utc).unwrap();
            assert_eq!((r.calls, r.ms, r.without_run, r.providers.len()), (2, 1000., 1, 2), "{name}");
        }
    }

    // 12. A reproducible full demo, with bounded work on partial weeks and months.
    #[test]
    fn the_made_up_period() {
        let period = week();
        let r = demo(&period, &utc).unwrap();
        assert_eq!(r, demo(&period, &utc).unwrap());
        assert_eq!((r.kinds.len(), r.open, r.days_recorded, r.providers.len()), (6, 2, 7, 4));
        assert!((5800..6200).contains(&r.calls));
        assert!((4000..4400).contains(&r.commands.calls));
        assert!((0.025..0.04).contains(&(f64::from(r.commands.failed) / f64::from(r.commands.calls))));
        assert_eq!(r.closed_together, 5);
        let claude = r.providers.iter().find(|p| p.provider == "claude").unwrap();
        let codex = r.providers.iter().find(|p| p.provider == "codex").unwrap();
        let opencode = r.providers.iter().find(|p| p.provider == "opencode").unwrap();
        let grok = r.providers.iter().find(|p| p.provider == "grok").unwrap();
        assert_eq!(grok.closed_together, 5);
        assert!(claude.calls > codex.calls && codex.calls > opencode.calls && opencode.calls > grok.calls);
        assert!(claude.exits.known * 100 < claude.commands * 2);
        assert_eq!(codex.exits.known, codex.commands);
        assert_eq!(opencode.exits.known, opencode.commands);
        assert!(grok.exits.known > grok.commands * 3 / 4);
        let exits = &r.commands.exits;
        assert!(exits.known > r.commands.calls / 3 && exits.known < r.commands.calls * 2 / 3);
        assert!([exits.one, exits.two, exits.timed_out, exits.not_runnable,
            exits.not_found, exits.signal, exits.other].iter().all(|&count| count >= 1), "{exits:?}");
        assert!(exits.one * 2 > exits.failed_known && exits.two < exits.one);
        assert!(r.without_run > 0 && r.busy_ms < r.ms);
        assert!(r.providers.iter().filter(|p| p.failed > 0).count() >= 3);

        assert!(r.programs.iter().filter(|row| row.calls > 0).count() >= 28);
        let unnamed = r.programs.iter().position(|row| row.name.is_none()).unwrap();
        assert!((1..8).contains(&unnamed));
        assert!((0.03..0.05).contains(&(f64::from(r.programs[unnamed].calls) / f64::from(r.commands.calls))));
        let leaders: Vec<_> = r.programs.iter().filter_map(|row| row.name.as_deref()).take(7).collect();
        assert_eq!(leaders, ["python3", "cargo", "npm", "pytest", "node", "make", "docker"]);
        for name in ["rg", "grep"] {
            let row = program(&r, Some(name));
            assert!((0.07..0.15).contains(&(f64::from(row.failed) / f64::from(row.calls))), "{name}");
            assert!(row.exits.one * 10 >= row.exits.failed_known * 9, "{name}");
        }
        assert!(program(&r, Some("diff")).exits.one >= 2);
        let ordinary = r.programs.iter().filter(|row| !matches!(row.name.as_deref(), Some("rg" | "grep" | "diff")));
        let one: u32 = ordinary.clone().map(|row| row.exits.one).sum();
        let two: u32 = ordinary.clone().map(|row| row.exits.two).sum();
        let failed_known: u32 = ordinary.map(|row| row.exits.failed_known).sum();
        assert!(two > 0 && one >= 2 * two && one * 2 > failed_known);

        let typical = r.programs.iter().filter(|row| row.name.is_some() && row.calls >= 20)
            .filter_map(|row| row.median_ms);
        assert!(typical.clone().filter(|&ms| ms >= 1000.).count() >= 5);
        assert!(typical.filter(|&ms| ms >= 10_000.).count() >= 2);
        for name in ["sed", "git", "cat"] {
            assert!(program(&r, Some(name)).median_ms.is_some_and(|ms| ms < 1000.), "{name}");
        }

        assert!(r.tools.iter().filter(|row| row.name.is_some()).count() >= 20);
        assert!(r.tools.iter().filter(|row| row.server.is_some()).count() >= 3);
        assert!(r.tools.iter().any(|row| row.name.is_some() && row.median_ms.is_some_and(|ms| ms >= 60_000.)));
        assert!(r.tools.iter().any(|row| row.kind == Kind::Tool && row.name.is_none()));
        let delegates: Vec<_> = r.tools.iter().filter(|row| row.name.as_deref() == Some("delegate_task")).collect();
        assert_eq!(delegates.len(), 1);
        assert_eq!(delegates[0].server.as_deref(), Some("t3-code"));
        for provider in ["claude", "codex"] {
            assert!(delegates[0].by_provider.iter().any(|share| share.provider == provider));
        }

        assert_eq!(r.longest.len(), 10);
        let names: std::collections::BTreeSet<_> = r.longest.iter().map(|call| call.name.as_deref()).collect();
        let providers: std::collections::BTreeSet<_> = r.longest.iter().map(|call| &call.provider).collect();
        assert!(names.len() >= 4 && providers.len() >= 3);
        for code in [124, 137] {
            assert!(r.longest.iter().any(|call| call.failed && call.exit == Some(code)));
        }
        let mut endings = std::collections::BTreeSet::new();
        for call in &r.longest {
            assert_eq!(call.thread.len(), 36);
            for (i, byte) in call.thread.bytes().enumerate() {
                if [8, 13, 18, 23].contains(&i) {
                    assert_eq!(byte, b'-');
                } else {
                    assert!(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
                }
            }
            assert!(endings.insert(&call.thread[28..]));
        }
        assert!((4500..5100).contains(&r.lengths[0].calls));
        assert!(r.lengths[3..].iter().map(|b| b.ms).sum::<f64>() > r.ms / 2.);
        assert!((120..220).contains(&r.lengths[3..].iter().map(|b| b.calls).sum::<u32>()));
        assert!((6..16).contains(&r.lengths[4].calls));
        assert_eq!((r.commands.listed, r.min_for_rate), (75, 20));

        let month = Period::new(ms("2026-11-12T12:00:00.000Z").unwrap(), Which::LastMonth, &utc);
        let last_month = demo(&month, &utc).unwrap();
        assert_eq!(last_month.longest.len(), 10);
        let names: std::collections::BTreeSet<_> = last_month.longest.iter().map(|call| call.name.as_deref()).collect();
        assert!(names.len() >= 6);
        let lengths: std::collections::BTreeSet<_> = last_month.longest.iter().map(|call| call.ms as u64).collect();
        let starts: std::collections::BTreeSet<_> = last_month.longest.iter()
            .map(|call| call.when.rsplit_once(' ').unwrap().1).collect();
        assert_eq!((lengths.len(), starts.len()), (10, 10));

        for (which, middle) in [(Which::ThisWeek, "2026-11-12T12:00:00.000Z"), (Which::ThisMonth, "2026-11-16T12:00:00.000Z")] {
            let mut period = Period::new(ms(middle).unwrap(), which, &utc);
            let middle = period.now;
            period.now = period.start() + 60_000.;
            let early = demo(&period, &utc).unwrap();
            assert!(early.is_empty());
            assert_eq!((early.days_elapsed, early.days_recorded), (1, 0));
            period.now = middle;
            let part = demo(&period, &utc).unwrap();
            assert!(part.calls > 0);
            period.now = period.end() - 60_000.;
            let end = demo(&period, &utc).unwrap();
            assert!(end.calls > part.calls && end.days_elapsed == end.days);
            assert!(end.longest.iter().all(|call| call.started < period.until()));
            if which == Which::ThisMonth {
                assert!(end.longest[0].when.contains(','));
            }
            let monday = Period::new(ms("2026-11-09T10:00:00.000Z").unwrap(), Which::ThisWeek, &utc).now;
            for now in [period.start() + 60_000., middle, period.end() - 60_000., monday] {
                let mut current = period.clone();
                current.now = now;
                let report = demo(&current, &utc).unwrap();
                assert!(report.longest.iter().all(|call| call.started + call.ms <= current.until()));
                assert!(report.programs.iter().all(|row| row.longest_ms <= current.until() - current.start()));
                if which == Which::ThisWeek && now == monday {
                    assert!(report.open >= 1);
                    assert!(report.ms <= (current.until() - current.start()) * f64::from(report.threads));
                }
            }
        }
    }

    // The programs whose exit code 1 the report reads as an answer are on the shared list.
    #[test]
    fn the_exit_one_readings_name_listed_programs() {
        for name in FOUND_NOTHING.iter().chain(&FILES_DIFFER) {
            assert!(program::listed(name).is_some(), "{name}");
        }
    }

    // The value reads back from its own JSON: the view's tests load saved values this way.
    #[test]
    fn the_value_reads_back_from_its_json() {
        let r = demo(&week(), &utc).unwrap();
        let back: Tools = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    // 13. The SQL callback accepts every SQLite input type, including invalid UTF-8 text.
    #[test]
    fn names_sql_function_is_total() {
        let db = Connection::open_in_memory().unwrap();
        program::register(&db).unwrap();
        for sql in ["NULL", "42", "1.5", "x'636172676f'", "''", "cast(x'ff' as text)", "'unknown command'"] {
            let got: Option<String> = db.query_row(&format!("SELECT t3_programs({sql})"), [], |r| r.get(0)).unwrap();
            assert_eq!(got, None, "{sql}");
        }
        for &(line, names, expected_label) in LINES {
            let got: Option<String> = db.query_row("SELECT t3_programs(?1)", [line], |r| r.get(0)).unwrap();
            assert_eq!(got, (!names.is_empty()).then(|| names.join(" ")), "{line}");
            assert_eq!(program::label(names), expected_label, "{line}");
            assert_eq!(program::programs(line), names, "{line}");
        }
    }

    // ── The skim (skim.rs) against today's read ────────────────────────────────

    fn item_stored(db: &Connection, id: i64, scope: (&str, Option<&str>), kind: &str, status: &str, payload: &skim::cases::Stored) {
        use rusqlite::types::Value as Sql;
        use skim::cases::Stored;
        let (value, expr) = match payload {
            Stored::Text(bytes) => (Sql::Blob(bytes.clone()), "CAST(?6 AS TEXT)"),
            Stored::Blob(bytes) => (Sql::Blob(bytes.clone()), "?6"),
            Stored::Integer(n) => (Sql::Integer(*n), "?6"),
            Stored::Real(f) => (Sql::Real(*f), "?6"),
            Stored::Null => (Sql::Null, "?6"),
        };
        db.execute(&format!("INSERT INTO {P}turn_items(item_id,thread_id,run_id,type,status,ordinal,payload_json)
                            VALUES(?1,?2,?3,?4,?5,?1,{expr})"), params![id, scope.0, scope.1, kind, status, value]).unwrap();
    }

    fn facts(db: &Connection, period: &Period, mode: Option<skim::Mode>) -> Result<String, String> {
        read_with(db, Path::new("test"), period, mode).map(|facts| format!("{facts:?}")).map_err(|error| {
            assert!(error.kind == "schema" && !format!("{error} {error:?}").contains("never"), "{error:?}");
            error.detail
        })
    }

    // One payload in one call row, read today and through the skim in both modes: the same
    // Facts, or both refused, with the skim's two exceptions (it refuses JSON5 and a BLOB; early
    // misses a fault past where it stops, and is misled by a key order T3 does not write).
    fn read_both(case: &skim::cases::Case, period: &Period) -> Result<(), String> {
        use skim::cases::Stored;
        let db = fixture();
        run(&db, "r", "t", "codex", period.start(), None);
        let kinds = ["command_execution", "dynamic_tool", "file_change", "file_search", "web_search"];
        let status = if case.kind.len() % 2 == 0 { "failed" } else { "completed" };
        item_stored(&db, 1, ("t", Some("r")), case.kind, status, &case.payload);
        let today = facts(&db, period, None);
        let full = facts(&db, period, Some(skim::Mode::Full));
        let early = facts(&db, period, Some(skim::Mode::Early));
        let head = facts(&db, period, Some(skim::Mode::Head));
        let text = matches!(case.payload, Stored::Text(_));
        let refused = kinds.contains(&case.kind) && (case.json5 && text || matches!(case.payload, Stored::Blob(_)));
        // A head read gives the full pass's value or early's, never a third (skim.rs, "Head reads").
        let same = |a: &Result<String, String>, b: &Result<String, String>| a == b || a.is_err() && b.is_err();
        if !same(&head, &full) && !same(&head, &early) { return Err(format!("head {head:?}\nfull {full:?}\nearly {early:?}")); }
        if case.in_t3_order() && !refused && today.is_ok() && head != today { return Err(format!("head {head:?}\ntoday {today:?}, keys in T3's order")); }
        if refused {
            if full.is_ok() { return Err(format!("full read what it must refuse: {full:?}")); }
        } else if today != full && !(today.is_err() && full.is_err()) {
            return Err(format!("full {full:?}\ntoday {today:?}"));
        }
        match (&today, &early) {
            (Ok(t), Ok(e)) if t != e && case.in_t3_order() => Err(format!("early {e:?}\ntoday {t:?}")),
            (Ok(_), Err(e)) if !refused => Err(format!("early refused a valid payload: {e}")),
            _ => Ok(()),
        }
    }

    fn skim_payloads(seed: u64, count: usize, big: bool) {
        let period = week();
        let (from, to) = (iso(period.start()), iso(period.until()));
        let mut rng = skim::cases::Rng(seed);
        for i in 0..count {
            let case = skim::cases::case(&mut rng, &from, &to, big);
            if let Err(why) = read_both(&case, &period) { panic!("seed {seed} case {i}: {why}\n{}", case.describe()); }
        }
    }

    // Whole databases of valid payloads, in runs and in child threads: the same Facts and the
    // same report. Early is compared where every payload keeps T3's key order.
    fn skim_databases(seed: u64, count: usize) {
        let period = week();
        let (from, to) = (iso(period.start()), iso(period.until()));
        let mut rng = skim::cases::Rng(seed);
        for d in 0..count {
            let db = fixture();
            let start = period.start();
            let scopes = [("t0", Some("r0")), ("t1", Some("r1")), ("t2", None), ("t3", None)];
            for ((id, run_id), provider) in scopes.iter().zip(["codex", "claudeAgent", "grok", "opencode"]) {
                thread(&db, id, provider, start, start + HOUR, run_id.is_none());
                if let Some(run_id) = run_id { run(&db, run_id, id, provider, start, None); }
            }
            let ordered_only = d % 2 == 0;
            let mut n = 0;
            while n < 40 + rng.below(200) as i64 {
                let case = skim::cases::case(&mut rng, &from, &to, false);
                if case.json5 || case.malformed || matches!(case.payload, skim::cases::Stored::Blob(_)) || ordered_only && !case.in_t3_order() { continue; }
                let scope = scopes[rng.below(scopes.len())];
                let status = rng.pick(&["completed", "failed", "running", "interrupted"]);
                item_stored(&db, n, scope, case.kind, status, &case.payload);
                n += 1;
            }
            let dir = Scratch::new();
            let path = dir.0.join("tools.sqlite");
            db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
            let today = read_with(&db, Path::new("test"), &period, None).unwrap();
            let report = summarize(&today, &period, &utc);
            let modes: &[skim::Mode] = if ordered_only { &[skim::Mode::Full, skim::Mode::Early, skim::Mode::Head] } else { &[skim::Mode::Full] };
            for &mode in modes {
                let got = read_with(&db, Path::new("test"), &period, Some(mode)).unwrap();
                assert_eq!(format!("{got:?}"), format!("{today:?}"), "seed {seed} database {d} {mode:?}");
                assert_eq!(summarize(&got, &period, &utc), report, "seed {seed} database {d} {mode:?}");
            }
            // The same report from the file, for off, full and (in T3's order) head, at 1 and 4 workers.
            let settings: &[Option<skim::Mode>] = if ordered_only { &[None, Some(skim::Mode::Full), Some(skim::Mode::Head)] } else { &[None, Some(skim::Mode::Full)] };
            for &mode in settings {
                for workers in [1, 4] {
                    assert_eq!(report_with(&path, &period, workers, mode), report, "seed {seed} database {d} {mode:?} {workers} workers");
                }
            }
        }
    }

    #[test]
    fn the_skim_reads_the_same_calls() {
        skim_payloads(7, 4000, false);
        skim_databases(11, 60);
    }

    #[test]
    #[ignore = "a long run: cargo test --release -p loupe-core -- --ignored skim"]
    fn the_skim_reads_the_same_calls_long() {
        for seed in 100..110 {
            skim_payloads(seed, 4000, true);
            skim_databases(seed, 100);
        }
    }

    // The head query loads a payload only for a command. SQLite codes each column read where it
    // is used: in the ELSE branch the value's only use is typeof, so its column read carries
    // OPFLAG_TYPEOFARG (0x80) and skips the content (sqlite3.c, OP_Column). Were the read
    // hoisted out of the CASE, there would be one plain read for both branches.
    #[test]
    fn the_head_query_loads_no_payload_but_a_commands() {
        let db = fixture();
        skim::register(&db, skim::Mode::Head).unwrap();
        let column: i64 = db.query_row(&format!("SELECT cid FROM pragma_table_info('{P}turn_items') WHERE name='payload_json'"), [], |r| r.get(0)).unwrap();
        for sql in [head_calls("run_id=?1", " ORDER BY ordinal, rowid"), head_calls("thread_id=?1 AND run_id IS NULL", "")] {
            let mut q = db.prepare(&format!("EXPLAIN {sql}")).unwrap();
            let reads: Vec<i64> = q.query_map(params![0, 0., 0.], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?, r.get::<_, i64>(6)?))).unwrap()
                .map(Result::unwrap).filter(|(op, p2, _)| op == "Column" && *p2 == column).map(|(_, _, p5)| p5).collect();
            assert_eq!(reads.len(), 2, "{reads:?}");
            assert_eq!(reads.iter().filter(|&&p5| p5 & 0x80 != 0).count(), 1, "{reads:?}");
        }
    }

    #[test]
    fn item_queries_seek_by_run_or_child_thread_on_supported_schemas() {
        for schema in [
            include_str!("../../../checks/fixtures/t3-schema-older-074.sql"),
            include_str!("../../../checks/fixtures/t3-schema-replayed-083.sql"),
            include_str!("../../../checks/fixtures/t3-schema-main-096.sql"),
        ] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(schema).unwrap();
            for mode in [None, Some(skim::Mode::Full), Some(skim::Mode::Early), Some(skim::Mode::Head)] {
                register_call_functions(&db, mode).unwrap();
                let sql = source_queries(&db, mode).unwrap();
                let mut queries = vec![(sql.run_calls, "run_id=?"), (sql.child_calls, "thread_id=?")];
                #[cfg(unix)]
                queries.extend([
                    (call_sql(mode, "run_id=?1", " ORDER BY ordinal, rowid", ", ordinal"), "run_id=?"),
                    (named_call_sql(mode), "run_id=? AND ordinal=?"),
                ]);
                for (sql, key) in queries {
                    let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                    let args = vec![rusqlite::types::Value::Null; stmt.parameter_count()];
                    let plan = stmt.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
                        .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
                    assert!(matches!(plan.as_slice(), [step] if step.starts_with("SEARCH ") && step.contains(key)),
                        "{mode:?}: {sql} produced {plan:?}");
                }
            }
        }
    }

    // Two calls of one run that share an ordinal come in rowid order on every path, and the run
    // query needs no sort for it on T3's schema or the demo's: its index ends in the rowid.
    #[test]
    fn a_runs_ties_go_by_rowid() {
        let (db, period) = (fixture(), week());
        let t = period.start();
        run(&db, "r", "t", "codex", t, None);
        // Item ids that sort against their rowids, all at one ordinal and one start.
        for (rowid, id) in [(3, "a"), (1, "c"), (2, "b")] {
            db.execute(&format!("INSERT INTO {P}turn_items(rowid,item_id,thread_id,run_id,type,status,ordinal,payload_json) VALUES(?1,?2,'t','r','file_change','completed',7,?3)"),
                params![rowid, id, timing(t, Some(1000. * rowid as f64), "toolName", "").to_string()]).unwrap();
        }
        let ends: Vec<Option<f64>> = read_with(&db, Path::new("test"), &period, None).unwrap().calls.iter().map(|call| call.end).collect();
        assert_eq!(ends, [1., 2., 3.].map(|n| Some(t + 1000. * n)).to_vec());
        for mode in [skim::Mode::Full, skim::Mode::Early, skim::Mode::Head] {
            let got: Vec<Option<f64>> = read_with(&db, Path::new("test"), &period, Some(mode)).unwrap().calls.iter().map(|call| call.end).collect();
            assert_eq!(got, ends, "{mode:?}");
        }
        for schema in [DEMO_SCHEMA, include_str!("../../../checks/fixtures/t3-schema-main-096.sql")] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(schema).unwrap();
            for mode in [None, Some(skim::Mode::Full), Some(skim::Mode::Head)] {
                if let Some(mode) = mode { skim::register(&db, mode).unwrap(); } else { program::register(&db).unwrap(); }
                let sql = queries("child_thread_id", "started_at", "completed_at", mode).run_calls;
                let plan: Vec<String> = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap()
                    .query_map(rusqlite::params_from_iter([rusqlite::types::Value::Text("r".into()), 0.0.into(), 1.0.into()].into_iter().take(if mode.is_some() { 3 } else { 1 })
                        .collect::<Vec<rusqlite::types::Value>>()), |r| r.get(3)).unwrap().map(Result::unwrap).collect();
                assert!(plan.iter().any(|step| step.contains("INDEX")), "{plan:?}");
                assert!(!plan.iter().any(|step| step.contains("TEMP B-TREE")), "{mode:?} {plan:?}");
            }
        }
    }
}
