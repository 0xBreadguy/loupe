//! Trace: one task of one agent as spans on a time axis, worked out from T3's own records
//! each time it is asked for.
//!
//! Nothing here is in the original app (`oracle/`); it is a deliberate addition, and the
//! original rejects `--trace` as it rejects `--recap`.
//!
//! The reader is read-only and narrow. Its queries return identifiers, timestamps, statuses,
//! a tool's name, a delegate's model, the thread's and a delegate's title, a failure's class,
//! and a command's listed programs. No Trace query returns a command line: the command is handed,
//! inside the query, to `t3_programs` (`program::register`), which SQLite calls while it
//! evaluates the query and which can only return names from `program::PROGRAMS`. Never a
//! prompt, a reply, reasoning, a result, an output, a path or a message.
use crate::{
    anatomy::{self, outside, union},
    program, recap,
    source::SourceError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};
const P: &str = "orchestration_v2_projection_";
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Latest,
    Thread(String),
    Run(String),
}
/// Version of the `--trace --json` object.
///
/// Bump this when a field is removed, renamed, or changes meaning or unit.
/// Adding a field does not bump it. The trace screen and its step cache use
/// [`Trace`] itself; this number is added only when the object is printed.
pub const JSON_SCHEMA: u32 = 1;

/// `--trace --json`, with [`JSON_SCHEMA`] as the first key.
pub fn printed_json(report: &Trace) -> Result<String, serde_json::Error> {
    crate::report_json::with_schema(report, JSON_SCHEMA)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Trace {
    pub thread: String,
    pub title: Option<String>,
    pub delegate_thread: bool,
    pub provider: String,
    pub model: String,
    pub requested: f64,
    pub ended: Option<f64>,
    pub now: f64,
    pub state: State,
    pub runs: Vec<TraceRun>,
    pub spans: Vec<Span>,
    pub marks: Vec<Mark>,
    pub split: Split,
    pub steps: Steps,
    pub previous: Option<String>,
    pub next: Option<String>,
    /// Stretches of more than `QUIET_FOLD_MS` in which nothing was in flight: no run open,
    /// no queue, no tool call, no ask, no delegate. Their time is in the split (`between`);
    /// a screen may fold them on its axis, saying how long they were.
    pub quiet: Vec<(f64, f64)>,
    /// The stretches in which the thread was waiting on its person (`anatomy::waiting`): the
    /// waiting part of the split.
    pub waiting: Vec<(f64, f64)>,
    /// Asks that belong to this task's stretch but could not be placed on a run (a request with
    /// no item and no run known for its node, or a runless card before the thread's first
    /// task): counted here, not drawn and not in the split.
    #[serde(skip_serializing_if = "is_zero")]
    pub unplaced_asks: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// How long nothing must be in flight before a screen may fold the stretch: half an hour.
pub const QUIET_FOLD_MS: f64 = 30. * 60_000.;
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TraceRun {
    pub id: String,
    pub ordinal: i64,
    pub requested: f64,
    pub started: Option<f64>,
    pub completed: Option<f64>,
    pub status: String,
    pub continuation: bool,
    /// Where T3 has the run in its queue while it waits to start, when it records one.
    pub queue_position: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Waiting,
    Delegated,
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Queue,
    Tool,
    Ask,
    Delegate,
    Seen,
    Model,
    Between,
}
/// One stretch of the task's time. `key` stays the same across reads of the task, so a
/// screen can keep a row selected: T3's item or subagent id, or for the spans worked out here
/// (queue, seen, model, between) their kind, run and start.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Span {
    pub key: String,
    pub kind: Kind,
    pub category: String,
    pub label: String,
    pub start: f64,
    pub end: Option<f64>,
    pub status: String,
    pub measured: bool,
    pub run: Option<usize>,
    pub delegate: Option<Delegate>,
    /// A command's listed programs in order, when it has more than its label says.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub programs: Vec<&'static str>,
    /// A tool call whose recorded end is not its own: the provider closed it together with
    /// others at one instant (`anatomy::closed_together`). It counts as a call and adds no time.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub closed_together: bool,
    /// A question T3 records as answerable by a later message: the run may work past it, and
    /// it is never counted as waiting.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub by_message: bool,
    /// For an ask: the stretches in which it held the person (`anatomy::blocking`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<(f64, f64)>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Delegate {
    pub native: bool,
    pub child_thread: Option<String>,
    pub remote: bool,
    pub model: String,
    pub title: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    Steer,
    StopAsked,
    Restart,
    Error,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Mark {
    pub kind: MarkKind,
    pub at: f64,
    pub run: usize,
    pub detail: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Split {
    pub total: f64,
    pub queue: f64,
    pub waiting: f64,
    pub tools: f64,
    pub seen: f64,
    pub model: f64,
    pub between: f64,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Steps {
    pub tools: u32,
    pub failed: u32,
    pub asks: u32,
    pub delegates: u32,
    pub under_a_second: u32,
    /// Tool calls closed together with others, whose time is not counted.
    pub closed_together: u32,
    pub by_category: Vec<(String, u32, f64)>,
}
fn stamp(v: Option<String>) -> Option<f64> {
    v.as_deref().and_then(recap::ms)
}
fn span(kind: Kind, category: &str, label: &str, start: f64, end: Option<f64>, status: &str, run: Option<usize>) -> Span {
    Span {
        key: String::new(),
        kind,
        category: category.into(),
        label: label.into(),
        start,
        end,
        status: if end.is_none() { "running".into() } else { status.into() },
        measured: kind != Kind::Model,
        run,
        delegate: None,
        programs: vec![],
        closed_together: false,
        by_message: false,
        holds: vec![],
    }
}
/// Open T3's database for Trace: read-only and told to refuse writes, as `recap::open` does.
pub fn open(path: &Path) -> Result<Connection, SourceError> {
    recap::open(path)
}
/// The threads whose id ends with `ending`, at most three: so a short id, as the Tools view
/// prints one, can name a thread. Compared exactly, not as a pattern: ids hold `%`.
pub fn threads_ending(path: &Path, ending: &str) -> Result<Vec<String>, SourceError> {
    let db = open(path)?;
    let fail = |e: rusqlite::Error| SourceError { kind: "schema", path: path.to_owned(), detail: e.to_string() };
    let mut q = db
        .prepare(&format!("SELECT thread_id FROM {P}threads WHERE length(?1) > 0 AND substr(thread_id, -length(?1)) = ?1 ORDER BY thread_id LIMIT 3"))
        .map_err(fail)?;
    let found = q.query_map([ending], |r| r.get::<_, String>(0)).map_err(fail)?.collect::<rusqlite::Result<Vec<_>>>().map_err(fail)?;
    Ok(found)
}
pub fn load(path: &Path, target: &Target, now: f64) -> Result<Option<Trace>, SourceError> {
    read_from(&open(path)?, path, target, now)
}
pub fn read_from(db: &Connection, path: &Path, target: &Target, now: f64) -> Result<Option<Trace>, SourceError> {
    read_cached(db, path, target, now, &mut ReadCache::default())
}

/// `read_from`, keeping the task's steps between reads: a step whose row T3 has not rewritten
/// since is not parsed again. T3 writes a step in one place, an upsert that replaces the whole
/// row with its `updated_at` (`turn-item.updated`, app.asar byte 61178827), so an unchanged
/// `updated_at` is an unchanged step. Steps still open, and steps written in the last
/// `RECENT_MS`, are read again every time; every `FULL_EVERY`th read reads everything, so a
/// rewrite that kept its time is picked up within that many reads.
pub fn read_cached(db: &Connection, path: &Path, target: &Target, now: f64, cache: &mut ReadCache) -> Result<Option<Trace>, SourceError> {
    let fail = |e: rusqlite::Error| {
        let not_db = matches!(&e, rusqlite::Error::SqliteFailure(c, _) if c.extended_code == 26);
        SourceError {
            path: path.to_owned(),
            kind: if not_db { "unreadable" } else { "schema" },
            detail: if not_db { "not a SQLite database".into() } else { e.to_string() },
        }
    };
    // The items query names a command's programs through `t3_programs`; registering it again
    // on a connection that has it replaces it.
    program::register(db).map_err(fail)?;
    db.execute_batch("BEGIN").map_err(fail)?;
    let result = read(db, target, now, cache);
    cache.reads = cache.reads.wrapping_add(1);
    if result.is_err() {
        *cache = ReadCache::default();
    }
    let ended = db.execute_batch("ROLLBACK");
    let trace = result.map_err(fail)?;
    ended.map_err(fail)?;
    Ok(trace)
}
/// A projection table's columns; empty when the table does not exist.
fn columns(db: &Connection, table: &str) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut q = db.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let names = q.query_map([format!("{P}{table}")], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(names)
}
fn has_column(db: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    // Schema capability only, to mirror Recap's older-schema fallback.
    db.query_row("SELECT count(*) FROM pragma_table_info(?1) WHERE name=?2", params![format!("{P}{table}"), column], |r| Ok(r.get::<_, i64>(0)? > 0))
}
/// What `read_cached` keeps between reads: each step's row as last parsed, by its id, with the
/// `updated_at` it was parsed at.
#[derive(Default)]
pub struct ReadCache {
    steps: HashMap<String, (Option<String>, Step)>,
    reads: u32,
}
/// Every `FULL_EVERY`th read parses every step (30 s at the dashboard's 2 s period).
const FULL_EVERY: u32 = 15;
/// Steps written this recently are parsed again on every read: two writes of one step in the
/// same millisecond would carry the same `updated_at`.
const RECENT_MS: f64 = 5_000.;

/// One step's row, as the items query returns it.
#[derive(Clone)]
struct Step {
    kind: String,
    status: String,
    updated: Option<f64>,
    started: Option<f64>,
    completed: Option<f64>,
    tool: Option<String>,
    programs: Vec<&'static str>,
    failure: Option<String>,
    id: String,
    has_node: bool,
    request: Option<String>,
}

impl Step {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<(Option<String>, Step)> {
        let updated: Option<String> = r.get(3)?;
        Ok((
            updated.clone(),
            Step {
                kind: r.get(0)?,
                status: r.get(1)?,
                updated: stamp(updated),
                started: stamp(r.get(4)?),
                completed: stamp(r.get(5)?),
                tool: r.get(6)?,
                // Back to the list's own strings: anything that is not a listed name is dropped.
                programs: r.get::<_, Option<String>>(7)?.as_deref().unwrap_or("").split(' ').filter_map(program::listed).collect(),
                failure: r.get(8)?,
                id: r.get(9)?,
                has_node: r.get(10)?,
                request: r.get(11)?,
            },
        ))
    }
}

fn read(db: &Connection, target: &Target, now: f64, cache: &mut ReadCache) -> rusqlite::Result<Option<Trace>> {
    // Locate the owning thread; Latest prefers an unfinished run, then its latest clock.
    let mut latest_run = None;
    let thread: Option<String> = match target {
        Target::Thread(t) => Some(t.clone()),
        Target::Run(r) => db.query_row(&format!("SELECT thread_id FROM {P}runs WHERE run_id=?1"), [r], |r| r.get(0)).optional()?,
        Target::Latest => {
            let selected = db.query_row(&format!("SELECT thread_id,run_id FROM {P}runs ORDER BY (completed_at IS NULL) DESC, max(requested_at,coalesce(json_extract(payload_json,'$.startedAt'),requested_at),coalesce(completed_at,requested_at)) DESC, run_id DESC LIMIT 1"), [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).optional()?;
            selected.map(|(thread, run)| {
                latest_run = Some(run);
                thread
            })
        }
    };
    let Some(thread) = thread else { return Ok(None) };
    // Title and lineage alone supply the header; never any message content.
    let header = db.query_row(&format!("SELECT json_extract(payload_json,'$.title'), json_extract(payload_json,'$.lineage.relationshipToParent') FROM {P}threads WHERE thread_id=?1"), [&thread], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))).optional()?;
    let Some((title, relation)) = header else { return Ok(None) };
    // Runs define task boundaries, provider/model header and the open intervals.
    let mut q = db.prepare(&format!("SELECT run_id,ordinal,requested_at,completed_at,json_extract(payload_json,'$.startedAt'),json_extract(payload_json,'$.status'),json_extract(payload_json,'$.providerInstanceId'),json_extract(payload_json,'$.modelSelection.model'),json_extract(payload_json,'$.workStartedAt'),json_type(payload_json,'$.delegatedCompletion'),json_extract(payload_json,'$.queuePosition') FROM {P}runs WHERE thread_id=?1 ORDER BY ordinal"))?;
    let rows = q.query_map([&thread], |r| {
        Ok((
            TraceRun {
                id: r.get(0)?,
                ordinal: r.get(1)?,
                requested: stamp(r.get(2)?).unwrap_or(0.),
                completed: stamp(r.get(3)?),
                started: stamp(r.get(4)?),
                status: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                continuation: r.get::<_, Option<String>>(8)?.is_some() || r.get::<_, Option<String>>(9)?.as_deref() == Some("object"),
                queue_position: r.get::<_, Option<i64>>(10).ok().flatten(),
            },
            r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        ))
    })?;
    let all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut trace = Trace {
        thread,
        title,
        delegate_thread: relation.as_deref() == Some("subagent"),
        provider: String::new(),
        model: String::new(),
        requested: now,
        ended: None,
        now,
        state: State::Queued,
        runs: vec![],
        spans: vec![],
        marks: vec![],
        split: Split::default(),
        steps: Steps::default(),
        previous: None,
        next: None,
        quiet: vec![],
        waiting: vec![],
        unplaced_asks: 0,
    };
    let pseudo = all.is_empty();
    if !pseudo {
        let wanted = match target {
            Target::Run(id) => all.iter().position(|r| &r.0.id == id).unwrap(),
            Target::Latest => all.iter().position(|r| Some(&r.0.id) == latest_run.as_ref()).unwrap(),
            _ => all.len() - 1,
        };
        let mut first = wanted;
        while first > 0 && all[first].0.continuation {
            first -= 1;
        }
        let mut end = wanted + 1;
        while end < all.len() && all[end].0.continuation {
            end += 1;
        }
        trace.previous = (first > 0).then(|| {
            let mut p = first - 1;
            while p > 0 && all[p].0.continuation {
                p -= 1;
            }
            all[p].0.id.clone()
        });
        trace.next = all.get(end).map(|r| r.0.id.clone());
        trace.provider = all[first].1.clone();
        trace.model = crate::terminal::short_model(&all[first].2);
        trace.runs = all[first..end].iter().map(|r| r.0.clone()).collect();
    }
    let column = |name: &'static str, fallback: &'static str| -> rusqlite::Result<&'static str> {
        Ok(if has_column(db, "subagents", name)? { name } else { fallback })
    };
    let started = column("started_at", "json_extract(payload_json,'$.startedAt')")?;
    let completed = column("completed_at", "json_extract(payload_json,'$.completedAt')")?;
    // Delegates supply timing, origin, child navigation and allowed display names only.
    // Claude's runToolUseId map (app.asar byte 228646267) is in-memory, not a
    // persisted parentItemId/nodeId join. Do not infer a tool/delegate link from time.
    // Narrowed through T3's indexed columns, so a refresh does not parse every delegate row T3
    // ever kept (27 ms with 12,000 of them, checks/bench/trace-read.py). A task's delegates
    // are among its thread's (`thread_id`, indexed, as the live reader finds them), and the
    // payload still decides the run. A delegate thread's own row is found by
    // `child_thread_id`, indexed; only if that finds nothing does the payload decide, so a
    // database whose column was never filled still reads. T3 writes both columns from the
    // same event fields as the payload (the subagents upsert, app.asar byte 61164506).
    let select = format!("SELECT json_extract(payload_json,'$.runId'),json_extract(payload_json,'$.origin'),json_extract(payload_json,'$.status'),{started},{completed},json_extract(payload_json,'$.childThreadId'),coalesce(json_extract(payload_json,'$.modelSelection.model'),json_extract(payload_json,'$.model')),json_extract(payload_json,'$.title'),json_type(payload_json,'$.remoteChild'),subagent_id FROM {P}subagents");
    let (filter, ids): (String, Vec<&str>) = if pseudo {
        let indexed = has_column(db, "subagents", "child_thread_id")?
            && db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {P}subagents WHERE child_thread_id=?1)"), [&trace.thread], |r| r.get::<_, bool>(0))?;
        let filter = if indexed { "child_thread_id=?" } else { "json_extract(payload_json,'$.childThreadId')=?" };
        (filter.into(), vec![&trace.thread])
    } else {
        let runs = trace.runs.iter().map(|r| r.id.as_str());
        let marks = vec!["?"; trace.runs.len()].join(",");
        let by_run = format!("json_extract(payload_json,'$.runId') IN ({marks})");
        if has_column(db, "subagents", "thread_id")? {
            (format!("thread_id=? AND {by_run}"), std::iter::once(trace.thread.as_str()).chain(runs).collect())
        } else {
            (by_run, runs.collect())
        }
    };
    let mut q = db.prepare(&format!("{select} WHERE {filter} ORDER BY {started}"))?;
    let mut rows = q.query(rusqlite::params_from_iter(ids))?;
    while let Some(r) = rows.next()? {
        let Some(a) = stamp(r.get(3)?) else { continue };
        let status = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        let end = if matches!(status.as_str(), "idle" | "pending" | "running" | "waiting") { None } else { stamp(r.get(4)?).or(Some(a)) };
        let model = crate::terminal::short_model(&r.get::<_, Option<String>>(6)?.unwrap_or_default());
        if pseudo {
            if !matches!(target, Target::Thread(_)) {
                return Ok(None);
            }
            trace.model = model;
            trace.runs.push(TraceRun {
                id: trace.thread.clone(),
                ordinal: 0,
                requested: a,
                started: Some(a),
                completed: end,
                status,
                continuation: false,
                queue_position: None,
            });
            break;
        }
        let run_id: Option<String> = r.get(0)?;
        let run = trace.runs.iter().position(|r| Some(&r.id) == run_id.as_ref());
        let native = r.get::<_, Option<String>>(1)?.as_deref() == Some("provider_native");
        let child_thread: Option<String> = r.get(5)?;
        let mut s = span(Kind::Delegate, "agent", &model, a, end, &status, run);
        s.key = r.get(9)?;
        s.delegate = Some(Delegate { native, remote: !native && child_thread.is_none(), child_thread, model, title: r.get(7)? });
        trace.spans.push(s);
    }
    if trace.runs.is_empty() {
        return Ok(None);
    }
    trace.requested = trace.runs[0].requested;
    let mut seen_items = vec![];
    // Items of the task's runs, by T3's run index: their type, status, times and id; a tool's
    // name; a failure's class; and for a command, only its listed programs. The command line
    // goes to `t3_programs` inside the query and never comes back out of it.
    let program = "t3_programs(json_extract(payload_json,'$.input'))";
    // Formal asks (questions and approvals) as T3's runtime requests record them, by request
    // id: kind, status, when asked and resolved, the node that asked, and whether a question
    // is answerable by a later message, the classification the live dashboard makes (a
    // missing, malformed or unfamiliar capability counts as holding the run). The request is
    // authoritative for an ask's times; its item is the fallback. It is pending as the live
    // reader decides (the open-asks branch's `source::REQUEST_FILTER`): `resolved_at IS NULL`,
    // and `pending`, or for an approval one of its older names (`open`, `waiting`,
    // `requested`); a question is pending only as `pending`. A recorded resolution ends it
    // even when the status was never updated.
    // Identifiers, statuses, times and a yes or no only.
    let request_columns = columns(db, "runtime_requests")?;
    let id_column = ["runtime_request_id", "request_id"].into_iter().find(|c| request_columns.contains(*c));
    let field = |name: &str| if request_columns.contains(name) { name.to_owned() } else { "NULL".to_owned() };
    let mut requests: HashMap<String, Request> = HashMap::new();
    if let (Some(id), true) = (id_column, request_columns.contains("kind")) {
        let capability = if request_columns.contains("payload_json") {
            "CASE WHEN kind='user_input' THEN CASE WHEN json_valid(payload_json)
                 THEN json_extract(payload_json,'$.responseCapability.type') IS 'message' ELSE 0 END ELSE 0 END"
        } else {
            "0"
        };
        let mut q = db.prepare(&format!(
            "SELECT {id}, kind, {resolved} IS NULL AND {status} IN ('pending','open','waiting','requested') AND (kind<>'user_input' OR {status}='pending'),
                    {}, {resolved}, {capability}, {}
             FROM {P}runtime_requests WHERE thread_id=?1 AND kind NOT IN ('dynamic_tool_call','auth_refresh')",
            field("created_at"),
            field("node_id"),
            status = field("status"),
            resolved = field("resolved_at"),
        ))?;
        let rows = q.query_map([&trace.thread], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Request {
                    question: r.get::<_, Option<String>>(1)?.as_deref() == Some("user_input"),
                    pending: r.get::<_, Option<bool>>(2)?.unwrap_or(false),
                    created: stamp(r.get(3)?),
                    resolved: stamp(r.get(4)?),
                    by_message: r.get::<_, Option<bool>>(5)?.unwrap_or(false),
                    node: r.get(6)?,
                },
            ))
        })?;
        for row in rows {
            let (id, request) = row?;
            requests.insert(id, request);
        }
    }
    let mut placed: std::collections::HashSet<String> = std::collections::HashSet::new();
    // When the person wrote in the thread: the update times of genuine human messages (role
    // user, createdBy user, no idle compaction), as T3 decides a link card was answered. Times
    // only. Without message provenance in an older schema, a link ends only when it changes.
    let messages = columns(db, "messages")?;
    let mut human: Vec<f64> = vec![];
    if ["thread_id", "role", "updated_at", "payload_json"].iter().all(|c| messages.contains(*c)) {
        let mut q = db.prepare(&format!(
            "SELECT updated_at FROM {P}messages WHERE thread_id=?1 AND role='user'
               AND CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.createdBy')='user'
                   AND json_extract(payload_json,'$.idleCompaction') IS NULL END
             ORDER BY updated_at"
        ))?;
        for row in q.query_map([&trace.thread], |r| r.get::<_, Option<String>>(0))? {
            if let Some(at) = stamp(row?) {
                human.push(at);
            }
        }
    }
    let node = if columns(db, "turn_items")?.contains("node_id") { "node_id IS NOT NULL" } else { "1" };
    let mut asks: Vec<AskRecord> = vec![];
    let filter = if pseudo { "thread_id=?1 AND run_id IS NULL" } else { "run_id=?1" };
    let types = "'command_execution','dynamic_tool','file_change','file_search','web_search','user_input_request','approval_request','secret_request','link_request','reasoning','assistant_message','compaction','user_message','run_interrupt_request','error'";
    let selected = format!("type,status,ordinal,updated_at,json_extract(payload_json,'$.startedAt'),json_extract(payload_json,'$.completedAt'),CASE WHEN type='dynamic_tool' THEN json_extract(payload_json,'$.toolName') END,CASE WHEN type='command_execution' THEN {program} END,CASE WHEN type='error' THEN json_extract(payload_json,'$.failure.class') END,turn_item_id,{node},CASE WHEN type IN ('user_input_request','approval_request') THEN json_extract(payload_json,'$.requestId') END");
    let mut q = db.prepare(&format!("SELECT {selected} FROM {P}turn_items WHERE {filter} AND type IN ({types}) ORDER BY ordinal"))?;
    // The cheap pass: which steps there are, in order, and when each was last written. Only
    // `updated_at` and the status are read from the row, never its payload.
    let mut light = db.prepare(&format!("SELECT turn_item_id,updated_at,status FROM {P}turn_items WHERE {filter} AND type IN ({types}) ORDER BY ordinal"))?;
    let mut by_id = db.prepare(&format!("SELECT {selected} FROM {P}turn_items WHERE turn_item_id=?1"))?;
    let full = cache.reads % FULL_EVERY == 0;
    let mut kept: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, run) in trace.runs.iter().enumerate() {
        let key = if pseudo { &trace.thread } else { &run.id };
        let mut steps: Vec<Step> = vec![];
        if full {
            for row in q.query_map([key], Step::from_row)? {
                let (raw, step) = row?;
                kept.insert(step.id.clone());
                cache.steps.insert(step.id.clone(), (raw, step.clone()));
                steps.push(step);
            }
        } else {
            let listed: Vec<(String, Option<String>, String)> = light.query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            let reusable: Vec<bool> = listed
                .iter()
                .map(|(id, raw, status)| {
                    let settled = !matches!(status.as_str(), "running" | "pending" | "waiting" | "requested");
                    let old = stamp(raw.clone()).is_some_and(|u| u < now - RECENT_MS);
                    settled && old && cache.steps.get(id).is_some_and(|(cached, step)| cached == raw && step.status == *status)
                })
                .collect();
            let stale = reusable.iter().filter(|r| !**r).count();
            if stale * 4 > listed.len() {
                // Most of the run changed: one pass costs less than a lookup per step.
                for row in q.query_map([key], Step::from_row)? {
                    let (raw, step) = row?;
                    kept.insert(step.id.clone());
                    cache.steps.insert(step.id.clone(), (raw, step.clone()));
                    steps.push(step);
                }
            } else {
                for ((id, _, _), reuse) in listed.into_iter().zip(reusable) {
                    let step = if reuse {
                        cache.steps[&id].1.clone()
                    } else {
                        match by_id.query_row([&id], Step::from_row).optional()? {
                            Some((raw, step)) => {
                                cache.steps.insert(id.clone(), (raw, step.clone()));
                                step
                            }
                            None => continue,
                        }
                    };
                    kept.insert(id);
                    steps.push(step);
                }
            }
        }
        let mut user_seen = false;
        for r in &steps {
            let (kind, status) = (r.kind.clone(), r.status.clone());
            let updated = r.updated.unwrap_or(now);
            let a = r.started;
            let b = r.completed;
            let at = a.or(b).unwrap_or(updated);
            if pseudo && (at < run.requested || at > run.completed.unwrap_or(now)) {
                continue;
            }
            let mark = match kind.as_str() {
                "user_message" => {
                    let old = user_seen;
                    user_seen = true;
                    old.then_some(MarkKind::Steer)
                }
                "run_interrupt_request" => Some(MarkKind::StopAsked),
                "error" => Some(MarkKind::Error),
                _ => None,
            };
            if let Some(kind) = mark {
                trace.marks.push(Mark { detail: if kind == MarkKind::Error { r.failure.clone() } else { None }, kind, at, run: i });
            }
            let ask = matches!(kind.as_str(), "user_input_request" | "approval_request" | "secret_request" | "link_request");
            // A step needs a start; an ask with none starts at its update, as T3 does.
            let Some(mut a) = a.or(ask.then_some(updated)) else { continue };
            if pseudo {
                a = a.max(run.requested);
            }
            let seen = matches!(kind.as_str(), "reasoning" | "assistant_message" | "compaction");
            if seen {
                seen_items.push((i, a, b.unwrap_or(if status == "running" { now } else { updated }), kind == "compaction"));
                continue;
            }
            if matches!(kind.as_str(), "user_message" | "run_interrupt_request" | "error") {
                continue;
            }
            let open = item_open(&kind, &status);
            let end = if pseudo && run.completed.is_some() {
                Some(b.unwrap_or(if open { now } else { updated }).min(run.completed.unwrap()))
            } else if open {
                None
            } else {
                Some(b.unwrap_or(updated))
            };
            let tool: Option<String> = r.tool.clone();
            let programs: Vec<&'static str> = r.programs.clone();
            let label = match kind.as_str() {
                "dynamic_tool" => tool.as_deref().unwrap_or("tool"),
                // A command with no listed program is "other": beside `cargo`, "command"
                // would read as a program of that name.
                "command_execution" => program::label(&programs).unwrap_or("other"),
                "file_change" => "edit",
                "file_search" => "search",
                "web_search" => "web search",
                "user_input_request" => "question",
                "approval_request" => "approval",
                "secret_request" => "secret",
                _ => "link",
            };
            let category = if ask {
                "ask"
            } else if kind == "file_search" {
                "read"
            } else {
                crate::model::category(&kind, tool.as_deref())
            };
            let mut end = end;
            let mut start = a;
            let mut by_question = false;
            if ask {
                let request_id: Option<String> = r.request.clone();
                let record = ask_record(
                    &kind,
                    r.id.clone(),
                    a,
                    end,
                    r.started.unwrap_or(updated),
                    r.has_node,
                    request_id.as_ref().and_then(|id| requests.get(id)),
                    if kind == "secret_request" && (run.completed.is_some()
                        || matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting"))
                    {
                        run.started.map(|start| (start, run.completed))
                    } else {
                        None
                    },
                    &human,
                );
                if let Some(id) = request_id {
                    placed.insert(id);
                }
                (start, end, by_question) = (record.ask.start, record.shown_end, record.by_message());
                asks.push(record);
            }
            let mut s = span(if ask { Kind::Ask } else { Kind::Tool }, category, label, start, end, &status, Some(i));
            s.key = r.id.clone();
            s.by_message = by_question;
            if programs.len() > 1 {
                s.programs = programs;
            }
            trace.spans.push(s);
        }
    }
    // Only this task's steps are kept: a step T3 removed, or one of a task no longer read,
    // leaves the cache.
    cache.steps.retain(|id, _| kept.contains(id));
    // Asks with no run, on a thread that has runs, are placed in the task whose stretch (its
    // first request, up to the next task's) holds their start: a thread's tasks follow one
    // another, so that is never ambiguous. One that starts before the thread's first task is
    // counted in `unplaced_asks`, not drawn, and only on that first task, so one card is one
    // count however many tasks are read.
    let window = (trace.runs[0].requested, trace.next.as_ref().and_then(|id| all.iter().find(|r| &r.0.id == id)).map_or(f64::INFINITY, |r| r.0.requested));
    let first_task = all.first().is_some_and(|r| r.0.id == trace.runs[0].id);
    if !pseudo {
        // `+ordinal` keeps the order but stops it choosing the thread's whole ordinal index: by
        // thread and type, or thread and run, SQLite reads only the asks with no run.
        let mut q = db.prepare(&format!(
            "SELECT type,status,updated_at,json_extract(payload_json,'$.startedAt'),json_extract(payload_json,'$.completedAt'),turn_item_id,{node},
                    CASE WHEN type IN ('user_input_request','approval_request') THEN json_extract(payload_json,'$.requestId') END
             FROM {P}turn_items WHERE thread_id=?1 AND run_id IS NULL
               AND type IN ('user_input_request','approval_request','secret_request','link_request') ORDER BY +ordinal"
        ))?;
        let mut rows = q.query([&trace.thread])?;
        while let Some(r) = rows.next()? {
            let (kind, status): (String, String) = (r.get(0)?, r.get(1)?);
            let updated = stamp(r.get(2)?).unwrap_or(now);
            let from = stamp(r.get(3)?).unwrap_or(updated);
            let request_id: Option<String> = r.get(7)?;
            if from < window.0 && first_task {
                trace.unplaced_asks += 1;
                continue;
            }
            if from < window.0 || from >= window.1 {
                continue;
            }
            let open = item_open(&kind, &status);
            let end = if open { None } else { Some(stamp(r.get(4)?).unwrap_or(updated)) };
            let record = ask_record(&kind, r.get(5)?, from, end, from, r.get::<_, bool>(6)?, request_id.as_ref().and_then(|id| requests.get(id)), None, &human);
            if let Some(id) = request_id {
                placed.insert(id);
            }
            let mut s = span(Kind::Ask, "ask", ask_label(&kind), record.ask.start, record.shown_end, &status, None);
            s.key = record.key.clone();
            s.by_message = record.by_message();
            trace.spans.push(s);
            asks.push(record);
        }
    }
    // Requests with no item: placed through the node that asked, when T3's nodes record which
    // run it belongs to and that run is one of this task's. One that cannot be placed is
    // counted in `unplaced_asks` when it was asked within the task's stretch, or before the
    // thread's first task when this is that task.
    let nodes = columns(db, "nodes")?;
    let node_run = |node: &str| -> rusqlite::Result<Option<String>> {
        if !nodes.contains("node_id") || !nodes.contains("run_id") {
            return Ok(None);
        }
        db.query_row(&format!("SELECT run_id FROM {P}nodes WHERE node_id=?1"), [node], |r| r.get::<_, Option<String>>(0)).optional().map(Option::flatten)
    };
    for (id, request) in &requests {
        if placed.contains(id) {
            continue;
        }
        let Some(created) = request.created else { continue };
        let run = match &request.node {
            Some(node) => node_run(node)?.and_then(|run| trace.runs.iter().position(|r| r.id == run)),
            None => None,
        };
        match run {
            Some(i) => {
                let kind = if request.question { "user_input_request" } else { "approval_request" };
                let record = ask_record(kind, format!("request:{id}"), created, None, created, true, Some(request), None, &human);
                let mut s = span(Kind::Ask, "ask", ask_label(kind), record.ask.start, record.shown_end, if request.pending { "pending" } else { "resolved" }, Some(i));
                s.key = record.key.clone();
                s.by_message = record.by_message();
                trace.spans.push(s);
                asks.push(record);
            }
            None if (created >= window.0 || first_task) && created < window.1 && request.node.as_ref().is_none_or(|n| node_run(n).ok().flatten().is_none()) => {
                trace.unplaced_asks += 1
            }
            None => {}
        }
    }
    // Every interval in which a run of this thread was open, whichever task it belongs to: a
    // link left open by this task stops waiting while a later task's run works. Whether one is
    // open now comes from the runs themselves (see `anatomy::holds_now`).
    let thread_runs: Vec<&TraceRun> = if pseudo { trace.runs.iter().collect() } else { all.iter().map(|(r, _, _)| r).collect() };
    let working: Vec<(f64, f64)> = thread_runs.iter().filter_map(|r| r.started.map(|a| (a, r.completed.unwrap_or(now)))).collect();
    let working_now = thread_runs.iter().any(|r| r.started.is_some() && r.completed.is_none());
    // Attempts are optional in older schemas; only restart reason and time are needed.
    let attempts: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [format!("{P}run_attempts")], |r| r.get(0))?;
    if attempts && !pseudo {
        let mut q = db.prepare(&format!(
            // The reason is in the payload: the table has no column for it (its columns, as T3
            // writes them, are in the test schema below).
            "SELECT run_id,json_extract(payload_json,'$.reason'),json_extract(payload_json,'$.startedAt') FROM {P}run_attempts WHERE run_id IN ({})",
            trace.runs.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        ))?;
        let mut rows = q.query(rusqlite::params_from_iter(trace.runs.iter().map(|r| &r.id)))?;
        while let Some(r) = rows.next()? {
            if r.get::<_, Option<String>>(1)?.as_deref() == Some("steering_restart") {
                if let Some(at) = stamp(r.get(2)?) {
                    let id: String = r.get(0)?;
                    trace.marks.push(Mark { kind: MarkKind::Restart, at, run: trace.runs.iter().position(|r| r.id == id).unwrap(), detail: None });
                }
            }
        }
    }
    finish(&mut trace, &seen_items, &asks, &working, working_now);
    Ok(Some(trace))
}
/// A formal ask as T3's runtime request records it.
struct Request {
    question: bool,
    pending: bool,
    created: Option<f64>,
    resolved: Option<f64>,
    by_message: bool,
    node: Option<String>,
}

/// One ask, ready for the shared rule: its key (to find its span), what `anatomy` needs,
/// whether T3 would count it at all (a card needs a node), and where its bar ends (a link at
/// the person's next message, a secret at its run's end).
struct AskRecord {
    key: String,
    ask: anatomy::Ask,
    eligible: bool,
    shown_end: Option<f64>,
}

impl AskRecord {
    fn by_message(&self) -> bool {
        self.ask.kind == anatomy::AskKind::Question { by_message: true }
    }
}

// T3 uses the item state "waiting" for open link and secret cards. Runtime
// requests have their own authoritative projection and older item-state aliases.
fn item_open(kind: &str, status: &str) -> bool {
    match kind {
        "secret_request" | "link_request" => status == "waiting",
        "user_input_request" | "approval_request" => matches!(status, "running" | "pending" | "waiting" | "requested"),
        _ => matches!(status, "running" | "pending"),
    }
}

fn ask_label(kind: &str) -> &'static str {
    match kind {
        "user_input_request" => "question",
        "approval_request" => "approval",
        "secret_request" => "secret",
        _ => "link",
    }
}

/// Build one ask's record from its item and, for a question or an approval, its runtime
/// request, which is authoritative for when it was asked and resolved. `from` is what a
/// human update must be strictly later than: the card's start, else its update.
#[allow(clippy::too_many_arguments)]
fn ask_record(
    kind: &str,
    key: String,
    start: f64,
    end: Option<f64>,
    from: f64,
    has_node: bool,
    request: Option<&Request>,
    own_run: Option<(f64, Option<f64>)>,
    human: &[f64],
) -> AskRecord {
    use anatomy::AskKind;
    let kind = match kind {
        "approval_request" => AskKind::Approval,
        "user_input_request" => AskKind::Question { by_message: request.is_some_and(|r| r.by_message) },
        "secret_request" => AskKind::Secret,
        _ => AskKind::Link,
    };
    let (mut start, mut end) = (start, end);
    if let Some(r) = request.filter(|_| matches!(kind, AskKind::Approval | AskKind::Question { .. })) {
        start = r.created.unwrap_or(start);
        // A closed request with no time of its own ends at the item's, or adds nothing.
        end = if r.pending { None } else { r.resolved.or(end).or(Some(start)) };
    }
    let human_after = (kind == AskKind::Link).then(|| human.iter().copied().find(|h| *h > from)).flatten();
    let stop = human_after.into_iter().chain(if kind == AskKind::Secret { own_run.and_then(|(_, done)| done) } else { None }).fold(f64::INFINITY, f64::min);
    let shown_end = if stop.is_finite() && end.is_none_or(|e| stop < e) { Some(stop.max(start)) } else { end };
    // A card counts with a node, as T3 counts it. A question or an approval counts only with
    // its runtime request, the record the live dashboard counts (`source::REQUEST_FILTER`
    // reads requests, never items): an item with no request is drawn and holds nobody.
    let eligible = if matches!(kind, AskKind::Secret | AskKind::Link) { has_node } else { request.is_some() };
    AskRecord { key, ask: anatomy::Ask { kind, start, end, own_run, human_after }, eligible, shown_end }
}

fn finish(t: &mut Trace, seen_items: &[(usize, f64, f64, bool)], records: &[AskRecord], working: &[(f64, f64)], working_now: bool) {
    let now = t.now;
    for (i, r) in t.runs.iter().enumerate() {
        if let Some(a) = r.started {
            if a > r.requested {
                t.spans.push(span(Kind::Queue, "queue", "queued", r.requested, Some(a), "completed", Some(i)));
            }
        } else if let Some(b) = r.completed {
            // Ended without starting, as when preparing its workspace failed: its whole
            // time was spent before the start.
            t.spans.push(span(Kind::Queue, "queue", "queued", r.requested, Some(b), &r.status, Some(i)));
        } else if matches!(r.status.as_str(), "queued" | "pending" | "starting") {
            t.spans.push(span(Kind::Queue, "queue", "queued", r.requested, None, "running", Some(i)));
        }
    }
    let last = t.runs.last().unwrap();
    let queued = last.started.is_none() && last.completed.is_none();
    let running = t.runs.iter().any(|r| r.started.is_some() && r.completed.is_none());
    // Waiting on the person, by the one rule Trace and Recap share (`anatomy`), against every
    // run of the thread that was working. Only asks T3 would count take part, and the state
    // comes from the asks pending now, the same records as the clock: a stretch reaching now
    // cannot tell an ask answered at now from one still pending. A question answerable by
    // message is open but holds nothing, and keeps the task neither open nor waiting.
    let counted: Vec<&AskRecord> = records.iter().filter(|r| r.eligible).collect();
    let waiting_now = counted.iter().any(|r| anatomy::holds_now(&r.ask, working_now, now));
    let asks_open = counted.iter().any(|r| anatomy::still_open(&r.ask));
    let open = running || queued || asks_open || t.spans.iter().any(|s| s.kind != Kind::Ask && s.end.is_none());
    t.ended = if open {
        None
    } else {
        t.runs
            .iter()
            .filter_map(|r| r.completed)
            .chain(t.spans.iter().filter(|s| s.kind != Kind::Ask).filter_map(|s| s.end))
            .chain(counted.iter().filter(|r| !r.by_message()).filter_map(|r| r.shown_end))
            .max_by(f64::total_cmp)
    };
    t.state = if queued {
        State::Queued
    } else if waiting_now {
        State::Waiting
    } else if running {
        State::Running
    } else if t.spans.iter().any(|s| s.kind == Kind::Delegate && s.end.is_none()) {
        State::Delegated
    } else {
        match last.status.as_str() {
            "failed" => State::Failed,
            "interrupted" => State::Interrupted,
            "cancelled" => State::Cancelled,
            _ => State::Completed,
        }
    };
    let (lo, hi) = (t.requested, t.ended.unwrap_or(now));
    // Calls the provider closed together at one instant have no length of their own: they are
    // marked, and add no time (the rule is shared with the Tools view, in `anatomy`).
    let calls: Vec<(f64, Option<f64>)> = t.spans.iter().filter(|s| s.kind == Kind::Tool).map(|s| (s.start, s.end)).collect();
    let mut flags = anatomy::closed_together(&calls).into_iter();
    for s in t.spans.iter_mut().filter(|s| s.kind == Kind::Tool) {
        s.closed_together = flags.next().unwrap_or(false);
    }
    // Each counted ask's own stretches, clipped to the task, on its span; their union is the
    // task's waiting.
    let mut all_holds = vec![];
    for record in &counted {
        let holds: Vec<(f64, f64)> =
            anatomy::blocking(&record.ask, working, now).into_iter().map(|(a, b)| (a.max(lo), b.min(hi))).filter(|(a, b)| b > a).collect();
        if let Some(span) = t.spans.iter_mut().find(|s| s.key == record.key) {
            span.holds = holds.clone();
        }
        all_holds.extend(holds);
    }
    let asks: Vec<(f64, f64)> = anatomy::merged(&all_holds, lo, hi);
    t.waiting = asks.clone();
    let tools: Vec<_> = t
        .spans
        .iter()
        .filter(|s| (s.kind == Kind::Tool && !s.closed_together) || s.delegate.as_ref().is_some_and(|d| d.native))
        .map(|s| (s.start, s.end.unwrap_or(now)))
        .collect();
    let runs: Vec<_> = t.runs.iter().filter_map(|r| r.started.map(|a| (a, r.completed.unwrap_or(now)))).collect();
    // Outside runs, asks win over the queue too, keeping all six parts disjoint.
    let outside_runs = outside(&[(lo, hi)], &runs, lo, hi);
    t.split.total = (hi - lo).max(0.);
    t.split.waiting = union(&asks, lo, hi);
    let queues: Vec<_> = t.spans.iter().filter(|s| s.kind == Kind::Queue).map(|s| (s.start, s.end.unwrap_or(now))).collect();
    let mut covered = runs.clone();
    covered.extend_from_slice(&asks);
    let queue = outside(&queues, &covered, lo, hi);
    t.split.queue = union(&queue, lo, hi);
    covered.extend(queue);
    // Gaps include any tail after a completed run, e.g. a delegate still in flight.
    for (a, b) in outside(&outside_runs, &covered, lo, hi) {
        t.split.between += b - a;
        t.spans.push(span(
            Kind::Between,
            "between",
            "no run open",
            a,
            if open && b == hi { None } else { Some(b) },
            if open && b == hi { "running" } else { "completed" },
            None,
        ));
    }
    for (i, r) in t.runs.iter().enumerate() {
        let Some(a) = r.started else { continue };
        let b = r.completed.unwrap_or(now).min(hi);
        let parts = anatomy::split(a, b, &tools, &asks);
        t.split.tools += union(&parts.tools, a, b);
        let mut seen_cover = vec![];
        for &(run, c, d, compaction) in seen_items {
            if run != i {
                continue;
            }
            for (e, f) in outside(&parts.model, &seen_cover, c.max(a), d.min(b)) {
                t.split.seen += f - e;
                t.spans.push(span(
                    Kind::Seen,
                    "think",
                    if compaction { "compaction" } else { "thinking" },
                    e,
                    if r.completed.is_none() && f == now { None } else { Some(f) },
                    if r.completed.is_none() && f == now { "running" } else { "completed" },
                    Some(i),
                ));
                seen_cover.push((e, f));
            }
        }
        for (c, d) in outside(&parts.model, &seen_cover, a, b) {
            t.split.model += d - c;
            t.spans.push(span(
                Kind::Model,
                "model",
                "model",
                c,
                if r.completed.is_none() && d == now { None } else { Some(d) },
                if r.completed.is_none() && d == now { "running" } else { "completed" },
                Some(i),
            ));
        }
    }
    // Quiet stretches: inside the task, outside everything in flight. A call closed together
    // with others has no end of its own, so only its start counts as activity.
    // Waiting on the person is something in flight; an ask that is not blocking is not.
    let mut busy: Vec<(f64, f64)> = runs.clone();
    busy.extend(t.waiting.iter().copied());
    for s in &t.spans {
        match s.kind {
            Kind::Queue | Kind::Delegate => busy.push((s.start, s.end.unwrap_or(now))),
            Kind::Tool if s.closed_together => busy.push((s.start, s.start)),
            Kind::Tool => busy.push((s.start, s.end.unwrap_or(now))),
            _ => {}
        }
    }
    t.quiet = outside(&[(lo, hi)], &busy, lo, hi).into_iter().filter(|(a, b)| b - a > QUIET_FOLD_MS).collect();
    let mut categories: BTreeMap<String, (u32, Vec<(f64, f64)>)> = BTreeMap::new();
    for s in &t.spans {
        match s.kind {
            Kind::Tool => {
                t.steps.tools += 1;
                t.steps.failed += u32::from(s.status == "failed");
                t.steps.closed_together += u32::from(s.closed_together);
                t.steps.under_a_second += u32::from(!s.closed_together && s.end.is_some_and(|b| b - s.start < 1000.));
                let entry = categories.entry(s.category.clone()).or_default();
                entry.0 += 1;
                if !s.closed_together {
                    entry.1.push((s.start, s.end.unwrap_or(now)));
                }
            }
            Kind::Ask => t.steps.asks += 1,
            Kind::Delegate => t.steps.delegates += 1,
            _ => (),
        }
    }
    t.steps.by_category = categories.into_iter().map(|(k, (n, v))| (k, n, union(&v, lo, hi))).collect();
    // Stable sort retains SQL ordinal for ties within a kind.
    // The spans worked out here are keyed by what they are, which run, and where they start.
    for s in t.spans.iter_mut().filter(|s| s.key.is_empty()) {
        s.key = format!("{}:{}:{}", serde_json::to_value(s.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(), s.run.map_or(-1, |r| r as i64), s.start as i64);
    }
    t.spans.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.kind.cmp(&b.kind)));
    t.marks.sort_by(|a, b| a.at.total_cmp(&b.at));
}

// Hand-built schema, deliberately containing no user database paths or discovery.
const SCHEMA: &str = "
CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_turn_items(turn_item_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,node_id TEXT DEFAULT 'n',type TEXT,status TEXT,ordinal INTEGER,updated_at TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_runtime_requests(runtime_request_id TEXT PRIMARY KEY,thread_id TEXT,node_id TEXT,provider_turn_id TEXT,kind TEXT,status TEXT,created_at TEXT,resolved_at TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_messages(message_id TEXT PRIMARY KEY,thread_id TEXT,role TEXT,streaming INTEGER,created_at TEXT,updated_at TEXT,payload_json TEXT);
CREATE INDEX turn_items_run_ordinal_idx ON orchestration_v2_projection_turn_items(run_id,ordinal);
CREATE TABLE orchestration_v2_projection_subagents(subagent_id TEXT PRIMARY KEY,thread_id TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_run_attempts(attempt_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,attempt_ordinal INTEGER,root_node_id TEXT,provider TEXT,provider_thread_id TEXT,provider_turn_id TEXT,status TEXT,payload_json TEXT,provider_instance_id TEXT);";
fn insert_run(db: &Connection, id: &str, ordinal: i64, a: f64, b: Option<f64>, continuation: bool) -> rusqlite::Result<()> {
    let mut p = serde_json::json!({"startedAt":recap::iso(a+100.),"status":if b.is_some() {"completed"} else {"running"},"providerInstanceId":"codex","modelSelection":{"model":"gpt-6.1-sol"}});
    if continuation {
        p["workStartedAt"] = serde_json::json!(recap::iso(a - 1000.));
    }
    db.execute(
        &format!("INSERT INTO {P}runs(run_id,thread_id,ordinal,payload_json,requested_at,completed_at) VALUES(?1,'t',?2,?3,?4,?5)"),
        params![id, ordinal, p.to_string(), recap::iso(a), b.map(recap::iso)],
    )?;
    Ok(())
}
fn insert_item(
    db: &Connection,
    id: usize,
    run: Option<&str>,
    kind: &str,
    status: &str,
    a: f64,
    b: Option<f64>,
    extra: serde_json::Value,
) -> rusqlite::Result<()> {
    let mut p = serde_json::json!({"startedAt":recap::iso(a),"completedAt":b.map(recap::iso),"input":"never read","output":"never read","text":"never read","fileName":"never read","prompt":"never read","result":"never read","message":"never read","questions":"never read","title":"never read"});
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    db.execute(
        &format!(
            "INSERT INTO {P}turn_items(turn_item_id,thread_id,run_id,type,status,ordinal,updated_at,payload_json) VALUES(?1,'t',?2,?3,?4,?5,?6,?7)"
        ),
        params![format!("i{id}"), run, kind, status, id as i64, recap::iso(b.unwrap_or(a)), p.to_string()],
    )?;
    Ok(())
}
fn database() -> rusqlite::Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(SCHEMA)?;
    db.execute(
        &format!("INSERT INTO {P}threads VALUES('t',?1)"),
        [serde_json::json!({"title":"A task","lineage":{"relationshipToParent":null}}).to_string()],
    )?;
    Ok(db)
}
/// The made-up task follows a Codex agent fixing flaky tests for nearly two hours.
/// Two runs with a delegate's work between them; about
/// 600 steps, most under a second; overlapping calls; a question answered after seven
/// minutes and a secret request that timed out; a native subagent, a delegate with its own
/// thread and one on another machine; reasoning over part of the model's time; someone
/// stepping in and asking it to stop; and a cargo test running for six minutes at `now`.
/// The same every time for the same `now`.
fn demo_fill(db: &Connection, now: f64) -> rusqlite::Result<()> {
    use serde_json::json;
    let a = now - 112. * 60_000.;
    let m = |minutes: f64| a + minutes * 60_000.;
    db.execute(&format!("UPDATE {P}threads SET payload_json=json_set(payload_json,'$.title','Fix the flaky checkout tests') WHERE thread_id='t'"), [])?;
    insert_run(db, "r1", 1, a, Some(m(45.)), false)?;
    insert_run(db, "r2", 2, m(61.), None, true)?;
    // A repeatable number in [0, 1) for a seed.
    let chance = |seed: u64| -> f64 {
        let mut h = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(0xd1b5_4a32_d192_ed03);
        h ^= h >> 31;
        h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h ^= h >> 29;
        (h >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut id = 0;
    let mut next = || {
        id += 1;
        id
    };
    // Each run opens with the message that started it.
    insert_item(db, next(), Some("r1"), "user_message", "completed", m(0.), Some(m(0.)), json!({}))?;
    insert_item(db, next(), Some("r2"), "user_message", "completed", m(61.), Some(m(61.)), json!({}))?;
    // The steps that took the time.
    for (run, kind, label, from, minutes, status) in [
        ("r1", "command_execution", "python3 never read", 3.0, 3.17, "completed"),
        ("r1", "command_execution", "cargo test never read", 10.2, 4.67, "failed"),
        ("r1", "user_input_request", "", 15.0, 7.0, "completed"),
        ("r2", "secret_request", "", 70.0, 1.0, "cancelled"),
        ("r2", "compaction", "", 70.5, 0.33, "completed"),
        ("r2", "command_execution", "cd never && python3 never read", 76.0, 1.2, "completed"),
        ("r2", "command_execution", "bash -lc 'cargo test never read 2>&1 | tail -40'", 88.0, 2.08, "failed"),
        ("r2", "command_execution", "./never-read --flag", 95.0, 1.5, "completed"),
    ] {
        // The question is a runtime request too, as T3 records one: the record that counts.
        let extra = if kind == "user_input_request" { json!({"input": label, "requestId": "q-demo"}) } else { json!({"input": label}) };
        insert_item(db, next(), Some(run), kind, status, m(from), Some(m(from + minutes)), extra)?;
    }
    db.execute(
        &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,kind,status,created_at,resolved_at,payload_json) VALUES('q-demo','t','user_input','resolved',?1,?2,'{{}}')"),
        params![recap::iso(m(15.)), recap::iso(m(22.))],
    )?;
    insert_item(db, next(), Some("r2"), "command_execution", "running", m(105.8), None, json!({"input": "mkdir -p never && cargo test never read"}))?;
    // Many small steps: four in five under a second.
    let quiet = [(15.0, 22.0)];
    for (run, lo, hi, count, seed) in [("r1", 0.1, 44.5, 300u64, 1u64), ("r2", 61.2, 105.5, 298, 2)] {
        let mut t = lo;
        for k in 0..count {
            let r = |n: u64| chance(seed * 100_000 + k * 10 + n);
            t += -((1. - r(0)).ln()) * (hi - lo) / count as f64;
            if t >= hi {
                break;
            }
            if quiet.iter().any(|(x, y)| (*x..=*y).contains(&t)) {
                continue;
            }
            let seconds = match r(1) {
                p if p < 0.80 => 0.05 + r(2) * 0.85,
                p if p < 0.95 => 1. + r(2) * 9.,
                _ => 10. + r(2) * 40.,
            };
            let (kind, extra) = match r(3) {
                p if p < 0.46 => {
                    let command = ["rg -n never", "git status", "sed -n 1,80p never", "cat never", "never-read"][(r(4) * 5.) as usize];
                    ("command_execution", json!({ "input": command }))
                }
                p if p < 0.76 => ("file_search", json!({})),
                p if p < 0.91 => ("file_change", json!({})),
                p if p < 0.94 => ("web_search", json!({})),
                _ => ("dynamic_tool", json!({"toolName": "mcp__t3-code__task_status"})),
            };
            let failed = kind == "command_execution" && r(5) < 0.02;
            insert_item(db, next(), Some(run), kind, if failed { "failed" } else { "completed" }, m(t), Some(m(t + seconds / 60.).min(m(hi))), extra)?;
        }
    }
    // Reasoning over part of the model's time.
    for (run, lo, hi, seed) in [("r1", 0.1, 45.0, 3u64), ("r2", 61.1, 112.0, 4)] {
        let (mut t, mut k) = (lo, 0u64);
        while t < hi {
            let length = 0.1 + chance(seed * 100_000 + k) * 1.3;
            if chance(seed * 100_000 + k + 50_000) < 0.45 {
                insert_item(db, next(), Some(run), "reasoning", "completed", m(t), Some(m((t + length).min(hi))), json!({}))?;
            }
            t += length + 0.05 + chance(seed * 100_000 + k + 70_000) * 0.55;
            k += 1;
        }
    }
    // Someone stepped in at 85 minutes, restarting the run, and asked it to stop at 100; it
    // carried on. A provider error it recovered from.
    insert_item(db, next(), Some("r2"), "user_message", "completed", m(85.), Some(m(85.)), json!({}))?;
    insert_item(db, next(), Some("r2"), "run_interrupt_request", "completed", m(100.), Some(m(100.)), json!({}))?;
    insert_item(db, next(), Some("r2"), "error", "failed", m(100.1), Some(m(100.1)), json!({"failure": {"class": "provider_error", "message": "never read"}}))?;
    db.execute(
        &format!("INSERT INTO {P}run_attempts(attempt_id,thread_id,run_id,attempt_ordinal,status,payload_json) VALUES('a2','t','r2',2,'completed',?1)"),
        [json!({"runId": "r2", "attemptOrdinal": 2, "reason": "steering_restart", "status": "completed", "startedAt": recap::iso(m(85.))}).to_string()],
    )?;
    // Delegates: a native subagent inside the run; one with its own thread, whose result woke
    // the second run; one on another machine.
    for (sid, origin, model, title, from, minutes, child) in [
        ("s-native", "provider_native", "gpt-6.1-terra", "Find the flaky assertion", 31.0, 4.0, Some("t-native")),
        ("s-suite", "app_owned", "gpt-6.1-terra", "Run the integration suite", 36.0, 25.0, Some("t-suite")),
        ("s-mac", "app_owned", "claude-opus-5-5", "Check it on the Mac", 40.0, 18.0, None),
    ] {
        db.execute(
            &format!("INSERT INTO {P}subagents VALUES(?1,'t',?2)"),
            params![sid, json!({"runId": "r1", "origin": origin, "status": "completed", "startedAt": recap::iso(m(from)), "completedAt": recap::iso(m(from + minutes)),
                "childThreadId": child, "model": model, "title": title, "prompt": "never read", "result": "never read"}).to_string()],
        )?;
    }
    Ok(())
}
/// The made-up task's database, for tests that need a file to read.
#[cfg(test)]
pub(crate) fn demo_database(now: f64) -> rusqlite::Result<Connection> {
    let db = database()?;
    demo_fill(&db, now)?;
    Ok(db)
}
pub fn demo(now: f64) -> Result<Trace, SourceError> {
    let path = Path::new("trace demo");
    let fail = |e: rusqlite::Error| SourceError { path: path.into(), kind: "schema", detail: e.to_string() };
    let db = database().map_err(fail)?;
    demo_fill(&db, now).map_err(fail)?;
    Ok(read_from(&db, path, &Target::Latest, now)?.expect("demo contains runs"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn at() -> f64 {
        recap::ms("2026-11-10T10:00:00.000Z").unwrap()
    }
    fn read(db: &Connection, target: Target, now: f64) -> Trace {
        let t = read_from(db, Path::new("synthetic"), &target, now).unwrap().unwrap();
        adds_up(&t);
        t
    }
    fn adds_up(t: &Trace) {
        let s = &t.split;
        assert!((s.total - s.queue - s.waiting - s.tools - s.seen - s.model - s.between).abs() < 0.001, "{:?}", s);
        assert!([s.queue, s.waiting, s.tools, s.seen, s.model, s.between].iter().all(|x| *x >= 0.));
    }
    fn single() -> Connection {
        let db = database().unwrap();
        insert_run(&db, "r", 1, at(), Some(at() + 10_000.), false).unwrap();
        db
    }
    #[test]
    fn overlap_and_ask_priority() {
        let db = single();
        for (i, kind, a, b) in [
            (1, "command_execution", 100., 5000.),
            (2, "file_search", 2000., 7000.),
            (3, "user_input_request", 3000., 6000.),
            (4, "approval_request", 5500., 8000.),
            (5, "reasoning", 100., 10_000.),
        ] {
            insert_item(&db, i, Some("r"), kind, "completed", at() + a, Some(at() + b), json!({"requestId": format!("q{i}")})).unwrap();
            if kind.ends_with("_request") {
                db.execute(
                    &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,kind,status,created_at,resolved_at) VALUES(?1,'t',?2,'resolved',?3,?4)"),
                    params![format!("q{i}"), if kind == "approval_request" { "command_approval" } else { "user_input" }, recap::iso(at() + a), recap::iso(at() + b)],
                )
                .unwrap();
            }
        }
        db.execute(&format!("INSERT INTO {P}subagents VALUES('n','t',?1)"),[json!({"runId":"r","origin":"provider_native","status":"completed","startedAt":recap::iso(at()+2000.),"completedAt":recap::iso(at()+9000.)}).to_string()]).unwrap();
        let t = read(&db, Target::Latest, at() + 20_000.);
        assert_eq!((t.split.queue, t.split.waiting, t.split.tools, t.split.seen, t.split.model), (100., 5000., 3900., 1000., 0.));
        assert_eq!((t.steps.tools, t.steps.asks), (2, 2));
    }
    #[test]
    fn tasks_and_navigation() {
        let db = single();
        insert_run(&db, "r2", 2, at() + 20_000., Some(at() + 30_000.), true).unwrap();
        insert_run(&db, "r3", 3, at() + 40_000., Some(at() + 50_000.), false).unwrap();
        db.execute(&format!("UPDATE {P}runs SET payload_json=json_set(payload_json,'$.delegatedCompletion',json('{{}}')) WHERE run_id='r3'"), [])
            .unwrap();
        insert_run(&db, "r4", 4, at() + 60_000., Some(at() + 70_000.), false).unwrap();
        let t = read(&db, Target::Run("r2".into()), at() + 80_000.);
        assert_eq!(t.runs.len(), 3);
        assert_eq!(t.next.as_deref(), Some("r4"));
        assert_eq!(t.previous, None);
        let t = read(&db, Target::Thread("t".into()), at() + 80_000.);
        assert_eq!(t.runs.len(), 1);
        assert_eq!(t.previous.as_deref(), Some("r"));
        assert!(read_from(&db, Path::new("test"), &Target::Run("unknown".into()), at()).unwrap().is_none());
        assert!(read_from(&db, Path::new("test"), &Target::Thread("unknown".into()), at()).unwrap().is_none());
    }
    #[test]
    fn latest_prefers_running_over_newer_finished_thread() {
        let db = database().unwrap();
        insert_run(&db, "open", 1, at(), None, false).unwrap();
        db.execute(&format!("INSERT INTO {P}threads VALUES('other',?1)"), [json!({"title":"Other"}).to_string()]).unwrap();
        insert_run(&db, "new", 2, at() + 20_000., Some(at() + 30_000.), false).unwrap();
        assert_eq!(read(&db, Target::Latest, at() + 40_000.).runs[0].id, "open");
        db.execute(&format!("UPDATE {P}runs SET thread_id='other' WHERE run_id='new'"), []).unwrap();
        assert_eq!(read(&db, Target::Latest, at() + 40_000.).thread, "t");
        db.execute(&format!("UPDATE {P}runs SET completed_at=?1 WHERE run_id='open'"), [recap::iso(at() + 10_000.)]).unwrap();
        assert_eq!(read(&db, Target::Latest, at() + 40_000.).thread, "other");
    }
    #[test]
    fn open_tool_and_wait_after_run_and_cancelled_ask() {
        let db = database().unwrap();
        insert_run(&db, "r", 1, at(), None, false).unwrap();
        insert_item(&db, 1, Some("r"), "command_execution", "running", at() + 1000., None, json!({"input":"cargo test"})).unwrap();
        let t = read(&db, Target::Latest, at() + 10_000.);
        assert_eq!(t.state, State::Running);
        assert_eq!(t.ended, None);
        assert!(t.spans.iter().any(|s| s.kind == Kind::Tool && s.end.is_none()));
        db.execute(
            &format!("UPDATE {P}runs SET completed_at=?1,payload_json=json_set(payload_json,'$.status','completed')"),
            [recap::iso(at() + 5000.)],
        )
        .unwrap();
        db.execute(&format!("DELETE FROM {P}turn_items"), []).unwrap();
        insert_item(&db, 2, Some("r"), "link_request", "waiting", at() + 4000., None, json!({})).unwrap();
        let t = read(&db, Target::Latest, at() + 10_000.);
        assert_eq!(t.state, State::Waiting);
        // A link waits only where no run works: from the run's end at 5 s, not from 4 s.
        assert_eq!(t.split.waiting, 5000.);
        db.execute(&format!("UPDATE {P}turn_items SET type='user_input_request',payload_json=json_set(payload_json,'$.requestId','q')"), []).unwrap();
        db.execute(
            &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,kind,status,created_at) VALUES('q','t','user_input','pending',?1)"),
            [recap::iso(at() + 4000.)],
        )
        .unwrap();
        assert_eq!(read(&db, Target::Latest, at() + 10_000.).state, State::Waiting);
        db.execute(&format!("DELETE FROM {P}runtime_requests"), []).unwrap();
        db.execute(&format!("UPDATE {P}turn_items SET type='secret_request',status='cancelled',updated_at=?1"), [recap::iso(at() + 8000.)]).unwrap();
        let t = read(&db, Target::Latest, at() + 10_000.);
        // A secret card asks only while its own run is open: the run ended at 5 s, so the card
        // stopped asking there, though it was cancelled at 8 s; one second of waiting.
        assert_eq!(t.ended, Some(at() + 5000.));
        assert_eq!(t.split.waiting, 1000.);
    }
    #[test]
    fn queue_delegate_and_terminal_states() {
        let db = single();
        for (status, state) in [("failed", State::Failed), ("interrupted", State::Interrupted), ("cancelled", State::Cancelled)] {
            db.execute(&format!("UPDATE {P}runs SET payload_json=json_set(payload_json,'$.status',?1)"), [status]).unwrap();
            assert_eq!(read(&db, Target::Latest, at() + 20_000.).state, state);
        }
        db.execute(
            &format!("INSERT INTO {P}subagents VALUES('d','t',?1)"),
            [json!({"runId":"r","origin":"app_owned","status":"waiting","startedAt":recap::iso(at()+5000.),"model":"gpt-6"}).to_string()],
        )
        .unwrap();
        assert_eq!(read(&db, Target::Latest, at() + 20_000.).state, State::Delegated);
        db.execute(&format!("UPDATE {P}runs SET completed_at=NULL,payload_json=json_set(payload_json,'$.status','queued','$.startedAt',NULL)"), [])
            .unwrap();
        let t = read(&db, Target::Latest, at() + 20_000.);
        assert_eq!(t.state, State::Queued);
        assert_eq!(t.split.queue, 20_000.);
    }
    #[test]
    fn native_thread_window() {
        let db = database().unwrap();
        db.execute(&format!("INSERT INTO {P}subagents VALUES('n','parent',?1)"),[json!({"childThreadId":"t","origin":"provider_native","status":"completed","startedAt":recap::iso(at()),"completedAt":recap::iso(at()+10_000.),"model":"claude-haiku-5-5"}).to_string()]).unwrap();
        db.execute(&format!("UPDATE {P}threads SET payload_json=json_set(payload_json,'$.lineage.relationshipToParent','subagent')"), []).unwrap();
        for (i, start) in [(1, -1000.), (2, 1000.), (3, 11_000.)] {
            insert_item(&db, i, None, "file_search", "completed", at() + start, Some(at() + start + 500.), json!({})).unwrap();
        }
        let t = read(&db, Target::Thread("t".into()), at() + 20_000.);
        assert_eq!(t.steps.tools, 1);
        assert_eq!(t.runs.len(), 1);
        assert!(t.delegate_thread);
        assert_eq!(t.split.total, 10_000.);
        assert!(read_from(&db, Path::new("test"), &Target::Latest, at()).unwrap().is_none());
    }
    #[test]
    fn command_names_come_from_the_shared_list() {
        let db = single();
        for (i, (command, want, chain)) in [
            ("cargo test x", "cargo", vec![]),
            ("/usr/bin/git log", "git", vec![]),
            ("  rg -n foo", "rg", vec![]),
            ("bash -lc \"cargo test\"", "cargo", vec![]),
            ("cd x && cargo test", "cargo", vec![]),
            ("FOO=1 cargo t", "cargo", vec![]),
            ("mkdir -p out && cargo build 2>&1 | tail -5", "cargo", vec!["mkdir", "cargo", "tail"]),
            ("./never-read", "other", vec![]),
            ("", "other", vec![]),
            ("\t\n/usr/bin/python3\tnever read", "python3", vec![]),
        ]
        .iter()
        .enumerate()
        {
            insert_item(&db, i, Some("r"), "command_execution", "completed", at() + 100. + i as f64, Some(at() + 500.), json!({"input":command}))
                .unwrap();
            let t = read(&db, Target::Latest, at() + 20_000.);
            let last = t.spans.iter().filter(|s| s.kind == Kind::Tool).last().unwrap();
            assert_eq!((last.label.as_str(), &last.programs), (*want, chain), "{command:?}");
            assert!(!serde_json::to_string(&t).unwrap().contains("never"));
        }
    }
    #[test]
    fn no_command_or_output_text_leaves_the_reader() {
        const SENTINEL: &str = "SENTINEL-7f3a";
        let db = single();
        let commands = [
            format!("cd /{SENTINEL} && cargo test {SENTINEL} 2>&1 | tail -20"),
            format!("bash -lc 'cat <<EOF > /tmp/{SENTINEL}\ngit status\nEOF\nrg {SENTINEL}'"),
            format!("{SENTINEL} --flag"),
            format!("echo \"{SENTINEL} && cargo\""),
        ];
        for (i, command) in commands.iter().enumerate() {
            let extra = json!({"input": command, "output": format!("{SENTINEL} {SENTINEL}"), "toolName": null});
            insert_item(&db, i, Some("r"), "command_execution", if i == 2 { "failed" } else { "completed" }, at() + 100. + i as f64, Some(at() + 900.), extra).unwrap();
        }
        let t = read(&db, Target::Latest, at() + 20_000.);
        let text = serde_json::to_string(&t).unwrap();
        assert!(!text.contains(SENTINEL), "{text}");
        // The heredoc's body is not a command: the wrapped script runs cat and rg.
        assert_eq!(t.spans.iter().filter(|s| s.kind == Kind::Tool).map(|s| s.label.as_str()).collect::<Vec<_>>(), ["cargo", "rg", "other", "echo"]);
        // A payload SQLite cannot parse fails the read, and the error does not carry its text.
        db.execute(&format!("UPDATE {P}turn_items SET payload_json=?1 WHERE turn_item_id='i0'"), [format!("{{\"input\":\"{SENTINEL}")]).unwrap();
        let error = read_from(&db, Path::new("synthetic"), &Target::Latest, at() + 20_000.).unwrap_err();
        assert!(!format!("{error} {error:?}").contains(SENTINEL), "{error:?}");
    }
    #[test]
    fn the_readers_sql_names_a_command_only_inside_t3_programs() {
        // The reader's own source, before its tests: every `$.input` is the argument of
        // `t3_programs`, so no query can return the command line.
        let source = include_str!("trace.rs");
        let reader = &source[..source.find("#[cfg(test)]").unwrap()];
        let allowed = "t3_programs(json_extract(payload_json,'$.input'))";
        let mentions = reader.matches("$.input").count();
        assert!(mentions >= 1);
        assert_eq!(reader.matches(allowed).count(), mentions, "a query names $.input outside t3_programs");
        let path_prefix = "$.";
        for field in ["output", "text", "fileName", "prompt", "result", "message", "questions", "summary"] {
            let path = format!("'{path_prefix}{field}'");
            assert!(!reader.contains(&path), "the reader names {path}");
        }
    }
    #[test]
    fn readonly_unchanged_and_transaction_errors() {
        let dir = std::env::temp_dir().join(format!("trace-readonly-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("synthetic.sqlite");
        single().execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let before = std::fs::read(&path).unwrap();
        let db = recap::open(&path).unwrap();
        assert!(db.execute(&format!("DELETE FROM {P}runs"), []).is_err());
        adds_up(&load(&path, &Target::Latest, at() + 20_000.).unwrap().unwrap());
        assert_eq!(before, std::fs::read(&path).unwrap());
        drop(db);
        let broken = Connection::open_in_memory().unwrap();
        assert_eq!(read_from(&broken, &path, &Target::Latest, at()).unwrap_err().kind, "schema");
        assert!(broken.is_autocommit());
        assert_eq!(load(&dir.join("absent"), &Target::Latest, at()).unwrap_err().kind, "missing");
        std::fs::write(dir.join("bad"), b"not a database").unwrap();
        assert_eq!(load(&dir.join("bad"), &Target::Latest, at()).unwrap_err().kind, "unreadable");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn one_definition_with_recap_for_each_run() {
        // Read two continuations together, then isolate each run in the same fixture
        // for Recap's aggregate API. This compares the actual per-run Trace pieces.
        for wanted in ["r", "r2"] {
            let db = database().unwrap();
            db.execute_batch(&format!("CREATE TABLE {P}provider_turns(thread_id TEXT,completed_at TEXT,payload_json TEXT);")).unwrap();
            for (n, id, base) in [(0, "r", at()), (1, "r2", at() + 20_000.)] {
                insert_run(&db, id, n + 1, base, Some(base + 10_000.), n > 0).unwrap();
                insert_item(&db, n as usize * 10, Some(id), "command_execution", "completed", base + 100., Some(base + 7000.), json!({})).unwrap();
                for (j, kind) in ["user_input_request", "approval_request"].iter().enumerate() {
                    let a = base + 3000. + j as f64 * 1000.;
                    let b = a + 2000.;
                    insert_item(&db, n as usize * 10 + 1 + j, Some(id), kind, "completed", a, Some(b), json!({})).unwrap();
                    db.execute(
                        &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,kind,status,created_at,resolved_at) VALUES(?1,'t','user_input','resolved',?2,?3)"),
                        params![format!("{id}{j}"), recap::iso(a), recap::iso(b)],
                    )
                    .unwrap();
                }
            }
            let t = read(&db, Target::Latest, at() + 40_000.);
            let i = t.runs.iter().position(|r| r.id == wanted).unwrap();
            let run = &t.runs[i];
            db.execute(&format!("DELETE FROM {P}runs WHERE run_id<>?1"), [wanted]).unwrap();
            db.execute(
                &format!("DELETE FROM {P}runtime_requests WHERE created_at<?1 OR created_at>=?2"),
                params![recap::iso(run.requested), recap::iso(run.completed.unwrap())],
            )
            .unwrap();
            let period = recap::Period::new(at() + 40_000., recap::Which::ThisWeek, &|_| None);
            let facts = recap::read_from(&db, Path::new("test"), &period).unwrap();
            let r = recap::summarize(&facts, &period, &|_| None);
            let intervals =
                |kind| t.spans.iter().filter(|s| s.kind == kind && s.run == Some(i)).map(|s| (s.start, s.end.unwrap())).collect::<Vec<_>>();
            let a = run.started.unwrap();
            let b = run.completed.unwrap();
            let waits = union(&intervals(Kind::Ask), a, b);
            let tools = outside(&intervals(Kind::Tool), &intervals(Kind::Ask), a, b);
            let model = union(&intervals(Kind::Model), a, b) + union(&intervals(Kind::Seen), a, b);
            assert!((r.split.waiting * 3_600_000. - waits).abs() < 0.001);
            assert!((r.split.tools * 3_600_000. - union(&tools, a, b)).abs() < 0.001);
            assert!((r.split.model * 3_600_000. - model).abs() < 0.001);
        }
    }
    #[test]
    fn deterministic_demo_and_seeded_tasks() {
        let a = demo(at()).unwrap();
        let b = demo(at()).unwrap();
        assert_eq!(a, b);
        adds_up(&a);
        for k in [Kind::Queue, Kind::Tool, Kind::Ask, Kind::Delegate, Kind::Seen, Kind::Model, Kind::Between] {
            assert!(a.spans.iter().any(|s| s.kind == k), "{k:?}");
        }
        for k in [MarkKind::Steer, MarkKind::StopAsked, MarkKind::Restart, MarkKind::Error] {
            assert!(a.marks.iter().any(|s| s.kind == k));
        }
        // Two long steps failed, and a few small ones; a question and a secret request; three
        // delegates; hundreds of steps, most under a second; a cargo test open at `now`.
        assert!(a.steps.failed >= 2);
        assert_eq!((a.steps.asks, a.steps.delegates), (2, 3));
        assert!(a.steps.tools > 400 && a.steps.under_a_second * 10 > a.steps.tools * 7, "{:?}", a.steps);
        assert!(a.spans.iter().any(|s| s.kind == Kind::Tool && s.end.is_none() && s.label == "cargo" && s.programs == ["mkdir", "cargo"]));
        assert_eq!((a.title.as_deref(), a.state.clone(), a.runs.len()), (Some("Fix the flaky checkout tests"), State::Running, 2));
        assert!(!serde_json::to_string(&a).unwrap().contains("never"));
        let db = single();
        let mut seed = 41u64;
        for i in 0..500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = (seed % 9900) as f64 + 100.;
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let b = (a + (seed % 3000) as f64).min(10_000.);
            insert_item(
                &db,
                i,
                Some("r"),
                ["command_execution", "secret_request", "reasoning"][i % 3],
                "completed",
                at() + a,
                Some(at() + b),
                json!({}),
            )
            .unwrap();
        }
        read(&db, Target::Latest, at() + 20_000.);
    }
    #[test]
    fn waiting_follows_the_rule_shared_with_the_live_asks() {
        // A run [0, 10 s); a question answered by message from 2 s, never answered; a link from
        // 8 s; the person writes at 30 s; a second run of the task [20 s, 25 s).
        let db = database().unwrap();
        insert_run(&db, "r", 1, at(), Some(at() + 10_000.), false).unwrap();
        insert_run(&db, "r2", 2, at() + 20_000., Some(at() + 25_000.), true).unwrap();
        insert_item(&db, 1, Some("r"), "user_input_request", "running", at() + 2000., None, json!({"requestId": "q1"})).unwrap();
        db.execute(&format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,payload_json) VALUES('q1','t','n','user_input','pending',?1,?2)"),
            params![recap::iso(at() + 2000.), json!({"responseCapability": {"type": "message"}}).to_string()]).unwrap();
        insert_item(&db, 2, Some("r"), "link_request", "waiting", at() + 8000., None, json!({})).unwrap();
        let human = |id: &str, at_ms: f64, payload: serde_json::Value| {
            db.execute(&format!("INSERT INTO {P}messages(message_id,thread_id,role,streaming,created_at,updated_at,payload_json) VALUES(?1,'t',?2,0,?3,?3,?4)"),
                params![id, if id.starts_with('a') { "assistant" } else { "user" }, recap::iso(at_ms), payload.to_string()]).unwrap();
        };
        // An agent's delivery, an idle compaction and the card's own instant do not answer it.
        human("u-agent", at() + 12_000., json!({"createdBy": "agent"}));
        human("u-compact", at() + 13_000., json!({"createdBy": "user", "idleCompaction": {"at": 1}}));
        human("u-equal", at() + 8000., json!({"createdBy": "user"}));
        human("u-real", at() + 30_000., json!({"createdBy": "user"}));
        let t = read(&db, Target::Run("r".into()), at() + 40_000.);
        let question = t.spans.iter().find(|s| s.label == "question").unwrap();
        assert!(question.by_message && question.end.is_none());
        // The link is open [8 s, 30 s) and waits where no run works: from the first run's end to
        // the second's start (20.1 s: the helper starts a run 100 ms after it is asked for), and
        // from the second's end to the person's message.
        let link = t.spans.iter().find(|s| s.label == "link").unwrap();
        assert_eq!(link.end, Some(at() + 30_000.));
        assert_eq!(t.waiting, [(at() + 10_000., at() + 20_100.), (at() + 25_000., at() + 30_000.)]);
        assert_eq!(t.split.waiting, 15_100.);
        // The open question answered by message keeps nothing waiting after the link is answered.
        assert_eq!(t.state, State::Completed);
        // A question whose capability is not recorded holds the run.
        db.execute(&format!("UPDATE {P}runtime_requests SET payload_json='{{}}'"), []).unwrap();
        let t = read(&db, Target::Run("r".into()), at() + 40_000.);
        assert!(!t.spans.iter().find(|s| s.label == "question").unwrap().by_message);
        assert!(t.waiting.iter().any(|(a, _)| *a == at() + 2000.));
        // A card without a node does not ask, as T3 counts it.
        db.execute(&format!("UPDATE {P}runtime_requests SET payload_json=?1"), [json!({"responseCapability": {"type": "message"}}).to_string()]).unwrap();
        db.execute(&format!("UPDATE {P}turn_items SET node_id=NULL WHERE type='link_request'"), []).unwrap();
        assert!(read(&db, Target::Run("r".into()), at() + 40_000.).waiting.is_empty());
    }
    // Cases from the open-asks thread's review of ef41ff8, copied as given: each was a
    // mismatch between the live asks and this history before the fix.
    #[test]
    fn live_review_link_suspends_across_task_boundary() {
        let db = single();
        insert_run(&db,"next",2,at()+20_000.,Some(at()+25_000.),false).unwrap();
        insert_item(&db,1,Some("r"),"link_request","waiting",at()+8_000.,None,json!({})).unwrap();
        let t=read(&db,Target::Run("r".into()),at()+30_000.);
        assert_eq!(t.waiting,[(at()+10_000.,at()+20_100.),(at()+25_000.,at()+30_000.)]);
    }
    #[test]
    fn live_review_link_missing_started_uses_update() {
        let db=single();
        insert_item(&db,1,Some("r"),"link_request","waiting",at()+8_000.,None,json!({"startedAt":null})).unwrap();
        let t=read(&db,Target::Run("r".into()),at()+30_000.);
        assert_eq!(t.waiting,[(at()+10_000.,at()+30_000.)]);
    }
    #[test]
    fn live_review_runless_link_on_thread_with_runs() {
        let db=single();
        insert_item(&db,1,None,"link_request","waiting",at()+8_000.,None,json!({})).unwrap();
        let t=read(&db,Target::Thread("t".into()),at()+30_000.);
        assert_eq!(t.waiting,[(at()+10_000.,at()+30_000.)]);
    }
    #[test]
    fn live_review_closed_nodeless_link_keeps_task_completed() {
        let db=single();
        insert_item(&db,1,Some("r"),"link_request","waiting",at()+8000.,None,json!({})).unwrap();
        db.execute(&format!("UPDATE {P}turn_items SET node_id=NULL"),[]).unwrap();
        let t=read(&db,Target::Run("r".into()),at()+30_000.);
        assert_eq!(t.ended,Some(at()+10_000.));
    }
    #[test]
    fn live_review_secret_on_live_run_sets_waiting_now() {
        let db=database().unwrap();
        insert_run(&db,"r",1,at(),None,false).unwrap();
        insert_item(&db,1,Some("r"),"secret_request","waiting",at()+2000.,None,json!({})).unwrap();
        let t=read(&db,Target::Run("r".into()),at()+4000.);
        assert_eq!(t.state,State::Waiting);
    }
    #[test]
    fn card_state_and_owning_run_must_allow_a_live_wait() {
        let db = single();
        insert_item(&db, 1, Some("r"), "link_request", "running", at() + 8000., None, json!({})).unwrap();
        let task = read(&db, Target::Run("r".into()), at() + 30_000.);
        assert_ne!(task.state, State::Waiting);
        assert_eq!(task.split.waiting, 0.);

        db.execute(&format!("UPDATE {P}runs SET completed_at=NULL, payload_json=json_set(payload_json,'$.status','queued')"), []).unwrap();
        db.execute(&format!("UPDATE {P}turn_items SET type='secret_request', status='waiting'"), []).unwrap();
        let task = read(&db, Target::Run("r".into()), at() + 30_000.);
        assert_ne!(task.state, State::Waiting);
        assert_eq!(task.split.waiting, 0.);
    }

    #[test]
    fn live_review_runtime_end_from_own_projection() {
        let db=single();
        insert_item(&db,1,Some("r"),"user_input_request","waiting",at()+2000.,None,json!({"requestId":"q"})).unwrap();
        db.execute(&format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,resolved_at,payload_json) VALUES('q','t','n','user_input','resolved',?1,?2,'{{}}')"),params![recap::iso(at()+2000.),recap::iso(at()+6000.)]).unwrap();
        let t=read(&db,Target::Run("r".into()),at()+30_000.);
        assert_eq!(t.waiting,[(at()+2000.,at()+6000.)]);
    }
    #[test]
    fn live_review_link_answered_exactly_now() {
        let db=single();
        insert_item(&db,1,Some("r"),"link_request","waiting",at()+8000.,None,json!({})).unwrap();
        db.execute(&format!("INSERT INTO {P}messages(message_id,thread_id,role,streaming,created_at,updated_at,payload_json) VALUES('u','t','user',0,?1,?1,'{{\"createdBy\":\"user\"}}')"),[recap::iso(at()+30_000.)]).unwrap();
        let t=read(&db,Target::Run("r".into()),at()+30_000.);
        assert_eq!(t.state,State::Completed);
    }
    // The asks thread's confirmation cases on dec5e91.
    #[test]
    fn confirm_runtime_only_with_node_is_placed() {
        let db = single();
        db.execute_batch(&format!("CREATE TABLE {P}nodes(node_id TEXT,run_id TEXT); INSERT INTO {P}nodes VALUES('n','r');")).unwrap();
        db.execute(
            &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,resolved_at,payload_json) VALUES('q','t','n','user_input','resolved',?1,?2,'{{}}')"),
            params![recap::iso(at() + 2000.), recap::iso(at() + 6000.)],
        )
        .unwrap();
        let t = read(&db, Target::Run("r".into()), at() + 30_000.);
        assert_eq!(t.waiting, [(at() + 2000., at() + 6000.)]);
        assert_eq!(t.unplaced_asks, 0);
    }
    #[test]
    fn confirm_finished_run_endpoint_starts_link_wait() {
        // The run finished at this instant: no run is open, so the pending link holds now.
        let db = single();
        insert_item(&db, 1, Some("r"), "link_request", "waiting", at() + 8000., None, json!({})).unwrap();
        let t = read(&db, Target::Run("r".into()), at() + 10_000.);
        assert_eq!(t.state, State::Waiting);
    }
    #[test]
    fn confirm_one_unplaced_card_counted_once_across_tasks() {
        let db = single();
        insert_run(&db, "next", 2, at() + 20_000., Some(at() + 25_000.), false).unwrap();
        insert_item(&db, 1, None, "link_request", "waiting", at() - 8000., None, json!({})).unwrap();
        let counts = ["r", "next"].map(|id| read(&db, Target::Run(id.into()), at() + 30_000.).unplaced_asks);
        assert_eq!(counts, [1, 0]);
    }
    #[test]
    fn confirm_recorded_resolution_ends_stale_status() {
        let db = single();
        insert_item(&db, 1, Some("r"), "user_input_request", "waiting", at() + 2000., None, json!({"requestId":"q"})).unwrap();
        db.execute(
            &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,resolved_at,payload_json) VALUES('q','t','n','user_input','pending',?1,?2,'{{}}')"),
            params![recap::iso(at() + 2000.), recap::iso(at() + 6000.)],
        )
        .unwrap();
        let t = read(&db, Target::Run("r".into()), at() + 30_000.);
        assert_eq!(t.waiting, [(at() + 2000., at() + 6000.)]);
    }
    #[test]
    fn older_pending_status_names_still_hold() {
        // The live reader's older names for pending ('open', 'waiting', 'requested') keep an
        // approval open while nothing records its resolution.
        for status in ["open", "waiting", "requested"] {
            let db = single();
            insert_item(&db, 1, Some("r"), "approval_request", "waiting", at() + 2000., None, json!({"requestId":"a"})).unwrap();
            db.execute(
                &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,payload_json) VALUES('a','t','n','command_approval',?1,?2,'{{}}')"),
                params![status, recap::iso(at() + 2000.)],
            )
            .unwrap();
            let t = read(&db, Target::Run("r".into()), at() + 30_000.);
            assert_eq!((t.state, t.waiting), (State::Waiting, vec![(at() + 2000., at() + 30_000.)]), "{status}");
        }
    }
    #[test]
    fn a_question_or_approval_without_its_request_holds_nobody() {
        // The live dashboard counts runtime requests, never items: an item with no request is
        // drawn, and adds no waiting, keeps nothing open and does not make the task wait.
        for kind in ["user_input_request", "approval_request"] {
            let db = single();
            insert_item(&db, 1, Some("r"), kind, "waiting", at() + 2000., None, json!({"requestId":"gone"})).unwrap();
            let t = read(&db, Target::Run("r".into()), at() + 30_000.);
            assert_eq!((t.state, t.waiting.clone(), t.ended), (State::Completed, vec![], Some(at() + 10_000.)), "{kind}");
            assert!(t.spans.iter().any(|s| s.kind == Kind::Ask && s.holds.is_empty()), "{kind}");
        }
    }
    #[test]
    fn older_pending_status_names_do_not_hold_a_question() {
        // The older names are kept for approvals only: a question is pending only as
        // `pending`, as the live reader's filter has it.
        for status in ["open", "waiting", "requested"] {
            let db = single();
            insert_item(&db, 1, Some("r"), "user_input_request", "waiting", at() + 2000., None, json!({"requestId":"q"})).unwrap();
            db.execute(
                &format!("INSERT INTO {P}runtime_requests(runtime_request_id,thread_id,node_id,kind,status,created_at,payload_json) VALUES('q','t','n','user_input',?1,?2,'{{}}')"),
                params![status, recap::iso(at() + 2000.)],
            )
            .unwrap();
            let t = read(&db, Target::Run("r".into()), at() + 30_000.);
            assert_eq!((t.state, t.waiting), (State::Completed, vec![]), "{status}");
        }
    }
    #[test]
    fn long_stretches_with_nothing_in_flight_are_quiet() {
        // Three runs of one task: a ten-hour gap with nothing in flight, then a gap of two hours
        // that a delegate covers, then a gap of twenty minutes.
        let db = database().unwrap();
        let hour = 3_600_000.;
        insert_run(&db, "r1", 1, at(), Some(at() + hour), false).unwrap();
        insert_run(&db, "r2", 2, at() + 11. * hour, Some(at() + 12. * hour), true).unwrap();
        insert_run(&db, "r3", 3, at() + 14. * hour, Some(at() + 15. * hour), true).unwrap();
        insert_run(&db, "r4", 4, at() + 15.33 * hour, Some(at() + 16. * hour), true).unwrap();
        db.execute(&format!("INSERT INTO {P}subagents VALUES('d','t',?1)"), [json!({"runId": "r2", "origin": "app_owned", "status": "completed",
            "startedAt": recap::iso(at() + 11.9 * hour), "completedAt": recap::iso(at() + 14.05 * hour), "childThreadId": "c"}).to_string()]).unwrap();
        let t = read(&db, Target::Run("r1".into()), at() + 20. * hour);
        assert_eq!(t.runs.len(), 4);
        // Only the ten hours, from the first run's end to the second's start, less nothing.
        assert_eq!(t.quiet.len(), 1, "{:?}", t.quiet);
        let (a, b) = t.quiet[0];
        assert!((a - (at() + hour)).abs() < 1. && (b - (at() + 11. * hour)).abs() < 1.);
        // Its time is still in the split, as no run open.
        assert!(t.split.between >= 10. * hour);
    }
    #[test]
    fn calls_closed_together_add_no_time() {
        // Three commands closed at the run's end, at one instant, minutes after they started.
        let db = single();
        for (i, start) in [(1, 100.), (2, 3000.), (3, 6000.)] {
            insert_item(&db, i, Some("r"), "command_execution", "completed", at() + start, Some(at() + 9000.), json!({"input": "cargo test"})).unwrap();
        }
        // A batch started together and closed together is left alone.
        for (i, start) in [(4, 200.), (5, 700.)] {
            insert_item(&db, i, Some("r"), "file_search", "completed", at() + start, Some(at() + 1500.), json!({})).unwrap();
        }
        let t = read(&db, Target::Latest, at() + 20_000.);
        let marked: Vec<bool> = t.spans.iter().filter(|s| s.kind == Kind::Tool).map(|s| s.closed_together).collect();
        assert_eq!(marked.iter().filter(|m| **m).count(), 3);
        assert_eq!((t.steps.tools, t.steps.closed_together), (5, 3));
        // Only the batch's 1.3 s are tools; the rest of the run is the model's.
        assert_eq!(t.split.tools, 1300.);
    }
    #[test]
    fn a_short_id_names_the_one_thread_it_ends() {
        let dir = std::env::temp_dir().join(format!("trace-ending-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("synthetic.sqlite");
        let _ = std::fs::remove_file(&path);
        let db = single();
        for id in ["thread:delegated-task:command%3Aabc12345", "thread:delegated-task:command%3Axyz12345", "thread:other"] {
            db.execute(&format!("INSERT INTO {P}threads VALUES(?1,'{{}}')"), [id]).unwrap();
        }
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        assert_eq!(threads_ending(&path, "abc12345").unwrap(), ["thread:delegated-task:command%3Aabc12345"]);
        assert_eq!(threads_ending(&path, "12345").unwrap().len(), 2);
        // A `%` in the ending is a character, not a pattern.
        assert!(threads_ending(&path, "%").unwrap().is_empty());
        assert!(threads_ending(&path, "").unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn keys_are_unique_and_stable() {
        let (a, b) = (demo(at()).unwrap(), demo(at() + 1000.).unwrap());
        let keys = |t: &Trace| t.spans.iter().map(|s| s.key.clone()).collect::<Vec<_>>();
        let mut unique = keys(&a);
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), a.spans.len());
        assert!(a.spans.iter().all(|s| !s.key.is_empty()));
        // A second read a second later keeps every key of a span that did not change.
        let later = keys(&b);
        assert!(a.spans.iter().filter(|s| matches!(s.kind, Kind::Tool | Kind::Ask | Kind::Delegate)).all(|s| later.contains(&s.key)));
    }
    #[test]
    fn a_run_that_never_started_is_queue_time_and_ends() {
        let db = database().unwrap();
        insert_run(&db, "r", 1, at(), Some(at() + 5000.), false).unwrap();
        db.execute(&format!("UPDATE {P}runs SET payload_json=json_remove(json_set(payload_json,'$.status','failed'),'$.startedAt')"), []).unwrap();
        let t = read(&db, Target::Latest, at() + 60_000.);
        assert_eq!((t.state.clone(), t.ended, t.split.queue, t.split.total), (State::Failed, Some(at() + 5000.), 5000., 5000.));
    }
    #[test]
    fn the_watch_reads_an_ended_task_again_only_after_a_commit() {
        use std::sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        };
        let dir = std::env::temp_dir().join(format!("t3-trace-watch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ended.sqlite");
        single().execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let reads = Arc::new(AtomicU32::new(0));
        let seen = reads.clone();
        let watch = crate::trace_live::watch(&path, Target::Run("r".into()), std::time::Duration::from_millis(20), move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let settle = || std::thread::sleep(std::time::Duration::from_millis(400));
        settle();
        assert_eq!(reads.load(Ordering::SeqCst), 1, "read once, then nothing while nothing is committed");
        let writer = Connection::open(&path).unwrap();
        writer.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        writer.execute(&format!("UPDATE {P}threads SET payload_json=json_set(payload_json,'$.title','Renamed')"), []).unwrap();
        settle();
        let after = reads.load(Ordering::SeqCst);
        assert!(after >= 2, "a commit is read");
        settle();
        assert_eq!(reads.load(Ordering::SeqCst), after, "and then nothing again");
        drop(watch);
        drop(writer);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
    #[test]
    fn cached_reads_match_fresh_reads_while_t3_rewrites_steps() {
        // The made-up task (about 600 steps, one still running), read through one cache as the
        // live watch does, against a fresh read of the same moment after each kind of write T3
        // makes: a step rewritten with a new time, a new step, a step gone.
        let now = at() + 112. * 60_000.;
        let db = database().unwrap();
        demo_fill(&db, now).unwrap();
        let mut cache = ReadCache::default();
        let same = |cache: &mut ReadCache, why: &str| {
            let cached = read_cached(&db, Path::new("t"), &Target::Latest, now, cache).unwrap().unwrap();
            let fresh = read_from(&db, Path::new("t"), &Target::Latest, now).unwrap().unwrap();
            assert_eq!(serde_json::to_value(&cached).unwrap(), serde_json::to_value(&fresh).unwrap(), "{why}");
        };
        same(&mut cache, "the first read, which parses everything");
        same(&mut cache, "the second, from the cache");
        assert!(cache.steps.len() > 500);
        let later = recap::iso(now - 1000.);
        db.execute(&format!("UPDATE {P}turn_items SET status='failed',updated_at=?1,payload_json=json_set(payload_json,'$.completedAt',?1) WHERE turn_item_id='i5'"), [&later]).unwrap();
        same(&mut cache, "an old step rewritten with a new time");
        insert_item(&db, 9_999, Some("r2"), "command_execution", "completed", now - 3000., Some(now - 2000.), json!({"input": "cargo test"})).unwrap();
        same(&mut cache, "a new step");
        db.execute(&format!("DELETE FROM {P}turn_items WHERE turn_item_id='i7'"), []).unwrap();
        same(&mut cache, "a step gone");
        assert!(!cache.steps.contains_key("i7"));
        // A status change is seen from the cheap pass, even with the time kept.
        db.execute(&format!("UPDATE {P}turn_items SET status='failed' WHERE turn_item_id='i6'"), []).unwrap();
        same(&mut cache, "a status change that kept its time");
        // A payload rewrite that kept both is not seen until the next full read, within
        // FULL_EVERY reads.
        db.execute(&format!("UPDATE {P}turn_items SET payload_json=json_set(payload_json,'$.completedAt',?1) WHERE turn_item_id='i4'"), [recap::iso(now - 60_000.)]).unwrap();
        let stale = read_cached(&db, Path::new("t"), &Target::Latest, now, &mut cache).unwrap().unwrap();
        let fresh = read_from(&db, Path::new("t"), &Target::Latest, now).unwrap().unwrap();
        assert_ne!(serde_json::to_value(&stale).unwrap(), serde_json::to_value(&fresh).unwrap(), "the cache was used");
        while cache.reads % FULL_EVERY != 0 {
            read_cached(&db, Path::new("t"), &Target::Latest, now, &mut cache).unwrap();
        }
        same(&mut cache, "a rewrite that kept its time, at the full read");
    }
    #[test]
    fn delegates_through_t3_indexed_columns_and_without_them() {
        // On T3's own schema: a task's delegates are found within its thread, never another
        // thread's; a delegate thread's own row by `child_thread_id`, and by the payload when
        // that column was never filled.
        for schema in [
            include_str!("../../../checks/fixtures/t3-schema-main-096.sql"),
            include_str!("../../../checks/fixtures/t3-schema-older-074.sql"),
        ] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(schema).unwrap();
            for thread in ["t", "c", "other"] {
                db.execute(&format!("INSERT INTO {P}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json) VALUES(?1,'p','A task','codex','full-access','default',?2,?2,?3)"),
                    params![thread, recap::iso(at()), json!({"title": "A task", "lineage": {"relationshipToParent": if thread == "c" { json!("subagent") } else { json!(null) }}}).to_string()]).unwrap();
            }
            db.execute(&format!("INSERT INTO {P}runs(run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json) VALUES('r','t',1,'codex','completed',?1,?2,?3)"),
                params![recap::iso(at()), recap::iso(at() + 10_000.), json!({"startedAt": recap::iso(at() + 100.), "status": "completed"}).to_string()]).unwrap();
            let delegate = |id: &str, thread: &str, child: Option<&str>| {
                let payload = json!({"runId": "r", "threadId": thread, "childThreadId": child, "origin": "provider_native", "status": "completed",
                    "startedAt": recap::iso(at() + 2000.), "completedAt": recap::iso(at() + 4000.), "model": "gpt-6"});
                db.execute(&format!("INSERT INTO {P}subagents(subagent_id,thread_id,run_id,parent_node_id,provider,child_thread_id,origin,status,started_at,completed_at,updated_at,payload_json) VALUES(?1,?2,'r','n','codex',?3,'provider_native','completed',?4,?5,?5,?6)"),
                    params![id, thread, child, recap::iso(at() + 2000.), recap::iso(at() + 4000.), payload.to_string()]).unwrap();
            };
            delegate("s", "t", Some("c"));
            // A row on another thread that claims the run: not this task's delegate.
            delegate("x", "other", None);
            let t = read(&db, Target::Run("r".into()), at() + 20_000.);
            assert_eq!(t.steps.delegates, 1);
            assert_eq!(t.spans.iter().filter(|s| s.kind == Kind::Delegate).map(|s| s.key.as_str()).collect::<Vec<_>>(), ["s"]);
            let child = read(&db, Target::Thread("c".into()), at() + 20_000.);
            assert!(child.delegate_thread && child.runs.len() == 1, "found by child_thread_id");
            db.execute(&format!("UPDATE {P}subagents SET child_thread_id=NULL"), []).unwrap();
            let child = read(&db, Target::Thread("c".into()), at() + 20_000.);
            assert!(child.delegate_thread && child.runs.len() == 1, "found by the payload when the column is empty");
        }
    }
    #[test]
    fn schema_fixtures() {
        for schema in [
            include_str!("../../../checks/fixtures/t3-schema-main-096.sql"),
            include_str!("../../../checks/fixtures/t3-schema-older-074.sql"),
            include_str!("../../../checks/fixtures/t3-schema-replayed-083.sql"),
        ] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(schema).unwrap();
            // Real DDL requires additional non-null metadata on insertion, never on selection.
            db.execute(&format!("INSERT INTO {P}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json) VALUES('t','p','A task','codex','full-access','default',?1,?1,?2)"),params![recap::iso(at()),json!({"title":"A task"}).to_string()]).unwrap();
            db.execute(&format!("INSERT INTO {P}runs(run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json) VALUES('r','t',1,'codex','completed',?1,?2,?3)"),params![recap::iso(at()),recap::iso(at()+10_000.),json!({"startedAt":recap::iso(at()+100.),"status":"completed"}).to_string()]).unwrap();
            insert_item(
                &db,
                1,
                Some("r"),
                "command_execution",
                "completed",
                at() + 1000.,
                Some(at() + 2000.),
                json!({"input":"/usr/bin/git status"}),
            )
            .unwrap();
            db.execute(&format!("INSERT INTO {P}subagents(subagent_id,thread_id,parent_node_id,provider,origin,status,started_at,completed_at,updated_at,payload_json) VALUES('s','t','n','codex','provider_native','completed',?1,?2,?2,?3)"),params![recap::iso(at()+3000.),recap::iso(at()+4000.),json!({"runId":"r","origin":"provider_native","status":"completed","model":"gpt-6"}).to_string()]).unwrap();
            // run_attempts is newer than these fixtures: T3 writes it with these columns, the
            // reason inside the payload.
            db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {P}run_attempts(attempt_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,attempt_ordinal INTEGER,root_node_id TEXT,provider TEXT,provider_thread_id TEXT,provider_turn_id TEXT,status TEXT,payload_json TEXT,provider_instance_id TEXT)")).unwrap();
            db.execute(&format!("INSERT INTO {P}run_attempts(attempt_id,thread_id,run_id,attempt_ordinal,status,payload_json) VALUES('a','t','r',2,'completed',?1)"),
                [json!({"reason": "steering_restart", "startedAt": recap::iso(at() + 5000.)}).to_string()]).unwrap();
            db.execute(&format!("INSERT INTO {P}run_attempts(attempt_id,thread_id,run_id,attempt_ordinal,status,payload_json) VALUES('b','t','r',1,'completed','{{}}')"), []).unwrap();
            let t = read(&db, Target::Latest, at() + 20_000.);
            assert_eq!(t.marks.iter().filter(|m| m.kind == MarkKind::Restart).count(), 1);
            let t = read(&db, Target::Latest, at() + 20_000.);
            assert_eq!(t.split.tools, 2000.);
            assert_eq!(t.steps.delegates, 1);

            let card_at = recap::iso(at() + 8000.);
            db.execute(&format!("INSERT INTO {P}turn_items
                (turn_item_id,thread_id,run_id,node_id,ordinal,type,status,updated_at,payload_json)
                VALUES('link','t','r','n',2,'link_request','waiting',?1,'{{}}')"), [&card_at]).unwrap();
            // The same instant and an agent's delivery leave the link open.
            db.execute(&format!("INSERT INTO {P}messages
                (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
                VALUES('equal','t','user',0,?1,?1,'{{\"createdBy\":\"user\"}}'),
                      ('auto','t','user',0,?1,?2,'{{\"createdBy\":\"agent\"}}')"),
                params![card_at, recap::iso(at() + 12_000.)]).unwrap();
            let read_link = || read(&db, Target::Run("r".into()), at() + 20_000.);
            let open = read_link();
            assert_eq!(open.spans.iter().find(|s| s.label == "link").unwrap().end, None);
            assert_eq!(open.waiting, [(at() + 10_000., at() + 20_000.)]);

            db.execute(&format!("INSERT INTO {P}messages
                (message_id,thread_id,role,streaming,created_at,updated_at,payload_json)
                VALUES('human','t','user',0,?1,?2,'{{\"createdBy\":\"user\"}}')"),
                params![card_at, recap::iso(at() + 14_000.)]).unwrap();
            let answered = read_link();
            assert_eq!(answered.spans.iter().find(|s| s.label == "link").unwrap().end, Some(at() + 14_000.));
            assert_eq!(answered.waiting, [(at() + 10_000., at() + 14_000.)]);

            db.execute_batch(&format!("DROP TABLE {P}messages")).unwrap();
            let missing = read_link();
            assert_eq!(missing.spans.iter().find(|s| s.label == "link").unwrap().end, None);
            assert_eq!(missing.waiting, open.waiting);
        }
    }
    #[test]
    #[ignore = "synthetic load measurement; run explicitly in release"]
    fn measure_load() {
        let db = single();
        const ITEMS: usize = 1024;
        for i in 0..ITEMS {
            insert_item(
                &db,
                i,
                Some("r"),
                "command_execution",
                "completed",
                at() + 100. + i as f64,
                Some(at() + 2000. + i as f64),
                json!({"input":"cargo test never read","output":"never read".repeat(512)}),
            )
            .unwrap();
        }
        let dir = std::env::temp_dir().join(format!("trace-bench-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("synthetic.sqlite");
        db.execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let bytes: i64 = db.query_row(&format!("SELECT sum(length(payload_json)) FROM {P}turn_items"), [], |r| r.get(0)).unwrap();
        load(&path, &Target::Latest, at() + 20_000.).unwrap();
        let mut trials = vec![];
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let t = load(&path, &Target::Latest, at() + 20_000.).unwrap().unwrap();
            trials.push(start.elapsed().as_secs_f64() * 1000.);
            assert_eq!(t.steps.tools, ITEMS as u32);
            adds_up(&t);
        }
        println!("{ITEMS} items, {bytes} payload bytes; load ms in trial order: {trials:?}");
        trials.sort_by(f64::total_cmp);
        println!("median {} ms, range {}..{} ms", trials[2], trials[0], trials[4]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
