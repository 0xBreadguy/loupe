//! Read-only T3 orchestration projection adapter.
//!
//! This intentionally mirrors oracle/src/source.ts's SQL projection boundary:
//! no authentication tables, prompts, message bodies, results, tool output or
//! reasoning bodies are selected into Rust. The only command text is the same
//! bounded command-execution excerpt the terminal dashboard displays.
use crate::model::{self, Agent, Context, Event, Item, Point, Shelf, Snapshot, Step};
use rusqlite::{
    Connection, OpenFlags, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const PREFIX: &str = "orchestration_v2_projection_";

/// Scale-lab phase timing, off unless `enable`d (`--bench-source` or opt-in reader-poll profiling does).
/// When off, each query pays one relaxed atomic load and no allocation; query
/// text and results never change.
pub mod timing {
    use serde::Serialize;
    use std::{
        collections::HashMap,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicBool, Ordering},
        },
        time::Instant,
    };
    #[derive(Clone, Copy, Debug, Default, Serialize)]
    pub struct Query {
        pub prepare_ns: u64,
        pub step_ns: u64,
        pub rows: u64,
        pub calls: u64,
    }
    #[derive(Clone, Debug, Default, Serialize)]
    pub struct Data {
        pub queries: HashMap<String, Query>,
        pub json_ns: u64,
        pub build_ns: u64,
        pub decorate_ns: u64,
        /// Inclusive named scopes; nested scopes must not be summed.
        pub phases_ns: HashMap<String, u64>,
    }
    static ON: AtomicBool = AtomicBool::new(false);
    static DATA: OnceLock<Mutex<Data>> = OnceLock::new();
    pub fn enable(on: bool) {
        DATA.get_or_init(|| Mutex::new(Data::default()));
        ON.store(on, Ordering::Relaxed);
    }
    pub fn on() -> bool {
        ON.load(Ordering::Relaxed)
    }
    pub fn reset() {
        if let Some(d) = DATA.get() {
            *d.lock().unwrap() = Data::default();
        }
    }
    pub fn take() -> Data {
        DATA.get()
            .map(|d| d.lock().unwrap().clone())
            .unwrap_or_default()
    }
    /// A disabled scope does not read the clock or allocate.
    pub struct Phase(Option<(&'static str, Instant)>);
    pub fn phase(name: &'static str) -> Phase {
        Phase(on().then(|| (name, Instant::now())))
    }
    impl Drop for Phase {
        fn drop(&mut self) {
            if let Some((name, start)) = self.0 {
                let elapsed = start.elapsed().as_nanos() as u64;
                if let Some(d) = DATA.get() {
                    *d.lock().unwrap().phases_ns.entry(name.into()).or_default() += elapsed;
                }
            }
        }
    }
    /// Short per-query label; the same rules exist in checks/bench/source-baseline.ts.
    fn label(sql: &str) -> String {
        let compact: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        for (needle, name) in [
            ("sqlite_master", "tables"),
            ("pragma_table_info", "table_info"),
            ("projection_threads ", "threads"),
            ("provider_turns", "provider_turns"), // refined below
            ("projection_runs", "runs"),          // refined below
            ("projection_subagents", "subagents"),
            ("projection_provider_threads", "provider_threads"),
            ("projection_nodes", "nodes"),
            ("projection_projects", "projects"),
            ("runtime_requests", "requests"),
            ("turn_items", "turn_items"), // refined below
        ] {
            if compact.contains(needle) {
                return match name {
                    "provider_turns" if compact.contains("rank=1") => "latest_turns",
                    "provider_turns" => "history",
                    "runs" if compact.contains("completed_at IS NULL OR") => "spans",
                    "turn_items" if compact.contains("LIMIT 3000") => "items",
                    "turn_items" if compact.contains("LIMIT 8000") => "events",
                    "turn_items" => "plans",
                    other => other,
                }
                .into();
            }
        }
        compact.chars().take(80).collect()
    }
    pub fn query(sql: &str, prepare: Instant, stepped: Instant, done: Instant, rows: usize) {
        if let Some(d) = DATA.get() {
            let mut d = d.lock().unwrap();
            let q = d.queries.entry(label(sql)).or_default();
            q.prepare_ns += (stepped - prepare).as_nanos() as u64;
            q.step_ns += (done - stepped).as_nanos() as u64;
            q.rows += rows as u64;
            q.calls += 1;
        }
    }
    fn add(field: impl Fn(&mut Data) -> &mut u64, start: Instant) {
        if let Some(d) = DATA.get() {
            let mut d = d.lock().unwrap();
            *field(&mut d) += start.elapsed().as_nanos() as u64;
        }
    }
    pub fn add_json(start: Instant) {
        add(|d| &mut d.json_ns, start)
    }
    pub fn add_build(start: Instant) {
        add(|d| &mut d.build_ns, start)
    }
    pub fn add_decorate(start: Instant) {
        add(|d| &mut d.decorate_ns, start)
    }
}

const TABLES: [&str; 8] = [
    "threads",
    "runs",
    "subagents",
    "nodes",
    "provider_threads",
    "provider_turns",
    "turn_items",
    "runtime_requests",
];
pub(crate) const WINDOW: f64 = 10_800_000.;
type Json = Value;

#[derive(Clone, Debug)]
pub struct SourceError {
    pub kind: &'static str,
    pub path: PathBuf,
    pub detail: String,
}
impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            "missing" => write!(
                f,
                "No T3 database at {}. Is T3 Code installed and opened once?",
                self.path.display()
            ),
            "unreadable" => write!(
                f,
                "Can't read the T3 database at {}: {}",
                self.path.display(),
                self.detail
            ),
            _ => write!(f, "Unsupported T3 schema; {}", self.detail),
        }
    }
}
impl std::error::Error for SourceError {}
fn problem(path: &Path, kind: &'static str, detail: impl Into<String>) -> SourceError {
    SourceError {
        kind,
        path: path.to_owned(),
        detail: detail.into(),
    }
}
fn db_error(path: &Path, error: rusqlite::Error) -> SourceError {
    let not_db = matches!(&error,rusqlite::Error::SqliteFailure(e,_) if e.extended_code==26);
    problem(
        path,
        if not_db { "unreadable" } else { "schema" },
        if not_db {
            "not a SQLite database".into()
        } else {
            error.to_string()
        },
    )
}
/// An explicit path is required; this function never discovers or opens a real DB by itself.
pub fn snapshot(path: &Path, now: f64) -> Result<Snapshot, SourceError> {
    if !now.is_finite() {
        return Err(problem(path, "schema", "invalid snapshot clock"));
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        if !path.exists() {
            problem(path, "missing", "")
        } else if path.is_dir() {
            problem(path, "unreadable", "it is a folder, not a file")
        } else {
            problem(path, "unreadable", error.to_string())
        }
    })?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|e| db_error(path, e))?;
    db.execute_batch("PRAGMA query_only=ON; BEGIN")
        .map_err(|e| db_error(path, e))?;
    let result = read(&db, path, now);
    // End the read transaction even when schema/JSON checks failed. No commit or migration exists.
    let rollback = db.execute_batch("ROLLBACK").map_err(|e| db_error(path, e));
    match result {
        Ok(value) => {
            rollback?;
            Ok(value)
        }
        Err(error) => {
            let _ = rollback;
            Err(error)
        }
    }
}
pub(crate) fn all(db: &Connection, sql: &str, args: &[SqlValue]) -> rusqlite::Result<Vec<Json>> {
    let timed = timing::on().then(Instant::now);
    let mut stmt = db.prepare_cached(sql)?;
    let stepped = timed.map(|_| Instant::now());
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt.query_map(params_from_iter(args.iter()), |row| {
        let mut value = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let item = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) => json!(v),
                ValueRef::Text(v) => Value::String(String::from_utf8_lossy(v).into_owned()),
                ValueRef::Blob(_) => Value::Null,
            };
            value.insert(name.clone(), item);
        }
        Ok(Value::Object(value))
    })?;
    let out: Vec<Json> = rows.collect::<Result<_, _>>()?;
    if let (Some(prepare), Some(stepped)) = (timed, stepped) {
        timing::query(sql, prepare, stepped, Instant::now(), out.len());
    }
    Ok(out)
}
/// Static, narrow field projection. Full payload_json is never returned by a query.
pub(crate) fn projection(table: &str) -> String {
    let (flat, nested): (&[&str], &[(&str, &[&str])]) = match table {
        "threads" => (
            &[
                "id",
                "projectId",
                "title",
                "activeProviderThreadId",
                "worktreePath",
                "branch",
                "updatedAt",
                "settledOverride",
                "snoozedUntil",
            ],
            &[
                ("delegatedFrom", &["environmentId", "threadId", "title"]),
                (
                    "lineage",
                    &["relationshipToParent", "parentThreadId", "sideChat"],
                ),
                ("modelSelection", &["model", "options"]),
            ],
        ),
        "runs" => (
            &[
                "id",
                "threadId",
                "status",
                "providerThreadId",
                "startedAt",
                "requestedAt",
                "completedAt",
            ],
            &[("modelSelection", &["model", "options"])],
        ),
        "subagents" => (
            &[
                "id",
                "threadId",
                "parentNodeId",
                "runId",
                "providerThreadId",
                "childThreadId",
                "status",
                "title",
                "model",
                "startedAt",
                "completedAt",
                "updatedAt",
            ],
            &[("remoteChild", &["environmentId", "threadId", "label"])],
        ),
        "provider_threads" => (&["id", "contextUsage"], &[]),
        "provider_turns" => (
            &["providerThreadId", "model", "tokenUsage", "turnTokenUsage"],
            &[],
        ),
        _ => unreachable!("only static projection tables"),
    };
    let mut parts: Vec<String> = flat
        .iter()
        .map(|k| format!("'{k}', json_extract(payload_json,'$.{k}')"))
        .collect();
    for (key, fields) in nested {
        let inner = fields
            .iter()
            .map(|field| format!("'{field}', json_extract(payload_json,'$.{key}.{field}')"))
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("'{key}', CASE WHEN json_type(payload_json,'$.{key}')='object' THEN json_object({inner}) END"));
    }
    format!("json_object({})", parts.join(","))
}
fn payloads(
    db: &Connection,
    table: &str,
    clause: &str,
    args: &[SqlValue],
) -> rusqlite::Result<Vec<Json>> {
    let rows = all(
        db,
        &format!(
            "SELECT {} AS payload FROM {PREFIX}{table} {clause}",
            projection(table)
        ),
        args,
    )?;
    let timed = timing::on().then(Instant::now);
    let out = rows
        .into_iter()
        .map(|r| {
            serde_json::from_str(str_at(&r, "payload")).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .collect::<rusqlite::Result<Vec<Json>>>()?;
    if let Some(t) = timed {
        timing::add_json(t);
    }
    Ok(out)
}
pub(crate) fn is_toolish(kind: &str) -> bool {
    TOOLISH.split(',').any(|t| t.trim_matches('\'') == kind)
}
pub(crate) fn is_event_kind(kind: &str) -> bool {
    !matches!(kind, "checkpoint" | "todo_list")
}
pub(crate) fn str_at<'a>(row: &'a Json, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn optional(row: &Json, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(Into::into)
}
pub(crate) fn truthy_str(row: &Json, key: &str) -> Option<String> {
    optional(row, key).filter(|s| !s.is_empty())
}
pub(crate) fn obj(row: &Json, key: &str) -> Json {
    row.get(key)
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
}
pub(crate) fn number(row: &Json, key: &str) -> f64 {
    match row.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                0.
            } else {
                s.trim().parse().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Bool(b)) => {
            if *b {
                1.
            } else {
                0.
            }
        }
        None | Some(Value::Null) => 0.,
        _ => f64::NAN,
    }
}
fn marks(n: usize) -> String {
    if n == 0 {
        "NULL".into()
    } else {
        vec!["?"; n].join(",")
    }
}
fn strings(values: impl IntoIterator<Item = String>) -> Vec<SqlValue> {
    values.into_iter().map(SqlValue::Text).collect()
}
pub(crate) fn iso(stamp: f64) -> String {
    let millis = stamp.floor() as i64;
    let seconds = millis.div_euclid(1000);
    let days = seconds.div_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let dayseconds = seconds.rem_euclid(86400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        dayseconds / 3600,
        (dayseconds % 3600) / 60,
        dayseconds % 60,
        millis.rem_euclid(1000)
    )
}
pub(crate) const ITEM_FIELDS:&str="thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,
 json_extract(payload_json,'$.startedAt') AS started_at,
 json_extract(payload_json,'$.toolName') AS tool,
 json_extract(payload_json,'$.exitCode') AS exit_code,
 substr(COALESCE(json_extract(payload_json,'$.title'),json_extract(payload_json,'$.toolName'),json_extract(payload_json,'$.label'),''),1,240) AS label,
 CASE WHEN type='command_execution' THEN substr(json_extract(payload_json,'$.input'),1,240) ELSE '' END AS command,
 CASE WHEN type='file_change' THEN substr(json_extract(payload_json,'$.fileName'),1,240) ELSE '' END AS file";
pub(crate) const TOOLISH: &str = "'command_execution','dynamic_tool','file_change','web_search','subagent','handoff','notification','user_input_request','secret_request','link_request','reasoning','assistant_message'";
pub(crate) fn read(db: &Connection, path: &Path, now: f64) -> Result<Snapshot, SourceError> {
    read_tracked(db, path, now, &mut ReadReport::default())
}
#[derive(Default)]
pub(crate) struct ReadReport {
    pub rows: usize,
    pub threads: usize,
    pub valid_until: f64,
}
pub(crate) fn read_tracked(
    db: &Connection,
    path: &Path,
    now: f64,
    report: &mut ReadReport,
) -> Result<Snapshot, SourceError> {
    let rows = std::cell::Cell::new(0usize);
    report.valid_until = f64::INFINITY;
    let query = |sql: &str, args: &[SqlValue]| {
        let result = all(db, sql, args).map_err(|e| db_error(path, e))?;
        rows.set(rows.get() + result.len());
        Ok::<_, SourceError>(result)
    };
    let existing: HashSet<String> =
        query("SELECT name FROM sqlite_master WHERE type='table'", &[])?
            .iter()
            .map(|r| str_at(r, "name").into())
            .collect();
    let mut missing: Vec<_> = TABLES
        .iter()
        .map(|t| format!("{PREFIX}{t}"))
        .filter(|t| !existing.contains(t))
        .collect();
    if !missing.is_empty() {
        missing.sort();
        return Err(problem(
            path,
            "schema",
            format!("missing {}", missing.join(", ")),
        ));
    }
    let load = |table: &str, clause: &str, args: &[SqlValue]| {
        let result = payloads(db, table, clause, args).map_err(|e| db_error(path, e))?;
        rows.set(rows.get() + result.len());
        Ok::<_, SourceError>(result)
    };
    let threads = load(
        "threads",
        "WHERE deleted_at IS NULL AND archived_at IS NULL",
        &[],
    )?;
    let ids: Vec<String> = threads.iter().map(|r| str_at(r, "id").into()).collect();
    report.threads = ids.len();
    report.rows = rows.get();
    if ids.is_empty() {
        return Ok(Snapshot {
            agents: Vec::new(),
            at: now,
            path: path.to_string_lossy().into(),
        });
    }
    let params = strings(ids.clone());
    let where_ids = format!("WHERE thread_id IN ({})", marks(ids.len()));
    let mut latest = HashMap::new();
    let mut latest_order = Vec::new();
    for run in load(
        "runs",
        &format!("{where_ids} ORDER BY ordinal DESC"),
        &params,
    )? {
        let id = str_at(&run, "threadId").to_string();
        if !latest.contains_key(&id) {
            latest_order.push(id.clone());
            latest.insert(id, run);
        }
    }
    let subs = load("subagents", &where_ids, &params)?;
    let providers: HashMap<String, Json> = load("provider_threads", &where_ids, &params)?
        .into_iter()
        .map(|p| (str_at(&p, "id").into(), p))
        .collect();
    let mut turns = HashMap::new();
    {
        let rows = query(
            &format!(
                "SELECT {} AS payload_json FROM (SELECT payload_json,ROW_NUMBER() OVER(PARTITION BY provider_thread_id ORDER BY ordinal DESC,rowid DESC) AS rank FROM {PREFIX}provider_turns {where_ids}) WHERE rank=1",
                projection("provider_turns")
            ),
            &params,
        )?;
        let timed = timing::on().then(Instant::now);
        for row in rows {
            let p: Json = serde_json::from_str(str_at(&row, "payload_json"))
                .map_err(|e| problem(path, "schema", e.to_string()))?;
            turns.insert(str_at(&p, "providerThreadId").to_string(), p);
        }
        if let Some(t) = timed {
            timing::add_json(t);
        }
    }
    let read_nodes = timing::phase("read_nodes");
    // Anything unproven, and any error, reads as the oracle does, with its errors.
    let nodes = match nodes_from_indexes(db, &where_ids, &params) {
        Ok(Some((nodes, selected))) => {
            rows.set(rows.get() + selected);
            nodes
        }
        _ => query(
            &format!("SELECT node_id,parent_node_id,run_id FROM {PREFIX}nodes {where_ids}"),
            &params,
        )?
        .into_iter()
        .map(|r| (str_at(&r, "node_id").into(), r))
        .collect(),
    };
    drop(read_nodes);
    let projects: HashMap<String, Json> = query(
        "SELECT project_id,title,workspace_root FROM projection_projects",
        &[],
    )?
    .into_iter()
    .map(|r| (str_at(&r, "project_id").into(), r))
    .collect();
    let asked = query(
        &format!("SELECT name FROM pragma_table_info('{PREFIX}runtime_requests')"),
        &[],
    )?
    .iter()
    .any(|r| str_at(r, "name") == "created_at");
    let requests = query(
        &format!(
            "SELECT thread_id,node_id,kind,status,{} FROM {PREFIX}runtime_requests {where_ids} AND resolved_at IS NULL AND status IN ('pending','open','waiting','requested')",
            if asked {
                "created_at"
            } else {
                "NULL AS created_at"
            }
        ),
        &params,
    )?;
    let mut seen = HashSet::new();
    let mut run_ids = Vec::new();
    for id in &latest_order {
        if let Some(run) = latest.get(id) {
            if let Some(id) = truthy_str(run, "id") {
                if seen.insert(id.clone()) {
                    run_ids.push(id);
                }
            }
        }
    }
    for sub in &subs {
        if let Some(id) = truthy_str(sub, "runId") {
            if seen.insert(id.clone()) {
                run_ids.push(id);
            }
        }
    }
    let run_marks = marks(run_ids.len());
    let runless: Vec<String> = ids
        .iter()
        .filter(|id| !latest.contains_key(*id))
        .cloned()
        .collect();
    let mut item_params = strings(run_ids.clone());
    item_params.extend(strings(runless.clone()));
    let read_items = timing::phase("read_items");
    let items = query(
        &format!(
            "SELECT {ITEM_FIELDS} FROM {PREFIX}turn_items WHERE (run_id IN ({run_marks}) OR (run_id IS NULL AND thread_id IN ({}))) AND type IN ({TOOLISH}) ORDER BY updated_at DESC,ordinal DESC LIMIT 3000",
            marks(runless.len())
        ),
        &item_params,
    )?;
    drop(read_items);
    let since = iso(now - WINDOW);
    let mut since_params = params.clone();
    since_params.push(SqlValue::Text(since));
    let read_events = timing::phase("read_events");
    let events = query(
        &format!(
            "SELECT {ITEM_FIELDS} FROM {PREFIX}turn_items {where_ids} AND updated_at>=? AND type NOT IN ('checkpoint','todo_list') ORDER BY updated_at DESC LIMIT 8000"
        ),
        &since_params,
    )?;
    drop(read_events);
    let history = query(
        &format!(
            "SELECT provider_thread_id,thread_id,started_at,completed_at,json_extract(payload_json,'$.tokenUsage.usedTokens') AS used,json_extract(payload_json,'$.tokenUsage.maxTokens') AS max,json_extract(payload_json,'$.model') AS model FROM (SELECT *,ROW_NUMBER() OVER(PARTITION BY provider_thread_id ORDER BY ordinal DESC) AS rank FROM {PREFIX}provider_turns {where_ids}) WHERE rank<=160"
        ),
        &params,
    )?;
    let spans = query(
        &format!(
            "SELECT thread_id,requested_at,completed_at,json_extract(payload_json,'$.startedAt') AS started_at FROM {PREFIX}runs {where_ids} AND (completed_at IS NULL OR completed_at>=?)"
        ),
        &since_params,
    )?;
    report.valid_until = deadline(&events, &spans, now);
    let read_plans = timing::phase("read_plans");
    let plans_sql = format!(
        "SELECT thread_id,run_id,node_id,provider_thread_id,updated_at,json_extract(payload_json,'$.steps') AS steps FROM {PREFIX}turn_items WHERE type='todo_list' AND run_id IN ({run_marks}) ORDER BY updated_at DESC"
    );
    let run_params = strings(run_ids);
    let plans = match todo_lists(db, &plans_sql, &run_marks, &run_params) {
        Ok(Some(found)) => {
            rows.set(rows.get() + found.len());
            found
        }
        _ => query(&plans_sql, &run_params)?,
    };
    drop(read_plans);
    report.rows = rows.get();
    assemble(
        threads, latest, subs, providers, turns, nodes, projects, requests, items, events, history,
        spans, plans, path, now,
    )
}

/// One SQL value as the oracle's row maps hold it.
fn value_json(value: ValueRef<'_>) -> Json {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => json!(v),
        ValueRef::Real(v) => json!(v),
        ValueRef::Text(v) => Value::String(String::from_utf8_lossy(v).into_owned()),
        ValueRef::Blob(_) => Value::Null,
    }
}
/// Uncached statement for SQL whose text changes with its arity, timed under
/// the same labels as [`all`].
fn each_row(
    db: &Connection,
    sql: &str,
    args: &[SqlValue],
    mut f: impl FnMut(&[String], &rusqlite::Row<'_>) -> rusqlite::Result<()>,
) -> rusqlite::Result<usize> {
    let timed = timing::on().then(Instant::now);
    let mut stmt = db.prepare(sql)?;
    let stepped = timed.map(|_| Instant::now());
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(params_from_iter(args.iter()))?;
    let mut n = 0;
    while let Some(row) = rows.next()? {
        f(&names, row)?;
        n += 1;
    }
    if let (Some(prepare), Some(stepped)) = (timed, stepped) {
        timing::query(sql, prepare, stepped, Instant::now(), n);
    }
    Ok(n)
}
/// `EXPLAIN QUERY PLAN` as a tree: each step's detail and its parent's position.
fn plan(
    db: &Connection,
    sql: &str,
    args: &[SqlValue],
) -> rusqlite::Result<Vec<(Option<usize>, String)>> {
    let mut ids = Vec::new();
    let mut steps = Vec::new();
    each_row(db, &format!("EXPLAIN QUERY PLAN {sql}"), args, |_, row| {
        let id: i64 = row.get(0)?;
        let parent: i64 = row.get(1)?;
        let detail: String = row.get(3)?;
        steps.push((ids.iter().rposition(|i| *i == parent), detail));
        ids.push(id);
        Ok(())
    })?;
    Ok(steps)
}
/// T3's partial index over the turn items that are not tool calls. Its key is
/// `(thread_id, ordinal, type, run_id)`; the WHERE below is its definition,
/// which a query has to repeat for SQLite to use it.
const CONVERSATION: &str = "type NOT IN ( 'command_execution', 'reasoning', 'file_change', 'file_search', 'web_search', 'dynamic_tool' )";
/// The oracle's todo-list query (`oracle`), with its candidates found first.
///
/// The oracle visits every item of the selected runs through the run index
/// and looks each one up in the table to test its type. The candidates (todo
/// lists of those runs) are found instead from T3's conversation index alone,
/// in any order: that query's WHERE is the oracle's plus a term every todo
/// list satisfies, so its set is the oracle's set whatever the plan. Then the
/// oracle's own query runs with `+rowid IN (candidates)` added, which SQLite
/// tests on the run index entry before the table lookup. It selects the same
/// rows and columns and evaluates `json_extract` on the same rows as the
/// oracle, and it is used only when SQLite plans it exactly as it plans the
/// oracle, so its sorter receives the same rows in the same order and rows
/// tied on `updated_at` come out as the oracle's do. `None` (no usable index,
/// any other plan, a shadowed row id) means the caller runs the oracle.
fn todo_lists(
    db: &Connection,
    oracle: &str,
    run_marks: &str,
    args: &[SqlValue],
) -> rusqlite::Result<Option<Vec<Json>>> {
    let find = format!(
        "SELECT rowid FROM {PREFIX}turn_items INDEXED BY {PREFIX}turn_items_conversation_idx WHERE type='todo_list' AND run_id IN ({run_marks}) AND {CONVERSATION}"
    );
    // A missing or redefined index fails to prepare under INDEXED BY.
    let covering =
        format!("SCAN {PREFIX}turn_items USING COVERING INDEX {PREFIX}turn_items_conversation_idx");
    let steps = plan(db, &find, args)?;
    if steps.len() != 1 || steps[0].1 != covering {
        return Ok(None);
    }
    let mut ids = Vec::new();
    let mut usable = true;
    each_row(db, &find, args, |_, row| {
        match row.get_ref(0)? {
            ValueRef::Integer(id) => ids.push(SqlValue::Integer(id)),
            // A column named rowid would shadow the row id.
            _ => usable = false,
        }
        Ok(())
    })?;
    if !usable {
        return Ok(None);
    }
    if ids.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let narrowed = oracle.replace(
        " ORDER BY updated_at DESC",
        &format!(
            " AND +rowid IN ({}) ORDER BY updated_at DESC",
            marks(ids.len())
        ),
    );
    let mut narrowed_args = args.to_vec();
    narrowed_args.extend(ids);
    if plan(db, oracle, args)? != plan(db, &narrowed, &narrowed_args)? {
        return Ok(None);
    }
    let mut out = Vec::new();
    each_row(
        db,
        &format!("{narrowed} -- plans"),
        &narrowed_args,
        |names, row| {
            let mut value = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                value.insert(name.clone(), value_json(row.get_ref(i)?));
            }
            out.push(Value::Object(value));
            Ok(())
        },
    )?;
    Ok(Some(out))
}
/// `node_id → {node_id, parent_node_id, run_id}` for the given threads, read
/// from T3's covering indexes instead of one table lookup per node: the thread
/// index gives each selected row id and its run, the primary-key and parent
/// indexes give the other two fields by row id. The map has one entry per
/// distinct node id, so the order rows arrive in cannot matter, except for
/// rows whose id is not a usable key (NULL, a blob, or empty), which the
/// attribution walk never looks up. If two selected rows share a node id (a
/// schema without the primary key), the oracle's last-row-wins order would
/// matter, so this returns `None` and the caller reads as the oracle does.
/// Returns the map and the number of rows selected.
fn nodes_from_indexes(
    db: &Connection,
    where_ids: &str,
    params: &[SqlValue],
) -> rusqlite::Result<Option<(HashMap<String, Json>, usize)>> {
    // The selected rows in index order, and each one's position by row id.
    let mut rowids = Vec::new();
    let mut run_ids = Vec::new();
    let mut usable = true;
    each_row(
        db,
        &format!("SELECT rowid,run_id FROM {PREFIX}nodes {where_ids}"),
        params,
        |_, row| {
            match row.get_ref(0)? {
                ValueRef::Integer(id) => {
                    rowids.push(id);
                    run_ids.push(value_json(row.get_ref(1)?));
                }
                // A column named rowid would shadow the row id.
                _ => usable = false,
            }
            Ok(())
        },
    )?;
    if !usable {
        return Ok(None);
    }
    let n = rowids.len();
    let mut position: HashMap<i64, usize, std::hash::BuildHasherDefault<RowidHasher>> =
        HashMap::with_capacity_and_hasher(n, Default::default());
    for (i, id) in rowids.iter().enumerate() {
        if position.insert(*id, i).is_some() {
            return Ok(None);
        }
    }
    let mut node_ids = vec![Value::Null; n];
    let mut parents = vec![Value::Null; n];
    for (column, target) in [("node_id", &mut node_ids), ("parent_node_id", &mut parents)] {
        let mut found = 0usize;
        each_row(
            db,
            &format!("SELECT rowid,{column} FROM {PREFIX}nodes"),
            &[],
            |_, row| {
                if let ValueRef::Integer(id) = row.get_ref(0)? {
                    if let Some(&i) = position.get(&id) {
                        target[i] = value_json(row.get_ref(1)?);
                        found += 1;
                    }
                }
                Ok(())
            },
        )?;
        if found != n {
            return Ok(None);
        }
    }
    let mut nodes = HashMap::with_capacity(n);
    for ((node_id, parent_node_id), run_id) in node_ids.into_iter().zip(parents).zip(run_ids) {
        let key = node_id.as_str().unwrap_or("").to_owned();
        let usable_key = !key.is_empty();
        let mut row = serde_json::Map::new();
        row.insert("node_id".into(), node_id);
        row.insert("parent_node_id".into(), parent_node_id);
        row.insert("run_id".into(), run_id);
        if nodes.insert(key, Value::Object(row)).is_some() && usable_key {
            return Ok(None);
        }
    }
    Ok(Some((nodes, n)))
}
/// Row ids are already well spread; skip SipHash for the join above.
#[derive(Default)]
struct RowidHasher(u64);
impl std::hash::Hasher for RowidHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn write_i64(&mut self, value: i64) {
        self.0 = (value as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// SQL rows remain owned JSON; cached items are borrowed in place. The oracle
/// still decodes only selected items and failed events, as before.
pub(crate) enum ItemRows<'a> {
    Json(Vec<Json>),
    Typed(Vec<&'a Item>),
}
impl From<Vec<Json>> for ItemRows<'_> {
    fn from(rows: Vec<Json>) -> Self {
        Self::Json(rows)
    }
}
impl<'a> From<Vec<&'a Item>> for ItemRows<'a> {
    fn from(rows: Vec<&'a Item>) -> Self {
        Self::Typed(rows)
    }
}

/// Shared assembly boundary: both SQL-selected and cache-selected narrow rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble<'a>(
    threads: Vec<Json>,
    latest: HashMap<String, Json>,
    subs: Vec<Json>,
    providers: HashMap<String, Json>,
    turns: HashMap<String, Json>,
    nodes: HashMap<String, Json>,
    projects: HashMap<String, Json>,
    requests: Vec<Json>,
    items: impl Into<ItemRows<'a>>,
    events: impl Into<ItemRows<'a>>,
    history: Vec<Json>,
    spans: Vec<Json>,
    plans: Vec<Json>,
    path: &Path,
    now: f64,
) -> Result<Snapshot, SourceError> {
    let _assembly = timing::phase("assembly");
    let owned;
    let items = match items.into() {
        ItemRows::Json(rows) => {
            let timed = timing::on().then(Instant::now);
            owned = rows
                .into_iter()
                .map(|row| {
                    serde_json::from_value(row).map_err(|e| problem(path, "schema", e.to_string()))
                })
                .collect::<Result<Vec<Item>, _>>()?;
            if let Some(t) = timed {
                timing::add_json(t);
            }
            owned.iter().collect::<Vec<_>>()
        }
        ItemRows::Typed(rows) => rows,
    };
    let events = events.into();
    let timed = timing::on().then(Instant::now);
    let mut agents = build_agents(
        &threads, &latest, &subs, &providers, &turns, &nodes, &projects, &requests, &items, path,
    )?;
    if let Some(t) = timed {
        timing::add_build(t);
    }
    let timed = timing::on().then(Instant::now);
    let decorated = decorate(
        &mut agents,
        &subs,
        &nodes,
        &events,
        &history,
        &spans,
        &plans,
        path,
    );
    if let Some(t) = timed {
        timing::add_decorate(t);
    }
    decorated?;
    Ok(Snapshot {
        agents: agents.into_iter().map(std::sync::Arc::new).collect(),
        at: now,
        path: path.to_string_lossy().into(),
    })
}
pub(crate) struct Attributor<'a> {
    sub_ids: HashSet<&'a str>,
    aliases: HashMap<&'a str, &'a str>,
    nodes: &'a HashMap<String, Json>,
    native: HashMap<&'a str, &'a str>,
}
impl<'a> Attributor<'a> {
    pub(crate) fn new(agents: &'a [Agent], subs: &'a [Json], nodes: &'a HashMap<String, Json>) -> Self {
        let mut native = HashMap::new();
        for a in agents {
            if a.kind == "subagent" {
                if let Some(pid) = a.provider_id.as_ref().filter(|p| !p.is_empty()) {
                    native.entry(pid.as_str()).or_insert(a.id.as_str());
                }
            }
        }
        let mut result = Self {
            sub_ids: subs.iter().map(|s| str_at(s, "id")).collect(),
            aliases: subs
                .iter()
                .filter_map(|s| {
                    truthy_str(s, "childThreadId")
                        .map(|_| (str_at(s, "id"), str_at(s, "childThreadId")))
                })
                .collect(),
            nodes,
            native,
        };
        for a in agents {
            for id in a.subagent_ids.iter().flatten() {
                result.aliases.insert(id, &a.id);
            }
        }
        for sub in subs {
            if let (Some(id), Some(pid)) = (
                result.owner(Some(str_at(sub, "id"))).map(str::to_owned),
                sub.get("providerThreadId").and_then(Value::as_str),
            ) {
                // Resolve back to an input-backed reference for the map's lifetime.
                let id = agents
                    .iter()
                    .find(|a| a.id == id)
                    .map(|a| a.id.as_str())
                    .unwrap_or_else(|| str_at(sub, "id"));
                result.native.entry(pid).or_insert(id);
            }
        }
        result
    }
    pub(crate) fn owner<'b>(&'b self, node: Option<&'b str>) -> Option<&'b str> {
        let mut node = node;
        let mut seen = HashSet::new();
        while let Some(id) = node.filter(|id| !id.is_empty()) {
            if !seen.insert(id) {
                break;
            }
            if self.sub_ids.contains(id) {
                return Some(self.aliases.get(id).copied().unwrap_or(id));
            }
            node = self
                .nodes
                .get(id)
                .and_then(|r| r.get("parent_node_id"))
                .and_then(Value::as_str);
        }
        None
    }
    pub(crate) fn item_target<'b>(&'b self, row: &'b Item) -> &'b str {
        self.route_ids(
            row.provider_thread_id.as_deref(),
            row.node_id.as_deref(),
            &row.thread_id,
        )
    }
    /// Same attribution as `item_target`, from the three id fields alone.
    pub(crate) fn route_ids<'b>(
        &'b self,
        provider_thread_id: Option<&'b str>,
        node_id: Option<&'b str>,
        thread_id: &'b str,
    ) -> &'b str {
        provider_thread_id
            .filter(|id| !id.is_empty())
            .and_then(|pid| self.native.get(pid).copied())
            .or_else(|| self.owner(node_id))
            .unwrap_or(thread_id)
    }
    pub(crate) fn target<'b>(&'b self, row: &'b Json) -> &'b str {
        row.get("provider_thread_id")
            .and_then(Value::as_str)
            .and_then(|pid| self.native.get(pid).copied())
            .or_else(|| self.owner(row.get("node_id").and_then(Value::as_str)))
            .unwrap_or_else(|| str_at(row, "thread_id"))
    }
}
pub(crate) fn context(
    pid: Option<&str>,
    providers: &HashMap<String, Json>,
    turns: &HashMap<String, Json>,
    path: &Path,
) -> Result<(Context, Json), SourceError> {
    let pid = pid.filter(|id| !id.is_empty());
    let p = pid.and_then(|id| providers.get(id));
    let turn = pid
        .and_then(|id| turns.get(id))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let fallback = p
        .map(|p| obj(p, "contextUsage"))
        .unwrap_or_else(|| json!({}));
    let mut ctx = turn
        .get("tokenUsage")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| fallback.clone());
    if ctx.get("usedTokens").is_none_or(Value::is_null) {
        ctx = fallback;
    }
    Ok((
        serde_json::from_value(ctx).map_err(|e| problem(path, "schema", e.to_string()))?,
        turn,
    ))
}
/// One definition of the heavy per-agent fields. The SQL oracle and the live
/// cache both call these, so a capped item, event, history point, span or plan
/// cannot be assembled two different ways.
pub(crate) fn attach_item(agent: &mut Agent, item: &Item) {
    if item.run_id.as_deref() == agent.run.get("id").and_then(Value::as_str) {
        agent.items.push(item.clone());
    }
}
pub(crate) fn attach_request(agent: &mut Agent, kind: &str, created_at: Option<&str>) {
    agent.requests.push(kind.to_string());
    if let Some(asked) = created_at.filter(|s| !s.is_empty()) {
        if agent.asked.as_ref().is_none_or(|old| asked < old.as_str()) {
            agent.asked = Some(asked.to_string());
        }
    }
}
pub(crate) fn finish_agent_body(agent: &mut Agent, known: &HashSet<String>) {
    agent.items.sort_by(|p, q| q.updated_at.cmp(&p.updated_at));
    if let Some(item) = agent.items.first() {
        if item.updated_at.as_str() > agent.updated.as_deref().unwrap_or("") {
            agent.updated = Some(item.updated_at.clone());
        }
    }
    if !agent.requests.is_empty() && model::live(agent) {
        agent.status = "waiting".into();
    }
    if agent
        .parent
        .as_ref()
        .is_none_or(|parent| parent == &agent.id || !known.contains(parent))
    {
        agent.parent = None;
    }
    agent.now = agent
        .items
        .iter()
        .find(|i| {
            live_status(&i.status)
                && !matches!(i.kind.as_str(), "reasoning" | "assistant_message")
        })
        .or_else(|| agent.items.iter().find(|i| live_status(&i.status)))
        .cloned();
    for item in &agent.items {
        if matches!(item.kind.as_str(), "reasoning" | "assistant_message") {
            continue;
        }
        *agent
            .mix
            .entry(model::category(&item.kind, item.tool.as_deref()).into())
            .or_insert(0) += 1;
        if item.status == "failed" {
            agent.failures += 1;
        }
    }
}
pub(crate) fn attach_event(
    agent: &mut Agent,
    at: f64,
    kind: &str,
    tool: Option<&str>,
    failed: bool,
    failed_item: Option<&Item>,
) {
    agent.events.push(Event {
        t: at,
        cat: model::category(kind, tool).into(),
        failed,
    });
    if failed {
        if let Some(item) = failed_item {
            agent.recent_failures.push(item.clone());
        }
    }
}
pub(crate) fn attach_history(
    agent: &mut Agent,
    used: f64,
    max: f64,
    at: f64,
    provider: &str,
    model_name: &str,
) {
    if used.is_finite() && max.is_finite() && max > 0. {
        agent.history.push(Point {
            pct: (used / max).min(1.),
            used,
            max,
            at,
            provider: provider.into(),
            model: model_name.into(),
        });
    }
}
pub(crate) fn consider_plan(agent: &mut Agent, run_id: Option<&str>, steps_json: Option<&str>) {
    if agent.plan.is_some() || run_id != agent.run.get("id").and_then(Value::as_str) {
        return;
    }
    if let Ok(Value::Array(steps)) =
        serde_json::from_str::<Value>(steps_json.unwrap_or("[]"))
    {
        if !steps.is_empty() {
            agent.plan = Some(
                steps
                    .iter()
                    .map(|s| Step {
                        text: optional(s, "text")
                            .or_else(|| optional(s, "step"))
                            .unwrap_or_default(),
                        status: optional(s, "status").unwrap_or_else(|| "pending".into()),
                    })
                    .collect(),
            );
        }
    }
}
pub(crate) fn attach_span(
    agent: &mut Agent,
    started_at: Option<&str>,
    requested_at: Option<&str>,
    completed_at: Option<&str>,
) {
    let start = model::ms(started_at).or_else(|| model::ms(requested_at));
    if let Some(start) = start {
        agent.spans.push((start, model::ms(completed_at)));
    }
}
pub(crate) fn finish_agent_tail(agent: &mut Agent) {
    agent.history.sort_by(|p, q| p.at.total_cmp(&q.at));
    agent.events.sort_by(|p, q| p.t.total_cmp(&q.t));
    if agent.id != agent.thread_id {
        if let Some(start) = model::ms(agent.started.as_deref()) {
            agent.spans.push((start, model::ms(agent.ended.as_deref())));
        }
    }
}
/// Dangling-parent clear and cycle break, the structural half of `finish_agent_body`
/// plus `break_cycles`. Content does not feed either step, so the live reader can
/// hash these parents before it rebuilds any agent's items.
pub(crate) fn seal_parents(agents: &mut [Agent]) {
    let known: HashSet<String> = agents.iter().map(|a| a.id.clone()).collect();
    for agent in agents.iter_mut() {
        if agent
            .parent
            .as_ref()
            .is_none_or(|parent| parent == &agent.id || !known.contains(parent))
        {
            agent.parent = None;
        }
    }
    break_cycles(agents);
}
pub(crate) fn break_cycles(agents: &mut [Agent]) {
    let index: HashMap<String, usize> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id.clone(), i))
        .collect();
    let mut done = HashSet::new();
    for start in 0..agents.len() {
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(i) = current {
            if done.contains(&i) {
                break;
            }
            if let Some(at) = path.iter().position(|p| *p == i) {
                let cut = *path[at..]
                    .iter()
                    .min_by_key(|i: &&usize| &agents[**i].id)
                    .unwrap();
                agents[cut].parent = None;
                break;
            }
            path.push(i);
            current = agents[i]
                .parent
                .as_ref()
                .and_then(|id| index.get(id).copied());
        }
        done.extend(path);
    }
}
fn build_agents(
    threads: &[Json],
    latest: &HashMap<String, Json>,
    subs: &[Json],
    providers: &HashMap<String, Json>,
    turns: &HashMap<String, Json>,
    nodes: &HashMap<String, Json>,
    projects: &HashMap<String, Json>,
    requests: &[Json],
    items: &[&Item],
    path: &Path,
) -> Result<Vec<Agent>, SourceError> {
    let mut agents = build_structure(
        threads, latest, subs, providers, turns, nodes, projects, path,
    )?;
    apply_attributed_body(&mut agents, subs, nodes, requests, items);
    Ok(agents)
}
/// One thread's shell, before subagent links, remote placeholders and cycle breaking.
/// `build_structure` and the live reader both use this, so a patched shell cannot
/// grow a different run, context or shelf than the oracle's full walk.
pub(crate) fn thread_agent(
    thread: &Json,
    run: &Json,
    providers: &HashMap<String, Json>,
    turns: &HashMap<String, Json>,
    projects: &HashMap<String, Json>,
    path: &Path,
) -> Result<Agent, SourceError> {
    let tid = str_at(thread, "id").to_string();
    let pid = truthy_str(run, "providerThreadId")
        .or_else(|| truthy_str(thread, "activeProviderThreadId"));
    let (ctx, turn) = context(pid.as_deref(), providers, turns, path)?;
    let project = projects
        .get(str_at(thread, "projectId"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let lineage = obj(thread, "lineage");
    let selection = if run.get("modelSelection").is_some_and(|v| !v.is_null()) {
        obj(run, "modelSelection")
    } else {
        obj(thread, "modelSelection")
    };
    Ok(Agent {
        id: tid.clone(),
        parent: if str_at(&lineage, "relationshipToParent") == "subagent" {
            optional(&lineage, "parentThreadId")
        } else {
            None
        },
        title: optional(thread, "title").unwrap_or_else(|| "Untitled".into()),
        kind: "thread".into(),
        project: optional(&project, "title").unwrap_or_else(|| "Unknown project".into()),
        cwd: truthy_str(thread, "worktreePath")
            .or_else(|| truthy_str(&project, "workspace_root"))
            .unwrap_or_default(),
        model: truthy_str(&turn, "model")
            .or_else(|| truthy_str(&selection, "model"))
            .unwrap_or_else(|| "--".into()),
        status: optional(run, "status").unwrap_or_else(|| "idle".into()),
        run: run.clone(),
        provider_id: pid,
        context: ctx,
        turn,
        updated: optional(thread, "updatedAt"),
        started: truthy_str(run, "startedAt").or_else(|| truthy_str(run, "requestedAt")),
        ended: optional(run, "completedAt"),
        remote: None,
        thread_id: tid,
        branch: optional(thread, "branch"),
        options: selection
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        shelf: Some(Shelf {
            listed: str_at(&lineage, "relationshipToParent") != "subagent"
                && lineage.get("sideChat") != Some(&Value::Bool(true)),
            settled: str_at(thread, "settledOverride") == "settled",
            snoozed_until: optional(thread, "snoozedUntil"),
        }),
        ..Agent::default()
    })
}
/// Identity, parent, kind and remote placeholders. No items, events, requests or plans.
pub(crate) fn build_structure(
    threads: &[Json],
    latest: &HashMap<String, Json>,
    subs: &[Json],
    providers: &HashMap<String, Json>,
    turns: &HashMap<String, Json>,
    nodes: &HashMap<String, Json>,
    projects: &HashMap<String, Json>,
    path: &Path,
) -> Result<Vec<Agent>, SourceError> {
    let structure = timing::phase("structure_build");
    let mut agents = Vec::new();
    for thread in threads {
        let tid = str_at(thread, "id").to_string();
        let run = latest.get(&tid).cloned().unwrap_or_else(|| json!({}));
        let agent = thread_agent(thread, &run, providers, turns, projects, path)?;
        agents.push(agent);
    }
    link_structure(&mut agents, subs, providers, turns, nodes, threads, path)?;
    drop(structure);
    Ok(agents)
}
/// Parents, subagent agents and remote placeholders. `agents` must already be the
/// active thread shells in the oracle's thread order, with link fields still at
/// `thread_agent` (kind `thread`, no subagent ids). `build_structure` calls this
/// for the full walk. A graph poll that can keep its shells does not.
pub(crate) fn link_structure(
    agents: &mut Vec<Agent>,
    subs: &[Json],
    providers: &HashMap<String, Json>,
    turns: &HashMap<String, Json>,
    nodes: &HashMap<String, Json>,
    threads: &[Json],
    path: &Path,
) -> Result<(), SourceError> {
    let mut index: HashMap<String, usize> = agents
        .iter()
        .enumerate()
        .map(|(i, agent)| (agent.id.clone(), i))
        .collect();
    let local_ids: HashSet<String> = index.keys().cloned().collect();
    let local_env = crate::integration::environment_id(path);
    let key = |v: &Value| {
        crate::integration::thread_ref(v).map(|(e, t)| serde_json::to_string(&(e, t)).unwrap())
    };
    let mut remote_ids: HashMap<String, String> = HashMap::new();
    let mut ordered_subs: Vec<&Json> = subs.iter().collect();
    ordered_subs.sort_by(|a, b| {
        let stamp = |s: &Json| {
            optional(s, "updatedAt")
                .or_else(|| optional(s, "startedAt"))
                .unwrap_or_default()
        };
        // `localeCompare` in the original: ICU order, not bytes, so mixed case and
        // punctuation sort as Node sorts them.
        crate::terminal::locale_compare(&stamp(b), &stamp(a))
            .then_with(|| crate::terminal::locale_compare(str_at(a, "id"), str_at(b, "id")))
    });
    let mut preferred = HashMap::new();
    for sub in &ordered_subs {
        if let Some(k) = sub.get("remoteChild").and_then(&key) {
            preferred.entry(k).or_insert(*sub);
        }
    }
    for entry in subs {
        let ref_key = entry.get("remoteChild").and_then(&key);
        let sub = ref_key
            .as_ref()
            .and_then(|k| preferred.get(k))
            .copied()
            .unwrap_or(entry);
        let known = entry
            .get("remoteChild")
            .and_then(crate::integration::thread_ref)
            .and_then(|(e, t)| {
                if Some(e) == local_env.as_deref() && local_ids.contains(t) {
                    Some(t.to_owned())
                } else {
                    ref_key.as_ref().and_then(|k| remote_ids.get(k).cloned())
                }
            });
        let existing = index
            .get(str_at(entry, "childThreadId"))
            .copied()
            .or_else(|| known.as_ref().and_then(|k| index.get(k).copied()));
        if let Some(i) = existing {
            agents[i]
                .subagent_ids
                .get_or_insert_with(Vec::new)
                .push(str_at(entry, "id").into());
            if let Some(k) = ref_key {
                remote_ids.insert(k, agents[i].id.clone());
            }
            continue;
        }
        let pid = optional(sub, "providerThreadId");
        let (ctx, turn) = context(pid.as_deref(), providers, turns, path)?;
        let id = str_at(sub, "id").to_string();
        let project = index
            .get(str_at(sub, "threadId"))
            .map(|i| agents[*i].project.clone())
            .unwrap_or_default();
        let a = Agent {
            id: id.clone(),
            parent: None,
            subagent_ids: Some(vec![str_at(entry, "id").into()]),
            title: truthy_str(sub, "title").unwrap_or_else(|| "Subagent".into()),
            kind: "subagent".into(),
            project,
            cwd: String::new(),
            model: truthy_str(&turn, "model")
                .or_else(|| truthy_str(sub, "model"))
                .unwrap_or_else(|| "--".into()),
            status: optional(sub, "status").unwrap_or_else(|| "unknown".into()),
            run: json!({"id":sub.get("runId").cloned().unwrap_or(Value::Null)}),
            provider_id: pid,
            context: ctx,
            turn,
            updated: optional(sub, "updatedAt"),
            started: optional(sub, "startedAt"),
            ended: optional(sub, "completedAt"),
            remote: sub.get("remoteChild").filter(|v| !v.is_null()).cloned(),
            thread_id: str_at(sub, "threadId").into(),
            ..Agent::default()
        };
        if let Some(k) = ref_key {
            remote_ids.insert(k, id.clone());
        }
        if let Some(i) = index.get(&id).copied() {
            agents[i] = a;
        } else {
            index.insert(id, agents.len());
            agents.push(a);
        }
    }
    let parents = {
        let attr = Attributor::new(&agents, subs, nodes);
        let mut parented = HashSet::new();
        ordered_subs
            .iter()
            .filter_map(|sub| {
                let id = attr.owner(Some(str_at(sub, "id")))?.to_owned();
                if !parented.insert(id.clone()) {
                    return None;
                }
                let parent = attr
                    .owner(sub.get("parentNodeId").and_then(Value::as_str))
                    .map(str::to_owned)
                    .or_else(|| optional(sub, "threadId"));
                Some((id, parent))
            })
            .collect::<Vec<_>>()
    };
    for (id, parent) in parents {
        if let Some(i) = index.get(&id) {
            agents[*i].parent = parent;
            agents[*i].kind = "subagent".into();
        }
    }
    for thread in threads {
        let Some((env, tid)) = thread
            .get("delegatedFrom")
            .and_then(crate::integration::thread_ref)
        else {
            continue;
        };
        let i = index[str_at(thread, "id")];
        let lineage = obj(thread, "lineage");
        if str_at(&lineage, "relationshipToParent") == "subagent"
            && local_ids.contains(str_at(&lineage, "parentThreadId"))
        {
            continue;
        }
        let k = serde_json::to_string(&(env, tid)).unwrap();
        let parent = if Some(env) == local_env.as_deref() && local_ids.contains(tid) {
            tid.to_owned()
        } else if let Some(id) = remote_ids.get(&k) {
            id.clone()
        } else {
            let mut id = format!("remote:{k}");
            while index.contains_key(&id) {
                id.push_str(":ref");
            }
            let a = Agent {
                id: id.clone(),
                title: truthy_str(&obj(thread, "delegatedFrom"), "title")
                    .unwrap_or("Remote parent".into()),
                kind: "thread".into(),
                project: agents[i].project.clone(),
                model: "--".into(),
                status: "unknown".into(),
                run: json!({}),
                turn: json!({}),
                remote: Some(json!({"environmentId":env,"threadId":tid})),
                reference_only: Some(true),
                thread_id: tid.into(),
                ..Agent::default()
            };
            index.insert(id.clone(), agents.len());
            agents.push(a);
            remote_ids.insert(k, id.clone());
            id
        };
        agents[i].parent = Some(parent);
        agents[i].kind = "subagent".into();
    }
    Ok(())
}
pub(crate) fn apply_attributed_body(
    agents: &mut [Agent],
    subs: &[Json],
    nodes: &HashMap<String, Json>,
    requests: &[Json],
    items: &[&Item],
) {
    let content = timing::phase("agent_content_build");
    let index: HashMap<String, usize> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id.clone(), i))
        .collect();
    let targets = {
        let attr = Attributor::new(agents, subs, nodes);
        items
            .iter()
            .map(|item| attr.item_target(item).to_owned())
            .collect::<Vec<_>>()
    };
    for (item, target) in items.iter().zip(targets) {
        if let Some(i) = index.get(&target).copied() {
            attach_item(&mut agents[i], item);
        }
    }
    let targets = {
        let attr = Attributor::new(agents, subs, nodes);
        requests
            .iter()
            .map(|row| attr.target(row).to_owned())
            .collect::<Vec<_>>()
    };
    for (req, target) in requests.iter().zip(targets) {
        if let Some(i) = index.get(&target).copied() {
            attach_request(
                &mut agents[i],
                str_at(req, "kind"),
                truthy_str(req, "created_at").as_deref(),
            );
        }
    }
    let known: HashSet<String> = agents.iter().map(|a| a.id.clone()).collect();
    for a in agents.iter_mut() {
        finish_agent_body(a, &known);
    }
    drop(content);
    let _cycles = timing::phase("structure_cycles");
    break_cycles(agents);
}
fn live_status(status: &str) -> bool {
    matches!(
        status,
        "running" | "starting" | "preparing" | "queued" | "waiting" | "pending" | "active"
    )
}
pub(crate) fn decorate(
    agents: &mut [Agent],
    subs: &[Json],
    nodes: &HashMap<String, Json>,
    events: &ItemRows<'_>,
    history: &[Json],
    spans: &[Json],
    plans: &[Json],
    path: &Path,
) -> Result<(), SourceError> {
    let _content = timing::phase("agent_content_decorate");
    let index: HashMap<String, usize> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id.clone(), i))
        .collect();
    let event_targets = {
        let attr = Attributor::new(agents, subs, nodes);
        match events {
            ItemRows::Json(rows) => rows
                .iter()
                .map(|row| attr.target(row).to_owned())
                .collect::<Vec<_>>(),
            ItemRows::Typed(rows) => rows
                .iter()
                .map(|row| attr.item_target(row).to_owned())
                .collect(),
        }
    };
    let count = match events {
        ItemRows::Json(rows) => rows.len(),
        ItemRows::Typed(rows) => rows.len(),
    };
    for (n, target) in (0..count).zip(event_targets) {
        let (failed, updated, started, kind, tool) = match events {
            ItemRows::Json(rows) => {
                let row = &rows[n];
                (
                    str_at(row, "status") == "failed",
                    row.get("updated_at").and_then(Value::as_str),
                    row.get("started_at").and_then(Value::as_str),
                    str_at(row, "type"),
                    row.get("tool").and_then(Value::as_str),
                )
            }
            ItemRows::Typed(rows) => {
                let row = rows[n];
                (
                    row.status == "failed",
                    Some(row.updated_at.as_str()),
                    row.started_at.as_deref(),
                    row.kind.as_str(),
                    row.tool.as_deref(),
                )
            }
        };
        let time = model::ms(if failed { updated } else { started }).or_else(|| model::ms(updated));
        if let (Some(i), Some(t)) = (index.get(&target).copied(), time) {
            let failed_item = if failed {
                Some(match events {
                    ItemRows::Json(rows) => serde_json::from_value(rows[n].clone())
                        .map_err(|e| problem(path, "schema", e.to_string()))?,
                    ItemRows::Typed(rows) => rows[n].clone(),
                })
            } else {
                None
            };
            attach_event(&mut agents[i], t, kind, tool, failed, failed_item.as_ref());
        }
    }
    let history_targets = {
        let attr = Attributor::new(agents, subs, nodes);
        history
            .iter()
            .map(|r| attr.target(r).to_owned())
            .collect::<Vec<_>>()
    };
    for (row, target) in history.iter().zip(history_targets) {
        let a = index.get(&target).copied();
        let used = number(row, "used");
        let max = number(row, "max");
        if let Some(i) = a {
            attach_history(
                &mut agents[i],
                used,
                max,
                model::ms(row.get("started_at").and_then(Value::as_str))
                    .or_else(|| model::ms(row.get("completed_at").and_then(Value::as_str)))
                    .unwrap_or(0.),
                str_at(row, "provider_thread_id"),
                optional(row, "model").as_deref().unwrap_or(""),
            );
        }
    }
    for row in spans {
        if let Some(i) = index.get(str_at(row, "thread_id")).copied() {
            attach_span(
                &mut agents[i],
                row.get("started_at").and_then(Value::as_str),
                row.get("requested_at").and_then(Value::as_str),
                row.get("completed_at").and_then(Value::as_str),
            );
        }
    }
    for a in agents.iter_mut() {
        finish_agent_tail(a);
    }
    let plan_targets = {
        let attr = Attributor::new(agents, subs, nodes);
        plans
            .iter()
            .map(|row| attr.target(row).to_owned())
            .collect::<Vec<_>>()
    };
    for (row, target) in plans.iter().zip(plan_targets) {
        if let Some(i) = index.get(&target).copied() {
            consider_plan(
                &mut agents[i],
                row.get("run_id").and_then(Value::as_str),
                row.get("steps").and_then(Value::as_str),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "t3-rune-source-{}-{}-{name}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn schema(db: &Connection) {
        db.execute_batch(&format!("CREATE TABLE {PREFIX}threads(thread_id TEXT,payload_json TEXT,deleted_at TEXT,archived_at TEXT);
        CREATE TABLE {PREFIX}runs(thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
        CREATE TABLE {PREFIX}subagents(thread_id TEXT,payload_json TEXT);
        CREATE TABLE {PREFIX}nodes(thread_id TEXT,node_id TEXT,parent_node_id TEXT,run_id TEXT);
        CREATE TABLE {PREFIX}provider_threads(thread_id TEXT,payload_json TEXT);
        CREATE TABLE {PREFIX}provider_turns(thread_id TEXT,provider_thread_id TEXT,ordinal INTEGER,payload_json TEXT,started_at TEXT,completed_at TEXT);
        CREATE TABLE {PREFIX}turn_items(thread_id TEXT,run_id TEXT,node_id TEXT,provider_thread_id TEXT,type TEXT,status TEXT,updated_at TEXT,ordinal INTEGER,payload_json TEXT);
        CREATE TABLE {PREFIX}runtime_requests(thread_id TEXT,node_id TEXT,kind TEXT,status TEXT,resolved_at TEXT);
        CREATE TABLE projection_projects(project_id TEXT,title TEXT,workspace_root TEXT);
        CREATE TABLE auth_tokens(value TEXT);" )).unwrap();
    }
    #[test]
    fn missing_paths_are_not_created_and_unknown_schema_stays_unchanged() {
        let missing = path("missing");
        assert_eq!(snapshot(&missing, 0.).unwrap_err().kind, "missing");
        assert!(!missing.exists());
        let unknown = path("unknown");
        let db = Connection::open(&unknown).unwrap();
        db.execute_batch("CREATE TABLE untouched(value TEXT)")
            .unwrap();
        drop(db);
        let before = std::fs::read(&unknown).unwrap();
        assert_eq!(snapshot(&unknown, 0.).unwrap_err().kind, "schema");
        assert_eq!(std::fs::read(&unknown).unwrap(), before);
        std::fs::remove_file(unknown).unwrap();
    }
    #[test]
    fn narrow_projection_excludes_private_payload_fields_and_never_writes() {
        let file = path("privacy");
        let db = Connection::open(&file).unwrap();
        schema(&db);
        let put = |table: &str, cols: &str, payload: Value| {
            db.execute(
                &format!("INSERT INTO {PREFIX}{table} {cols}"),
                [payload.to_string()],
            )
            .unwrap();
        };
        put(
            "threads",
            "VALUES('t',?,NULL,NULL)",
            json!({"id":"t","projectId":"p","title":"Thread","activeProviderThreadId":"pt","delegatedFrom":{"prompt":"SECRET-delegated"},"forkedFrom":"SECRET-fork"}),
        );
        put(
            "runs",
            "VALUES('t',1,?,NULL,NULL)",
            json!({"id":"r","threadId":"t","status":"running","providerThreadId":"pt","delegatedCompletion":{"result":"SECRET-completion"}}),
        );
        put(
            "subagents",
            "VALUES('t',?)",
            json!({"id":"s","threadId":"t","runId":"r","status":"running","title":"Sub","prompt":"SECRET-prompt","result":"SECRET-result","progress":"SECRET-progress","remoteChild":{"label":"other machine","title":"SECRET-remote-title"}}),
        );
        put(
            "provider_threads",
            "VALUES('t',?)",
            json!({"id":"pt","goal":"SECRET-goal","nativeMetadata":{"note":"SECRET-native"},"contextUsage":{"usedTokens":1,"maxTokens":10}}),
        );
        put(
            "provider_turns",
            "VALUES('t','pt',1,?,NULL,NULL)",
            json!({"providerThreadId":"pt","model":"m","nativeTurnRef":"SECRET-turn-ref","tokenUsage":{"usedTokens":2,"maxTokens":10}}),
        );
        db.execute("INSERT INTO auth_tokens VALUES('SECRET-auth')", [])
            .unwrap();
        db.execute(&format!("INSERT INTO {PREFIX}turn_items VALUES('t','r',NULL,'pt','command_execution','completed','2026-11-12T12:00:00Z',1,?)"),[json!({"input":"cargo test -- synthetic-only","title":"Test","output":"SECRET-tool-output","result":"SECRET-tool-result","reasoning":"SECRET-reasoning-body"}).to_string()]).unwrap();
        drop(db);
        let before = std::fs::read(&file).unwrap();
        let result = snapshot(&file, 1_794_484_800_000.).unwrap();
        let rendered = serde_json::to_string(&result).unwrap();
        assert_eq!(result.agents.len(), 2);
        assert!(
            !rendered.contains("SECRET"),
            "private marker escaped SQL projection"
        );
        assert_eq!(
            result
                .agents
                .iter()
                .find(|a| a.id == "t")
                .unwrap()
                .context
                .used_tokens,
            Some(2.)
        );
        assert!(rendered.contains("cargo test -- synthetic-only"));
        assert!(rendered.contains("other machine"));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        std::fs::remove_file(file).unwrap();
    }
    #[test]
    fn cross_machine_retries_references_attribution_and_cycles() {
        let directory = path("cross-dir");
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("synthetic.sqlite");
        std::fs::write(directory.join("environment-id"), "machine-a\n").unwrap();
        let db = Connection::open(&file).unwrap();
        schema(&db);
        let threads = vec![
            json!({"id":"root"}),
            json!({"id":"leaf","delegatedFrom":{"environmentId":"machine-b","threadId":"parent"}}),
            json!({"id":"local","delegatedFrom":{"environmentId":"machine-a","threadId":"root"}}),
            json!({"id":"missing","delegatedFrom":{"environmentId":"machine-c","threadId":"unknown","title":"Absent","prompt":"SECRET"}}),
            json!({"id":"missing2","delegatedFrom":{"environmentId":"machine-c","threadId":"unknown"}}),
            json!({"id":"bad","delegatedFrom":{"environmentId":"../bad","threadId":"root"}}),
            json!({"id":"incomplete","delegatedFrom":{"environmentId":"machine-b"}}),
            json!({"id":"a","delegatedFrom":{"environmentId":"machine-a","threadId":"b"}}),
            json!({"id":"b","delegatedFrom":{"environmentId":"machine-a","threadId":"a"}}),
        ];
        for t in threads {
            db.execute(
                &format!("INSERT INTO {PREFIX}threads VALUES(?,?,NULL,NULL)"),
                [str_at(&t, "id"), &t.to_string()],
            )
            .unwrap();
        }
        for n in 0..3 {
            let id = format!("retry-{n}");
            let pid = format!("provider-{n}");
            let sub = json!({"id":id,"threadId":"root","runId":"r","providerThreadId":pid,"status":if n==2 {"completed"} else {"running"},"updatedAt":format!("2026-11-14T11:00:0{n}Z"),"remoteChild":{"environmentId":"machine-b","threadId":"parent","label":"B","result":"SECRET"}});
            db.execute(
                &format!("INSERT INTO {PREFIX}subagents VALUES('root',?)"),
                [sub.to_string()],
            )
            .unwrap();
            db.execute(&format!("INSERT INTO {PREFIX}runtime_requests VALUES('root',?,'approval','pending',NULL)"),[&id]).unwrap();
            db.execute(&format!("INSERT INTO {PREFIX}provider_turns VALUES('root',?,?,?,'2026-11-14T11:00:00Z',NULL)"),rusqlite::params![pid,n,json!({"providerThreadId":pid,"tokenUsage":{"usedTokens":n+1,"maxTokens":100}}).to_string()]).unwrap();
        }
        drop(db);
        let before = std::fs::read(&file).unwrap();
        let result = snapshot(&file, model::ms(Some("2026-11-14T12:00:00Z")).unwrap()).unwrap();
        let get = |id: &str| result.agents.iter().find(|a| a.id == id).unwrap();
        let remote = get("retry-2");
        assert_eq!(
            remote.subagent_ids.as_ref().unwrap(),
            &["retry-0", "retry-1", "retry-2"]
        );
        assert_eq!(remote.status, "completed");
        assert_eq!(remote.requests.len(), 3);
        assert_eq!(remote.history.len(), 3);
        assert_eq!(get("leaf").parent.as_deref(), Some("retry-2"));
        assert_eq!(get("local").parent.as_deref(), Some("root"));
        let reference = result
            .agents
            .iter()
            .find(|a| a.reference_only == Some(true))
            .unwrap();
        assert_eq!(reference.status, "unknown");
        assert!(reference.shelf.is_none());
        assert!(reference.started.is_none());
        assert_eq!(get("missing").parent, get("missing2").parent);
        assert_eq!(
            get("missing").parent.as_deref(),
            Some(reference.id.as_str())
        );
        assert!(get("bad").parent.is_none());
        assert!(get("incomplete").parent.is_none());
        assert!(get("a").parent.is_none());
        assert_eq!(get("b").parent.as_deref(), Some("a"));
        assert!(!serde_json::to_string(&result).unwrap().contains("SECRET"));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert_eq!(
            crate::integration::agent_thread_link(remote, None).as_deref(),
            Some("t3code://threads/machine-b/parent")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn synthetic_runs_preserve_other_day_and_ongoing_timing() {
        let directory = path("timing-dir");
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("synthetic.sqlite");
        let db = Connection::open(&file).unwrap();
        schema(&db);
        for (id, status, start, end) in [
            (
                "done",
                "completed",
                "2026-11-10T23:50:00Z",
                Some("2026-11-11T00:12:00Z"),
            ),
            ("live", "running", "2026-11-12T13:51:00Z", None),
            ("unknown", "idle", "2026-11-12T13:51:00Z", None),
        ] {
            db.execute(
                &format!("INSERT INTO {PREFIX}threads VALUES(?,?,NULL,NULL)"),
                [id, &json!({"id":id}).to_string()],
            )
            .unwrap();
            db.execute(&format!("INSERT INTO {PREFIX}runs VALUES(?,1,?,?,?)"),rusqlite::params![id,json!({"id":id,"threadId":id,"status":status,"startedAt":start,"completedAt":end}).to_string(),start,end]).unwrap();
        }
        drop(db);
        let now = model::ms(Some("2026-11-12T14:30:00Z")).unwrap();
        let snap = snapshot(&file, now).unwrap();
        let get = |id: &str| snap.agents.iter().find(|a| a.id == id).unwrap();
        let done = get("done");
        assert_eq!(crate::ui::run_length(done, now, false), "22m");
        assert_eq!(crate::ui::run_length(done, now + 86400000., false), "22m");
        let live = get("live");
        assert_eq!(crate::ui::run_length(live, now, true), "39m");
        assert_eq!(crate::ui::run_length(live, now + 60000., true), "40m");
        assert_eq!(crate::ui::run_length(get("unknown"), now, false), "");
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn sql_cutoff_clock_roundtrips_utc_including_negative_epoch() {
        for stamp in [0., -1., 1_794_484_800_123., 1_794_484_800_123. - WINDOW] {
            assert_eq!(model::ms(Some(&iso(stamp))), Some(stamp));
        }
    }
}

/// Project a database constructed entirely from the authored demo scenario.
/// Kept crate-private so live callers still require the read-only path API.
pub(crate) fn synthetic_snapshot(db: &Connection, now: f64) -> Result<Snapshot, SourceError> {
    let path = Path::new("synthetic:demo");
    db.execute_batch("PRAGMA query_only=ON; BEGIN")
        .map_err(|e| db_error(path, e))?;
    let result = read(db, path, now);
    let rollback = db.execute_batch("ROLLBACK").map_err(|e| db_error(path, e));
    match result {
        Ok(value) => {
            rollback?;
            Ok(value)
        }
        Err(error) => {
            let _ = rollback;
            Err(error)
        }
    }
}

pub(crate) fn deadline(events: &[Json], spans: &[Json], now: f64) -> f64 {
    deadline_stamps(
        events
            .iter()
            .filter_map(|v| {
                v.get("updated_at")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_str().unwrap_or(""))
            })
            .chain(spans.iter().filter_map(|v| {
                v.get("completed_at")
                    .filter(|v| !v.is_null())
                    .map(|v| v.as_str().unwrap_or(""))
            })),
        now,
    )
}
pub(crate) fn deadline_stamps<'a>(stamps: impl Iterator<Item = &'a str>, now: f64) -> f64 {
    let mut valid_until = f64::INFINITY;
    for text in stamps {
        let deadline = model::ms(Some(text))
            .filter(|stamp| iso(*stamp) == text && text.len() == 24)
            .map(|stamp| stamp + WINDOW + 1.)
            .unwrap_or(now);
        valid_until = valid_until.min(deadline);
    }
    if iso(now - WINDOW).len() != 24 {
        valid_until = now;
    }
    valid_until
}
