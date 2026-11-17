//! Scale lab: `--bench-source N` times [`crate::source::snapshot`] the way the
//! product calls it — the TypeScript Source opens a fresh read-only connection
//! for every snapshot, so each timed operation here does too. Phase timing comes
//! from [`crate::source::timing`] and is enabled only in this mode.
//!
//! Synthetic databases only: the path must be given explicitly with `--db`.
use crate::source::{self, timing};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, time::Instant};

/// Copy of source.rs's private `iso` (it must not change for this bench).
fn probe_iso(stamp: f64) -> String {
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

/// djb2 over agent ids in order; the same hash as checks/bench/source-baseline.ts.
fn agent_hash(snapshot: &crate::model::Snapshot) -> String {
    let mut h: u32 = 5381;
    for a in &snapshot.agents {
        for b in a.id.bytes() {
            h = h.wrapping_mul(33) ^ u32::from(b);
        }
    }
    format!("{h:08x}")
}

fn checksum(snapshot: &crate::model::Snapshot) -> serde_json::Value {
    json!({
        "agents": snapshot.agents.len(),
        "events": snapshot.agents.iter().map(|a| a.events.len()).sum::<usize>(),
        "items": snapshot.agents.iter().map(|a| a.items.len()).sum::<usize>(),
        "history": snapshot.agents.iter().map(|a| a.history.len()).sum::<usize>(),
        "agent_hash": agent_hash(snapshot),
    })
}

fn median(trials: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = trials.iter().copied().filter(|t| *t >= 0.).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted.get(sorted.len() / 2).copied().unwrap_or(-1.)
}

pub fn source(
    db: Option<String>,
    now_ms: Option<String>,
    n: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let db = db.ok_or("--bench-source needs --db PATH")?;
    let now: f64 = now_ms
        .ok_or("--bench-source needs --now-ms N")?
        .parse()?;
    if n == 0 || n > 100_000 {
        return Err("--bench-source iterations must be 1..100000".into());
    }
    let path = Path::new(&db);
    let sqlite_version = {
        let probe = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        probe.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0))?
    };
    timing::enable(true);
    let mut errors = 0u64;
    for _ in 0..2 {
        if source::snapshot(path, now).is_err() {
            errors += 1;
        }
    }
    timing::reset();
    let mut trials = Vec::with_capacity(n);
    let mut consumed = 0u64;
    let mut last = None;
    let mut first_error = None;
    for _ in 0..n {
        let start = Instant::now();
        match source::snapshot(path, now) {
            Ok(snap) => {
                trials.push(start.elapsed().as_secs_f64() * 1000.);
                consumed += std::hint::black_box(snap.agents.len()) as u64;
                last = Some(snap);
            }
            Err(e) => {
                trials.push(-1.);
                errors += 1;
                first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    if let Some(error) = &first_error {
        if last.is_none() {
            println!(
                "{}",
                json!({"implementation":"rust-t3-agents-core","path":db,"operations":n,"errors":errors,"first_error":error})
            );
            return Err("every snapshot failed".into());
        }
    }
    let last = last.ok_or("every snapshot failed")?;
    let phases = timing::take();
    let good: Vec<f64> = trials.iter().copied().filter(|t| *t >= 0.).collect();
    let mean = good.iter().sum::<f64>() / good.len() as f64;
    println!(
        "{}",
        json!({
            "implementation": "rust-t3-agents-core",
            "sqlite_version": sqlite_version,
            "path": db,
            "now": now,
            "operations": n,
            "warmup": 2,
            "errors": errors,
            "trials_ms": trials,
            "ms_per_op": mean,
            "median_ms": median(&trials),
            "min_ms": good.iter().copied().reduce(f64::min).unwrap_or(-1.),
            "max_ms": good.iter().copied().reduce(f64::max).unwrap_or(-1.),
            "consumed_checksum": consumed,
            "checksum": checksum(&last),
            "phases": phases,
        })
    );
    Ok(())
}

// ── Incremental-reader probes (`--bench-probe-incremental`) ─────────────────
//
// The same probes as checks/bench/incremental-probes.ts, through rusqlite
// on a kept-open read-only connection: PRAGMA data_version, the metadata
// last_sequence read, the changed-threads event-log scan (DISTINCT and raw),
// and the reader's query set restricted to 1, 1% and 10% of threads.
//
// Privacy boundary, same as the TypeScript probe: the event log is read as
// sequence/thread_id/event_type only (payloads are never selected) and the
// metadata table as projection_name/last_sequence only. Projection tables are
// queried with the same SQL text as crate::source (duplicated here; keep in
// sync — source.rs must not change for this bench).

/// Copy of source.rs's static projection (it stays private there).
fn probe_projection(table: &str) -> String {
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

const PROBE_ITEM_FIELDS: &str = "thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,
 json_extract(payload_json,'$.startedAt') AS started_at,
 json_extract(payload_json,'$.toolName') AS tool,
 json_extract(payload_json,'$.exitCode') AS exit_code,
 substr(COALESCE(json_extract(payload_json,'$.title'),json_extract(payload_json,'$.toolName'),json_extract(payload_json,'$.label'),''),1,240) AS label,
 CASE WHEN type='command_execution' THEN substr(json_extract(payload_json,'$.input'),1,240) ELSE '' END AS command,
 CASE WHEN type='file_change' THEN substr(json_extract(payload_json,'$.fileName'),1,240) ELSE '' END AS file";
const PROBE_TOOLISH: &str = "'command_execution','dynamic_tool','file_change','web_search','subagent','handoff','notification','user_input_request','secret_request','link_request','reasoning','assistant_message'";

fn probe_stats(trials: &mut Vec<f64>) -> serde_json::Value {
    trials.sort_by(|a, b| a.total_cmp(b));
    json!({
        "ms_median": trials.get(trials.len() / 2).copied().unwrap_or(-1.),
        "ms_min": trials.first().copied().unwrap_or(-1.),
        "ms_max": trials.last().copied().unwrap_or(-1.),
    })
}

fn probe_time(reps: usize, mut f: impl FnMut() -> usize) -> (serde_json::Value, usize) {
    let mut rows = f(); // warm
    let mut trials = Vec::with_capacity(reps);
    for _ in 0..reps {
        let start = Instant::now();
        rows = f();
        trials.push(start.elapsed().as_secs_f64() * 1000.);
    }
    let mut stats = probe_stats(&mut trials);
    stats["rows"] = json!(rows);
    (stats, rows)
}

fn probe_plan(db: &Connection, sql: &str, args: &[SqlValue]) -> Vec<String> {
    let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    stmt.query_map(params_from_iter(args.iter()), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn probe_all(db: &Connection, sql: &str, args: &[SqlValue]) -> Vec<serde_json::Value> {
    // prepare_cached: a kept-open incremental reader prepares once and reuses.
    let mut stmt = db.prepare_cached(sql).unwrap();
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt
        .query_map(params_from_iter(args.iter()), |row| {
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
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

/// The reader's thread-scoped queries with `ids` bound, mirroring source::read.
/// Returns rows read; `per_query` accumulates ms per label when given.
fn probe_read_threads(
    db: &Connection,
    prefix: &str,
    ids: &[String],
    now: f64,
    mut per_query: Option<&mut HashMap<String, f64>>,
) -> usize {
    let marks = |n: usize| {
        if n == 0 {
            "NULL".to_string()
        } else {
            vec!["?"; n].join(",")
        }
    };
    let texts = |v: &[String]| v.iter().cloned().map(SqlValue::Text).collect::<Vec<_>>();
    let where_ids = format!("WHERE thread_id IN ({})", marks(ids.len()));
    let mut rows = 0;
    let mut run = |label: &str, sql: &str, args: &[SqlValue], parse: bool| -> Vec<serde_json::Value> {
        let start = Instant::now();
        let out = probe_all(db, sql, args);
        if parse {
            for row in &out {
                for key in ["payload", "payload_json"] {
                    if let Some(text) = row.get(key).and_then(Value::as_str) {
                        let _ = serde_json::from_str::<Value>(text);
                    }
                }
            }
        }
        if let Some(pq) = per_query.as_deref_mut() {
            *pq.entry(label.to_string()).or_default() += start.elapsed().as_secs_f64() * 1000.;
        }
        rows += out.len();
        out
    };
    let params = texts(ids);
    run(
        "threads",
        &format!(
            "SELECT {} AS payload FROM {prefix}threads {where_ids} AND deleted_at IS NULL AND archived_at IS NULL",
            probe_projection("threads")
        ),
        &params,
        true,
    );
    let runs = run(
        "runs",
        &format!(
            "SELECT {} AS payload FROM {prefix}runs {where_ids} ORDER BY ordinal DESC",
            probe_projection("runs")
        ),
        &params,
        true,
    );
    let subs = run(
        "subagents",
        &format!(
            "SELECT {} AS payload FROM {prefix}subagents {where_ids}",
            probe_projection("subagents")
        ),
        &params,
        true,
    );
    run(
        "provider_threads",
        &format!(
            "SELECT {} AS payload FROM {prefix}provider_threads {where_ids}",
            probe_projection("provider_threads")
        ),
        &params,
        true,
    );
    run(
        "latest_turns",
        &format!(
            "SELECT {} AS payload_json FROM (SELECT payload_json,ROW_NUMBER() OVER(PARTITION BY provider_thread_id ORDER BY ordinal DESC,rowid DESC) AS rank FROM {prefix}provider_turns {where_ids}) WHERE rank=1",
            probe_projection("provider_turns")
        ),
        &params,
        true,
    );
    run(
        "nodes",
        &format!("SELECT node_id,parent_node_id,run_id FROM {prefix}nodes {where_ids}"),
        &params,
        false,
    );
    run(
        "requests",
        &format!("SELECT thread_id,node_id,kind,status,created_at FROM {prefix}runtime_requests {where_ids} AND resolved_at IS NULL AND status IN ('pending','open','waiting','requested')"),
        &params,
        false,
    );
    let mut latest: HashMap<String, String> = HashMap::new();
    let mut latest_order: Vec<String> = Vec::new();
    for row in &runs {
        let p: Value = serde_json::from_str(row["payload"].as_str().unwrap_or("")).unwrap();
        let tid = p["threadId"].as_str().unwrap_or("").to_string();
        if !tid.is_empty() && !latest.contains_key(&tid) {
            latest_order.push(tid.clone());
            latest.insert(tid, p["id"].as_str().unwrap_or("").to_string());
        }
    }
    let mut run_ids: Vec<String> = Vec::new();
    for tid in &latest_order {
        if let Some(id) = latest.get(tid) {
            if !id.is_empty() && !run_ids.contains(id) {
                run_ids.push(id.clone());
            }
        }
    }
    for row in &subs {
        let p: Value = serde_json::from_str(row["payload"].as_str().unwrap_or("")).unwrap();
        if let Some(id) = p["runId"].as_str().filter(|s| !s.is_empty()) {
            if !run_ids.contains(&id.to_string()) {
                run_ids.push(id.to_string());
            }
        }
    }
    let run_marks = marks(run_ids.len());
    let runless: Vec<String> = ids.iter().filter(|id| !latest.contains_key(*id)).cloned().collect();
    let mut item_params = texts(&run_ids);
    item_params.extend(texts(&runless));
    run(
        "items",
        &format!(
            "SELECT {PROBE_ITEM_FIELDS} FROM {prefix}turn_items WHERE (run_id IN ({run_marks}) OR (run_id IS NULL AND thread_id IN ({}))) AND type IN ({PROBE_TOOLISH}) ORDER BY updated_at DESC,ordinal DESC LIMIT 3000",
            marks(runless.len())
        ),
        &item_params,
        false,
    );
    let since = probe_iso(now - 10_800_000.);
    let mut since_params = params.clone();
    since_params.push(SqlValue::Text(since.clone()));
    run(
        "events",
        &format!("SELECT {PROBE_ITEM_FIELDS} FROM {prefix}turn_items {where_ids} AND updated_at>=? AND type NOT IN ('checkpoint','todo_list') ORDER BY updated_at DESC LIMIT 8000"),
        &since_params,
        false,
    );
    run(
        "history",
        &format!("SELECT provider_thread_id,thread_id,started_at,completed_at,json_extract(payload_json,'$.tokenUsage.usedTokens') AS used,json_extract(payload_json,'$.tokenUsage.maxTokens') AS max,json_extract(payload_json,'$.model') AS model FROM (SELECT *,ROW_NUMBER() OVER(PARTITION BY provider_thread_id ORDER BY ordinal DESC) AS rank FROM {prefix}provider_turns {where_ids}) WHERE rank<=160"),
        &params,
        false,
    );
    run(
        "spans",
        &format!("SELECT thread_id,requested_at,completed_at,json_extract(payload_json,'$.startedAt') AS started_at FROM {prefix}runs {where_ids} AND (completed_at IS NULL OR completed_at>=?)"),
        &since_params,
        false,
    );
    run(
        "plans",
        &format!("SELECT thread_id,run_id,node_id,provider_thread_id,updated_at,json_extract(payload_json,'$.steps') AS steps FROM {prefix}turn_items WHERE type='todo_list' AND run_id IN ({run_marks}) ORDER BY updated_at DESC"),
        &texts(&run_ids),
        false,
    );
    rows
}

pub fn probe_incremental(
    db: Option<String>,
    now_ms: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db.ok_or("--bench-probe-incremental needs --db PATH")?;
    let now: f64 = now_ms
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(1_794_484_800_000.);
    let path = Path::new(&db_path);
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.execute_batch("PRAGMA query_only=ON")?;
    let prefix = "orchestration_v2_projection_";
    let (cheap, mid, slow) = (2000usize, 50usize, 10usize);

    let mut dv_stmt = conn.prepare("PRAGMA data_version")?;
    let (data_version, _) = probe_time(cheap, || {
        dv_stmt.query_row([], |r| r.get::<_, i64>(0)).unwrap();
        0
    });

    let mut meta_stmt = conn.prepare(
        "SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='thread-projections'",
    )?;
    let (last_sequence, max_seq_rows) = probe_time(cheap, || {
        meta_stmt.query_row([], |r| r.get::<_, i64>(0)).unwrap() as usize
    });
    let max_seq = max_seq_rows as i64;

    let changed_sql = "SELECT DISTINCT thread_id FROM orchestration_v2_events WHERE sequence > ?";
    let changed_raw_sql = "SELECT thread_id FROM orchestration_v2_events WHERE sequence > ?";
    let mut changed = serde_json::Map::new();
    for depth in [1i64, 100, 10_000] {
        let from = max_seq - depth;
        let (distinct, distinct_rows) = probe_time(mid, || {
            probe_all(&conn, changed_sql, &[SqlValue::Integer(from)]).len()
        });
        let _ = distinct_rows;
        let mut entry = distinct;
        entry["plan"] = json!(probe_plan(&conn, changed_sql, &[SqlValue::Integer(from)]));
        changed.insert(format!("{depth}_events"), entry);
        let (raw, raw_rows) = probe_time(mid, || {
            let rows = probe_all(&conn, changed_raw_sql, &[SqlValue::Integer(from)]);
            let set: std::collections::HashSet<_> = rows
                .iter()
                .filter_map(|r| r["thread_id"].as_str())
                .collect();
            // rows scanned vs threads changed: report the deduped count
            let _ = rows.len();
            set.len()
        });
        let mut entry = raw;
        entry["rows"] = json!(raw_rows);
        entry["plan"] = json!(probe_plan(&conn, changed_raw_sql, &[SqlValue::Integer(from)]));
        changed.insert(format!("{depth}_events_no_distinct"), entry);
    }

    let all_ids: Vec<String> = probe_all(
        &conn,
        &format!("SELECT thread_id FROM {prefix}threads ORDER BY thread_id"),
        &[],
    )
    .iter()
    .filter_map(|r| r["thread_id"].as_str().map(String::from))
    .collect();
    let pct1_step = (all_ids.len() as f64 / (all_ids.len() as f64 * 0.01).ceil().max(1.)).round().max(1.) as usize;
    let sets: Vec<(&str, Vec<String>)> = vec![
        ("one", vec![all_ids[all_ids.len() / 2].clone()]),
        ("pct1", all_ids.iter().step_by(pct1_step).cloned().collect()),
        ("pct10", all_ids.iter().step_by(10).cloned().collect()),
    ];
    let mut reread = serde_json::Map::new();
    for (name, ids) in &sets {
        let mut per_query = HashMap::new();
        let mut per_query_opt = (name == &"one").then_some(&mut per_query);
        let (stats, rows) = probe_time(slow, || {
            probe_read_threads(&conn, prefix, ids, now, per_query_opt.as_mut().map(|p| &mut **p))
        });
        let _ = rows;
        let mut entry = stats;
        entry["threads"] = json!(ids.len());
        if name == &"one" {
            let per: HashMap<String, f64> = per_query.into_iter().map(|(k, v)| (k, v / slow as f64)).collect();
            entry["per_query_ms_total"] = json!(per);
        }
        reread.insert(name.to_string(), entry);
    }

    let cutoff = probe_iso(now - 2000.);
    let scan_sql = format!("SELECT DISTINCT thread_id FROM {prefix}turn_items WHERE updated_at >= ?");
    let (updated_scan, _) = probe_time(mid, || {
        probe_all(&conn, &scan_sql, &[SqlValue::Text(cutoff.clone())]).len()
    });
    let mut updated_scan = updated_scan;
    updated_scan["plan"] = json!(probe_plan(&conn, &scan_sql, &[SqlValue::Text(cutoff.clone())]));
    updated_scan["cutoff"] = json!(cutoff);

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "implementation": "rust-t3-agents-core",
            "db": db_path,
            "now": now,
            "threads": all_ids.len(),
            "max_sequence": max_seq,
            "reps": { "cheap": cheap, "mid": mid, "slow": slow },
            "data_version": data_version,
            "last_sequence": last_sequence,
            "changed_threads": changed,
            "reread": reread,
            "updated_at_scan": updated_scan,
        }))?
    );
    Ok(())
}

/// `--bench-reader-poll`: drive the incremental reader from outside. Each line on stdin asks
/// for one poll; one JSON line comes back saying how the poll was served and what it cost.
/// A driver changes the database between lines, so the timed region is the poll alone. With
/// `--self-check` every poll also runs the full reader in the same transaction and reports
/// whether the two agreed (that cost is in `self_check_ms`, outside `poll_ms`).
pub fn reader_poll(
    db: Option<String>,
    now: Option<String>,
    self_check: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, Write};
    let path = db.ok_or("--bench-reader-poll needs --db PATH")?;
    let now: Option<f64> = now.map(|v| v.parse()).transpose()?;
    let mut reader = crate::live::Reader::open(&path)?.with_self_check(self_check);
    // The status watcher's reader: agents as `status_view`, no item text read.
    if std::env::var_os("T3_AGENTS_STATUS_ONLY").is_some() {
        reader = reader.status_only();
    }
    // Explicit opt-in, scoped to this headless benchmark process.
    let profile = std::env::var_os("T3_AGENTS_READER_POLL_PROFILE").is_some();
    crate::source::timing::enable(profile);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        // A line that holds a number moves the poll clock to it (the soak harness drives the
        // stream's clock this way); an empty line keeps the clock the process started with.
        let at = line
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .or(now)
            .unwrap_or_else(crate::clock::wall_now);
        if profile {
            crate::source::timing::reset();
        }
        let started = Instant::now();
        let update = reader.poll(at)?;
        let elapsed = started.elapsed();
        // The product's worker does the same after a full read; excluded from the timing.
        if update.served == crate::live::Served::Full {
            crate::clock::release_memory();
        }
        // Diagnosis on synthetic databases: keep the snapshot that disagreed with the full
        // reader, so the two can be compared field by field.
        if update.mismatch {
            if let Some(dir) = std::env::var_os("T3_AGENTS_DUMP_MISMATCH") {
                let file = std::path::Path::new(&dir).join(format!("mismatch-{}.json", reader.mismatches()));
                std::fs::write(file, serde_json::to_vec(&update.snapshot)?)?;
            }
        }
        let changed = match &update.changed {
            crate::model::Changed::Everything => -1,
            crate::model::Changed::Agents(ids) => ids.len() as i64,
        };
        let c = &update.counters;
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.;
        let account = std::env::var_os("T3_AGENTS_CACHE_ACCOUNT")
            .is_some()
            .then(|| reader.cache_account())
            .flatten();
        let heap = std::env::var_os("T3_AGENTS_HEAP_ACCOUNT")
            .is_some()
            .then(crate::live::heap_stats)
            .flatten();
        writeln!(
            out,
            "{}",
            json!({
                "served": format!("{:?}", update.served),
                "fallback_reason": c.fallback_reason.map(|r| format!("{r:?}")),
                "poll_ms": ms(elapsed) - ms(c.self_check),
                "self_check_ms": ms(c.self_check),
                "read_ms": ms(c.read),
                "profile": c.phases,
                "reconcile_ms": ms(c.reconcile),
                "rows_read": c.rows_read,
                "threads_reread": c.threads_reread,
                "agents": update.snapshot.agents.len(),
                "changed": changed,
                "mismatch": update.mismatch,
                "mismatches": reader.mismatches(),
                "counters": format!("{c:?}"),
                "cache_account": account,
                "heap": heap,
            })
        )?;
        out.flush()?;
    }
    // At end of input, optionally take the reader apart and say what each structure held.
    if std::env::var_os("T3_AGENTS_HEAP_ACCOUNT").is_some() {
        let ledger = reader.heap_teardown();
        writeln!(out, "{}", json!({ "heap_teardown": ledger }))?;
        out.flush()?;
    }
    Ok(())
}
