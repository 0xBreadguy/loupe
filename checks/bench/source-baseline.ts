// Times the TypeScript read path the way the product uses it: app.ts's
// refresh() and status.ts's line() call `new Source(path).snapshot(now)`, and
// Source.snapshot opens a fresh read-only connection every call. So each timed
// operation here is one full Source.snapshot(now), connection open included.
//
//   node checks/bench/source-baseline.ts --db PATH --now-ms N [--ops N] [--warmup N]
//   node checks/bench/source-baseline.ts --db PATH --explain
//
// Phase breakdown without editing oracle/: DatabaseSync.prototype.prepare is
// patched to time prepare and row materialization per query text, and
// JSON.parse is patched to time payload decoding. Whatever remains is assembly
// (buildAgents + decorate + attribution + connection open/close); the open/close
// share is estimated by a separate probe and reported as such.
import { performance } from "node:perf_hooks"
import { DatabaseSync } from "node:sqlite"
import { Source, PREFIX, type Snapshot } from "../../oracle/src/source.ts"

const args = process.argv.slice(2)
function arg(name: string, fallback?: string): string {
  const i = args.indexOf(name)
  if (i < 0 && fallback !== undefined) return fallback
  if (i < 0 || args[i + 1] === undefined) throw new Error(`missing ${name}`)
  return args[i + 1]
}
const has = (name: string) => args.includes(name)
const path = arg("--db")

/** Same hash as the Rust --bench-source mode: djb2 over agent ids in order. */
function agentHash(snap: Snapshot): string {
  let h = 5381 >>> 0
  for (const a of snap.agents) for (let i = 0; i < a.id.length; i++) h = ((h * 33) ^ a.id.charCodeAt(i)) >>> 0
  return h.toString(16).padStart(8, "0")
}

function checksum(snap: Snapshot) {
  return {
    agents: snap.agents.length,
    events: snap.agents.reduce((n, a) => n + a.events.length, 0),
    items: snap.agents.reduce((n, a) => n + a.items.length, 0),
    history: snap.agents.reduce((n, a) => n + a.history.length, 0),
    agent_hash: agentHash(snap),
  }
}

// ── Phase instrumentation ───────────────────────────────────────────────────

interface QueryRec { prepare_ms: number; step_ms: number; rows: number; calls: number }
const queries = new Map<string, QueryRec>()
let jsonMs = 0
let jsonCalls = 0

/** Short per-query label. The same rules exist in crates/core/src/source.rs's timing module. */
function label(sql: string): string {
  if (sql.includes("sqlite_master")) return "tables"
  if (sql.includes("pragma_table_info")) return "table_info"
  if (sql.includes("projection_threads ")) return "threads"
  if (sql.includes("projection_runs") && sql.includes("completed_at IS NULL OR")) return "spans"
  if (sql.includes("projection_runs")) return "runs"
  if (sql.includes("projection_subagents")) return "subagents"
  if (sql.includes("projection_provider_threads")) return "provider_threads"
  if (sql.includes("provider_turns") && sql.includes("rank=1")) return "latest_turns"
  if (sql.includes("provider_turns") && sql.includes("rank <= 160")) return "history"
  if (sql.includes("projection_nodes")) return "nodes"
  if (sql.includes("projection_projects")) return "projects"
  if (sql.includes("runtime_requests")) return "requests"
  if (sql.includes("turn_items") && sql.includes("LIMIT 3000")) return "items"
  if (sql.includes("turn_items") && sql.includes("LIMIT 8000")) return "events"
  if (sql.includes("turn_items") && sql.includes("todo_list")) return "plans"
  return sql.replace(/\s+/g, " ").slice(0, 80)
}

function instrument() {
  const origPrepare = DatabaseSync.prototype.prepare
  DatabaseSync.prototype.prepare = function (sql: string, ...rest: unknown[]) {
    const t0 = performance.now()
    const stmt = origPrepare.call(this, sql, ...(rest as []))
    const prepareMs = performance.now() - t0
    const key = label(sql)
    let rec = queries.get(key)
    if (!rec) queries.set(key, (rec = { prepare_ms: 0, step_ms: 0, rows: 0, calls: 0 }))
    rec.prepare_ms += prepareMs
    rec.calls += 1
    const origAll = stmt.all.bind(stmt)
    stmt.all = (...allArgs: Parameters<typeof stmt.all>) => {
      const t1 = performance.now()
      const rows = origAll(...allArgs)
      rec.step_ms += performance.now() - t1
      rec.rows += rows.length
      return rows
    }
    return stmt
  }
  const origParse = JSON.parse
  JSON.parse = function (text: string, reviver?: unknown) {
    const t = performance.now()
    const out = origParse(text, reviver as never)
    jsonMs += performance.now() - t
    jsonCalls++
    return out
  } as typeof JSON.parse
}

// ── EXPLAIN QUERY PLAN + index inventory ────────────────────────────────────

function explain() {
  const db = new DatabaseSync(path, { readOnly: true })
  try {
    // --ids N: bind N real thread ids instead of 3 (the planner's choice with
    // a full swarm's worth of IN parameters is the interesting case).
    const idCount = Number(arg("--ids", "3"))
    const ids = (db.prepare(`SELECT thread_id FROM ${PREFIX}threads ORDER BY thread_id LIMIT ?`).all(idCount) as { thread_id: string }[]).map((r) => r.thread_id)
    const marks = ids.map(() => "?").join(",")
    const where = `WHERE thread_id IN (${marks})`
    const since = new Date(1794484800000 - 3 * 3600_000).toISOString()
    const itemFields = "thread_id,run_id,node_id,provider_thread_id,type,status,updated_at"
    const shapes: [string, string, unknown[]][] = [
      ["tables", "SELECT name FROM sqlite_master WHERE type='table'", []],
      ["threads", `SELECT json_object('id', json_extract(payload_json,'$.id')) AS payload FROM ${PREFIX}threads WHERE deleted_at IS NULL AND archived_at IS NULL`, []],
      ["runs", `SELECT json_object('id', json_extract(payload_json,'$.id')) AS payload FROM ${PREFIX}runs ${where} ORDER BY ordinal DESC`, ids],
      ["subagents", `SELECT json_object('id', json_extract(payload_json,'$.id')) AS payload FROM ${PREFIX}subagents ${where}`, ids],
      ["provider_threads", `SELECT json_object('id', json_extract(payload_json,'$.id')) AS payload FROM ${PREFIX}provider_threads ${where}`, ids],
      ["latest_turns", `SELECT payload_json FROM (SELECT payload_json, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC, rowid DESC) AS rank FROM ${PREFIX}provider_turns ${where}) WHERE rank=1`, ids],
      ["nodes", `SELECT node_id,parent_node_id,run_id FROM ${PREFIX}nodes ${where}`, ids],
      ["projects", "SELECT project_id,title,workspace_root FROM projection_projects", []],
      ["requests", `SELECT thread_id,node_id,kind,status,created_at FROM ${PREFIX}runtime_requests ${where} AND resolved_at IS NULL AND status IN ('pending','open','waiting','requested')`, ids],
      ["items", `SELECT ${itemFields} FROM ${PREFIX}turn_items WHERE (run_id IN (${marks}) OR (run_id IS NULL AND thread_id IN (${marks}))) AND type IN ('command_execution','reasoning') ORDER BY updated_at DESC, ordinal DESC LIMIT 3000`, [...ids, ...ids]],
      ["events", `SELECT ${itemFields} FROM ${PREFIX}turn_items ${where} AND updated_at >= ? AND type NOT IN ('checkpoint','todo_list') ORDER BY updated_at DESC LIMIT 8000`, [...ids, since]],
      ["history", `SELECT provider_thread_id FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC) AS rank FROM ${PREFIX}provider_turns ${where}) WHERE rank <= 160`, ids],
      ["spans", `SELECT thread_id,requested_at,completed_at FROM ${PREFIX}runs ${where} AND (completed_at IS NULL OR completed_at >= ?)`, [...ids, since]],
      ["plans", `SELECT thread_id,updated_at FROM ${PREFIX}turn_items WHERE type='todo_list' AND run_id IN (${marks}) ORDER BY updated_at DESC`, ids],
    ]
    const plans: Record<string, string[]> = {}
    for (const [name, sql, params] of shapes) {
      plans[name] = (db.prepare(`EXPLAIN QUERY PLAN ${sql}`).all(...(params as never[])) as { detail: string }[]).map((r) => r.detail)
    }
    const indexes = db.prepare("SELECT name, tbl_name, sql FROM sqlite_master WHERE type='index' ORDER BY tbl_name").all()
    const version = (db.prepare("SELECT sqlite_version() AS v").get() as { v: string }).v
    console.log(JSON.stringify({ path, sqlite_version: version, indexes, plans }, null, 1))
  } finally {
    db.close()
  }
}

// ── Main ────────────────────────────────────────────────────────────────────

if (has("--explain")) {
  explain()
} else {
  const now = Number(arg("--now-ms"))
  const ops = Number(arg("--ops", "20"))
  const warmup = Number(arg("--warmup", "3"))
  if (!Number.isSafeInteger(ops) || ops < 1) throw new Error("positive --ops required")
  const source = new Source(path)
  const sqliteVersion = (new DatabaseSync(path, { readOnly: true }).prepare("SELECT sqlite_version() AS v").get() as { v: string }).v

  // Open/close probe: what one connection cycle costs without any query.
  let openCloseMs = 0
  for (let i = 0; i < 10; i++) {
    const t = performance.now()
    const db = new DatabaseSync(path, { readOnly: true, timeout: 250 })
    db.exec("PRAGMA query_only=ON")
    db.close()
    openCloseMs += performance.now() - t
  }
  openCloseMs /= 10

  instrument()
  let errors = 0
  const fail = () => {
    errors++
    return null
  }
  for (let i = 0; i < warmup; i++) {
    try {
      source.snapshot(now)
    } catch {
      fail()
    }
  }
  queries.clear()
  jsonMs = 0
  jsonCalls = 0
  const trials: number[] = []
  let consumed = 0
  let last: Snapshot | null = null
  for (let i = 0; i < ops; i++) {
    const t = performance.now()
    try {
      const snap = source.snapshot(now)
      trials.push(performance.now() - t)
      consumed += snap.agents.length
      last = snap
    } catch {
      trials.push(-1)
      fail()
    }
  }
  if (!last) throw new Error("every snapshot failed")
  const totalSql = [...queries.values()].reduce((n, q) => n + q.prepare_ms + q.step_ms, 0)
  const totalTrials = trials.filter((t) => t >= 0).reduce((a, b) => a + b, 0)
  const sorted = [...trials].filter((t) => t >= 0).sort((a, b) => a - b)
  console.log(JSON.stringify({
    implementation: "typescript-node-sqlite",
    sqlite_version: sqliteVersion,
    path, now, operations: ops, warmup, errors,
    trials_ms: trials.map((t) => Math.round(t * 1000) / 1000),
    ms_per_op: Math.round((totalTrials / sorted.length) * 1000) / 1000,
    median_ms: sorted[Math.floor(sorted.length / 2)],
    min_ms: sorted[0],
    max_ms: sorted[sorted.length - 1],
    consumed_checksum: consumed,
    ...checksum(last),
    phases: {
      queries: Object.fromEntries([...queries.entries()].map(([k, q]) => [k, {
        prepare_ms: Math.round(q.prepare_ms * 1000) / 1000,
        step_ms: Math.round(q.step_ms * 1000) / 1000,
        rows: q.rows, calls: q.calls,
      }])),
      sql_ms_total: Math.round(totalSql * 1000) / 1000,
      json_ms: Math.round(jsonMs * 1000) / 1000,
      json_calls: jsonCalls,
      assembly_ms: Math.round((totalTrials - totalSql - jsonMs) * 1000) / 1000,
      open_close_probe_ms: Math.round(openCloseMs * 1000) / 1000,
    },
  }))
}
