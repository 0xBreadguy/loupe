// Incremental-reader cost probes, on a kept-open read-only connection, against a
// real-schema (t3) scale database. Measures the pieces an incremental reader
// would pay per poll, so the per-poll estimate can be built from measured parts:
//
//   node checks/bench/incremental-probes.ts --db PATH [--now-ms N] [--reps N] [--slow-reps N]
//
// Probes:
//   data_version    PRAGMA data_version on the kept-open connection
//   last_sequence   SELECT last_sequence FROM orchestration_v2_projection_metadata
//   changed_threads SELECT DISTINCT thread_id FROM orchestration_v2_events WHERE sequence > ?
//                   for 1, 100 and 10,000 new events (with EXPLAIN QUERY PLAN)
//   reread          the reader's query set restricted to 1 thread, 1% and 10%
//                   of threads (per-query timing and plans for the 1-thread case)
//   updated_at_scan the change-detection alternative: DISTINCT thread_id FROM
//                   turn_items WHERE updated_at >= <now - 2s> (with plan)
//
// Privacy boundary: the event log is read as sequence/thread_id/event_type only
// (event payloads are never selected); the metadata table as projection_name and
// last_sequence only. Projection tables are read exactly the way
// oracle/src/source.ts reads them — the probes replay its query shapes with
// the ids of the measured thread set bound.
import { performance } from "node:perf_hooks"
import { DatabaseSync, type StatementSync } from "node:sqlite"
import { PREFIX, projection } from "../../oracle/src/source.ts"

const args = process.argv.slice(2)
function arg(name: string, fallback?: string): string {
  const i = args.indexOf(name)
  if (i < 0 && fallback !== undefined) return fallback
  if (i < 0 || args[i + 1] === undefined) throw new Error(`missing ${name}`)
  return args[i + 1]
}
const path = arg("--db")
const now = Number(arg("--now-ms", "1794484800000"))
const REPS = Number(arg("--reps", "2000")) // cheap probes
const MID_REPS = Number(arg("--mid-reps", "50")) // changed-thread scans
const SLOW_REPS = Number(arg("--slow-reps", "10")) // re-reads and full scans

// Keep in sync with oracle/src/source.ts FIELDS (not exported there).
const FIELDS: Record<string, Record<string, true | string[]>> = {
  threads: { id: true, projectId: true, title: true, activeProviderThreadId: true, worktreePath: true, branch: true,
    updatedAt: true, settledOverride: true, snoozedUntil: true,
    lineage: ["relationshipToParent", "parentThreadId", "sideChat"],
    delegatedFrom: ["environmentId", "threadId", "title"], modelSelection: ["model", "options"] },
  runs: { id: true, threadId: true, status: true, providerThreadId: true, startedAt: true, requestedAt: true,
    completedAt: true, modelSelection: ["model", "options"] },
  subagents: { id: true, threadId: true, parentNodeId: true, runId: true, providerThreadId: true, childThreadId: true,
    status: true, title: true, model: true, startedAt: true, completedAt: true, updatedAt: true,
    remoteChild: ["environmentId", "threadId", "label"] },
  provider_threads: { id: true, contextUsage: true },
  provider_turns: { providerThreadId: true, model: true, tokenUsage: true, turnTokenUsage: true },
}
const TOOLISH = ["command_execution", "dynamic_tool", "file_change", "web_search", "subagent", "handoff",
  "notification", "user_input_request", "secret_request", "link_request"]
const ITEM_FIELDS = `thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,
    json_extract(payload_json,'$.startedAt') AS started_at,
    json_extract(payload_json,'$.toolName') AS tool,
    json_extract(payload_json,'$.exitCode') AS exit_code,
    substr(COALESCE(json_extract(payload_json,'$.title'), json_extract(payload_json,'$.toolName'),
      json_extract(payload_json,'$.label'),''),1,240) AS label,
    CASE WHEN type='command_execution' THEN substr(json_extract(payload_json,'$.input'),1,240) ELSE '' END AS command,
    CASE WHEN type='file_change' THEN substr(json_extract(payload_json,'$.fileName'),1,240) ELSE '' END AS file`

const db = new DatabaseSync(path, { readOnly: true, timeout: 250 })
db.exec("PRAGMA query_only=ON")
// A kept-open incremental reader prepares its statements once and reuses them
// across polls; the probe does the same (the full-snapshot baseline pays
// prepare per snapshot because it opens a fresh connection every time).
const stmtCache = new Map<string, StatementSync>()
const cached = (sql: string) => {
  let s = stmtCache.get(sql)
  if (!s) stmtCache.set(sql, (s = db.prepare(sql)))
  return s
}

function time(label: string, reps: number, fn: () => unknown): { ms_median: number; ms_min: number; ms_max: number; rows: number } {
  const trials: number[] = []
  let rows = 0
  fn() // warm
  for (let i = 0; i < reps; i++) {
    const t = performance.now()
    const out = fn()
    trials.push(performance.now() - t)
    if (Array.isArray(out)) rows = out.length
    else if (typeof out === "number") rows = out
  }
  trials.sort((a, b) => a - b)
  return {
    ms_median: Math.round(trials[Math.floor(trials.length / 2)] * 1000) / 1000,
    ms_min: Math.round(trials[0] * 1000) / 1000,
    ms_max: Math.round(trials[trials.length - 1] * 1000) / 1000,
    rows,
  }
}
const plan = (sql: string, ...params: (string | number)[]) =>
  (db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...params) as { detail: string }[]).map((r) => r.detail)

// ── A: PRAGMA data_version ──────────────────────────────────────────────────
const dvStmt = db.prepare("PRAGMA data_version")
const dataVersion = time("data_version", REPS, () => (dvStmt.get() as { data_version: number }).data_version)

// ── B: last_sequence ────────────────────────────────────────────────────────
const metaStmt = db.prepare("SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='thread-projections'")
const lastSequence = time("last_sequence", REPS, () => (metaStmt.get() as { last_sequence: number }).last_sequence)
const maxSeq = (metaStmt.get() as { last_sequence: number }).last_sequence

// ── C: changed threads from the event log ───────────────────────────────────
const changedSql = "SELECT DISTINCT thread_id FROM orchestration_v2_events WHERE sequence > ?"
const changedStmt = db.prepare(changedSql)
// Without DISTINCT the primary key serves a range scan and the reader dedupes
// host-side; compare the two plans.
const changedRawSql = "SELECT thread_id FROM orchestration_v2_events WHERE sequence > ?"
const changedRawStmt = db.prepare(changedRawSql)
const changed: Record<string, unknown> = {}
for (const depth of [1, 100, 10_000]) {
  const from = maxSeq - depth
  changed[`${depth}_events`] = {
    ...time(`changed_${depth}`, MID_REPS, () => changedStmt.all(from) as { thread_id: string }[]),
    plan: plan(changedSql, from),
  }
  changed[`${depth}_events_no_distinct`] = {
    ...time(`changed_raw_${depth}`, MID_REPS, () => {
      const rows = changedRawStmt.all(from) as { thread_id: string }[]
      return new Set(rows.map((r) => r.thread_id)).size
    }),
    plan: plan(changedRawSql, from),
  }
}

// ── D: re-reading a subset of threads with the reader's own queries ─────────
const allIds = (db.prepare(`SELECT thread_id FROM ${PREFIX}threads ORDER BY thread_id`).all() as { thread_id: string }[]).map((r) => r.thread_id)
const pickEvery = (n: number) => allIds.filter((_, i) => i % n === 0)
const sets: Record<string, string[]> = {
  one: [allIds[Math.floor(allIds.length / 2)]],
  pct1: pickEvery(Math.max(1, Math.round(allIds.length / Math.max(1, Math.ceil(allIds.length * 0.01))))),
  pct10: pickEvery(10),
}

/** The reader's thread-scoped queries with `ids` bound, mirroring source.ts read(). */
function readThreads(ids: string[], perQuery?: Map<string, number>): number {
  const marks = ids.map(() => "?").join(",")
  const where = `WHERE thread_id IN (${marks})`
  let rows = 0
  const run = <T>(label: string, sql: string, params: (string | number)[]): T[] => {
    const t = perQuery ? performance.now() : 0
    const out = cached(sql).all(...params) as T[]
    if (perQuery) perQuery.set(label, (perQuery.get(label) ?? 0) + performance.now() - t)
    rows += out.length
    return out
  }
  run<{ payload: string }>("threads", `SELECT ${projection(FIELDS.threads)} AS payload FROM ${PREFIX}threads ${where} AND deleted_at IS NULL AND archived_at IS NULL`, ids)
    .forEach((r) => JSON.parse(r.payload))
  const runs = run<{ payload: string }>("runs", `SELECT ${projection(FIELDS.runs)} AS payload FROM ${PREFIX}runs ${where} ORDER BY ordinal DESC`, ids)
    .map((r) => JSON.parse(r.payload))
  const subs = run<{ payload: string }>("subagents", `SELECT ${projection(FIELDS.subagents)} AS payload FROM ${PREFIX}subagents ${where}`, ids)
    .map((r) => JSON.parse(r.payload))
  run<{ payload: string }>("provider_threads", `SELECT ${projection(FIELDS.provider_threads)} AS payload FROM ${PREFIX}provider_threads ${where}`, ids)
    .forEach((r) => JSON.parse(r.payload))
  run<{ payload_json: string }>("latest_turns", `SELECT ${projection(FIELDS.provider_turns)} AS payload_json FROM (
      SELECT payload_json, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC, rowid DESC) AS rank
      FROM ${PREFIX}provider_turns ${where}) WHERE rank=1`, ids)
    .forEach((r) => JSON.parse(r.payload_json))
  run("nodes", `SELECT node_id,parent_node_id,run_id FROM ${PREFIX}nodes ${where}`, ids)
  run("requests", `SELECT thread_id,node_id,kind,status,created_at FROM ${PREFIX}runtime_requests ${where}
      AND resolved_at IS NULL AND status IN ('pending','open','waiting','requested')`, ids)
  const latest = new Map<string, { id?: string }>()
  for (const r of runs) if (!latest.has(r.threadId)) latest.set(r.threadId, r)
  const runIds = [...new Set([...latest.values()].map((r) => r.id).concat(subs.map((s) => s.runId)).filter(Boolean))] as string[]
  const runMarks = runIds.map(() => "?").join(",") || "NULL"
  const runless = ids.filter((id) => !latest.has(id))
  const runlessMarks = runless.map(() => "?").join(",") || "NULL"
  run("items", `SELECT ${ITEM_FIELDS} FROM ${PREFIX}turn_items
      WHERE (run_id IN (${runMarks}) OR (run_id IS NULL AND thread_id IN (${runlessMarks})))
        AND type IN (${[...TOOLISH, "reasoning", "assistant_message"].map((t) => `'${t}'`).join(",")})
      ORDER BY updated_at DESC, ordinal DESC LIMIT 3000`, [...runIds, ...runless])
  const since = new Date(now - 3 * 3600_000).toISOString()
  run("events", `SELECT ${ITEM_FIELDS} FROM ${PREFIX}turn_items ${where} AND updated_at >= ? AND type NOT IN ('checkpoint','todo_list')
      ORDER BY updated_at DESC LIMIT 8000`, [...ids, since])
  run("history", `SELECT provider_thread_id,thread_id,started_at,completed_at,
      json_extract(payload_json,'$.tokenUsage.usedTokens') AS used,
      json_extract(payload_json,'$.tokenUsage.maxTokens') AS max,
      json_extract(payload_json,'$.model') AS model
    FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC) AS rank
      FROM ${PREFIX}provider_turns ${where}) WHERE rank <= 160`, ids)
  run("spans", `SELECT thread_id,requested_at,completed_at,json_extract(payload_json,'$.startedAt') AS started_at
      FROM ${PREFIX}runs ${where} AND (completed_at IS NULL OR completed_at >= ?)`, [...ids, since])
  run("plans", `SELECT thread_id,run_id,node_id,provider_thread_id,updated_at,json_extract(payload_json,'$.steps') AS steps
      FROM ${PREFIX}turn_items WHERE type='todo_list' AND run_id IN (${runMarks}) ORDER BY updated_at DESC`, runIds)
  return rows
}

const reread: Record<string, unknown> = {}
for (const [name, ids] of Object.entries(sets)) {
  const perQuery = new Map<string, number>()
  const total = time(`reread_${name}`, SLOW_REPS, () => readThreads(ids, name === "one" ? perQuery : undefined))
  const entry: Record<string, unknown> = { threads: ids.length, ...total }
  if (name === "one") {
    entry.per_query_ms_total = Object.fromEntries([...perQuery.entries()].map(([k, v]) => [k, Math.round((v / SLOW_REPS) * 1000) / 1000]))
    const id = ids[0]
    const runIds = (db.prepare(`SELECT run_id FROM ${PREFIX}runs WHERE thread_id=?`).all(id) as { run_id: string }[]).map((r) => r.run_id)
    const rm = runIds.map(() => "?").join(",") || "NULL"
    const since = new Date(now - 3 * 3600_000).toISOString()
    entry.plans = {
      threads: plan(`SELECT payload_json FROM ${PREFIX}threads WHERE thread_id IN (?) AND deleted_at IS NULL AND archived_at IS NULL`, id),
      runs: plan(`SELECT payload_json FROM ${PREFIX}runs WHERE thread_id IN (?) ORDER BY ordinal DESC`, id),
      subagents: plan(`SELECT payload_json FROM ${PREFIX}subagents WHERE thread_id IN (?)`, id),
      provider_threads: plan(`SELECT payload_json FROM ${PREFIX}provider_threads WHERE thread_id IN (?)`, id),
      latest_turns: plan(`SELECT payload_json FROM (SELECT payload_json, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC, rowid DESC) AS rank FROM ${PREFIX}provider_turns WHERE thread_id IN (?)) WHERE rank=1`, id),
      nodes: plan(`SELECT node_id,parent_node_id,run_id FROM ${PREFIX}nodes WHERE thread_id IN (?)`, id),
      requests: plan(`SELECT thread_id,node_id,kind,status,created_at FROM ${PREFIX}runtime_requests WHERE thread_id IN (?) AND resolved_at IS NULL AND status IN ('pending','open','waiting','requested')`, id),
      items: plan(`SELECT thread_id FROM ${PREFIX}turn_items WHERE (run_id IN (${rm}) OR (run_id IS NULL AND thread_id IN (?))) AND type IN ('command_execution','reasoning') ORDER BY updated_at DESC, ordinal DESC LIMIT 3000`, ...runIds, id),
      events: plan(`SELECT thread_id FROM ${PREFIX}turn_items WHERE thread_id IN (?) AND updated_at >= ? AND type NOT IN ('checkpoint','todo_list') ORDER BY updated_at DESC LIMIT 8000`, id, since),
      history: plan(`SELECT provider_thread_id FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC) AS rank FROM ${PREFIX}provider_turns WHERE thread_id IN (?)) WHERE rank <= 160`, id),
      spans: plan(`SELECT thread_id FROM ${PREFIX}runs WHERE thread_id IN (?) AND (completed_at IS NULL OR completed_at >= ?)`, id, since),
      plans: plan(`SELECT thread_id FROM ${PREFIX}turn_items WHERE type='todo_list' AND run_id IN (${rm}) ORDER BY updated_at DESC`, ...runIds),
    }
  }
  reread[name] = entry
}

// ── E: the updated_at alternative for change detection ──────────────────────
const cutoff = new Date(now - 2000).toISOString() // a 2 s poll interval
const scanSql = `SELECT DISTINCT thread_id FROM ${PREFIX}turn_items WHERE updated_at >= ?`
const scanStmt = db.prepare(scanSql)
const updatedAtScan = { cutoff, ...time("updated_at_scan", MID_REPS, () => scanStmt.all(cutoff)), plan: plan(scanSql, cutoff) }

console.log(JSON.stringify({
  db: path, now, threads: allIds.length, max_sequence: maxSeq,
  reps: { cheap: REPS, mid: MID_REPS, slow: SLOW_REPS },
  data_version: dataVersion,
  last_sequence: lastSequence,
  changed_threads: changed,
  reread,
  updated_at_scan: updatedAtScan,
}, null, 1))
db.close()
