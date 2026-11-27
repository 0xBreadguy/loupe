//! Recap: a week or a month of agent work, worked out from T3's own records each time it is
//! asked for.
//!
//! Nothing here is in the original app (`oracle/`); it is a deliberate addition, and the
//! original rejects `--recap` as it rejects the theme options.
//!
//! The reader is read-only and narrow. It selects identifiers, timestamps, statuses, counts,
//! token and line numbers, and model names. It never selects a prompt, a reply, a result, a
//! failure message, a title, a file path or a command line. Each query below says why it
//! reads what it reads.
use crate::{
    anatomy,
    fanout,
    platform::LocalTime,
    skim,
    source::SourceError,
    terminal::{self, ProviderFamily},
};
#[cfg(unix)]
use crate::cache;
use rusqlite::{Connection, params};
#[cfg(any(unix, test))]
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    time::Duration,
};

const P: &str = "orchestration_v2_projection_";
pub const HOUR: f64 = 3_600_000.;
/// The days of a week, Monday first, as a card names them.
pub const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const TOOLS: [&str; 5] = ["command_execution", "dynamic_tool", "file_change", "file_search", "web_search"];

// ── Time ────────────────────────────────────────────────────────────────────

/// Calendar fields for epoch seconds in the reader's zone. Tests pass a fixed zone.
pub type Local<'a> = &'a dyn Fn(i64) -> Option<LocalTime>;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// `2026-11-11T22:15:46.654Z` to epoch milliseconds. T3 writes UTC with a `Z`.
pub fn ms(text: &str) -> Option<f64> {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| text.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?);
    let secs = days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?;
    let frac = match text.get(19..) {
        Some(rest) if rest.starts_with('.') => {
            let digits: String = rest[1..].chars().take_while(char::is_ascii_digit).collect();
            format!("0.{digits}").parse::<f64>().unwrap_or(0.)
        }
        _ => 0.,
    };
    Some(secs as f64 * 1000. + (frac * 1000.).round())
}

/// Epoch milliseconds to the form T3 writes, so a bound compares as text with its columns.
pub fn iso(stamp: f64) -> String {
    let millis = stamp.floor() as i64;
    let (secs, milli) = (millis.div_euclid(1000), millis.rem_euclid(1000));
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn utc(seconds: i64) -> LocalTime {
    let text = iso(seconds as f64 * 1000.);
    let n = |r: std::ops::Range<usize>| text[r].parse::<i32>().unwrap_or(0);
    LocalTime { year: n(0..4), month0: n(5..7) - 1, day: n(8..10), hour: n(11..13), minute: n(14..16), second: n(17..19) }
}

fn at(local: Local, seconds: i64) -> LocalTime {
    local(seconds).unwrap_or_else(|| utc(seconds))
}

/// The first instant of the local day that holds `seconds`: its midnight, or where a clock
/// change skips midnight, the instant the day begins at.
fn midnight(local: Local, seconds: i64) -> i64 {
    let day = |t: &LocalTime| (t.year, t.month0, t.day);
    let since = |t: &LocalTime| i64::from(t.hour * 3600 + t.minute * 60 + t.second);
    let target = at(local, seconds);
    // Back by what the clock shows. A clock change between that midnight and `seconds`
    // leaves this off by the change: an hour in most zones, half an hour on Lord Howe Island.
    let mut start = seconds - since(&target);
    for _ in 0..4 {
        let shown = at(local, start);
        if day(&shown) != day(&target) {
            // Still in the day before: on to the midnight its clock is counting toward.
            start += 86400 - since(&shown);
        } else if since(&shown) == 0 || day(&at(local, start - since(&shown))) != day(&target) {
            // Midnight, or the day's first instant where midnight itself was skipped.
            break;
        } else {
            start -= since(&shown);
        }
    }
    // Where a clock is set back across midnight the day shows midnight twice, and begins at
    // the first.
    let before = at(local, start - 1);
    if day(&before) == day(&target) {
        start -= since(&before) + 1;
    }
    start
}

/// Monday is 0.
fn weekday(t: &LocalTime) -> i64 {
    (days_from_civil(i64::from(t.year), i64::from(t.month0) + 1, i64::from(t.day)) + 3).rem_euclid(7)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// The last full week, Monday to Sunday.
    LastWeek,
    /// The current week, so far.
    ThisWeek,
    /// The last full calendar month.
    LastMonth,
    /// The current calendar month, so far.
    ThisMonth,
}

impl Which {
    pub fn span(self) -> Span {
        if matches!(self, Which::LastMonth | Which::ThisMonth) { Span::Month } else { Span::Week }
    }
    pub fn so_far(self) -> bool {
        matches!(self, Which::ThisWeek | Which::ThisMonth)
    }
}

/// How long a period is: a week, Monday to Sunday, or a calendar month.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Span {
    #[default]
    Week,
    Month,
}

/// A week or a month in the reader's zone: the boundaries of its days in epoch ms, one more
/// than it has days, from its first midnight to the midnight after its last day.
#[derive(Clone, Debug, PartialEq)]
pub struct Period {
    pub days: Vec<f64>,
    /// Each day as the stretches of it that fall in one hour on the clock, in order: from,
    /// to, the hour, and how long that hour lasted on the clock that day. Twenty-four of an
    /// hour each, except on a day a clock changes: an hour the clock is set back to comes
    /// twice and lasts two, and one it is set forward over is short or missing.
    hours: Vec<Vec<(f64, f64, usize, f64)>>,
    pub span: Span,
    pub so_far: bool,
    pub now: f64,
}

impl Period {
    pub fn new(now: f64, which: Which, local: Local) -> Period {
        // The midnight before, by stepping two hours back from a midnight.
        let back = |day: i64| midnight(local, day - 7200);
        let mut first = midnight(local, (now / 1000.).floor() as i64);
        let length = match which.span() {
            Span::Week => {
                for _ in 0..weekday(&at(local, first)) + if which == Which::LastWeek { 7 } else { 0 } {
                    first = back(first);
                }
                7
            }
            Span::Month => {
                // Back to the 1st; for the last month, the day before it and back to that month's 1st.
                let to_first = |mut day: i64| {
                    while at(local, day).day > 1 {
                        day = back(day);
                    }
                    day
                };
                first = to_first(first);
                if which == Which::LastMonth {
                    first = to_first(back(first));
                }
                let t = at(local, first);
                let next = if t.month0 == 11 { (i64::from(t.year) + 1, 1) } else { (i64::from(t.year), i64::from(t.month0) + 2) };
                (days_from_civil(next.0, next.1, 1) - days_from_civil(i64::from(t.year), i64::from(t.month0) + 1, 1)) as usize
            }
        };
        let mut days = Vec::with_capacity(length + 1);
        let mut day = first;
        for _ in 0..=length {
            days.push(day as f64 * 1000.);
            day = midnight(local, day + 26 * 3600);
        }
        let hours = days
            .windows(2)
            .map(|bounds| {
                let (mut from, end) = ((bounds[0] / 1000.) as i64, (bounds[1] / 1000.) as i64);
                let mut spans: Vec<(f64, f64, usize, f64)> = vec![];
                while from < end {
                    // To the end of the hour the clock shows. Clocks change on the hour.
                    let clock = at(local, from);
                    let to = (from + 3600 - i64::from(clock.minute * 60 + clock.second)).min(end);
                    spans.push((from as f64 * 1000., to as f64 * 1000., clock.hour.rem_euclid(24) as usize, 0.));
                    from = to;
                }
                let lasted = |hour: usize| spans.iter().filter(|span| span.2 == hour).map(|span| span.1 - span.0).sum::<f64>();
                let lasts: Vec<f64> = spans.iter().map(|span| lasted(span.2)).collect();
                spans.iter().zip(lasts).map(|(span, lasted)| (span.0, span.1, span.2, lasted)).collect()
            })
            .collect();
        Period { days, hours, span: which.span(), so_far: which.so_far(), now }
    }
    /// How many days it has.
    pub fn len(&self) -> usize {
        self.days.len() - 1
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn start(&self) -> f64 {
        self.days[0]
    }
    pub fn end(&self) -> f64 {
        self.days[self.len()]
    }
    /// The midnight its last day begins at.
    pub fn last_day(&self) -> f64 {
        self.days[self.len() - 1]
    }
    /// Where the period's records stop: its end, or now when it is still running.
    pub fn until(&self) -> f64 {
        self.end().min(self.now.max(self.start()))
    }
}

// ── What is read ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct Run {
    id: String,
    thread: String,
    provider: String,
    model: String,
    status: String,
    requested: f64,
    started: Option<f64>,
    completed: Option<f64>,
    /// An automatic continuation after a delegate finished: not a task of its own.
    continuation: bool,
    /// Whether the first message came from a person; absent when its author is unknown.
    created_by: Option<bool>,
    /// Whether that message came from the phone.
    source: bool,
    tools: Vec<(f64, f64)>,
    calls: u32,
    commands: u32,
    commands_failed: u32,
    compactions: u32,
    files: u64,
    added: u64,
    removed: u64,
    cause: Option<Cause>,
}

/// The item facts of one run, without identifiers, card state or T3 text.
#[derive(Clone, Debug, Default, PartialEq)]
struct RunFacts {
    /// Whether the first message came from a person; absent when its author is unknown.
    created_by: Option<bool>,
    /// Whether that message came from the phone.
    source: bool,
    tools: Vec<(f64, f64)>,
    calls: u32,
    commands: u32,
    commands_failed: u32,
    compactions: u32,
    files: u64,
    added: u64,
    removed: u64,
    cause: Option<Cause>,
    /// The ordered locations of every card, including rows without nodes.
    card_ordinals: Vec<i64>,
    /// An item with no integer ordinal cannot be found again by ordinal.
    unaddressable_card: bool,
}

impl Run {
    /// Apply the independently read item facts to this run.
    fn apply(&mut self, facts: RunFacts) {
        self.created_by = facts.created_by;
        self.source = facts.source;
        self.tools = facts.tools;
        self.calls = facts.calls;
        self.commands = facts.commands;
        self.commands_failed = facts.commands_failed;
        self.compactions = facts.compactions;
        self.files = facts.files;
        self.added = facts.added;
        self.removed = facts.removed;
        self.cause = facts.cause;
    }
}

impl RunFacts {
    /// The version of the compact, little-endian item facts format.
    #[cfg(any(unix, test))]
    const VERSION: u32 = 2;
    /// Bound a hit to 128 indexed reads, with at most 1 KiB of ordinal data.
    #[cfg(any(unix, test))]
    const MAX_CARD_ORDINALS: usize = 128;
    /// The store accepts at most 1 MiB per entry.
    #[cfg(any(unix, test))]
    const MAX_BYTES: usize = 1024 * 1024;
    /// Fixed fields, including the tool and ordinal counts.
    #[cfg(any(unix, test))]
    const HEADER_BYTES: usize = 57;

    /// Keep numeric facts and exact timestamp bits for a later read.
    #[cfg(any(unix, test))]
    fn to_bytes(&self) -> Option<Vec<u8>> {
        if self.unaddressable_card || self.card_ordinals.len() > Self::MAX_CARD_ORDINALS {
            return None;
        }
        let size = Self::HEADER_BYTES + self.card_ordinals.len() * 8;
        if self.tools.len() > (Self::MAX_BYTES - size) / 16 {
            return None;
        }
        let mut bytes = Self::VERSION.to_le_bytes().to_vec();
        bytes.push(match self.created_by { None => 0, Some(false) => 1, Some(true) => 2 });
        bytes.push(u8::from(self.source));
        bytes.push(match self.cause { None => 0, Some(Cause::Setup) => 1, Some(Cause::Limit) => 2, Some(Cause::Provider) => 3, Some(Cause::Other) => 4 });
        bytes.extend(self.calls.to_le_bytes());
        bytes.extend(self.commands.to_le_bytes());
        bytes.extend(self.commands_failed.to_le_bytes());
        bytes.extend(self.compactions.to_le_bytes());
        bytes.extend(self.files.to_le_bytes());
        bytes.extend(self.added.to_le_bytes());
        bytes.extend(self.removed.to_le_bytes());
        bytes.extend((self.tools.len() as u64).to_le_bytes());
        for (a, b) in &self.tools {
            bytes.extend(a.to_bits().to_le_bytes());
            bytes.extend(b.to_bits().to_le_bytes());
        }
        bytes.extend((self.card_ordinals.len() as u16).to_le_bytes());
        for ordinal in &self.card_ordinals {
            bytes.extend(ordinal.to_le_bytes());
        }
        Some(bytes)
    }

    /// Refuse incomplete, unknown or impossible stored facts without trusting their lengths.
    #[cfg(any(unix, test))]
    fn from_bytes(mut bytes: &[u8]) -> Option<Self> {
        /// Take a fixed number of bytes only when all of them remain.
        fn take<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
            let value = bytes.get(..N)?.try_into().ok()?;
            *bytes = &bytes[N..];
            Some(value)
        }
        if bytes.len() > Self::MAX_BYTES {
            return None;
        }
        if u32::from_le_bytes(take(&mut bytes)?) != Self::VERSION {
            return None;
        }
        let created_by = match take::<1>(&mut bytes)?[0] { 0 => None, 1 => Some(false), 2 => Some(true), _ => return None };
        let source = match take::<1>(&mut bytes)?[0] { 0 => false, 1 => true, _ => return None };
        let cause = match take::<1>(&mut bytes)?[0] { 0 => None, 1 => Some(Cause::Setup), 2 => Some(Cause::Limit), 3 => Some(Cause::Provider), 4 => Some(Cause::Other), _ => return None };
        let calls = u32::from_le_bytes(take(&mut bytes)?);
        let commands = u32::from_le_bytes(take(&mut bytes)?);
        let commands_failed = u32::from_le_bytes(take(&mut bytes)?);
        let compactions = u32::from_le_bytes(take(&mut bytes)?);
        let files = u64::from_le_bytes(take(&mut bytes)?);
        let added = u64::from_le_bytes(take(&mut bytes)?);
        let removed = u64::from_le_bytes(take(&mut bytes)?);
        let count = u64::from_le_bytes(take(&mut bytes)?);
        if count > (bytes.len() / 16) as u64 || commands_failed > commands {
            return None;
        }
        let mut tools = Vec::new();
        for _ in 0..count {
            let a = f64::from_bits(u64::from_le_bytes(take(&mut bytes)?));
            let b = f64::from_bits(u64::from_le_bytes(take(&mut bytes)?));
            if !a.is_finite() || !b.is_finite() || b < a {
                return None;
            }
            tools.push((a, b));
        }
        let count = usize::from(u16::from_le_bytes(take(&mut bytes)?));
        if count > Self::MAX_CARD_ORDINALS || bytes.len() != count * 8 {
            return None;
        }
        let mut card_ordinals = Vec::with_capacity(count);
        for _ in 0..count {
            let ordinal = i64::from_le_bytes(take(&mut bytes)?);
            if card_ordinals.last().is_some_and(|&last| last >= ordinal) {
                return None;
            }
            card_ordinals.push(ordinal);
        }
        Some(Self { created_by, source, tools, calls, commands, commands_failed, compactions, files, added, removed, cause, card_ordinals, unaddressable_card: false })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    Setup,
    Limit,
    Provider,
    Other,
}

/// A normalized ask, retaining Recap's thread and period timestamps.
#[derive(Clone, Debug, Default)]
struct Ask {
    thread: String,
    asked: f64,
    answered: Option<f64>,
    /// Shared waiting inputs; absent only in hand-built interval tests.
    record: Option<anatomy::Ask>,
}

#[derive(Clone, Debug)]
struct Handoff {
    run: Option<String>,
    native: bool,
    model: String,
    status: String,
    started: Option<f64>,
    completed: Option<f64>,
}

#[derive(Clone, Debug)]
struct Turn {
    thread: String,
    completed: f64,
    usage: Option<String>,
    input: Option<f64>,
    output: Option<f64>,
    cached: Option<f64>,
    used: Option<f64>,
    max: Option<f64>,
}

#[derive(Clone, Debug)]
struct Pull {
    key: String,
    state: String,
    merged: Option<f64>,
    closed: Option<f64>,
    linked: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    runs: Vec<Run>,
    asks: Vec<Ask>,
    handoffs: Vec<Handoff>,
    turns: Vec<Turn>,
    pulls: Vec<Pull>,
    /// thread → (project, is a delegate's thread)
    threads: HashMap<String, (String, bool)>,
    first_record: Option<f64>,
    /// All working runs for link threads, including runs outside the period.
    working: HashMap<String, Vec<(f64, f64)>>,
}

fn source_error(path: &Path, kind: &'static str, detail: impl Into<String>) -> SourceError {
    SourceError { kind, path: path.to_owned(), detail: detail.into() }
}

/// Open T3's database the way the dashboard's reader does: read-only, and told to refuse writes.
pub fn open(path: &Path) -> Result<Connection, SourceError> {
    let db = crate::source::open_read_only(path).map_err(|e| {
        if !path.exists() {
            source_error(path, "missing", "")
        } else if path.is_dir() {
            source_error(path, "unreadable", "it is a folder, not a file")
        } else {
            source_error(path, "unreadable", e.to_string())
        }
    })?;
    db.busy_timeout(Duration::from_millis(250)).map_err(|e| source_error(path, "schema", e.to_string()))?;
    db.execute_batch("PRAGMA query_only=ON").map_err(|e| source_error(path, "schema", e.to_string()))?;
    Ok(db)
}

/// Read a period from an open database, in one read transaction that is always ended.
pub fn read_from(db: &Connection, path: &Path, period: &Period) -> Result<Facts, SourceError> {
    let times = TimeQuery::configured();
    read_in_transaction(db, path, || read(db, period, |facts| read_items(db, facts, period.now, times)))
}

/// End the read transaction even when one of its queries fails.
fn read_in_transaction(db: &Connection, path: &Path, read: impl FnOnce() -> rusqlite::Result<Facts>) -> Result<Facts, SourceError> {
    let fail = |e: rusqlite::Error| {
        let not_db = matches!(&e, rusqlite::Error::SqliteFailure(c, _) if c.extended_code == 26);
        source_error(path, if not_db { "unreadable" } else { "schema" }, if not_db { "not a SQLite database".into() } else { e.to_string() })
    };
    db.execute_batch("BEGIN").map_err(fail)?;
    let result = read();
    let ended = db.execute_batch("ROLLBACK");
    let facts = result.map_err(fail)?;
    ended.map_err(fail)?;
    Ok(facts)
}

/// Prepare a reader query, keeping its text available to the query-plan guard in tests.
#[inline]
fn prepare<'a>(db: &'a Connection, sql: &str) -> rusqlite::Result<rusqlite::Statement<'a>> {
    #[cfg(test)]
    PREPARED.with(|statements| statements.borrow_mut().push(sql.to_owned()));
    db.prepare(sql)
}

#[cfg(test)]
thread_local! {
    /// Queries prepared by this test thread's reader, isolated from parallel tests.
    static PREPARED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn has_column(db: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    prepare(db, "SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2")?.query_row(params![format!("{P}{table}"), column], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
}

/// Legacy open states of an item used as the fallback for a runtime request.
fn request_item_open(status: &str) -> bool {
    matches!(status, "running" | "pending" | "waiting" | "requested")
}

/// Normalize a card outside the period's run pass, using only times and identifiers.
fn card(r: &rusqlite::Row<'_>, now: f64) -> rusqlite::Result<Option<Ask>> {
    let stamp = |v: Option<String>| v.as_deref().and_then(ms);
    let kind: String = r.get(0)?;
    let status: String = r.get(1)?;
    let start = stamp(r.get(2)?).or(stamp(r.get(4)?)).unwrap_or(now);
    let end = if status == "waiting" { None } else { Some(stamp(r.get(3)?).or(stamp(r.get(4)?)).unwrap_or(now)) };
    let end_of_run = stamp(r.get(7)?);
    let record = anatomy::Ask {
        kind: if kind == "secret_request" { anatomy::AskKind::Secret } else { anatomy::AskKind::Link },
        start,
        end,
        own_run: stamp(r.get(6)?).map(|a| (a, end_of_run)),
        human_after: None,
    };
    Ok(Some(Ask { thread: r.get(5)?, asked: start, answered: end, record: Some(record) }))
}

/// Read the live period records around the supplied per-run item pass.
fn read(db: &Connection, period: &Period, item_pass: impl FnOnce(&mut Facts) -> rusqlite::Result<()>) -> rusqlite::Result<Facts> {
    let (from, to) = (iso(period.start()), iso(period.until()));
    let stamp = |v: Option<String>| v.as_deref().and_then(ms);
    let mut facts = Facts::default();

    // Runs open at any point in the period: when each was asked for, started and finished, how
    // it ended, which provider and model ran it, and whether it only continues an earlier one.
    let mut index = HashMap::new();
    {
        let mut q = prepare(db, &format!(
            "SELECT run_id, thread_id, requested_at, completed_at, json_extract(payload_json,'$.startedAt'),
                    json_extract(payload_json,'$.status'), json_extract(payload_json,'$.providerInstanceId'),
                    json_extract(payload_json,'$.modelSelection.model'), json_type(payload_json,'$.delegatedCompletion')
             FROM {P}runs WHERE requested_at < ?2 AND (completed_at IS NULL OR completed_at >= ?1)"
        ))?;
        let rows = q.query_map(params![from, to], |r| {
            Ok(Run {
                id: r.get(0)?,
                thread: r.get(1)?,
                requested: stamp(r.get(2)?).unwrap_or(0.),
                completed: stamp(r.get(3)?),
                started: stamp(r.get(4)?),
                status: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                provider: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                model: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                continuation: r.get::<_, Option<String>>(8)?.is_some_and(|t| t == "object"),
                ..Run::default()
            })
        })?;
        for run in rows {
            let run = run?;
            index.insert(run.id.clone(), facts.runs.len());
            facts.runs.push(run);
        }
    }

    item_pass(&mut facts)?;

    // Requests supply authoritative times and the yes/no capability, never their contents.
    // The item's request id joins once within its thread, and supplies fallback times.
    {
        let id = if has_column(db, "runtime_requests", "runtime_request_id")? { "runtime_request_id" } else { "request_id" };
        let field = |name: &str| -> rusqlite::Result<String> {
            Ok(if has_column(db, "runtime_requests", name)? { format!("r.{name}") } else { "NULL".into() })
        };
        let status = field("status")?;
        let capability = if has_column(db, "runtime_requests", "payload_json")? {
            "CASE WHEN r.kind='user_input' THEN CASE WHEN json_valid(r.payload_json)
                 THEN json_extract(r.payload_json,'$.responseCapability.type') IS 'message' ELSE 0 END ELSE 0 END"
        } else {
            "0"
        };
        let mut q = prepare(db, &format!(
            "SELECT r.thread_id,r.created_at,r.resolved_at,r.kind,
                    r.resolved_at IS NULL AND {status} IN ('pending','open','waiting','requested')
                        AND (r.kind<>'user_input' OR {status}='pending'), {capability},
                    i.updated_at,json_extract(i.payload_json,'$.startedAt'),
                    json_extract(i.payload_json,'$.completedAt'),i.status
             FROM {P}runtime_requests r LEFT JOIN {P}turn_items i ON i.rowid=(
                 SELECT x.rowid FROM {P}turn_items x WHERE x.thread_id=r.thread_id
                   AND x.type IN ('user_input_request','approval_request')
                   AND json_extract(x.payload_json,'$.requestId')=r.{id} ORDER BY x.ordinal LIMIT 1)
             WHERE r.kind NOT IN ('dynamic_tool_call','auth_refresh') AND (r.created_at < ?1 OR r.created_at IS NULL)
               AND (r.resolved_at IS NULL OR r.resolved_at >= ?2)"
        ))?;
        // A request resolved before the period began holds nothing in it, so it is not read:
        // without that bound every request ever recorded would be joined to its item on each
        // Recap. One with no recorded resolution is kept, since its end may be its item's.
        let mut rows = q.query(params![to, from])?;
        while let Some(r) = rows.next()? {
            let updated = stamp(r.get(6)?);
            let Some(start) = stamp(r.get(1)?).or(stamp(r.get(7)?)).or(updated) else { continue };
            let item_status: Option<String> = r.get(9)?;
            let item_end = if item_status.as_deref().is_some_and(request_item_open) { None } else { stamp(r.get(8)?).or(updated) };
            let end = if r.get::<_, Option<bool>>(4)?.unwrap_or(false) { None } else { stamp(r.get(2)?).or(item_end).or(Some(start)) };
            if end.is_some_and(|b| b <= start) {
                continue;
            }
            let question = r.get::<_, String>(3)? == "user_input";
            let kind = if question { anatomy::AskKind::Question { by_message: r.get::<_, bool>(5)? } } else { anatomy::AskKind::Approval };
            let record = anatomy::Ask { kind, start, end, own_run: None, human_after: None };
            facts.asks.push(Ask { thread: r.get(0)?, asked: start, answered: end, record: Some(record) });
        }
    }
    // A link card can be open in the period though its run ended before it: a link left in an
    // idle gap. One whose run is in the period was read above; the others are found through
    // one of T3's own indexes, never by reading every item, which on a real database is most
    // of the file. Since T3's migration 089 there is an index of a thread's items by type, and
    // each thread's link cards are looked up in it. Before that there is only T3's index of
    // links still waiting, so a link that has since ended is not found there; with neither
    // index (T3 before migration 074, and this module's made-up data) none is looked for.
    // The index is named in the query because SQLite, left to choose, reads every item. A
    // secret card counts only inside its own run, so one whose run is not in the period adds
    // nothing and is not read.
    {
        let has_index = |name: &str| -> rusqlite::Result<bool> {
            prepare(db, "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1")?.query_row([format!("{P}turn_items_{name}")], |r| Ok(r.get::<_, i64>(0)? > 0))
        };
        let found = if has_index("thread_type_idx")? {
            Some(format!("{P}threads t CROSS JOIN {P}turn_items i INDEXED BY {P}turn_items_thread_type_idx ON i.thread_id=t.thread_id AND i.type='link_request' LEFT JOIN {P}runs r ON r.run_id=i.run_id WHERE i.node_id IS NOT NULL"))
        } else if has_index("waiting_link_idx")? {
            Some(format!("{P}turn_items i INDEXED BY {P}turn_items_waiting_link_idx LEFT JOIN {P}runs r ON r.run_id=i.run_id WHERE i.type='link_request' AND i.status='waiting' AND i.node_id IS NOT NULL"))
        } else {
            None
        };
        if let Some(found) = found {
            let mut q = prepare(db, &format!(
                "SELECT i.type,i.status,json_extract(i.payload_json,'$.startedAt'),json_extract(i.payload_json,'$.completedAt'),
                        i.updated_at,i.thread_id,json_extract(r.payload_json,'$.startedAt'),r.completed_at,i.run_id
                 FROM {found}"
            ))?;
            let mut rows = q.query([])?;
            while let Some(r) = rows.next()? {
                if r.get::<_, Option<String>>(8)?.is_some_and(|run| index.contains_key(&run)) {
                    continue;
                }
                if let Some(ask) = card(r, period.now)? {
                    // Ended before the period began: it holds nothing in it.
                    if ask.answered.is_none_or(|end| end >= period.start()) {
                        facts.asks.push(ask);
                    }
                }
            }
        }
    }
    // A link is answered by T3's first genuine human update after its start. Read only
    // message times and provenance, never text, and only for threads with a period link.
    let links: HashSet<String> = facts.asks.iter().filter(|a| {
        a.record.is_some_and(|r| r.kind == anatomy::AskKind::Link && r.start < period.until() && r.end.is_none_or(|b| b >= period.start()))
    }).map(|a| a.thread.clone()).collect();
    let messages = ["thread_id", "role", "updated_at", "payload_json"].into_iter()
        .map(|c| has_column(db, "messages", c)).collect::<rusqlite::Result<Vec<_>>>()?.into_iter().all(|v| v);
    for thread in links {
        let mut human = vec![];
        if messages {
            let mut q = prepare(db, &format!(
                "SELECT updated_at FROM {P}messages WHERE thread_id=?1 AND role='user'
                   AND CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.createdBy')='user'
                     AND json_extract(payload_json,'$.idleCompaction') IS NULL END ORDER BY updated_at"
            ))?;
            for row in q.query_map([&thread], |r| r.get::<_, Option<String>>(0))? {
                if let Some(at) = stamp(row?) {
                    human.push(at);
                }
            }
        }
        let mut bounds = (f64::INFINITY, f64::NEG_INFINITY);
        for ask in facts.asks.iter_mut().filter(|a| a.thread == thread) {
            if let Some(record) = ask.record.as_mut().filter(|r| r.kind == anatomy::AskKind::Link) {
                record.human_after = human.iter().copied().find(|h| *h > record.start);
                ask.answered = record.end.into_iter().chain(record.human_after).min_by(f64::total_cmp);
                bounds.0 = bounds.0.min(record.start);
                bounds.1 = bounds.1.max(ask.answered.unwrap_or(period.now));
            }
        }
        // Every working run overlapping the links, not just the period's own runs.
        let mut q = prepare(db, &format!(
            "SELECT json_extract(payload_json,'$.startedAt'),completed_at FROM {P}runs
             WHERE thread_id=?1 AND requested_at < ?3 AND (completed_at IS NULL OR completed_at >= ?2)"
        ))?;
        for row in q.query_map(params![thread, iso(bounds.0), iso(bounds.1)], |r| Ok((stamp(r.get(0)?), stamp(r.get(1)?))))? {
            if let (Some(a), b) = row? {
                facts.working.entry(thread.clone()).or_default().push((a, b.unwrap_or(period.now)));
            }
        }
    }

    // Work handed to another agent: its model, how it ended, when it ran, the run that handed
    // it over, and whether the provider ran it inside that run. Never its prompt or result.
    {
        let column = |name: &str, path: &str| -> rusqlite::Result<String> {
            Ok(if has_column(db, "subagents", name)? { name.to_owned() } else { format!("json_extract(payload_json,'$.{path}')") })
        };
        let (started, completed) = (column("started_at", "startedAt")?, column("completed_at", "completedAt")?);
        let mut q = prepare(db, &format!(
            "SELECT json_extract(payload_json,'$.runId'), json_extract(payload_json,'$.origin'), json_extract(payload_json,'$.status'),
                    coalesce(json_extract(payload_json,'$.modelSelection.model'), json_extract(payload_json,'$.model')), {started}, {completed}
             FROM {P}subagents WHERE {started} < ?2 AND ({completed} IS NULL OR {completed} >= ?1)"
        ))?;
        let rows = q.query_map(params![from, to], |r| {
            Ok(Handoff {
                run: r.get(0)?,
                native: r.get::<_, Option<String>>(1)?.as_deref() == Some("provider_native"),
                status: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                model: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                started: stamp(r.get(4)?),
                completed: stamp(r.get(5)?),
            })
        })?;
        for row in rows {
            facts.handoffs.push(row?);
        }
    }

    // Provider turns finished in the period: token counts, and how full the context was.
    {
        let x = |path: &str| format!("json_extract(payload_json,'$.{path}')");
        let mut q = prepare(db, &format!(
            "SELECT thread_id, completed_at, {}, {}, {}, {}, {}, {} FROM {P}provider_turns WHERE completed_at >= ?1 AND completed_at < ?2",
            x("turnTokenUsage.usageStatus"),
            x("turnTokenUsage.inputTokens"),
            x("turnTokenUsage.outputTokens"),
            x("turnTokenUsage.cachedInputTokens"),
            x("tokenUsage.usedTokens"),
            x("tokenUsage.maxTokens")
        ))?;
        let rows = q.query_map(params![from, to], |r| {
            Ok((r.get::<_, String>(0)?, stamp(r.get(1)?), r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
        })?;
        for row in rows {
            let (thread, completed, usage, input, output, cached, used, max) = row?;
            if let Some(completed) = completed {
                facts.turns.push(Turn { thread, completed, usage, input, output, cached, used, max });
            }
        }
    }

    // Threads: the project each belongs to and whether it is a delegate's, to count projects
    // and to say who a task came from; and when the oldest record on this machine begins.
    {
        let mut q = prepare(db, &format!(
            "SELECT thread_id, json_extract(payload_json,'$.projectId'), json_extract(payload_json,'$.lineage.relationshipToParent'),
                    json_extract(payload_json,'$.createdAt') FROM {P}threads"
        ))?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?, stamp(r.get(3)?))))?;
        for row in rows {
            let (thread, project, relation, created) = row?;
            facts.threads.insert(thread, (project.unwrap_or_default(), relation.as_deref() == Some("subagent")));
            if let Some(created) = created {
                facts.first_record = Some(facts.first_record.map_or(created, |f: f64| f.min(created)));
            }
        }
    }

    // Linked pull requests: state, and when each was linked, merged or closed. The host,
    // repository and number are read only to count a pull request linked on two threads once.
    {
        let mut q = prepare(db, &format!(
            "SELECT json_extract(p.value,'$.host') || '/' || json_extract(p.value,'$.repository') || '#' || json_extract(p.value,'$.number'),
                    json_extract(p.value,'$.snapshot.state'), json_extract(p.value,'$.snapshot.mergedAt'),
                    json_extract(p.value,'$.snapshot.closedAt'), json_extract(p.value,'$.linkedAt')
             FROM {P}threads t, json_each(t.payload_json,'$.pullRequests') p WHERE json_type(t.payload_json,'$.pullRequests')='array'"
        ))?;
        let rows = q.query_map([], |r| {
            Ok(Pull {
                key: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                state: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                merged: stamp(r.get(2)?),
                closed: stamp(r.get(3)?),
                linked: stamp(r.get(4)?),
            })
        })?;
        for row in rows {
            facts.pulls.push(row?);
        }
    }
    let _ = index;
    Ok(facts)
}

/// Item-pass counts from the open stores used by one Recap invocation.
#[derive(Clone, Copy, Default)]
struct CacheStats {
    opened: bool,
    hits: usize,
    finished: usize,
    written: usize,
}

thread_local! {
    /// Only the command that requested diagnostics collects a report.
    static CACHE_STATS: std::cell::Cell<Option<CacheStats>> = const { std::cell::Cell::new(None) };
}

/// Start collecting the report before the CLI reads either of its possible periods.
pub fn request_cache_stats() {
    CACHE_STATS.set(Some(CacheStats::default()));
}

/// Take the report once, after the CLI finishes writing its output.
pub fn take_cache_stats() -> Option<String> {
    let stats = CACHE_STATS.take()?;
    Some(if stats.opened {
        format!("loupe: cache: {} of {} finished runs from the cache, {} written", stats.hits, stats.finished, stats.written)
    } else {
        "loupe: cache: not used".into()
    })
}

/// Add a pass's counts to the report. Hits plus writes can be less than the
/// finished count because the store validates eligibility on its own.
#[cfg(unix)]
fn record_cache_stats(hits: usize, finished: usize, written: usize) {
    if let Some(mut stats) = CACHE_STATS.get() {
        stats.opened = true;
        stats.hits += hits;
        stats.finished += finished;
        stats.written += written;
        CACHE_STATS.set(Some(stats));
    }
}

/// The store family for one run's item facts.
#[cfg(unix)]
const RUN_CACHE_KIND: &str = "recap.run";

/// Recap considers a completed run finished once its status is no longer active.
#[cfg(unix)]
fn finished(run: &Run) -> bool {
    run.completed.is_some() && !matches!(run.status.as_str(), "running" | "waiting" | "starting")
}

/// Whether Recap may use the store: only when `LOUPE_CACHE=1` asks for it, and never when
/// `LOUPE_NO_CACHE=1` turns the store off. It is asked for, not the default, because of
/// what it costs where it does not hit. Measured on made-up weeks of about 580 runs: a read
/// that fills the store takes 1.6 to 2 times an uncached one; a read from it takes a fifth to
/// a half; with a commit to T3's database between two reads, a third to nine tenths. The
/// store keeps a run's entry across commits that do not touch that run's pages only while
/// SQLite's log is continuous: after the log is reset, on a database with no log, or when a
/// commit lands during the read, every run is read again, at the filling cost. How often that
/// happens on a machine where agents are working has not been measured. This function is the
/// one place to change when the store becomes the default.
#[cfg(unix)]
fn cache_enabled() -> bool {
    std::env::var_os("LOUPE_CACHE").is_some_and(|value| value == "1")
        && !std::env::var_os("LOUPE_NO_CACHE").is_some_and(|value| value == "1")
}

/// The SQL and row representation for the two timestamps of an item.
#[derive(Clone, Copy)]
enum TimeQuery {
    Sqlite,
    Full,
    Head,
}

impl TimeQuery {
    /// Choose the timestamp parser once for the whole read.
    fn configured() -> Self {
        // Recap has no early form, so early uses the full skim. SQLite's string
        // extraction fails the read on a numeric timestamp; the skim treats it as absent.
        match skim::choice(std::env::var(skim::VARIABLE).ok().as_deref()) {
            None => Self::Sqlite,
            Some(skim::Mode::Head) => Self::Head,
            Some(skim::Mode::Early | skim::Mode::Full) => Self::Full,
        }
    }

    /// Select the timestamps, keeping the other columns at the same positions in every mode.
    fn columns(self) -> &'static str {
        match self {
            Self::Sqlite => "json_extract(payload_json,'$.startedAt'), json_extract(payload_json,'$.completedAt')",
            Self::Full => "t3_times(payload_json), NULL",
            Self::Head => "t3_times_head(rowid, typeof(payload_json)), NULL",
        }
    }

    /// Register any function this connection needs to prepare the timestamp query.
    fn register(self, db: &Connection) -> rusqlite::Result<()> {
        match self {
            Self::Sqlite => Ok(()),
            Self::Full => skim::register_times(db),
            Self::Head => skim::register_heads(db),
        }
    }

    /// Read a time pair, leaving the end unread for cards that are still open.
    fn read(self, row: &rusqlite::Row<'_>, ended: bool) -> rusqlite::Result<(Option<f64>, Option<f64>)> {
        if let Self::Sqlite = self {
            let start = row_stamp(row, 2)?;
            let end = if ended { row_stamp(row, 3)? } else { None };
            return Ok((start, end));
        }
        let bytes: [u8; 16] = row.get(2)?;
        let stamp = |bytes: [u8; 8]| {
            let value = f64::from_le_bytes(bytes);
            (!value.is_nan()).then_some(value)
        };
        let (start, end) = bytes.split_at(8);
        Ok((stamp(start.try_into().unwrap()), ended.then(|| stamp(end.try_into().unwrap())).flatten()))
    }
}

/// Read a timestamp that SQLite returned as text.
fn row_stamp(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<Option<f64>> {
    Ok(row.get::<_, Option<String>>(column)?.as_deref().and_then(ms))
}

/// A run query and the timestamp representation it selects.
struct ItemQuery<'db> {
    statement: rusqlite::Statement<'db>,
    times: TimeQuery,
}

/// The facts and live cards returned by one run's item pass.
type RunItems = (RunFacts, Vec<Ask>);

/// An item pass together with its worker's snapshot proof.
type WorkerItems = (bool, rusqlite::Result<RunItems>);

/// A read-only worker prepares one query for all the runs it reads.
struct ItemReader {
    now: f64,
    times: TimeQuery,
}

impl fanout::Reader for ItemReader {
    type Scope = Run;
    type Output = WorkerItems;
    type Prepared<'db> = (ItemQuery<'db>, bool);

    /// Register timestamps and prepare the run query on this connection.
    fn prepare<'db>(&self, db: &'db Connection, pinned: bool) -> rusqlite::Result<Self::Prepared<'db>> {
        self.times.register(db)?;
        Ok((prepare_items(db, self.times)?, pinned))
    }

    /// Keep the result and any row error paired with this worker's proof.
    fn read(&self, prepared: &mut Self::Prepared<'_>, run: &Run) -> rusqlite::Result<Self::Output> {
        let (query, pinned) = prepared;
        Ok((*pinned, items_of(query, run, self.now)))
    }
}

/// Workers for a cached pass must prove they see the main connection's snapshot.
#[cfg(unix)]
struct CachedItemReader<'a> {
    items: &'a ItemReader,
    token: cache::SourceToken,
}

#[cfg(unix)]
impl<'a> fanout::Reader for CachedItemReader<'a> {
    type Scope = &'a Run;
    type Output = WorkerItems;
    type Prepared<'db> = (ItemQuery<'db>, bool);

    /// Probe in autocommit, then prove the pinned transaction against the main token.
    fn pin<'db>(&self, db: &'db Connection, path: &Path) -> rusqlite::Result<(rusqlite::Transaction<'db>, bool)> {
        let probe = cache::Snapshot::probe(db, path);
        let tx = db.unchecked_transaction()?;
        tx.query_row(fanout::PIN_SNAPSHOT, [], |_| Ok(()))?;
        let pinned = probe.is_some_and(|probe| probe.matches(db, &self.token));
        Ok((tx, pinned))
    }

    /// Register timestamps and prepare the run query on this connection.
    fn prepare<'db>(&self, db: &'db Connection, pinned: bool) -> rusqlite::Result<Self::Prepared<'db>> {
        self.items.prepare(db, pinned)
    }

    /// Return this run's facts alongside the cache proof from its worker.
    fn read(&self, prepared: &mut Self::Prepared<'_>, run: &&Run) -> rusqlite::Result<Self::Output> {
        self.items.read(prepared, run)
    }
}

/// The source and worker limit for a file read.
struct ItemWorkers<'a> {
    path: &'a Path,
    count: usize,
}

impl ItemWorkers<'_> {
    /// Small passes stay serial to avoid opening connections for only a few queries.
    fn count(&self, runs: usize) -> Option<usize> {
        (self.count > 1 && runs >= 4).then_some(self.count.min(runs))
    }

    /// Try the file's workers. Startup failures fall back to the main transaction.
    fn read<R>(&self, count: usize, runs: &[R::Scope], reader: &R) -> Option<Vec<WorkerItems>>
    where
        R: fanout::Reader<Output = WorkerItems>,
    {
        fanout::read_scopes(self.path, count, runs, open, reader).ok()
    }
}

/// Read file items with workers when there are enough runs.
fn read_file_items(db: &Connection, facts: &mut Facts, reader: &ItemReader, workers: &ItemWorkers<'_>) -> rusqlite::Result<()> {
    if let Some(count) = workers.count(facts.runs.len()) {
        // Without a cache token the default pin can see a commit after the main
        // connection pinned. This changes nothing for a finished period. In a
        // period still running, a run in progress may show newly committed items.
        if let Some(batch) = workers.read(count, &facts.runs, reader) {
            if batch.iter().all(|(pinned, items)| *pinned && items.is_ok()) {
                for (run, (_, items)) in facts.runs.iter_mut().zip(batch) {
                    let (items, asks) = items?;
                    run.apply(items);
                    facts.asks.extend(asks);
                }
                return Ok(());
            }
        }
    }
    // Rerun a failing pass in serial order on the main snapshot for the same
    // source error a one-connection read would give.
    read_items(db, facts, reader.now, reader.times)
}

/// Look up the period's finished runs together and save the facts read on misses together.
#[cfg(unix)]
fn read_cached_items(
    db: &Connection,
    facts: &mut Facts,
    reader: &ItemReader,
    workers: &ItemWorkers<'_>,
    mut cache: cache::Cache<'_>,
    before_workers: impl FnOnce(),
) -> rusqlite::Result<()> {
    let version = i64::from(RunFacts::VERSION);
    let requested: Vec<_> = facts.runs.iter().filter(|run| finished(run)).map(|run| run.id.as_str()).collect();
    let finished_count = requested.len();
    let hits = cache.lookup_many(RUN_CACHE_KIND, version, &requested);
    let cached = CachedItemReader { items: reader, token: cache.source_token() };
    let mut pending = Vec::new();
    let used = read_items_with_hits(
        db, facts, reader.now, reader.times, hits,
        |misses| {
            let count = workers.count(misses.len())?;
            before_workers();
            workers.read(count, misses, &cached)
        },
        |index, run, items| {
            if finished(run) {
                if let Some(bytes) = items.to_bytes() {
                    pending.push((index, bytes));
                }
            }
        },
    )?;
    let entries: Vec<_> = pending.into_iter().map(|(index, bytes)| (facts.runs[index].id.as_str(), bytes)).collect();
    let written = cache.write_many(RUN_CACHE_KIND, version, &entries);
    record_cache_stats(used, finished_count, written);
    Ok(())
}

/// Use decoded hits and refresh their cards; send the item facts of each miss to `save`.
#[cfg(any(unix, test))]
fn read_items_with_hits(
    db: &Connection,
    facts: &mut Facts,
    now: f64,
    times: TimeQuery,
    mut hits: HashMap<String, Vec<u8>>,
    read_misses: impl FnOnce(&[&Run]) -> Option<Vec<WorkerItems>>,
    mut save: impl FnMut(usize, &Run, &RunFacts),
) -> rusqlite::Result<usize> {
    times.register(db)?;
    let index = if hits.is_empty() { None } else { run_card_index(db)? };
    let decoded: Vec<_> = facts.runs.iter().map(|run| {
        index.as_ref()?;
        hits.remove(&run.id).and_then(|bytes| RunFacts::from_bytes(&bytes))
    }).collect();
    let misses: Vec<_> = facts.runs.iter().zip(&decoded)
        .filter_map(|(run, hit)| hit.is_none().then_some(run)).collect();
    let mut results = read_misses(&misses).into_iter().flatten();
    let mut cards = None;
    let mut items = None;
    let mut used = 0;
    for (position, (run, hit)) in facts.runs.iter_mut().zip(decoded).enumerate() {
        if let (Some(index), Some(hit)) = (&index, hit) {
            if !hit.card_ordinals.is_empty() {
                let query = match &mut cards {
                    Some(query) => query,
                    slot => slot.insert(prepare_run_cards(db, index, times)?),
                };
                facts.asks.extend(run_cards(query, run, now, &hit.card_ordinals)?);
            }
            run.apply(hit);
            used += 1;
            continue;
        }
        let (items, asks) = match results.next() {
            Some((true, Ok(result))) => result,
            // Recheck errors and unproven results on the pinned main connection
            // before contributing anything to the report or store.
            _ => {
                let query = match &mut items {
                    Some(query) => query,
                    slot => slot.insert(prepare_items(db, times)?),
                };
                items_of(query, run, now)?
            }
        };
        save(position, run, &items);
        run.apply(items);
        facts.asks.extend(asks);
    }
    Ok(used)
}

/// Prepare the item pass: tool times and outcomes, author and origin, failure class and code,
/// compactions, checkpoint counts, and the locations of secret and link cards.
fn prepare_items(db: &Connection, times: TimeQuery) -> rusqlite::Result<ItemQuery<'_>> {
    let kinds = TOOLS.iter().chain(["user_message", "error", "checkpoint", "compaction", "secret_request", "link_request"].iter()).map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
    let columns = times.columns();
    let statement = prepare(db, &format!(
        "SELECT type, status, {columns},
                CASE WHEN type='user_message' THEN json_extract(payload_json,'$.createdBy') END,
                CASE WHEN type='user_message' THEN json_extract(payload_json,'$.creationSource') END,
                CASE WHEN type='error' THEN json_extract(payload_json,'$.failure.class') END,
                CASE WHEN type='error' THEN json_extract(payload_json,'$.failure.code') END,
                CASE WHEN type='checkpoint' THEN json_array_length(payload_json,'$.files') END,
                CASE WHEN type='checkpoint' THEN (SELECT sum(json_extract(f.value,'$.additions')) FROM json_each(payload_json,'$.files') f) END,
                CASE WHEN type='checkpoint' THEN (SELECT sum(json_extract(f.value,'$.deletions')) FROM json_each(payload_json,'$.files') f) END, updated_at,thread_id,node_id,ordinal
         FROM {P}turn_items WHERE run_id=?1 AND type IN ({kinds}) ORDER BY ordinal, rowid"
    ))?;
    Ok(ItemQuery { statement, times })
}

/// Read every run's items directly from T3.
fn read_items(db: &Connection, facts: &mut Facts, now: f64, times: TimeQuery) -> rusqlite::Result<()> {
    times.register(db)?;
    let mut query = prepare_items(db, times)?;
    for run in &mut facts.runs {
        let (items, asks) = items_of(&mut query, run, now)?;
        run.apply(items);
        facts.asks.extend(asks);
    }
    Ok(())
}

/// Select the real schema's run/ordinal index, or the equivalent fixture index.
#[cfg(any(unix, test))]
fn run_card_index(db: &Connection) -> rusqlite::Result<Option<String>> {
    prepare(db, "SELECT name FROM sqlite_master WHERE type='index' AND name IN (?1,'demo_items_by_run') ORDER BY name DESC LIMIT 1")?
        .query_row([format!("{P}turn_items_run_ordinal_idx")], |row| row.get(0))
        .optional()
}

/// Prepare the narrow lookup of all card rows at a run's ordinal.
#[cfg(any(unix, test))]
fn prepare_run_cards<'a>(db: &'a Connection, index: &str, times: TimeQuery) -> rusqlite::Result<ItemQuery<'a>> {
    let columns = match times {
        TimeQuery::Sqlite => "json_extract(payload_json,'$.startedAt'),json_extract(payload_json,'$.completedAt')",
        _ => times.columns(),
    };
    let statement = prepare(db, &format!(
        "SELECT type,status,{columns},
                updated_at,thread_id,node_id
         FROM {P}turn_items INDEXED BY {index} WHERE run_id=?1 AND ordinal=?2
           AND type IN ('secret_request','link_request') ORDER BY rowid"
    ))?;
    Ok(ItemQuery { statement, times })
}

/// Read each remembered ordinal from this transaction, using the card's current state.
#[cfg(any(unix, test))]
fn run_cards(query: &mut ItemQuery<'_>, run: &Run, now: f64, ordinals: &[i64]) -> rusqlite::Result<Vec<Ask>> {
    let mut asks = Vec::new();
    for ordinal in ordinals {
        let mut rows = query.statement.query(params![run.id, ordinal])?;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(0)?;
            let status: Option<String> = row.get(1)?;
            if let Some(ask) = run_card(row, &kind, status.as_deref(), run, now, 4, query.times)? {
                asks.push(ask);
            }
        }
    }
    Ok(asks)
}

/// Make an ask from either run-item query. The last three card columns start at `updated`.
fn run_card(row: &rusqlite::Row<'_>, kind: &str, status: Option<&str>, run: &Run, now: f64, updated: usize, times: TimeQuery) -> rusqlite::Result<Option<Ask>> {
    if row.get::<_, Option<String>>(updated + 2)?.is_none() {
        return Ok(None);
    }
    // A completed run supplies a historical bound. With no completion time,
    // only the same live run states used by source::read_ask_cards can hold a secret.
    if kind == "secret_request" && run.completed.is_none()
        && !matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting")
    {
        return Ok(None);
    }
    let open = status == Some("waiting");
    let (start, end) = times.read(row, !open)?;
    let start = start.or(row_stamp(row, updated)?).unwrap_or(now);
    let end = if open {
        None
    } else {
        Some(end.or(row_stamp(row, updated)?).unwrap_or(now))
    };
    let record = anatomy::Ask {
        kind: if kind == "secret_request" { anatomy::AskKind::Secret } else { anatomy::AskKind::Link },
        start,
        end,
        own_run: run.started.map(|a| (a, run.completed)),
        human_after: None,
    };
    Ok(Some(Ask { thread: row.get(updated + 1)?, asked: start, answered: end, record: Some(record) }))
}

/// Read one run's item facts and its cards, without changing the period's facts.
fn items_of(items: &mut ItemQuery<'_>, run: &Run, now: f64) -> rusqlite::Result<RunItems> {
    let mut facts = RunFacts::default();
    let mut asks = Vec::new();
    let mut rows = items.statement.query(params![run.id])?;
    while let Some(r) = rows.next()? {
        let (kind, status): (String, Option<String>) = (r.get(0)?, r.get(1)?);
        match kind.as_str() {
            "secret_request" | "link_request" => {
                match r.get_ref(14)?.as_i64() {
                    Ok(ordinal) if facts.card_ordinals.last() != Some(&ordinal) => {
                        facts.card_ordinals.push(ordinal);
                    }
                    Ok(_) => {}
                    Err(_) => facts.unaddressable_card = true,
                }
                if let Some(ask) = run_card(r, &kind, status.as_deref(), run, now, 11, items.times)? {
                    asks.push(ask);
                }
            }
            "user_message" => {
                // The first message that says who wrote it decides, with where that message
                // came from. One with no author leaves the question open for the next.
                if facts.created_by.is_none() {
                    facts.created_by = r.get::<_, Option<String>>(4)?.map(|who| who == "user");
                    facts.source = r.get::<_, Option<String>>(5)?.as_deref() == Some("mobile");
                }
            }
            "error" => {
                let (class, code): (Option<String>, Option<String>) = (r.get(6)?, r.get(7)?);
                facts.cause = Some(cause(class.as_deref().unwrap_or(""), code.as_deref().unwrap_or("")));
            }
            "checkpoint" => {
                facts.files += r.get::<_, Option<i64>>(8)?.unwrap_or(0).max(0) as u64;
                facts.added += r.get::<_, Option<i64>>(9)?.unwrap_or(0).max(0) as u64;
                facts.removed += r.get::<_, Option<i64>>(10)?.unwrap_or(0).max(0) as u64;
            }
            "compaction" => facts.compactions += 1,
            _ => {
                facts.calls += 1;
                if kind == "command_execution" {
                    facts.commands += 1;
                    facts.commands_failed += u32::from(status.as_deref() == Some("failed"));
                }
                if let (Some(a), Some(b)) = items.times.read(r, true)? {
                    facts.tools.push((a, b));
                }
            }
        }
    }
    Ok((facts, asks))
}

fn cause(class: &str, code: &str) -> Cause {
    if class == "usage_limit" || code.contains("limit") {
        Cause::Limit
    } else if code == "workspace_preparation_failed" || class == "validation_error" {
        Cause::Setup
    } else if class == "provider_error" || class == "transport_error" {
        Cause::Provider
    } else {
        Cause::Other
    }
}

// ── What is worked out ──────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Split {
    pub tools: f64,
    pub model: f64,
    pub waiting: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Tasks {
    pub total: u32,
    pub from_you: u32,
    pub from_phone: u32,
    pub from_agents: u32,
    pub finished: u32,
    pub failed: u32,
    pub stopped: u32,
    pub cancelled: u32,
    pub running: u32,
    pub failed_setup: u32,
    pub failed_limit: u32,
    pub failed_provider: u32,
    /// Tasks stopped on a plan limit, by provider, most first.
    pub limit_by: Vec<(String, u32)>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Model {
    pub name: String,
    /// claude, openai, grok, gemini or other: which provider color it wears.
    pub family: &'static str,
    pub hours: f64,
    pub runs: u32,
    /// `None` when the provider reports no tokens.
    pub tokens_in: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Handed {
    pub count: u32,
    pub hours: f64,
    pub failed: u32,
    /// From a model, to a model, how many times: the three commonest.
    pub pairs: Vec<(String, String, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Asks {
    pub count: u32,
    pub hours: f64,
    pub typical_ms: Option<f64>,
    pub longest_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Tokens {
    pub input: f64,
    pub output: f64,
    pub cached: f64,
    /// Providers that report none, with the hours their runs took.
    pub left_out: Vec<(String, f64)>,
    pub reported: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub kind: &'static str,
    pub number: String,
    pub text: String,
    pub rests_on: String,
}

/// One value for each day of a period. A day before the records begin, or one that has not
/// come yet, is not a day with nothing in it: it is written as `null`, not as a zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerDay<T> {
    values: Vec<T>,
    /// The days that can hold records: from this one, up to but not this one.
    known: (usize, usize),
}

impl<T: Clone> PerDay<T> {
    pub fn new(days: usize, value: T, known: (usize, usize)) -> PerDay<T> {
        PerDay { values: vec![value; days], known }
    }
}

impl<T> std::ops::Deref for PerDay<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.values
    }
}

impl<T> std::ops::DerefMut for PerDay<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.values
    }
}

impl<T: Serialize> Serialize for PerDay<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.values.iter().enumerate().map(|(day, value)| (day >= self.known.0 && day < self.known.1).then_some(value)))
    }
}

/// Version of the `--recap --json` object.
///
/// Bump this when a field is removed, renamed, or changes meaning or unit.
/// Adding a field does not bump it. This is not the cache entry version: the
/// Insights screen and the opt-in cache use [`Recap`] itself, and the number
/// is added only when the object is printed.
pub const JSON_SCHEMA: u32 = 1;

/// `--recap --json`, with [`JSON_SCHEMA`] as the first key.
pub fn printed_json(report: &Recap) -> Result<String, serde_json::Error> {
    crate::report_json::with_schema(report, JSON_SCHEMA)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Recap {
    /// `week` or `month`.
    pub period: Span,
    /// Local dates, `2026-11-09`, for the period's first and last day.
    pub from: String,
    pub to: String,
    pub so_far: bool,
    /// Days of the period on or after the first record on this machine, out of the days elapsed.
    pub days_recorded: u32,
    pub days_elapsed: u32,
    /// How many days the period has: 7, or the month's 28 to 31.
    pub days: u32,
    /// The weekday of the period's first day, Monday being 0. A week's is 0.
    pub first_weekday: u32,
    /// The first day of the period that has records, its first day being 0; `days` when none has.
    pub first_recorded_day: u32,
    pub hours: f64,
    pub split: Split,
    /// Agent hours for each day of the period, its first day first.
    pub by_day: PerDay<f64>,
    /// Agents running, on average, in each hour of each day.
    pub by_hour: PerDay<[f32; 24]>,
    /// Days with any agent time, and the agent hours of a typical one: their median.
    pub days_active: u32,
    pub typical_day: Option<f64>,
    pub most_at_once: u32,
    pub most_at_once_at: Option<f64>,
    /// The local day and time of it: `Thu 14:20` in a week, `Thu 12, 14:20` in a month.
    pub most_at_once_when: Option<String>,
    pub runs: u32,
    pub threads: u32,
    pub projects: u32,
    pub tasks: Tasks,
    pub models: Vec<Model>,
    pub lines_added: u64,
    pub lines_removed: u64,
    pub file_changes: u64,
    pub runs_changing_files: u32,
    pub prs_merged: u32,
    pub prs_open: u32,
    pub prs_closed: u32,
    pub handed: Handed,
    pub asks: Asks,
    pub tokens: Tokens,
    pub compactions: u32,
    pub turns_past_90: u32,
    pub tool_calls: u32,
    /// Tool calls a provider marked finished all at once, at one instant, so that none has an
    /// end of its own (`anatomy::closed_together`). Each is still one of `tool_calls`; its
    /// stretch is not counted as tool time.
    pub closed_together: u32,
    pub commands: u32,
    pub commands_failed: u32,
    pub findings: Vec<Finding>,
}

impl Recap {
    pub fn is_empty(&self) -> bool {
        self.runs == 0
    }
}

/// A set of intervals as pieces in order that do not overlap, each clipped to `[lo, hi]`.
fn merged(spans: &[(f64, f64)], lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let mut clipped: Vec<(f64, f64)> = spans.iter().map(|&(a, b)| (a.max(lo), b.min(hi))).filter(|(a, b)| b > a).collect();
    clipped.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut pieces: Vec<(f64, f64)> = vec![];
    for (a, b) in clipped {
        match pieces.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => pieces.push((a, b)),
        }
    }
    pieces
}

/// Total length of a set of intervals, each clipped to `[lo, hi]`.
fn union(spans: &[(f64, f64)], lo: f64, hi: f64) -> f64 {
    merged(spans, lo, hi).iter().map(|(a, b)| b - a).sum()
}

/// The parts of `spans` within `[lo, hi]` that nothing in `cover` covers.
fn outside(spans: &[(f64, f64)], cover: &[(f64, f64)], lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let cover = merged(cover, lo, hi);
    let mut left = vec![];
    for (mut a, b) in merged(spans, lo, hi) {
        for &(c, d) in &cover {
            if d <= a || c >= b {
                continue;
            }
            if c > a {
                left.push((a, c));
            }
            a = a.max(d);
        }
        if a < b {
            left.push((a, b));
        }
    }
    left
}

/// Add the time from `a` to `b` to the day and the hours it falls in. A day's number is
/// time, so a day with 25 hours in it can hold 25. An hour's number is agents on average:
/// the time in that hour on the clock over the time the hour lasted that day, so one agent
/// all day is 1 in every hour, the hour that comes twice when clocks go back included.
fn spread(recap: &mut Recap, period: &Period, a: f64, b: f64) {
    for (day, bounds) in period.days.windows(2).enumerate() {
        if b.min(bounds[1]) - a.max(bounds[0]) <= 0. {
            continue;
        }
        recap.by_day[day] += (b.min(bounds[1]) - a.max(bounds[0])) / HOUR;
        for &(from, to, hour, lasted) in &period.hours[day] {
            let part = b.min(to) - a.max(from);
            if part > 0. {
                recap.by_hour[day][hour] += (part / lasted) as f32;
            }
        }
    }
}

fn family_name(family: ProviderFamily) -> &'static str {
    match family {
        ProviderFamily::Claude => "claude",
        ProviderFamily::OpenAi => "openai",
        ProviderFamily::Grok => "grok",
        ProviderFamily::Gemini => "gemini",
        ProviderFamily::Other => "other",
    }
}

fn provider_label(provider: &str) -> String {
    match provider {
        "claudeAgent" => "claude".into(),
        other => terminal::clean(other).to_lowercase(),
    }
}

/// The local date of an instant, `2026-11-09`.
pub fn local_date(local: Local, stamp: f64) -> String {
    date(local, stamp)
}

fn date(local: Local, stamp: f64) -> String {
    let t = at(local, (stamp / 1000.).floor() as i64);
    format!("{:04}-{:02}-{:02}", t.year, t.month0 + 1, t.day)
}

/// `114h`, `6.6h` under ten hours, `40m` under one.
pub fn hours_text(hours: f64) -> String {
    if hours >= 9.95 {
        format!("{}h", hours.round())
    } else if hours >= 1. {
        format!("{:.1}h", hours)
    } else {
        format!("{}m", (hours * 60.).round())
    }
}

/// `2m`, `1h 52m`, `45s`.
pub fn span_text(millis: f64) -> String {
    let seconds = (millis / 1000.).round() as i64;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", (seconds as f64 / 60.).round())
    } else {
        let minutes = (seconds as f64 / 60.).round() as i64;
        if minutes % 60 == 0 { format!("{}h", minutes / 60) } else { format!("{}h {}m", minutes / 60, minutes % 60) }
    }
}

/// `1,234`; from 100,000, `341K`.
pub fn count_text(n: u64) -> String {
    if n >= 100_000 {
        return short_text(n as f64, 10.);
    }
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `341K`, `4.0B`, `15M`, `812`: one decimal under `one_decimal_under`, whole above.
pub fn short_text(n: f64, one_decimal_under: f64) -> String {
    let (value, unit) = if n >= 1e9 {
        (n / 1e9, "B")
    } else if n >= 1e6 {
        (n / 1e6, "M")
    } else if n >= 1e3 {
        (n / 1e3, "K")
    } else {
        (n, "")
    };
    if unit.is_empty() || value >= one_decimal_under - 0.05 {
        format!("{}{unit}", value.round())
    } else {
        format!("{value:.1}{unit}")
    }
}

/// Lines added or removed: `+412`, `-8,012`, `+34.1K`, `+341K`, `+1.2M`. Exact under ten
/// thousand, where a rounded figure would claim a precision it does not have. An ASCII minus.
pub fn lines_text(sign: char, n: u64) -> String {
    if n < 10_000 {
        format!("{sign}{}", count_text(n))
    } else {
        format!("{sign}{}", short_text(n as f64, 100.))
    }
}

pub fn summarize(facts: &Facts, period: &Period, local: Local) -> Recap {
    let (lo, hi) = (period.start(), period.until());
    let elapsed = period.days.windows(2).filter(|d| d[0] < hi).count() as u32;
    let recorded = match facts.first_record {
        Some(first) => period.days.windows(2).filter(|d| d[0] < hi && d[1] > first).count() as u32,
        None => 0,
    };
    let length = period.len();
    let first_recorded = match facts.first_record {
        Some(first) => period.days.windows(2).position(|d| d[1] > first).unwrap_or(length),
        None => length,
    };
    let known = (first_recorded, elapsed as usize);
    let mut recap = Recap {
        period: period.span,
        from: date(local, period.start()),
        to: date(local, period.last_day()),
        so_far: period.so_far,
        days_recorded: recorded,
        days_elapsed: elapsed,
        days: length as u32,
        first_weekday: weekday(&at(local, (period.start() / 1000.).floor() as i64)) as u32,
        first_recorded_day: first_recorded as u32,
        by_day: PerDay::new(length, 0., known),
        by_hour: PerDay::new(length, [0.; 24], known),
        ..Recap::default()
    };
    let by_id: HashMap<&str, &Run> = facts.runs.iter().map(|r| (r.id.as_str(), r)).collect();

    // When each run was open, clipped to the period. A run still going is open until now.
    let open = |r: &Run| -> Option<(f64, f64)> {
        let a = r.started?;
        let b = r.completed.or_else(|| matches!(r.status.as_str(), "running" | "waiting" | "starting").then_some(period.now))?;
        let (a, b) = (a.max(lo), b.min(hi));
        (b > a).then_some((a, b))
    };
    // Shared blocking pieces per thread, ordered so sums are repeatable.
    let mut asks_by_thread: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
    let mut answered_by_thread: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
    let mut open_by_thread: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();
    let mut native_by_run: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();
    let mut answered = vec![];
    let mut working = facts.working.clone();
    for run in &facts.runs {
        if let Some(a) = run.started {
            working.entry(run.thread.clone()).or_default().push((a, run.completed.unwrap_or(period.now)));
        }
    }
    for ask in &facts.asks {
        let record = ask.record.unwrap_or(anatomy::Ask {
            kind: anatomy::AskKind::Approval,
            start: ask.asked,
            end: ask.answered,
            own_run: None,
            human_after: None,
        });
        let pieces = anatomy::blocking(&record, working.get(&ask.thread).map_or(&[], Vec::as_slice), period.now);
        asks_by_thread.entry(ask.thread.as_str()).or_default().extend_from_slice(&pieces);
        let ended = ask.answered.is_some() || (record.kind == anatomy::AskKind::Secret && record.own_run.is_some_and(|(_, b)| b.is_some()));
        if ended {
            answered_by_thread.entry(ask.thread.as_str()).or_default().extend_from_slice(&pieces);
        }
        if ask.asked >= lo && ask.asked < hi && !pieces.is_empty() {
            recap.asks.count += 1;
            if ended {
                answered.push(pieces.iter().map(|(a, b)| b - a).sum::<f64>());
            }
        }
    }
    for h in &facts.handoffs {
        if let (true, Some(run), Some(a)) = (h.native, &h.run, h.started) {
            native_by_run.entry(run.as_str()).or_default().push((a, h.completed.unwrap_or(period.now)));
        }
    }

    // Hours, their three-way split, the shape of the week, and the most agents at once.
    let (mut run_ms, mut tool_ms) = (0., 0.);
    let mut edges = vec![];
    let mut models: BTreeMap<String, (f64, u32, f64, bool, String)> = BTreeMap::new();
    let mut provider_ms: HashMap<&str, f64> = HashMap::new();
    let (mut threads, mut projects) = (HashSet::new(), HashSet::new());
    // Calls a provider marked finished all at once, when the run ended, have no end of their
    // own: taken as recorded, a run whose calls were closed that way is tool time from its
    // first call on. The rule that finds them is shared with Trace and Tools
    // (`anatomy::closed_together`, among one thread's calls), and so is what is done with
    // them: each still counts as a call, and its stretch is not tool time. Nothing is put in
    // its place, since nothing records when it really ended; the time falls to the model's
    // part, and the tools' part can only be too small.
    let mut calls_by_thread: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (r, run) in facts.runs.iter().enumerate() {
        if open(run).is_none() {
            continue;
        }
        for c in 0..run.tools.len() {
            calls_by_thread.entry(run.thread.as_str()).or_default().push((r, c));
        }
    }
    let mut together: HashSet<(usize, usize)> = HashSet::new();
    for places in calls_by_thread.values() {
        let calls: Vec<(f64, Option<f64>)> = places.iter().map(|&(r, c)| (facts.runs[r].tools[c].0, Some(facts.runs[r].tools[c].1))).collect();
        for (place, closed) in places.iter().zip(anatomy::closed_together(&calls)) {
            if closed {
                together.insert(*place);
            }
        }
    }
    recap.closed_together = together.len() as u32;
    for (r, run) in facts.runs.iter().enumerate() {
        let Some((a, b)) = open(run) else { continue };
        recap.runs += 1;
        run_ms += b - a;
        open_by_thread.entry(run.thread.as_str()).or_default().push((a, b));
        let asked = asks_by_thread.get(run.thread.as_str()).cloned().unwrap_or_default();
        let mut busy: Vec<(f64, f64)> = run.tools.iter().enumerate().filter(|(c, _)| !together.contains(&(r, *c))).map(|(_, call)| *call).collect();
        busy.extend(native_by_run.get(run.id.as_str()).into_iter().flatten().copied());
        let parts = anatomy::split(a, b, &busy, &asked);
        tool_ms += union(&parts.tools, a, b);
        edges.push((a, 1));
        edges.push((b, -1));
        spread(&mut recap, period, a, b);
        let entry = models.entry(run.model.clone()).or_insert((0., 0, 0., false, run.provider.clone()));
        entry.0 += b - a;
        entry.1 += 1;
        *provider_ms.entry(run.provider.as_str()).or_default() += b - a;
        threads.insert(run.thread.as_str());
        if let Some((project, _)) = facts.threads.get(&run.thread) {
            projects.insert(project.as_str());
        }
        recap.tool_calls += run.calls;
        recap.commands += run.commands;
        recap.commands_failed += run.commands_failed;
        recap.compactions += run.compactions;
        recap.lines_added += run.added;
        recap.lines_removed += run.removed;
        recap.file_changes += run.files;
        recap.runs_changing_files += u32::from(run.files > 0);
    }
    // A thread's own waiting instants count once even if two of its runs overlap.
    let mut wait_in_runs = 0.;
    for (thread, runs) in &open_by_thread {
        for (a, b) in merged(runs, lo, hi) {
            wait_in_runs += union(asks_by_thread.get(thread).map_or(&[], Vec::as_slice), a, b);
        }
    }
    // The time a thread waited on you with no run of its own open: its run had ended. It is
    // agent time too, so it goes into the day and the hours it fell in, and into the most at
    // once. Deliberate difference from Trace: Trace counts a still-open ask to now so one
    // task agrees with the live dashboard. Recap counts nothing outside runs until an ask
    // ends (a link can end at a human update), then counts its whole blocking interval.
    // Otherwise an ask nobody came back to would add every hour since in every period.
    let mut wait_after_runs = 0.;
    for (thread, asked) in &answered_by_thread {
        for (a, b) in outside(asked, open_by_thread.get(thread).map_or(&[][..], Vec::as_slice), lo, hi) {
            wait_after_runs += b - a;
            spread(&mut recap, period, a, b);
            edges.push((a, 1));
            edges.push((b, -1));
        }
    }
    let waiting = wait_in_runs + wait_after_runs;
    recap.asks.hours = waiting / HOUR;
    recap.split = Split { tools: tool_ms / HOUR, model: (run_ms - wait_in_runs - tool_ms).max(0.) / HOUR, waiting: waiting / HOUR };
    recap.hours = recap.split.tools + recap.split.model + recap.split.waiting;
    recap.threads = threads.len() as u32;
    recap.projects = projects.len() as u32;
    edges.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    let mut live = 0i64;
    for (when, step) in edges {
        live += step;
        if live > i64::from(recap.most_at_once) {
            recap.most_at_once = live as u32;
            recap.most_at_once_at = Some(when);
        }
    }
    recap.most_at_once_when = recap.most_at_once_at.map(|when| {
        let t = at(local, (when / 1000.).floor() as i64);
        let day = DAYS[weekday(&t) as usize];
        match period.span {
            Span::Week => format!("{day} {:02}:{:02}", t.hour, t.minute),
            Span::Month => format!("{day} {}, {:02}:{:02}", t.day, t.hour, t.minute),
        }
    });
    let mut active: Vec<f64> = recap.by_day.iter().copied().filter(|hours| *hours > 0.).collect();
    active.sort_by(f64::total_cmp);
    recap.days_active = active.len() as u32;
    recap.typical_day = (!active.is_empty()).then(|| {
        let mid = active.len() / 2;
        if active.len() % 2 == 1 { active[mid] } else { (active[mid - 1] + active[mid]) / 2. }
    });
    answered.sort_by(f64::total_cmp);
    if !answered.is_empty() {
        let mid = answered.len() / 2;
        recap.asks.typical_ms = Some(if answered.len() % 2 == 1 { answered[mid] } else { (answered[mid - 1] + answered[mid]) / 2. });
        recap.asks.longest_ms = answered.last().copied();
    }

    // Tasks: a run someone asked for in the period. A continuation is part of an earlier task.
    for run in facts.runs.iter().filter(|r| !r.continuation && r.requested >= lo && r.requested < hi) {
        let t = &mut recap.tasks;
        t.total += 1;
        let delegate = facts.threads.get(&run.thread).is_some_and(|(_, d)| *d);
        match run.created_by {
            Some(true) => {
                t.from_you += 1;
                t.from_phone += u32::from(run.source);
            }
            Some(_) => t.from_agents += 1,
            None if delegate => t.from_agents += 1,
            None => t.from_you += 1,
        }
        match run.status.as_str() {
            "completed" => t.finished += 1,
            "failed" => {
                t.failed += 1;
                match run.cause {
                    Some(Cause::Setup) => t.failed_setup += 1,
                    Some(Cause::Limit) => {
                        t.failed_limit += 1;
                        let label = provider_label(&run.provider);
                        match t.limit_by.iter_mut().find(|(name, _)| *name == label) {
                            Some(entry) => entry.1 += 1,
                            None => t.limit_by.push((label, 1)),
                        }
                    }
                    Some(Cause::Provider) => t.failed_provider += 1,
                    _ => {}
                }
            }
            "interrupted" => t.stopped += 1,
            "cancelled" | "rolled_back" => t.cancelled += 1,
            _ => t.running += 1,
        }
    }

    // Tokens, by the run each turn belongs to: a thread's runs follow one another, so a turn
    // belongs to the run that was open when it finished.
    let mut runs_by_thread: HashMap<&str, Vec<&Run>> = HashMap::new();
    for run in &facts.runs {
        runs_by_thread.entry(run.thread.as_str()).or_default().push(run);
    }
    let mut silent: HashMap<&str, bool> = HashMap::new();
    for turn in &facts.turns {
        let run = runs_by_thread.get(turn.thread.as_str()).and_then(|runs| {
            runs.iter().find(|r| r.started.is_some_and(|a| a <= turn.completed) && r.completed.is_none_or(|b| turn.completed <= b + 5000.))
        });
        let reports = turn.usage.as_deref().is_some_and(|u| u != "unavailable") && turn.input.is_some();
        if let Some(run) = run {
            let quiet = silent.entry(run.provider.as_str()).or_insert(true);
            *quiet = *quiet && !reports;
            if let (true, Some(entry)) = (reports, models.get_mut(&run.model)) {
                entry.2 += turn.input.unwrap_or(0.);
                entry.3 = true;
            }
        }
        if reports {
            recap.tokens.reported = true;
            recap.tokens.input += turn.input.unwrap_or(0.);
            recap.tokens.output += turn.output.unwrap_or(0.);
            recap.tokens.cached += turn.cached.unwrap_or(0.);
        }
        if let (Some(used), Some(max)) = (turn.used, turn.max) {
            recap.turns_past_90 += u32::from(max > 0. && used / max > 0.9);
        }
    }
    let mut left_out: Vec<(String, f64)> =
        silent.iter().filter(|(_, quiet)| **quiet).map(|(p, _)| (provider_label(p), provider_ms.get(p).copied().unwrap_or(0.) / HOUR)).collect();
    left_out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    recap.tokens.left_out = left_out;

    let mut rows: Vec<Model> = models
        .into_iter()
        .map(|(id, (millis, runs, tokens, reported, provider))| Model {
            family: family_name(terminal::family(&id, Some(&provider))),
            name: terminal::short_model(&id),
            hours: millis / HOUR,
            runs,
            tokens_in: reported.then_some(tokens),
        })
        .collect();
    rows.sort_by(|a, b| b.hours.total_cmp(&a.hours).then(a.name.cmp(&b.name)));
    recap.models = rows;

    // Handed to agents: delegations that started in the period.
    let mut pairs: BTreeMap<(String, String), u32> = BTreeMap::new();
    for h in &facts.handoffs {
        let Some(a) = h.started.filter(|a| *a >= lo && *a < hi) else { continue };
        recap.handed.count += 1;
        recap.handed.hours += (h.completed.unwrap_or(period.now).min(hi) - a).max(0.) / HOUR;
        recap.handed.failed += u32::from(matches!(h.status.as_str(), "failed" | "interrupted" | "cancelled"));
        if let Some(parent) = h.run.as_deref().and_then(|id| by_id.get(id)) {
            *pairs.entry((terminal::short_model(&parent.model), terminal::short_model(&h.model))).or_default() += 1;
        }
    }
    let mut pairs: Vec<(String, String, u32)> = pairs.into_iter().map(|((from, to), n)| (from, to, n)).collect();
    pairs.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    pairs.truncate(3);
    recap.handed.pairs = pairs;

    // Pull requests, each counted once however many threads link it.
    let mut seen = HashSet::new();
    for pull in &facts.pulls {
        if !seen.insert(pull.key.as_str()) {
            continue;
        }
        let within = |t: Option<f64>| t.is_some_and(|t| t >= lo && t < hi);
        if within(pull.merged) {
            recap.prs_merged += 1;
        } else if pull.state == "closed" && within(pull.closed) {
            recap.prs_closed += 1;
        } else if pull.state == "open" && pull.linked.is_some_and(|t| t < hi) {
            recap.prs_open += 1;
        }
    }
    recap.findings = findings(&recap);
    recap
}

/// Up to three plain facts, each with what it rests on. Fixed rules, in order of weight; a
/// rule below its threshold says nothing. `mark` names the palette role of the bar drawn
/// beside it: a plan limit is what `warn` means, and waiting on you is what `hot` means.
fn findings(r: &Recap) -> Vec<Finding> {
    let mut out = vec![];
    let t = &r.tasks;
    let wait = |millis: f64| terminal::span(Some(millis / 1000.));
    let limits = t.failed_limit >= 1;
    if limits {
        let mut by = t.limit_by.clone();
        by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.push(Finding {
            kind: "limit",
            number: t.failed_limit.to_string(),
            text: format!("{} stopped on a plan limit", if t.failed_limit == 1 { "task" } else { "tasks" }),
            rests_on: by.iter().map(|(name, n)| format!("{name} {n}")).collect::<Vec<_>>().join(" · "),
        });
    }
    if r.asks.count >= 3 && r.asks.hours >= 0.5 {
        let mut rests = vec![];
        if let Some(typical) = r.asks.typical_ms {
            rests.push(format!("typical {}", wait(typical)));
        }
        if let Some(longest) = r.asks.longest_ms {
            rests.push(format!("longest {}", wait(longest)));
        }
        out.push(Finding {
            kind: "waiting",
            number: hours_text(r.asks.hours),
            text: format!("agents waited on you, over {} asks", r.asks.count),
            rests_on: rests.join(" · "),
        });
    }
    // A plan limit already has its own line, so it is not counted here a second time.
    let failed = if limits { t.failed - t.failed_limit } else { t.failed };
    if t.failed >= 3 && failed >= 1 {
        let mut rests = vec![];
        for (n, label) in [(t.failed_setup, "setup"), (if limits { 0 } else { t.failed_limit }, "plan limit"), (t.failed_provider, "provider error")] {
            if n > 0 {
                rests.push(format!("{label} {n}"));
            }
        }
        out.push(Finding {
            kind: "failed",
            number: failed.to_string(),
            text: format!("{}{} failed, of {}", if limits { "other " } else { "" }, if failed == 1 { "task" } else { "tasks" }, count_text(u64::from(t.total))),
            rests_on: rests.join(" · "),
        });
    }
    if r.compactions >= 3 {
        out.push(Finding {
            kind: "context",
            number: r.compactions.to_string(),
            text: "times a thread was compacted to make room".into(),
            rests_on: format!("{} turns ran past 90% full", r.turns_past_90),
        });
    } else if r.turns_past_90 >= 3 {
        out.push(Finding {
            kind: "context",
            number: r.turns_past_90.to_string(),
            text: "turns ran past 90% full".into(),
            rests_on: format!("{} compactions", r.compactions),
        });
    }
    if t.from_you >= 20 && t.from_phone * 10 >= t.from_you {
        out.push(Finding {
            kind: "phone",
            number: format!("{}%", (f64::from(t.from_phone) * 100. / f64::from(t.from_you)).round()),
            text: "of the tasks you started came from your phone".into(),
            rests_on: format!("{} of {}", t.from_phone, t.from_you),
        });
    }
    if let Some((from, to, n)) = r.handed.pairs.first().filter(|pair| r.handed.count >= 10 && pair.2 * 4 >= r.handed.count) {
        out.push(Finding {
            kind: "handed",
            number: n.to_string(),
            text: format!("of {} handoffs went from {from} to {to}", r.handed.count),
            rests_on: format!("{} did not finish", r.handed.failed),
        });
    }
    out.truncate(3);
    out
}

/// Read and work out a period from T3's database.
pub fn load(path: &Path, period: &Period, local: Local) -> Result<Recap, SourceError> {
    let db = open(path)?;
    let reader = ItemReader { now: period.now, times: TimeQuery::configured() };
    let workers = ItemWorkers { path, count: fanout::configured_workers() };
    #[cfg(unix)]
    let probe = if cache_enabled() { cache::Snapshot::probe(&db, path) } else { None };
    let facts = read_in_transaction(&db, path, || {
        #[cfg(unix)]
        let cache = probe.and_then(|probe| probe.open(&db));
        read(&db, period, |facts| {
            #[cfg(unix)]
            if let Some(cache) = cache {
                record_cache_stats(0, 0, 0);
                return read_cached_items(&db, facts, &reader, &workers, cache, || {});
            }
            read_file_items(&db, facts, &reader, &workers)
        })
    })?;
    Ok(summarize(&facts, period, local))
}

// ── A made-up week or month ─────────────────────────────────────────────────

/// A made-up period, built as a small in-memory T3 database and read through the same reader,
/// so `--demo` shows what a real week or month would without opening anyone's records.
pub fn demo(period: &Period, local: Local) -> Result<Recap, SourceError> {
    let path = Path::new(":memory:");
    let fail = |e: rusqlite::Error| source_error(path, "schema", e.to_string());
    let db = Connection::open_in_memory().map_err(fail)?;
    let first = weekday(&at(local, (period.start() / 1000.).floor() as i64)) as usize;
    demo_fill(&db, period, first).map_err(fail)?;
    let facts = read_from(&db, path, period)?;
    Ok(summarize(&facts, period, local))
}

pub(crate) const DEMO_SCHEMA: &str = "
CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,payload_json TEXT,deleted_at TEXT,archived_at TEXT);
CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_subagents(subagent_id TEXT PRIMARY KEY,thread_id TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_nodes(node_id TEXT PRIMARY KEY,thread_id TEXT,parent_node_id TEXT,run_id TEXT);
CREATE TABLE orchestration_v2_projection_provider_turns(thread_id TEXT,provider_thread_id TEXT,ordinal INTEGER,payload_json TEXT,started_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_turn_items(item_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,node_id TEXT,provider_thread_id TEXT,type TEXT,status TEXT,updated_at TEXT,ordinal INTEGER,payload_json TEXT);
CREATE INDEX demo_items_by_run ON orchestration_v2_projection_turn_items(run_id, ordinal);
CREATE TABLE orchestration_v2_projection_runtime_requests(request_id TEXT PRIMARY KEY,thread_id TEXT,node_id TEXT,kind TEXT,status TEXT,created_at TEXT,resolved_at TEXT);";

/// A repeatable number in `[0, 1)` for a seed: the demo is the same every time.
fn chance(seed: u64) -> f64 {
    let mut h = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0xd1b5_4a32_d192_ed03);
    h ^= h >> 31;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 29;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// `first_weekday` is the weekday of the period's first day, Monday being 0.
fn demo_fill(db: &Connection, period: &Period, first_weekday: usize) -> rusqlite::Result<()> {
    use serde_json::json;
    // (model, provider, share of the tasks, minutes a task takes, reports tokens)
    const MODELS: [(&str, &str, f64, f64, bool); 6] = [
        ("claude-opus-5-5", "claudeAgent", 0.30, 9., true),
        ("gpt-6.1-sol", "codex", 0.27, 8., true),
        ("claude-sonnet-5-5", "claudeAgent", 0.18, 7., true),
        ("grok-4.7", "grok", 0.10, 9., false),
        ("claude-haiku-5-5", "claudeAgent", 0.10, 4., true),
        ("deepseek/deepseek-flash", "opencode", 0.05, 6., true),
    ];
    // Tasks a day, Monday first: a working week with a busy Thursday and a quiet weekend.
    const PER_DAY: [u64; 7] = [58, 96, 112, 171, 104, 39, 51];
    db.execute_batch(DEMO_SCHEMA)?;
    db.execute_batch("BEGIN")?;
    let minute = 60_000.;
    let mut seed = 0u64;
    let mut next = || {
        seed += 1;
        chance(seed)
    };
    let (mut thread_no, mut run_no, mut item_no, mut pulls) = (0u64, 0u64, 0u64, 0u64);
    let mut item = |db: &Connection, thread: &str, run: &str, kind: &str, status: &str, at: f64, payload: serde_json::Value| -> rusqlite::Result<()> {
        item_no += 1;
        db.execute(
            &format!("INSERT INTO {P}turn_items VALUES(?1,?2,?3,?4,NULL,?5,?6,?7,?8,?9)"),
            params![format!("i{item_no}"), thread, run, format!("n{item_no}"), kind, status, iso(at), item_no as i64, payload.to_string()],
        )?;
        Ok(())
    };
    for day in 0..period.len() {
        let (day_start, day_end) = (period.days[day], period.days[day + 1].min(period.until()));
        if day_start >= period.until() {
            break;
        }
        // A month is the same working week over again, each day busier or quieter than the
        // last by a repeatable amount, and busier as the month goes on.
        let tasks = match period.span {
            Span::Week => PER_DAY[day],
            Span::Month => {
                let drift = 0.72 + 0.4 * day as f64 / period.len() as f64 + 0.3 * chance(7_919 + day as u64);
                (PER_DAY[(first_weekday + day) % 7] as f64 * drift).round() as u64
            }
        };
        // Work starts between 7:30 and 20:30. A day still going holds only the share of it
        // that has had time to begin, and none before 7:30. `idle` stops the day when no
        // thread can start anything in what is left of it; without it, a day asked for early
        // in the morning never filled and never ended.
        let room = (((day_end - day_start) / HOUR - 7.5) / 13.).clamp(0., 1.);
        let tasks = (tasks as f64 * room).round() as u64;
        let mut idle = 0;
        let mut done = 0;
        while done < tasks && idle < 200 {
            let before = done;
            // A thread holds a few tasks, one after another.
            thread_no += 1;
            let thread = format!("t{thread_no}");
            let delegate = next() < 0.28;
            let pick = next();
            let mut share = 0.;
            let model = MODELS.iter().find(|m| { share += m.2; pick < share }).unwrap_or(&MODELS[0]);
            let mut pull_requests = vec![];
            if !delegate && next() < 0.07 && pulls < 14 {
                pulls += 1;
                let merged = next() < 0.68;
                let when = iso((day_start + (10. + next() * 9.) * HOUR).min(day_end - minute));
                pull_requests.push(json!({"host": "example.com", "repository": "demo/app", "number": pulls, "linkedAt": when,
                    "snapshot": {"state": if merged { "merged" } else if next() < 0.2 { "closed" } else { "open" },
                                 "mergedAt": if merged { json!(when) } else { json!(null) }, "closedAt": json!(null)}}));
            }
            db.execute(
                &format!("INSERT INTO {P}threads VALUES(?1,?2,NULL,NULL)"),
                params![thread, json!({"projectId": format!("p{}", thread_no % 9), "createdAt": iso(day_start),
                    "lineage": {"relationshipToParent": if delegate { json!("subagent") } else { json!(null) }}, "pullRequests": pull_requests}).to_string()],
            )?;
            // Work starts in the morning and thins out at night.
            let mut clock = day_start + (7.5 + next() * 13. * room) * HOUR;
            for ordinal in 0..(1 + (next() * 6.) as u64) {
                if done >= tasks || clock >= day_end - 3. * minute {
                    break;
                }
                done += 1;
                run_no += 1;
                let run = format!("r{run_no}");
                let length = (model.3 * (0.25 + next() * 2.6) * minute).min(day_end - clock - minute);
                let (started, completed) = (clock + 2000., clock + 2000. + length);
                let roll = next();
                let status = if roll < 0.966 { "completed" } else if roll < 0.982 { "failed" } else if roll < 0.992 { "interrupted" } else { "cancelled" };
                db.execute(
                    &format!("INSERT INTO {P}runs VALUES(?1,?2,?3,?4,?5,?6)"),
                    params![run, thread, ordinal as i64 + 1, json!({"status": status, "providerInstanceId": model.1, "startedAt": iso(started),
                        "modelSelection": {"model": model.0}}).to_string(), iso(clock), iso(completed)],
                )?;
                let by_phone = next() < 0.31;
                item(db, &thread, &run, "user_message", "completed", clock, json!({
                    "createdBy": if delegate { "agent" } else { "user" }, "creationSource": if delegate { "mcp" } else if by_phone { "mobile" } else { "web" }}))?;
                // Tool calls take a little under half the run.
                let mut cursor = started + length * 0.04;
                while cursor < completed - length * 0.06 {
                    let took = length * (0.02 + next() * 0.11);
                    let command = next() < 0.68;
                    let failed = command && next() < 0.043;
                    item(db, &thread, &run, if command { "command_execution" } else { "dynamic_tool" }, if failed { "failed" } else { "completed" }, cursor + took,
                        json!({"startedAt": iso(cursor), "completedAt": iso((cursor + took).min(completed))}))?;
                    cursor += took + length * (0.03 + next() * 0.13);
                }
                if status == "failed" {
                    let why = next();
                    let (class, code) = if why < 0.42 { ("validation_error", "workspace_preparation_failed") } else if why < 0.67 { ("usage_limit", "provider.plan-limit") } else { ("provider_error", "provider.unavailable") };
                    item(db, &thread, &run, "error", "failed", completed, json!({"failure": {"class": class, "code": code}}))?;
                }
                let mut files = vec![];
                if next() < 0.42 {
                    for _ in 0..(1 + (next() * 10.) as u64) {
                        files.push(json!({"kind": "modified", "additions": (next() * next() * 220.) as u64, "deletions": (next() * next() * 24.) as u64}));
                    }
                }
                item(db, &thread, &run, "checkpoint", "completed", completed, json!({"files": files}))?;
                if next() < 0.036 {
                    item(db, &thread, &run, "compaction", "completed", completed, json!({}))?;
                }
                if next() < 0.046 {
                    let asked = started + length * 0.5;
                    let wait = if next() < 0.12 { (40. + next() * 75.) * minute } else { (0.4 + next() * 4.5) * minute };
                    db.execute(&format!("INSERT INTO {P}nodes VALUES(?1,?2,NULL,?3)"), params![format!("q{run_no}"), thread, run])?;
                    db.execute(
                        &format!("INSERT INTO {P}runtime_requests VALUES(?1,?2,?3,'user_input','resolved',?4,?5)"),
                        params![format!("a{run_no}"), thread, format!("q{run_no}"), iso(asked), iso((asked + wait).min(period.until()))],
                    )?;
                }
                if next() < 0.33 {
                    let child = &MODELS[((next() * 6.) as usize).min(5)];
                    let took = (4. + next() * 24.) * minute;
                    let ended = next();
                    db.execute(
                        &format!("INSERT INTO {P}subagents VALUES(?1,?2,?3)"),
                        params![format!("s{run_no}"), thread, json!({"runId": run, "origin": if next() < 0.2 { "provider_native" } else { "app_owned" },
                            "status": if ended < 0.975 { "completed" } else { "failed" }, "model": child.0, "startedAt": iso(started + length * 0.2),
                            "completedAt": iso((started + length * 0.2 + took).min(period.until()))}).to_string()],
                    )?;
                }
                let input = (0.4 + next() * 9.5) * 1e6;
                let usage = if model.4 { json!({"usageStatus": "complete", "inputTokens": input as u64, "outputTokens": (input * 0.0037) as u64, "cachedInputTokens": (input * 0.98) as u64}) } else { json!({"usageStatus": "unavailable"}) };
                let fill = next();
                db.execute(
                    &format!("INSERT INTO {P}provider_turns VALUES(?1,NULL,?2,?3,?4,?5)"),
                    params![thread, ordinal as i64 + 1, json!({"turnTokenUsage": usage, "tokenUsage": if model.4 { json!({"usedTokens": (fill * fill * 950_000.) as u64, "maxTokens": 1_000_000}) } else { json!(null) }}).to_string(),
                        iso(started), iso(completed)],
                )?;
                clock = completed + (1. + next() * 16.) * minute;
            }
            idle = if done == before { idle + 1 } else { 0 };
        }
    }
    db.execute_batch("COMMIT")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All item facts survive storage, including timestamp bits and tool order.
    #[test]
    fn run_facts_bytes() {
        let full = RunFacts { created_by: Some(true), source: true, tools: vec![(1_795_069_560_000.125, 1_795_069_560_001.75), (-0., 0.)], calls: 30, commands: 4, commands_failed: 2, compactions: 3, files: 8, added: 90, removed: 12, cause: Some(Cause::Provider), card_ordinals: vec![-3, 4, i64::MAX], ..RunFacts::default() };
        for facts in [full.clone(), RunFacts::default()] {
            let bytes = facts.to_bytes().unwrap();
            let restored = RunFacts::from_bytes(&bytes).unwrap();
            assert_eq!(restored, facts);
            assert_eq!(restored.to_bytes(), Some(bytes.clone()));
            for end in 0..bytes.len() {
                assert_eq!(RunFacts::from_bytes(&bytes[..end]), None, "cut at {end}");
            }
            let mut appended = bytes.clone();
            appended.push(0);
            assert_eq!(RunFacts::from_bytes(&appended), None);
            let mut version = bytes;
            version[..4].copy_from_slice(&(RunFacts::VERSION + 1).to_le_bytes());
            assert_eq!(RunFacts::from_bytes(&version), None);
        }
        for facts in [
            RunFacts { tools: vec![(2., 1.)], ..full.clone() },
            RunFacts { tools: vec![(f64::NAN, 1.)], ..full.clone() },
            RunFacts { tools: vec![(0., f64::INFINITY)], ..full.clone() },
            RunFacts { commands_failed: 5, ..full.clone() },
            RunFacts { card_ordinals: vec![9, 7], ..full.clone() },
            RunFacts { card_ordinals: vec![7, 7], ..full.clone() },
        ] {
            assert_eq!(RunFacts::from_bytes(&facts.to_bytes().unwrap()), None);
        }
        for offset in [4, 5, 6] {
            let mut bytes = full.to_bytes().unwrap();
            bytes[offset] = 255;
            assert_eq!(RunFacts::from_bytes(&bytes), None);
        }
        let typical = RunFacts { tools: vec![(1_795_069_560_000.125, 1_795_069_560_001.75); 30], card_ordinals: vec![], ..full.clone() };
        assert_eq!(typical.to_bytes().unwrap().len(), 537);
        let max = RunFacts::MAX_CARD_ORDINALS as i64;
        let mut bounded = RunFacts { card_ordinals: (0..max).collect(), ..RunFacts::default() };
        assert!(RunFacts::from_bytes(&bounded.to_bytes().unwrap()).is_some());
        let mut too_many = bounded.to_bytes().unwrap();
        too_many[55..57].copy_from_slice(&((max + 1) as u16).to_le_bytes());
        too_many.extend(max.to_le_bytes());
        assert_eq!(RunFacts::from_bytes(&too_many), None);
        bounded.card_ordinals.push(max);
        assert_eq!(bounded.to_bytes(), None);
        assert_eq!(RunFacts { unaddressable_card: true, ..RunFacts::default() }.to_bytes(), None);
        let mut seed = 0x123456789abcdef0u64;
        for length in (0..1024).cycle().take(6000) {
            let bytes: Vec<u8> = (0..length).map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            }).collect();
            let _ = RunFacts::from_bytes(&bytes);
        }
    }

    /// A stored item pass produces the same complete card as a fresh item pass.
    #[test]
    fn stored_run_facts_preserve_recap() {
        let db = by_hand();
        for (n, kind) in ["dynamic_tool", "file_change", "file_search", "web_search", "compaction", "secret_request", "link_request"].iter().enumerate() {
            db.execute(&format!("INSERT INTO {P}turn_items VALUES(?1,'t1','r1','n',NULL,?2,'completed','2026-11-10T10:15:00.000Z',?3,?4)"), params![format!("extra{n}"), kind, (n + 10) as i64, r#"{"startedAt":"2026-11-10T10:01:00.000Z","completedAt":"2026-11-10T10:15:00.000Z"}"#]).unwrap();
        }
        db.execute_batch(&format!(
            r#"INSERT INTO {P}turn_items VALUES('no-node','t1','r1',NULL,NULL,'secret_request','waiting','2026-11-10T10:05:00.000Z',17,'{{}}');
             INSERT INTO {P}turn_items VALUES('same-ordinal','t1','r1','n',NULL,'secret_request','completed','2026-11-10T10:50:00.000Z',15,'{{"startedAt":"2026-11-10T10:40:00.000Z"}}');
             INSERT INTO {P}turn_items VALUES('secret-only','t1','r2','n',NULL,'secret_request','completed','2026-11-10T13:20:00.000Z',15,'{{"startedAt":"2026-11-10T13:10:00.000Z"}}');
             INSERT INTO {P}turn_items VALUES('link-only','t2','r3','n',NULL,'link_request','waiting','2026-11-10T10:15:00.000Z',16,'{{}}');
             INSERT INTO {P}runs VALUES('r4','t1',2,'{{}}','2026-11-10T14:00:00.000Z','2026-11-10T14:15:00.000Z');"#
        )).unwrap();
        let period = week();
        let normal = read_from(&db, Path::new("test"), &period).unwrap();
        let stored = read(&db, &period, |facts| {
            let mut items = prepare_items(&db, TimeQuery::Sqlite)?;
            let mut cards = prepare_run_cards(&db, "demo_items_by_run", TimeQuery::Sqlite)?;
            for run in &mut facts.runs {
                let (item_facts, _) = items_of(&mut items, run, period.now)?;
                let item_facts = RunFacts::from_bytes(&item_facts.to_bytes().unwrap()).unwrap();
                let expected: &[i64] = match run.id.as_str() {
                    "r1" => &[15, 16, 17],
                    "r2" => &[15],
                    "r3" => &[16],
                    _ => &[],
                };
                assert_eq!(item_facts.card_ordinals, expected);
                facts.asks.extend(run_cards(&mut cards, run, period.now, &item_facts.card_ordinals)?);
                run.apply(item_facts);
            }
            Ok(())
        }).unwrap();
        assert_eq!(summarize(&normal, &period, &zone), summarize(&stored, &period, &zone));
    }

    /// Reuse the same bytes after a card changes, independent of store invalidation.
    #[test]
    fn cached_run_reads_the_current_card_and_falls_back_without_an_index() {
        let db = by_hand();
        let period = week();
        db.execute(&format!(
            "INSERT INTO {P}turn_items VALUES('link','t2','r3','n',NULL,'link_request','waiting','2026-11-10T10:15:00.000Z',4,'{{}}')"
        ), []).unwrap();
        let open = read_from(&db, Path::new("fixture"), &period).unwrap();
        let run = open.runs.iter().find(|run| run.id == "r3").unwrap();
        let (items, _) = items_of(&mut prepare_items(&db, TimeQuery::Sqlite).unwrap(), run, period.now).unwrap();
        let hits = HashMap::from([(run.id.clone(), items.to_bytes().unwrap())]);
        db.execute(&format!(
            "UPDATE {P}turn_items SET status='completed', updated_at='2026-11-10T11:00:00.000Z',
             payload_json=?1 WHERE item_id='link'"
        ), [r#"{"startedAt":"2026-11-10T10:15:00.000Z"}"#]).unwrap();
        let direct = read_from(&db, Path::new("fixture"), &period).unwrap();
        let expected = summarize(&direct, &period, &zone);
        assert!(expected.split.waiting > summarize(&open, &period, &zone).split.waiting);
        for expected_hits in [1, 0] {
            let cached = read_in_transaction(&db, Path::new("fixture"), || {
                read(&db, &period, |facts| {
                    let used = read_items_with_hits(&db, facts, period.now, TimeQuery::Sqlite, hits.clone(), |_| None, |_, _, _| {})?;
                    assert_eq!(used, expected_hits);
                    Ok(())
                })
            }).unwrap();
            assert_eq!(summarize(&cached, &period, &zone), expected);
            if expected_hits == 1 {
                db.execute_batch("DROP INDEX demo_items_by_run").unwrap();
            }
        }
    }

    /// Cache hits can change the arrival order of cards without changing the Recap.
    #[test]
    fn summarize_is_independent_of_ask_order() {
        let db = plan_database(include_str!("../../../checks/fixtures/t3-schema-main-096.sql"));
        let period = week();
        let mut facts = read_from(&db, Path::new("fixture"), &period).unwrap();
        let expected = summarize(&facts, &period, &zone);
        assert!(facts.asks.len() >= 4);
        let mut seed = 7u64;
        for _ in 0..32 {
            for end in (1..facts.asks.len()).rev() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                facts.asks.swap(end, (seed >> 32) as usize % (end + 1));
            }
            assert_eq!(summarize(&facts, &period, &zone), expected);
        }
    }

    /// Replay T3's public schema and copy only the made-up reader inputs into it.
    fn plan_database(schema: &str) -> Connection {
        let source = by_hand();
        for (n, kind) in TOOLS.iter().chain(["user_message", "error", "checkpoint", "compaction", "secret_request", "link_request"].iter()).enumerate() {
            source.execute(&format!("INSERT INTO {P}turn_items VALUES(?1,'t1','r1','n',NULL,?2,'waiting','2026-11-10T10:15:00.000Z',?3,?4)"), params![format!("plan{n}"), kind, (n + 10) as i64, r#"{"startedAt":"2026-11-10T10:01:00.000Z","requestId":"q1"}"#]).unwrap();
        }
        source.execute(&format!("INSERT INTO {P}runs VALUES('old','t1',0,'{{}}','2026-11-01T00:00:00.000Z','2026-11-02T00:00:00.000Z')"), []).unwrap();
        source.execute(&format!("INSERT INTO {P}turn_items VALUES('old-link','t1','old','n',NULL,'link_request','waiting','2026-11-02T00:00:00.000Z',100,'{{}}')"), []).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(schema).unwrap();
        db.execute_batch(r#"INSERT INTO orchestration_v2_projection_messages
            (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
            VALUES('human','t1','user',0,'2026-11-02T00:00:00.000Z','2026-11-10T10:30:00.000Z','{"createdBy":"user"}');"#).unwrap();
        for table in ["threads", "runs", "turn_items", "runtime_requests", "subagents", "provider_turns"] {
            let name = format!("{P}{table}");
            let mut columns = db.prepare(&format!("PRAGMA table_info({name})")).unwrap();
            let columns = columns.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            let mut rows = source.prepare(&format!("SELECT * FROM {name}")).unwrap();
            let names: Vec<String> = rows.column_names().map(|s| s.to_string()).collect();
            let mut rows = rows.query([]).unwrap();
            let mut ordinal = 0i64;
            while let Some(row) = rows.next().unwrap() {
                ordinal += 1;
                let values: Vec<rusqlite::types::Value> = columns.iter().map(|(column, kind, required)| {
                    let old = match column.as_str() { "turn_item_id" => "item_id", "runtime_request_id" => "request_id", _ => column };
                    let value = names.iter().position(|n| n == old).map(|i| row.get::<_, rusqlite::types::Value>(i).unwrap());
                    if column == "ordinal" {
                        return rusqlite::types::Value::Integer(ordinal);
                    }
                    match value {
                        Some(v) if v != rusqlite::types::Value::Null || !required => v,
                        _ if *required => if kind == "INTEGER" { 0.into() } else { "fixture".to_string().into() },
                        _ => rusqlite::types::Value::Null,
                    }
                }).collect();
                let columns = columns.iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(",");
                let placeholders = vec!["?"; values.len()].join(",");
                db.execute(&format!("INSERT INTO {name}({columns}) VALUES({placeholders})"), rusqlite::params_from_iter(values)).unwrap();
            }
        }
        db
    }

    #[test]
    fn link_cutoff_for_an_earlier_run_on_pinned_schemas() {
        for schema in [
            include_str!("../../../checks/fixtures/t3-schema-main-096.sql"),
            include_str!("../../../checks/fixtures/t3-schema-replayed-083.sql"),
            include_str!("../../../checks/fixtures/t3-schema-older-074.sql"),
        ] {
            let db = plan_database(schema);
            let cutoff = || {
                let facts = read_from(&db, Path::new("fixture"), &week()).unwrap();
                // This run ended before the report period. Recap needs the older
                // waiting-link index on 074 and 083 to find its card.
                let link = facts.asks.iter().find(|ask| {
                    Some(ask.asked) == ms("2026-11-02T00:00:00.000Z")
                        && ask.record.is_some_and(|record| record.kind == anatomy::AskKind::Link)
                }).unwrap();
                link.answered
            };
            assert_eq!(cutoff(), ms("2026-11-10T10:30:00.000Z"));
            db.execute_batch(&format!("UPDATE {P}messages SET payload_json='{{\"createdBy\":\"agent\"}}'")).unwrap();
            assert_eq!(cutoff(), None);
            db.execute_batch(&format!("DROP TABLE {P}messages")).unwrap();
            assert_eq!(cutoff(), None);
        }
    }

    /// A private file source and cache for the worker tests.
    #[cfg(unix)]
    struct WorkerFixture {
        dir: std::path::PathBuf,
        source: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl WorkerFixture {
        /// Write four runs, enough to make the file reader use more than one connection.
        fn new() -> Self {
            let output = std::process::Command::new("mktemp").arg("-d").output().unwrap();
            assert!(output.status.success());
            let dir = std::path::PathBuf::from(std::str::from_utf8(&output.stdout).unwrap().trim_end());
            let source = dir.join("recap.sqlite");
            let fixture = Self { dir, source };
            let db = by_hand();
            db.execute_batch(&format!(
                "INSERT INTO {P}runs SELECT 'r4', thread_id, 2, payload_json, requested_at, completed_at
                 FROM {P}runs WHERE run_id='r2';
                 ALTER TABLE {P}runs ADD COLUMN status TEXT;
                 UPDATE {P}runs SET status=json_extract(payload_json,'$.status');"
            )).unwrap();
            db.execute("VACUUM INTO ?1", [fixture.source.to_str().unwrap()]).unwrap();
            fixture
        }
    }

    #[cfg(unix)]
    impl Drop for WorkerFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Check the worker queries directly so a serial retry cannot hide a setup failure.
    #[cfg(unix)]
    #[test]
    fn recap_workers_prepare_each_timestamp_query() {
        let fixture = WorkerFixture::new();
        let main = open(&fixture.source).unwrap();
        let period = week();
        let facts = read_from(&main, &fixture.source, &period).unwrap();
        for times in [TimeQuery::Sqlite, TimeQuery::Full, TimeQuery::Head] {
            let reader = ItemReader { times, now: period.now };
            let batch = fanout::read_scopes(&fixture.source, 2, &facts.runs, open, &reader).unwrap();
            assert_eq!(batch.len(), 4);
            for (run, (pinned, result)) in facts.runs.iter().zip(batch) {
                assert!(pinned);
                let (items, _) = result.unwrap();
                assert_eq!(items.calls, run.calls);
                assert_eq!(items.tools, run.tools);
            }
        }
    }

    /// A WAL commit between pins must change neither the report nor the store.
    #[cfg(target_os = "linux")]
    #[test]
    fn recap_cache_rereads_workers_on_a_newer_snapshot() {
        let fixture = WorkerFixture::new();
        let store = fixture.dir.join("cache/insights.sqlite3");
        let writer = Connection::open(&fixture.source).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=1").unwrap();
        let main = open(&fixture.source).unwrap();
        let period = week();
        let reader = ItemReader { times: TimeQuery::Full, now: period.now };
        let workers = ItemWorkers { path: &fixture.source, count: 2 };
        let probe = (0..200).find_map(|_| {
            let probed = cache::Snapshot::probe_with_store(&main, &fixture.source, store.clone());
            if probed.is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            probed
        }).expect("synthetic source settled");
        main.execute_batch("BEGIN").unwrap();
        let cache = probe.open(&main).unwrap();
        let before = read(&main, &period, |facts| read_items(&main, facts, reader.now, reader.times)).unwrap();
        let mut changed = false;
        let after = read(&main, &period, |facts| {
            read_cached_items(&main, facts, &reader, &workers, cache, || {
                writer.execute(&format!(
                    "INSERT INTO {P}turn_items VALUES('later','t1','r1',NULL,NULL,'command_execution',
                     'completed','2026-11-10T11:00:00.000Z',2,?1)"
                ), [r#"{"startedAt":"2026-11-10T10:55:00.000Z","completedAt":"2026-11-10T11:00:00.000Z"}"#]).unwrap();
                changed = true;
            })
        }).unwrap();
        main.execute_batch("ROLLBACK").unwrap();
        assert!(changed, "the commit must land before the workers pin");
        let old = summarize(&before, &period, &zone);
        assert_eq!(summarize(&after, &period, &zone), old);
        let later = read_from(&main, &fixture.source, &period).unwrap();
        assert_eq!(summarize(&later, &period, &zone).tool_calls, old.tool_calls + 1);
        let saved: i64 = Connection::open(&store).unwrap()
            .query_row("SELECT count(*) FROM facts", [], |row| row.get(0)).unwrap();
        assert_eq!(saved, 0);
    }

    /// Guard every prepared reader query against unbounded reads of item-sized tables.
    #[test]
    fn reader_query_plans() {
        let db = plan_database(include_str!("../../../checks/fixtures/t3-schema-main-096.sql"));
        let period = week();
        let direct = read_from(&db, Path::new("fixture"), &period).unwrap();
        let mut query = prepare_items(&db, TimeQuery::Sqlite).unwrap();
        let run = direct.runs.iter().find(|run| run.id == "r1").unwrap();
        let (items, _) = items_of(&mut query, run, period.now).unwrap();
        let hits = HashMap::from([(run.id.clone(), items.to_bytes().unwrap()), ("r2".into(), vec![])]);
        let mut statements = Vec::new();
        for times in [TimeQuery::Sqlite, TimeQuery::Full, TimeQuery::Head] {
            PREPARED.with(|sql| sql.borrow_mut().clear());
            let cached = read_in_transaction(&db, Path::new("fixture"), || {
                read(&db, &period, |facts| {
                    let used = read_items_with_hits(&db, facts, period.now, times, hits.clone(), |_| None, |_, _, _| {})?;
                    assert_eq!(used, 1);
                    Ok(())
                })
            }).unwrap();
            assert_eq!(summarize(&direct, &period, &zone), summarize(&cached, &period, &zone));
            let prepared = PREPARED.with(|sql| std::mem::take(&mut *sql.borrow_mut()));
            assert_eq!(prepared.len(), 22, "prepared statements: {prepared:#?}");
            assert_eq!(prepared.iter().filter(|sql| sql.contains(times.columns().split(',').next().unwrap()) && sql.contains("type IN (")).count(), 2);
            for sql in prepared {
                if !statements.contains(&sql) {
                    statements.push(sql);
                }
            }
        }
        for sql in statements {
            for sparse_links in [false, true].into_iter().filter(|sparse| !sparse || sql.contains("CROSS JOIN")) {
                // A sparse item table makes the unhinted link scan cheaper to SQLite. Exercise
                // that real planner choice after the full fixture has exercised every query.
                if sparse_links {
                    db.execute_batch("SAVEPOINT sparse; DELETE FROM orchestration_v2_projection_turn_items WHERE turn_item_id <> 'old-link'; ANALYZE").unwrap();
                }
                let mut explain = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                let bindings = vec![rusqlite::types::Value::Null; explain.parameter_count()];
                let plan = explain.query_map(rusqlite::params_from_iter(bindings), |r| r.get::<_, String>(3)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                if sparse_links {
                    db.execute_batch("ROLLBACK TO sparse; RELEASE sparse").unwrap();
                }
                eprintln!("SQL: {sql}\nPLAN: {plan:?}");
                if sql.contains("ordinal=?2") {
                    assert!(plan.iter().any(|step| step.contains("run_ordinal_idx (run_id=? AND ordinal=?)")));
                }
                for step in &plan {
                    if !step.starts_with("SCAN ") {
                        continue;
                    }
                    let table = step.split_whitespace().nth(1).unwrap();
                    for protected in ["orchestration_v2_projection_turn_items", "orchestration_v2_projection_messages", "orchestration_v2_events"] {
                        let words: Vec<_> = sql.split_whitespace().collect();
                        let alias = words.windows(2).any(|pair| pair[0] == protected && pair[1] == table);
                        assert!(table != protected && !alias, "protected table scan {step}\nSQL: {sql}\nPLAN: {plan:?}");
                    }
                    let accepted = matches!(table,
                        // Runs have no time index; there is one small row per run.
                        "orchestration_v2_projection_runs" |
                        // Handoffs have no global time index; there is one row per handoff.
                        "orchestration_v2_projection_subagents" |
                        // Provider turns have no completion-time index; no item payloads are read.
                        "orchestration_v2_projection_provider_turns" |
                        // Every thread supplies project, lineage and earliest-record facts.
                        "orchestration_v2_projection_threads" |
                        // Schema inspection is a small virtual table of column definitions.
                        "pragma_table_info" |
                        // SQLite's catalog supplies the availability of T3's link indexes.
                        "sqlite_master"
                    )
                    // Requests have no period index; there is one small row per request.
                    || (table == "r" && sql.contains(&format!("FROM {P}runtime_requests r")))
                    // Threads seed indexed link lookups and supply linked pull requests.
                    || (table == "t" && sql.contains(&format!("FROM {P}threads t")))
                    // Checkpoint files are counted within one already selected item.
                    || (table == "f" && sql.contains("json_each(payload_json,'$.files') f"))
                    // Pull requests are expanded within one already selected thread.
                    || (table == "p" && sql.contains("json_each(t.payload_json,'$.pullRequests') p"));
                    assert!(accepted, "unaccepted scan {step}\nSQL: {sql}\nPLAN: {plan:?}");
                }
            }
        }
    }

    fn zone(seconds: i64) -> Option<LocalTime> {
        Some(utc(seconds))
    }

    #[test]
    fn timestamps_round_trip() {
        let t = ms("2026-11-11T22:15:46.654Z").unwrap();
        assert_eq!(iso(t), "2026-11-11T22:15:46.654Z");
        assert_eq!(ms("2026-11-11T22:15:46Z").map(iso).as_deref(), Some("2026-11-11T22:15:46.000Z"));
        assert_eq!(ms("soon"), None);
    }

    #[test]
    fn the_last_week_is_monday_to_sunday_before_this_one() {
        // 2026-11-17 is a Tuesday.
        let now = ms("2026-11-17T22:26:00.000Z").unwrap();
        let last = Period::new(now, Which::LastWeek, &zone);
        assert_eq!(iso(last.start()), "2026-11-09T00:00:00.000Z");
        assert_eq!(iso(last.end()), "2026-11-16T00:00:00.000Z");
        assert_eq!(last.until(), last.end());
        let this = Period::new(now, Which::ThisWeek, &zone);
        assert_eq!(iso(this.start()), "2026-11-16T00:00:00.000Z");
        assert_eq!(this.until(), now);
        assert!(this.so_far);
    }

    #[test]
    fn a_month_is_its_calendar_days_in_local_time() {
        // 2026-11-17: the last full month is October, 31 days, and it began on a Thursday.
        let now = ms("2026-11-17T22:26:00.000Z").unwrap();
        let last = Period::new(now, Which::LastMonth, &zone);
        assert_eq!((iso(last.start()), iso(last.end()), last.len()), ("2026-10-01T00:00:00.000Z".into(), "2026-11-01T00:00:00.000Z".into(), 31));
        assert_eq!((last.span, last.so_far, iso(last.last_day())), (Span::Month, false, "2026-10-31T00:00:00.000Z".into()));
        let this = Period::new(now, Which::ThisMonth, &zone);
        assert_eq!((iso(this.start()), iso(this.end()), this.len(), this.until()), ("2026-11-01T00:00:00.000Z".into(), "2026-12-01T00:00:00.000Z".into(), 30, now));
        assert!(this.so_far);
        // Across a year's end, from the 1st itself, and a February in a leap year.
        let january = Period::new(ms("2027-01-01T00:00:00.000Z").unwrap(), Which::LastMonth, &zone);
        assert_eq!((iso(january.start()), january.len()), ("2026-12-01T00:00:00.000Z".into(), 31));
        let february = Period::new(ms("2028-03-09T10:00:00.000Z").unwrap(), Which::LastMonth, &zone);
        assert_eq!((iso(february.start()), iso(february.end()), february.len()), ("2028-02-01T00:00:00.000Z".into(), "2028-03-01T00:00:00.000Z".into(), 29));
        // A zone west of UTC: its days begin at its own midnight, and a clock change inside
        // the month (1 November 2026, in Los Angeles) moves no boundary off midnight.
        let west = |seconds: i64| Some(utc(seconds - if seconds < 1_793_523_600 { 7 } else { 8 } * 3600));
        let month = Period::new(ms("2026-11-17T22:26:00.000Z").unwrap(), Which::ThisMonth, &west);
        assert_eq!((iso(month.start()), iso(month.days[1]), iso(month.end()), month.len()),
            ("2026-11-01T07:00:00.000Z".into(), "2026-11-02T08:00:00.000Z".into(), "2026-12-01T08:00:00.000Z".into(), 30));
    }

    /// A zone as a list of clock changes: its offset in minutes, then each instant it
    /// changes at with the offset from then on.
    fn clock(first: i64, changes: &'static [(&'static str, i64)]) -> impl Fn(i64) -> Option<LocalTime> {
        move |seconds| {
            let offset = changes.iter().rev().find(|(at, _)| seconds >= (ms(at).unwrap() / 1000.) as i64).map_or(first, |change| change.1);
            Some(utc(seconds + offset * 60))
        }
    }

    // The two tests below came out of an independent review of the month, as failing tests.

    #[test]
    fn a_half_hour_clock_change_keeps_each_day_boundary_at_midnight() {
        // Australia/Lord_Howe, 2026-10-04: 02:00 jumps to 02:30.
        let local = clock(630, &[("2026-10-03T15:30:00.000Z", 660)]);
        let month = Period::new(ms("2026-11-15T12:00:00.000Z").unwrap(), Which::LastMonth, &local);
        for (day, &boundary) in month.days.iter().enumerate() {
            let t = at(&local, (boundary / 1000.) as i64);
            assert_eq!((t.hour, t.minute, t.second), (0, 0, 0), "boundary {day}: {}", iso(boundary));
        }
        assert_eq!(((month.days[4] - month.days[3]) / HOUR, month.len()), (23.5, 31));
        // And back again, 2027-04-04: 02:00 falls to 01:30, a day of 24 and a half hours.
        let local = clock(660, &[("2027-04-03T15:00:00.000Z", 630)]);
        let month = Period::new(ms("2027-05-15T12:00:00.000Z").unwrap(), Which::LastMonth, &local);
        assert!(month.days.iter().all(|&boundary| { let t = at(&local, (boundary / 1000.) as i64); (t.hour, t.minute, t.second) == (0, 0, 0) }));
        assert_eq!(((month.days[4] - month.days[3]) / HOUR, month.len()), (24.5, 30));
    }

    #[test]
    fn a_day_begins_at_its_first_instant_where_a_clock_change_touches_midnight() {
        // Midnight skipped: at the end of 2026-03-28 the clock jumps from 24:00 to 01:00.
        let local = clock(120, &[("2026-03-28T22:00:00.000Z", 180)]);
        let week = Period::new(ms("2026-04-01T12:00:00.000Z").unwrap(), Which::LastWeek, &local);
        // Sunday the 29th begins at 01:00 on its clock and lasts 23 hours.
        assert_eq!((iso(week.days[6]), (week.days[7] - week.days[6]) / HOUR), ("2026-03-28T22:00:00.000Z".into(), 23.));
        assert_eq!(at(&local, (week.days[6] / 1000.) as i64).hour, 1);
        // Midnight twice: at 01:00 on 2026-11-01 the clock is set back to 00:00. The day
        // begins at the first of its two midnights and lasts 25 hours.
        let local = clock(-240, &[("2026-11-01T05:00:00.000Z", -300)]);
        let month = Period::new(ms("2026-11-20T12:00:00.000Z").unwrap(), Which::ThisMonth, &local);
        assert_eq!((iso(month.start()), (month.days[1] - month.days[0]) / HOUR), ("2026-11-01T04:00:00.000Z".into(), 25.));
    }

    #[test]
    fn an_hour_that_comes_twice_or_not_at_all_is_still_agents_on_average() {
        // Los Angeles, 2026-11-01: clocks go back, and a single run covers the whole 25-hour
        // day. Left as it was, the hours were 24 slots of elapsed time and the 25th was lost.
        let local = clock(-420, &[("2026-11-01T09:00:00.000Z", -480)]);
        let month = Period::new(ms("2026-12-15T12:00:00.000Z").unwrap(), Which::LastMonth, &local);
        let run = |from: f64, to: f64| Run { id: "r".into(), thread: "t".into(), requested: from, started: Some(from), completed: Some(to), status: "completed".into(), ..Run::default() };
        let facts = Facts { first_record: Some(month.start()), runs: vec![run(month.start(), month.days[1])], ..Facts::default() };
        let r = summarize(&facts, &month, &local);
        // The day holds 25 agent hours, and one agent all day is 1 in every hour on the clock.
        assert_eq!((r.hours, r.by_day[0]), (25., 25.));
        assert!(r.by_hour[0].iter().all(|agents| (agents - 1.).abs() < 1e-6), "{:?}", r.by_hour[0]);
        // Each hour's number times how long the hour lasted is the day again.
        let lasted = |hour: usize| month.hours[0].iter().find(|span| span.2 == hour).map_or(0., |span| span.3 / HOUR);
        assert_eq!((lasted(0), lasted(1), lasted(2)), (1., 2., 1.));
        assert!(((0..24).map(|hour| f64::from(r.by_hour[0][hour]) * lasted(hour)).sum::<f64>() - 25.).abs() < 1e-6);
        // A run through only the second pass of the hour that came twice is half an agent there.
        let second = ms("2026-11-01T09:00:00.000Z").unwrap();
        let facts = Facts { first_record: Some(month.start()), runs: vec![run(second, second + HOUR)], ..Facts::default() };
        let r = summarize(&facts, &month, &local);
        assert!((r.by_hour[0][1] - 0.5).abs() < 1e-6 && r.by_hour[0][2] == 0. && r.by_day[0] == 1., "{:?}", r.by_hour[0]);

        // 2026-03-08: clocks go forward over 02:00. The day is 23 hours, and the hour that
        // never came holds nothing.
        let local = clock(-480, &[("2026-03-08T10:00:00.000Z", -420)]);
        let month = Period::new(ms("2026-03-20T12:00:00.000Z").unwrap(), Which::ThisMonth, &local);
        let facts = Facts { first_record: Some(month.start()), runs: vec![run(month.days[7], month.days[8])], ..Facts::default() };
        let r = summarize(&facts, &month, &local);
        assert_eq!((r.by_day[7], r.by_hour[7][2]), (23., 0.));
        assert!((0..24).filter(|hour| *hour != 2).all(|hour| (r.by_hour[7][hour] - 1.).abs() < 1e-6), "{:?}", r.by_hour[7]);
    }

    #[test]
    fn a_month_by_hand_adds_up_day_by_day() {
        let period = Period::new(ms("2026-12-03T12:00:00.000Z").unwrap(), Which::LastMonth, &zone);
        let r = summarize(&read_from(&by_hand(), Path::new("test"), &period).unwrap(), &period, &zone);
        // The same thread as the week by hand, seen in its month: Tuesday the 10th of November.
        assert_eq!((r.period, r.days, r.first_weekday, r.by_day.len(), r.by_hour.len()), (Span::Month, 30, 6, 30, 30));
        assert_eq!((r.from.as_str(), r.to.as_str()), ("2026-11-01", "2026-11-30"));
        assert!((r.hours - 3.5).abs() < 1e-9 && (r.by_day[9] - 3.5).abs() < 1e-9 && (r.by_day.iter().sum::<f64>() - r.hours).abs() < 1e-9);
        assert!((r.by_hour[9][10] - 1.5).abs() < 1e-5);
        assert_eq!((r.tasks.total, r.runs, r.most_at_once_when.as_deref()), (2, 3, Some("Tue 10, 10:10")));
        // The records begin on the 10th: nine days before them, twenty-one with them.
        assert_eq!((r.first_recorded_day, r.days_recorded, r.days_elapsed), (9, 21, 30));
        assert_eq!(serde_json::to_value(&r).unwrap()["period"], "month");
    }

    #[test]
    fn overlapping_spans_are_counted_once() {
        assert_eq!(union(&[(0., 10.), (5., 20.), (30., 40.)], 0., 100.), 30.);
        assert_eq!(union(&[(0., 10.), (5., 20.)], 8., 12.), 4.);
        assert_eq!(union(&[], 0., 1.), 0.);
        // What is left of some spans once others are taken away.
        assert_eq!(outside(&[(0., 10.), (20., 30.)], &[(5., 22.)], 0., 100.), vec![(0., 5.), (22., 30.)]);
        assert_eq!(outside(&[(0., 10.)], &[(0., 4.), (6., 20.)], 0., 100.), vec![(4., 6.)]);
        assert_eq!(outside(&[(0., 10.)], &[], 2., 8.), vec![(2., 8.)]);
        assert_eq!(outside(&[(0., 10.)], &[(0., 10.)], 0., 100.), vec![]);
    }

    #[test]
    fn two_threads_waiting_at_once_count_twice() {
        let period = week();
        let at = |time: &str| ms(&format!("2026-11-10T{time}:00.000Z")).unwrap();
        let ask = |thread: &str, from: &str, to: &str| Ask { thread: thread.into(), asked: at(from), answered: Some(at(to)), ..Ask::default() };
        // Thread a waits from 9 to 10 on two asks that overlap; thread b waits from 9:30 to 10:30.
        let facts = Facts { asks: vec![ask("a", "09:00", "09:40"), ask("a", "09:20", "10:00"), ask("b", "09:30", "10:30")], first_record: Some(period.start()), ..Facts::default() };
        let r = summarize(&facts, &period, &zone);
        assert!((r.hours - 2.).abs() < 1e-9 && (r.split.waiting - 2.).abs() < 1e-9 && r.asks.count == 3, "{} {:?}", r.hours, r.split);
        assert!((r.by_day[1] - 2.).abs() < 1e-9 && (r.by_hour[1][9] - 1.5).abs() < 1e-5 && (r.by_hour[1][10] - 0.5).abs() < 1e-5);
        // They are agent time, so the most at once counts them: both waited from 9:30 to 10.
        assert_eq!((r.most_at_once, r.most_at_once_when.as_deref()), (2, Some("Tue 09:30")));
    }

    #[test]
    fn an_ask_nobody_answered_counts_only_while_its_run_is_open() {
        let period = week();
        let at = |time: &str| ms(&format!("2026-11-10T{time}:00.000Z")).unwrap();
        let run = Run { id: "r".into(), thread: "a".into(), status: "completed".into(), requested: at("09:00"), started: Some(at("09:00")), completed: Some(at("10:00")), ..Run::default() };
        // Asked at 9:30 and never answered: half an hour of the run was waiting, and the days
        // since add nothing. Left as it was, this one ask would add the rest of the week.
        let facts = Facts { runs: vec![run], asks: vec![Ask { thread: "a".into(), asked: at("09:30"), answered: None, ..Ask::default() }], first_record: Some(period.start()), ..Facts::default() };
        let r = summarize(&facts, &period, &zone);
        assert!((r.hours - 1.).abs() < 1e-9 && (r.split.waiting - 0.5).abs() < 1e-9 && r.asks.count == 1, "{} {:?}", r.hours, r.split);
        assert_eq!((r.asks.typical_ms, r.most_at_once), (None, 1));
    }

    #[test]
    fn calls_marked_finished_all_at_once_count_as_calls_and_add_no_tool_time() {
        let period = week();
        let at = |time: &str| ms(&format!("2026-11-10T{time}.000Z")).unwrap();
        let run = |id: &str, thread: &str, tools: Vec<(f64, f64)>| Run {
            id: id.into(),
            thread: thread.into(),
            status: "completed".into(),
            requested: at("09:00:00"),
            started: Some(at("09:00:00")),
            completed: Some(at("10:00:00")),
            calls: tools.len() as u32,
            tools,
            ..Run::default()
        };
        // Thread a: three calls begun minutes apart and all ended at the run's last instant.
        // Thread b: two begun half a second apart and ended together, a batch run side by
        // side, which is left alone; and one ordinary call of two minutes.
        let closed = vec![(at("09:05:00"), at("10:00:00")), (at("09:20:00"), at("10:00:00")), (at("09:40:00"), at("10:00:00"))];
        let batch = vec![(at("09:05:00"), at("09:06:00")), (at("09:05:00") + 500., at("09:06:00")), (at("09:10:00"), at("09:12:00"))];
        let facts = Facts { runs: vec![run("r1", "a", closed), run("r2", "b", batch)], first_record: Some(period.start()), ..Facts::default() };
        let r = summarize(&facts, &period, &zone);
        assert_eq!((r.tool_calls, r.closed_together), (6, 3));
        // Two run hours. Only thread b's calls are tool time: one minute and two minutes.
        assert!((r.hours - 2.).abs() < 1e-9, "{}", r.hours);
        assert!((r.split.tools - 3. / 60.).abs() < 1e-9, "{:?}", r.split);
        assert!((r.split.model - (2. - 3. / 60.)).abs() < 1e-9, "{:?}", r.split);
        // The same three calls on one thread but in two runs are still found: the rule is the
        // thread's, not the run's.
        let mut first = run("r1", "a", vec![(at("09:05:00"), at("10:00:00"))]);
        first.completed = Some(at("09:30:00"));
        let second = run("r3", "a", vec![(at("09:20:00"), at("10:00:00")), (at("09:40:00"), at("10:00:00"))]);
        let facts = Facts { runs: vec![first, second], first_record: Some(period.start()), ..Facts::default() };
        assert_eq!(summarize(&facts, &period, &zone).closed_together, 3);
    }

    #[test]
    fn items_at_one_ordinal_are_read_in_the_order_they_were_written() {
        // Two messages at one ordinal, and two errors at another. The first message decides
        // who started the run and the last error decides its cause, so the order among items
        // that share an ordinal must be fixed: it is the order they were written in. A cached
        // run's entry is proved against that order too.
        let db = waiting_db(false, false);
        let mine = serde_json::json!({"createdBy": "user", "creationSource": "mobile"});
        let theirs = serde_json::json!({"createdBy": "agent", "creationSource": "web"});
        let limit = serde_json::json!({"failure": {"class": "usage_limit", "code": "x"}});
        let provider = serde_json::json!({"failure": {"class": "provider_error", "code": "x"}});
        // Written in this order, with ids that sort the other way round.
        for (id, kind, ordinal, payload) in [("z1", "user_message", 5, &mine), ("a2", "user_message", 5, &theirs), ("z3", "error", 7, &limit), ("a4", "error", 7, &provider)] {
            db.execute(&format!("INSERT INTO {P}turn_items VALUES(?1,'t','r','n',NULL,?2,'completed',?3,?4,?5)"), params![id, kind, waiting_at(10.), ordinal, payload.to_string()]).unwrap();
        }
        let period = week();
        let facts = read_from(&db, Path::new("fixture"), &period).unwrap();
        let run = facts.runs.iter().find(|run| run.id == "r").unwrap();
        let mut items = prepare_items(&db, TimeQuery::Sqlite).unwrap();
        let (found, _) = items_of(&mut items, run, period.now).unwrap();
        assert_eq!((found.created_by, found.source, found.cause), (Some(true), true, Some(Cause::Provider)));
        // The index gives that order by itself: nothing is sorted afterwards.
        let sql = items.statement.expanded_sql().unwrap_or_default();
        drop(items);
        let plan: Vec<String> = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap().query_map([], |row| row.get(3)).unwrap().collect::<Result<_, _>>().unwrap();
        assert!(plan.iter().any(|step| step.contains("demo_items_by_run")) && !plan.iter().any(|step| step.contains("TEMP B-TREE")), "{plan:?}");
    }

    #[test]
    fn the_first_message_that_names_its_author_says_who_started_a_run() {
        // A first message with no author leaves it open; the next one, from the person's
        // phone, decides.
        let db = waiting_db(false, false);
        db.execute(&format!("INSERT INTO {P}turn_items VALUES('m1','t','r','n',NULL,'user_message','completed',?1,1,'{{}}')"), params![waiting_at(10.)]).unwrap();
        let named = serde_json::json!({"createdBy": "user", "creationSource": "mobile"});
        db.execute(&format!("INSERT INTO {P}turn_items VALUES('m2','t','r','n',NULL,'user_message','completed',?1,2,?2)"), params![waiting_at(10.), named.to_string()]).unwrap();
        let recap = waiting_result(&db);
        assert_eq!((recap.tasks.from_you, recap.tasks.from_phone), (1, 1));
    }

    #[test]
    fn numbers_are_written_plainly() {
        assert_eq!((hours_text(114.2), hours_text(6.62), hours_text(0.67)), ("114h".into(), "6.6h".into(), "40m".into()));
        assert_eq!((span_text(45_000.), span_text(120_000.), span_text(6_720_000.)), ("45s".into(), "2m".into(), "1h 52m".into()));
        assert_eq!((count_text(3380), count_text(12)), ("3,380".into(), "12".into()));
        assert_eq!(
            (short_text(341_200., 10.), short_text(4.02e9, 10.), short_text(15.3e6, 10.), short_text(812., 10.)),
            ("341K".into(), "4.0B".into(), "15M".into(), "812".into())
        );
        assert_eq!((count_text(99_999), count_text(341_200)), ("99,999".into(), "341K".into()));
        assert_eq!(
            (lines_text('+', 412), lines_text('-', 8_012), lines_text('+', 34_120), lines_text('+', 341_200), lines_text('+', 1_240_000)),
            ("+412".into(), "-8,012".into(), "+34.1K".into(), "+341K".into(), "+1.2M".into())
        );
    }

    fn week() -> Period {
        Period::new(ms("2026-11-17T12:00:00.000Z").unwrap(), Which::LastWeek, &zone)
    }

    /// One thread, by hand: a two-hour run with a tool call, an ask that outlasts the run, a
    /// native subagent and a checkpoint, then a continuation that is not a task.
    fn by_hand() -> Connection {
        use serde_json::json;
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(DEMO_SCHEMA).unwrap();
        let p = |v: serde_json::Value| v.to_string();
        db.execute(&format!("INSERT INTO {P}threads VALUES('t1',?1,NULL,NULL)"), params![p(json!({"projectId": "p1", "createdAt": "2026-11-10T08:00:00.000Z",
            "lineage": {"relationshipToParent": null}, "pullRequests": [
                {"host": "h", "repository": "o/r", "number": 7, "linkedAt": "2026-11-10T09:00:00.000Z", "snapshot": {"state": "merged", "mergedAt": "2026-11-10T11:00:00.000Z", "closedAt": "2026-11-10T11:00:00.000Z"}},
                {"host": "h", "repository": "o/r", "number": 8, "linkedAt": "2026-11-10T09:30:00.000Z", "snapshot": {"state": "open", "mergedAt": null, "closedAt": null}}]}))]).unwrap();
        db.execute(&format!("INSERT INTO {P}threads VALUES('t2',?1,NULL,NULL)"), params![p(json!({"projectId": "p1", "createdAt": "2026-11-10T08:00:00.000Z",
            "lineage": {"relationshipToParent": "subagent"}, "pullRequests": [
                {"host": "h", "repository": "o/r", "number": 7, "linkedAt": "2026-11-10T09:00:00.000Z", "snapshot": {"state": "merged", "mergedAt": "2026-11-10T11:00:00.000Z", "closedAt": null}}]}))]).unwrap();
        let run = |id: &str, thread: &str, asked: &str, started: &str, done: &str, extra: serde_json::Value| {
            let mut payload = json!({"status": "completed", "providerInstanceId": "claudeAgent", "startedAt": started, "modelSelection": {"model": "claude-opus-5-5"}});
            payload.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            db.execute(&format!("INSERT INTO {P}runs VALUES(?1,?2,1,?3,?4,?5)"), params![id, thread, p(payload), asked, done]).unwrap();
        };
        run("r1", "t1", "2026-11-10T09:59:00.000Z", "2026-11-10T10:00:00.000Z", "2026-11-10T12:00:00.000Z", json!({}));
        run("r2", "t1", "2026-11-10T13:00:00.000Z", "2026-11-10T13:00:00.000Z", "2026-11-10T13:30:00.000Z", json!({"delegatedCompletion": {"taskIds": []}}));
        run("r3", "t2", "2026-11-10T10:10:00.000Z", "2026-11-10T10:10:00.000Z", "2026-11-10T10:40:00.000Z", json!({"status": "failed", "providerInstanceId": "grok", "modelSelection": {"model": "grok-4.7"}}));
        let item = |id: &str, run: &str, kind: &str, status: &str, payload: serde_json::Value| {
            db.execute(&format!("INSERT INTO {P}turn_items VALUES(?1,'t',?2,NULL,NULL,?3,?4,'2026-11-10T10:00:00.000Z',1,?5)"), params![id, run, kind, status, p(payload)]).unwrap();
        };
        item("i1", "r1", "user_message", "completed", json!({"createdBy": "user", "creationSource": "mobile", "text": "never read"}));
        item("i2", "r1", "command_execution", "failed", json!({"startedAt": "2026-11-10T10:00:00.000Z", "completedAt": "2026-11-10T10:30:00.000Z", "input": "never read"}));
        item("i3", "r1", "checkpoint", "completed", json!({"files": [{"path": "never/read", "kind": "modified", "additions": 40, "deletions": 5}, {"path": "x", "kind": "modified", "additions": 2, "deletions": 0}]}));
        item("i4", "r3", "error", "failed", json!({"failure": {"class": "validation_error", "code": "workspace_preparation_failed", "message": "never read"}}));
        db.execute(&format!("INSERT INTO {P}nodes VALUES('n1','t1',NULL,'r1')"), []).unwrap();
        // Asked half an hour before the run ends, answered half an hour after it.
        db.execute(&format!("INSERT INTO {P}runtime_requests VALUES('q1','t1','n1','user_input','resolved','2026-11-10T11:30:00.000Z','2026-11-10T12:30:00.000Z')"), []).unwrap();
        db.execute(&format!("INSERT INTO {P}runtime_requests VALUES('q2','t1','n1','auth_refresh','resolved','2026-11-10T10:00:00.000Z','2026-11-10T11:00:00.000Z')"), []).unwrap();
        // An ask that was dropped with no answer is not waiting on anyone.
        db.execute(&format!("INSERT INTO {P}runtime_requests VALUES('q3','t1','n1','user_input','cancelled','2026-11-10T14:00:00.000Z',NULL)"), []).unwrap();
        // A native subagent from 10:45 to 11:15 counts as a tool in flight.
        db.execute(&format!("INSERT INTO {P}subagents VALUES('s1','t1',?1)"), params![p(json!({"runId": "r1", "origin": "provider_native", "status": "completed",
            "model": "claude-haiku-5-5", "startedAt": "2026-11-10T10:45:00.000Z", "completedAt": "2026-11-10T11:15:00.000Z", "prompt": "never read"}))]).unwrap();
        db.execute(&format!("INSERT INTO {P}provider_turns VALUES('t1',NULL,1,?1,'2026-11-10T10:00:00.000Z','2026-11-10T11:59:00.000Z')"), params![p(json!({
            "turnTokenUsage": {"usageStatus": "complete", "inputTokens": 1000, "outputTokens": 10, "cachedInputTokens": 900}, "tokenUsage": {"usedTokens": 95, "maxTokens": 100}}))]).unwrap();
        db.execute(&format!("INSERT INTO {P}provider_turns VALUES('t2',NULL,1,?1,'2026-11-10T10:10:00.000Z','2026-11-10T10:39:00.000Z')"), params![p(json!({"turnTokenUsage": {"usageStatus": "unavailable"}}))]).unwrap();
        db
    }

    #[test]
    fn a_week_by_hand_adds_up() {
        let period = week();
        let r = summarize(&read_from(&by_hand(), Path::new("test"), &period).unwrap(), &period, &zone);
        // Runs: 2h + 30m + 30m. The ask adds the half hour it waited after its run ended.
        assert_eq!((r.runs, r.threads, r.projects), (3, 2, 1));
        assert!((r.hours - 3.5).abs() < 1e-9, "{}", r.hours);
        // Waiting on you: 11:30 to 12:30, in and after the run. Tools: 10:00–10:30 and 10:45–11:15.
        assert!((r.split.waiting - 1.0).abs() < 1e-9 && (r.split.tools - 1.0).abs() < 1e-9 && (r.split.model - 1.5).abs() < 1e-9, "{:?}", r.split);
        assert_eq!((r.asks.count, r.asks.typical_ms), (1, Some(HOUR)));
        // The continuation is not a task; the delegate's failed task was a setup failure.
        assert_eq!((r.tasks.total, r.tasks.from_you, r.tasks.from_phone, r.tasks.from_agents), (2, 1, 1, 1));
        assert_eq!((r.tasks.finished, r.tasks.failed, r.tasks.failed_setup), (1, 1, 1));
        assert_eq!((r.lines_added, r.lines_removed, r.file_changes, r.runs_changing_files), (42, 5, 2, 1));
        // The pull request linked on two threads counts once.
        assert_eq!((r.prs_merged, r.prs_open, r.prs_closed), (1, 1, 0));
        assert_eq!((r.tool_calls, r.commands, r.commands_failed, r.turns_past_90), (1, 1, 1, 1));
        assert_eq!((r.tokens.input, r.tokens.cached, r.tokens.left_out.clone()), (1000., 900., vec![("grok".to_string(), 0.5)]));
        assert_eq!(r.models.iter().map(|m| (m.name.as_str(), m.family, m.runs, m.tokens_in)).collect::<Vec<_>>(),
            vec![("opus 5.5", "claude", 2, Some(1000.)), ("grok-4.7", "grok", 1, None)]);
        assert_eq!((r.most_at_once, r.handed.count, r.days_recorded, r.days_elapsed), (2, 1, 6, 7));
        // Two were open from 10:10, when the delegate's run began.
        assert_eq!(r.most_at_once_when.as_deref(), Some("Tue 10:10"));
        // The days add up to the hours: the half hour waited after the run is in its day and hour.
        assert!((r.by_day.iter().sum::<f64>() - r.hours).abs() < 1e-9 && (r.by_day[1] - 3.5).abs() < 1e-9);
        assert!((r.by_hour[1][10] - 1.5).abs() < 1e-5 && (r.by_hour[1][12] - 0.5).abs() < 1e-5 && r.by_hour[1][13] == 0.5);
        assert_eq!((r.from.as_str(), r.to.as_str()), ("2026-11-09", "2026-11-15"));
    }

    #[test]
    fn the_reader_refuses_to_write_and_selects_no_text() {
        let dir = std::env::temp_dir().join(format!("loupe-recap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("statev2.sqlite");
        let _ = std::fs::remove_file(&path);
        by_hand().execute(&format!("VACUUM INTO '{}'", path.display()), []).unwrap();
        let db = open(&path).unwrap();
        assert!(db.execute(&format!("DELETE FROM {P}runs"), []).is_err());
        let before = std::fs::read(&path).unwrap();
        let period = week();
        let recap = summarize(&read_from(&db, &path, &period).unwrap(), &period, &zone);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!serde_json::to_string(&recap).unwrap().contains("never"));
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_database_says_so() {
        assert_eq!(open(Path::new("/nonexistent/loupe/statev2.sqlite")).unwrap_err().kind, "missing");
    }

    #[test]
    fn the_made_up_week_is_full_and_the_same_every_time() {
        let period = week();
        let (a, b) = (demo(&period, &zone).unwrap(), demo(&period, &zone).unwrap());
        assert_eq!(a, b);
        assert!(a.tasks.total > 500 && a.hours > 60. && a.models.len() >= 5, "{} {}", a.tasks.total, a.hours);
        assert!((a.split.tools + a.split.model + a.split.waiting - a.hours).abs() < 1e-6);
        // The seven days, and their hours, add up to the week.
        assert!((a.by_day.iter().sum::<f64>() - a.hours).abs() < 1e-6, "{} {}", a.by_day.iter().sum::<f64>(), a.hours);
        assert!((a.by_hour.iter().flatten().map(|h| f64::from(*h)).sum::<f64>() - a.hours).abs() < 1e-2);
        assert!(a.models.iter().any(|m| m.tokens_in.is_none()) && !a.tokens.left_out.is_empty());
        assert!(a.prs_merged > 0 && a.handed.count > 0 && a.findings.len() == 3);
        assert_eq!((a.days_recorded, a.days, a.first_weekday, a.period), (7, 7, 0, Span::Week));
    }

    #[test]
    fn made_up_data_for_a_day_still_going_ends() {
        // Asked for at 3:00, 7:31 and 12:00 on a Monday: nothing yet, next to nothing, a morning.
        let tasks = |time: &str, which: Which| demo(&Period::new(ms(&format!("2026-11-16T{time}:00.000Z")).unwrap(), which, &zone), &zone).unwrap().tasks.total;
        assert_eq!((tasks("03:00", Which::ThisWeek), tasks("07:31", Which::ThisWeek)), (0, 0));
        let noon = tasks("12:00", Which::ThisWeek);
        assert!(noon > 5 && noon < 58, "{noon}");
        // The same on the first morning of a month.
        assert_eq!(demo(&Period::new(ms("2026-11-01T03:00:00.000Z").unwrap(), Which::ThisMonth, &zone), &zone).unwrap().tasks.total, 0);
    }

    #[test]
    fn days_that_hold_no_records_are_null_in_json() {
        // The week by hand again, with the records beginning on its Tuesday and now its Thursday.
        let period = Period::new(ms("2026-11-12T12:00:00.000Z").unwrap(), Which::ThisWeek, &zone);
        let r = summarize(&read_from(&by_hand(), Path::new("test"), &period).unwrap(), &period, &zone);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["by_day"], serde_json::json!([null, 3.5, 0.0, 0.0, null, null, null]));
        assert!(json["by_hour"][0].is_null() && json["by_hour"][1][10].as_f64() == Some(1.5) && json["by_hour"][4].is_null());
        assert_eq!((r.days_active, r.typical_day, r.first_recorded_day, r.days_elapsed), (1, Some(3.5), 1, 4));
    }

    #[test]
    fn the_made_up_month_is_full_and_its_days_add_up() {
        let period = Period::new(ms("2026-11-17T12:00:00.000Z").unwrap(), Which::LastMonth, &zone);
        let (a, b) = (demo(&period, &zone).unwrap(), demo(&period, &zone).unwrap());
        assert_eq!(a, b);
        assert_eq!((a.days, a.days_recorded, a.first_weekday, a.by_day.len()), (31, 31, 3, 31));
        assert!(a.tasks.total > 2000 && a.hours > 300. && a.by_day.iter().all(|h| *h > 0.), "{} {}", a.tasks.total, a.hours);
        assert!((a.by_day.iter().sum::<f64>() - a.hours).abs() < 1e-6 && (a.split.tools + a.split.model + a.split.waiting - a.hours).abs() < 1e-6);
        // The month so far stops at now: the days that have not come hold nothing.
        let so_far = Period::new(ms("2026-11-17T12:00:00.000Z").unwrap(), Which::ThisMonth, &zone);
        let c = demo(&so_far, &zone).unwrap();
        assert_eq!((c.days, c.days_elapsed, c.so_far), (30, 17, true));
        assert!(c.by_day[..16].iter().all(|h| *h > 0.) && c.by_day[17..].iter().all(|h| *h == 0.));
    }

    /// A reader fixture with one run, from hour 10 to 11 of the period.
    fn waiting_db(capability_column: bool, messages: bool) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(DEMO_SCHEMA).unwrap();
        if capability_column {
            db.execute_batch(&format!("ALTER TABLE {P}runtime_requests ADD COLUMN payload_json TEXT")).unwrap();
        }
        if messages {
            db.execute_batch(&format!("CREATE TABLE {P}messages(thread_id TEXT,role TEXT,updated_at TEXT,payload_json TEXT)")).unwrap();
        }
        // The thread's own row, as T3 keeps one for every thread, and T3's index of a thread's
        // items by type (migration 089): link cards outside the period's runs are found
        // through the two.
        db.execute(&format!("INSERT INTO {P}threads VALUES('t','{{}}',NULL,NULL)"), []).unwrap();
        db.execute_batch(&format!("CREATE INDEX {P}turn_items_thread_type_idx ON {P}turn_items(thread_id,type,status)")).unwrap();
        waiting_run(&db, "r", "t", 10., 11.);
        db
    }

    /// A timestamp in fixture hours from the period's start.
    fn waiting_at(hour: f64) -> String {
        iso(week().start() + hour * HOUR)
    }

    /// Add one completed run to the reader fixture.
    fn waiting_run(db: &Connection, id: &str, thread: &str, a: f64, b: f64) {
        let payload = serde_json::json!({"startedAt": waiting_at(a), "status": "completed"});
        db.execute(&format!("INSERT INTO {P}runs VALUES(?1,?2,1,?3,?4,?5)"), params![id, thread, payload.to_string(), waiting_at(a), waiting_at(b)]).unwrap();
    }

    /// Add one request, with an optional resolution.
    fn waiting_request(db: &Connection, kind: &str, status: &str, end: Option<f64>) {
        db.execute(&format!("INSERT INTO {P}runtime_requests(request_id,thread_id,kind,status,created_at,resolved_at) VALUES('q','t',?1,?2,?3,?4)"), params![kind, status, waiting_at(10.5), end.map(waiting_at)]).unwrap();
    }

    /// Add an item with a node, a request id, and independently controlled times.
    fn waiting_item(db: &Connection, kind: &str, run: Option<&str>, status: &str, a: f64, b: Option<f64>) {
        let payload = serde_json::json!({"startedAt": waiting_at(a), "completedAt": b.map(waiting_at), "requestId": "q"});
        db.execute(&format!("INSERT INTO {P}turn_items VALUES('i','t',?1,'n',NULL,?2,?3,?4,1,?5)"), params![run, kind, status, waiting_at(b.unwrap_or(a)), payload.to_string()]).unwrap();
    }

    /// Link cards outside the period's runs are found through T3's own indexes, never by
    /// reading every item: both through the index of a thread's items by type, only those
    /// still waiting through the older index of waiting links, and none with neither.
    #[test]
    fn reader_finds_link_cards_of_earlier_runs_only_through_an_index() {
        let period = week();
        let links = |db: &Connection| {
            let facts = read_from(db, Path::new("fixture"), &period).unwrap();
            facts.asks.iter().filter(|ask| ask.record.is_some_and(|record| record.kind == anatomy::AskKind::Link)).count()
        };
        // A run that ended before the period, with one link still waiting and one that ended
        // two hours into the period.
        let db = waiting_db(false, false);
        waiting_run(&db, "old", "t", -2., -1.);
        waiting_item(&db, "link_request", Some("old"), "waiting", -1.5, None);
        let ended = serde_json::json!({"startedAt": waiting_at(-1.4), "completedAt": waiting_at(2.)});
        db.execute(&format!("INSERT INTO {P}turn_items VALUES('i2','t','old','n',NULL,'link_request','completed',?1,2,?2)"), params![waiting_at(2.), ended.to_string()]).unwrap();
        assert_eq!(links(&db), 2);
        // T3 before migration 089 has only its index of links still waiting.
        db.execute_batch(&format!(
            "DROP INDEX {P}turn_items_thread_type_idx;
             CREATE INDEX {P}turn_items_waiting_link_idx ON {P}turn_items(thread_id,updated_at DESC,item_id DESC) WHERE type='link_request' AND status='waiting'"
        )).unwrap();
        assert_eq!(links(&db), 1);
        // With neither index none is looked for, and the read still succeeds.
        db.execute_batch(&format!("DROP INDEX {P}turn_items_waiting_link_idx")).unwrap();
        assert_eq!(links(&db), 0);
        // A link of a run in the period needs no index: the period's own item pass reads it.
        waiting_item_as(&db, "i3", "link_request", Some("r"), "waiting", 10.5);
        assert_eq!(links(&db), 1);
    }

    /// Add one open card with its own id.
    fn waiting_item_as(db: &Connection, id: &str, kind: &str, run: Option<&str>, status: &str, a: f64) {
        let payload = serde_json::json!({"startedAt": waiting_at(a)});
        db.execute(&format!("INSERT INTO {P}turn_items VALUES(?1,'t',?2,'n',NULL,?3,?4,?5,3,?6)"), params![id, run, kind, status, waiting_at(a), payload.to_string()]).unwrap();
    }

    /// Exercise the complete reader and summary, rather than injecting normalized asks.
    fn waiting_result(db: &Connection) -> Recap {
        let period = week();
        summarize(&read_from(db, Path::new("fixture"), &period).unwrap(), &period, &zone)
    }

    /// Reader message capability never holds inside or after run.
    #[test]
    fn reader_message_capability_never_holds_inside_or_after_run() {
        for end in [None, Some(12.)] {
            let db = waiting_db(true, false);
            waiting_request(&db, "user_input", "pending", end);
            db.execute(&format!("UPDATE {P}runtime_requests SET payload_json=?1"), [r#"{"responseCapability":{"type":"message"}}"#]).unwrap();
            let r = waiting_result(&db);
            assert_eq!((r.split.waiting, r.asks.count, r.asks.typical_ms), (0., 0, None));
        }
    }

    /// Reader missing malformed and unfamiliar capabilities hold.
    #[test]
    fn reader_missing_malformed_and_unfamiliar_capabilities_hold() {
        for payload in [None, Some("{"), Some(r#"{"responseCapability":{"type":"later"}}"#)] {
            let db = waiting_db(true, false);
            waiting_request(&db, "user_input", "pending", Some(11.5));
            db.execute(&format!("UPDATE {P}runtime_requests SET payload_json=?1"), [payload]).unwrap();
            let r = waiting_result(&db);
            assert_eq!((r.split.waiting, r.asks.count, r.asks.typical_ms), (1., 1, Some(HOUR)));
        }
    }

    /// Reader older status names hold approvals but not questions.
    #[test]
    fn reader_older_status_names_hold_approvals_but_not_questions() {
        for status in ["open", "waiting", "requested"] {
            for kind in ["user_input", "command_approval"] {
                let db = waiting_db(false, false);
                waiting_request(&db, kind, status, None);
                let r = waiting_result(&db);
                let expected = if kind == "user_input" { (0., 0) } else { (0.5, 1) };
                assert_eq!((r.split.waiting, r.asks.count), expected, "{kind} {status}");
            }
        }
    }

    /// Waiting within overlapping runs of one thread is counted once.
    #[test]
    fn reader_one_threads_overlapping_runs_count_its_wait_once() {
        let db = waiting_db(false, false);
        waiting_run(&db, "parallel", "t", 10., 11.);
        waiting_request(&db, "command_approval", "resolved", Some(11.));
        assert_eq!(waiting_result(&db).split.waiting, 0.5);
    }

    /// A pending secret ends for duration statistics when its own run completes.
    #[test]
    fn reader_secret_run_completion_ends_its_blocking_duration() {
        let db = waiting_db(false, false);
        waiting_item(&db, "secret_request", Some("r"), "waiting", 10.5, None);
        let r = waiting_result(&db);
        assert_eq!((r.split.waiting, r.asks.count, r.asks.typical_ms), (0.5, 1, Some(0.5 * HOUR)));
        db.execute(&format!("UPDATE {P}turn_items SET node_id=NULL"), []).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 0.);
    }

    /// Reader secret is clipped at both ends to its own run.
    #[test]
    fn reader_secret_is_clipped_at_both_ends_to_its_own_run() {
        for run in [Some("r"), None] {
            let db = waiting_db(false, false);
            waiting_item(&db, "secret_request", run, "completed", 9., Some(12.));
            let r = waiting_result(&db);
            assert_eq!(r.split.waiting, if run.is_some() { 1. } else { 0. });
            assert_eq!(r.asks.typical_ms, if run.is_some() { Some(HOUR) } else { None });
        }
    }

    /// Reader link uses first strictly later genuine human and all working runs.
    #[test]
    fn reader_link_uses_first_strictly_later_genuine_human_and_all_working_runs() {
        let db = waiting_db(false, true);
        // This run ends before the period: it must still mask the link's blocking pieces.
        waiting_run(&db, "old", "t", -2., -1.);
        waiting_item(&db, "link_request", Some("old"), "waiting", -3., None);
        for (thread, role, hour, payload) in [
            ("t", "user", -3., r#"{"createdBy":"user"}"#),
            ("t", "assistant", -2.5, r#"{"createdBy":"user"}"#),
            ("t", "user", -2., r#"{"createdBy":"agent"}"#),
            ("t", "user", -1.5, r#"{"createdBy":"user","idleCompaction":true}"#),
            ("other", "user", 0., r#"{"createdBy":"user"}"#),
            ("t", "user", 12., r#"{"createdBy":"user"}"#),
            ("t", "user", 13., r#"{"createdBy":"user"}"#),
        ] {
            db.execute(&format!("INSERT INTO {P}messages VALUES(?1,?2,?3,?4)"), params![thread, role, waiting_at(hour), payload]).unwrap();
        }
        let period = week();
        let facts = read_from(&db, Path::new("fixture"), &period).unwrap();
        let ask = facts.asks[0].record.unwrap();
        let pieces = anatomy::blocking(&ask, &facts.working["t"], period.now);
        assert_eq!(anatomy::union(&pieces, f64::NEG_INFINITY, f64::INFINITY), 13. * HOUR);
        assert_eq!(waiting_result(&db).split.waiting, 11.);
        db.execute(&format!("UPDATE {P}turn_items SET node_id=NULL"), []).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 0.);
    }

    /// Reader request and item count once with authoritative times and closed fallback.
    #[test]
    fn reader_request_and_item_count_once_with_authoritative_times_and_closed_fallback() {
        let db = waiting_db(false, false);
        waiting_request(&db, "user_input", "pending", Some(11.5));
        waiting_item(&db, "user_input_request", Some("r"), "completed", 9., Some(13.));
        let r = waiting_result(&db);
        assert_eq!((r.split.waiting, r.asks.count, r.asks.typical_ms), (1., 1, Some(HOUR)));
        db.execute(&format!("UPDATE {P}runtime_requests SET resolved_at=NULL,status='closed'"), []).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 2.5);
        db.execute(&format!("UPDATE {P}turn_items SET status='waiting',payload_json=json_remove(payload_json,'$.completedAt')"), []).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 0.);
        db.execute(&format!("DELETE FROM {P}turn_items"), []).unwrap();
        assert_eq!(waiting_result(&db).asks.count, 0);
    }

    /// Reader older schema holds questions and links have no human cutoff.
    #[test]
    fn reader_older_schema_holds_questions_and_links_have_no_human_cutoff() {
        let db = waiting_db(false, false);
        waiting_request(&db, "user_input", "resolved", Some(11.5));
        assert_eq!(waiting_result(&db).split.waiting, 1.);
        db.execute(&format!("DELETE FROM {P}runtime_requests"), []).unwrap();
        waiting_item(&db, "link_request", None, "waiting", 10.5, None);
        assert_eq!(waiting_result(&db).split.waiting, 0.);
        db.execute(&format!("UPDATE {P}turn_items SET status='completed',updated_at=?1"), [waiting_at(12.)]).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 1.);
    }

    /// Deliberate difference from trace pending waits outside runs count only once ended.
    #[test]
    fn deliberate_difference_from_trace_pending_waits_outside_runs_count_only_once_ended() {
        for kind in ["command_approval", "user_input", "link_request"] {
            let db = waiting_db(false, false);
            if kind == "link_request" {
                waiting_item(&db, kind, Some("r"), "waiting", 10.5, None);
            } else {
                waiting_request(&db, kind, "pending", None);
            }
            assert_eq!(waiting_result(&db).split.waiting, if kind == "link_request" { 0. } else { 0.5 }, "{kind}");
            if kind == "link_request" {
                db.execute(&format!("UPDATE {P}turn_items SET status='completed',updated_at=?1"), [waiting_at(12.)]).unwrap();
            } else {
                db.execute(&format!("UPDATE {P}runtime_requests SET resolved_at=?1"), [waiting_at(12.)]).unwrap();
            }
            assert_eq!(waiting_result(&db).split.waiting, if kind == "link_request" { 1. } else { 1.5 }, "{kind}");
        }
        // Recap clips at the calendar boundary; Trace clips at its task's request time.
        let db = waiting_db(false, false);
        waiting_request(&db, "command_approval", "resolved", Some(1.));
        db.execute(&format!("UPDATE {P}runtime_requests SET created_at=?1"), [waiting_at(-1.)]).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 1.);
        // Recap can count an ended request without a node or item. Trace reports it
        // unplaced rather than assigning it to a task.
        db.execute(&format!("UPDATE {P}runtime_requests SET created_at=?1,resolved_at=?2"), params![waiting_at(12.), waiting_at(13.)]).unwrap();
        assert_eq!(waiting_result(&db).split.waiting, 1.);
        // The agreed Recap input is request rows. Trace can also place a formal item
        // whose runtime request row no longer exists.
        db.execute(&format!("DELETE FROM {P}runtime_requests"), []).unwrap();
        waiting_item(&db, "approval_request", Some("r"), "completed", 10.5, Some(12.));
        assert_eq!(waiting_result(&db).split.waiting, 0.);

    }

}
