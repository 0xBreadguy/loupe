// Deterministic generator of T3-shaped SQLite databases for the scale lab.
//
//   node checks/scripts/scale-db.ts --tier xxl [--schema t3|demo] [--db path] [overrides]
//   node checks/scripts/scale-db.ts --db path --mutate K [--touch N]
//   node checks/scripts/scale-db.ts --db path --stats
//   node checks/scripts/scale-db.ts --db path --stream SEED --count N [--exclude shape,shape] [--include shape] [--serve]
//
// --schema demo (the original lab): the schema oracle/src/demo.ts writes —
// every table oracle/src/source.ts queries, no secondary indexes, plain
// INSERTs in one big transaction. Kept byte-compatible so the old scale-lab
// numbers stay reproducible.
//
// --schema t3 (default): the real T3 Code schema from
// checks/fixtures/t3-schema.sql (thread indexes, partial live-row indexes,
// event log, projection metadata). Every write — initial fill and mutator — is
// one transaction that appends an orchestration_events row (version 2, the
// table a real installation writes; orchestration_v2_events stays empty),
// upserts the projection rows the way T3 does (INSERT ... ON CONFLICT DO
// UPDATE, so updated rows keep their rowid), and sets last_sequence in the
// thread-projections metadata row. Columns are filled consistently with
// payload_json (the real tables denormalize ids, status and timestamps into
// columns; the readers query those columns). Event payloads are invented
// filler. Projects are not thread-scoped: the initial fill records one
// version-2 project event per project, and the mutator never does — a project
// event in the suffix after a baseline makes the reader take a full read.
// Two metadata rows, event-log-prune-mark and event-log-pruned-through, record
// a prune floor at or below the cursor. The kept log above that floor is
// gap-free, and the newest sequence is thread-projections.last_sequence.
//
// Everything is invented. Times hang off the fixed epoch from
// checks/fixtures/workloads.json; all randomness comes from one seeded
// mulberry32 stream, so two runs with the same arguments produce identical
// databases (verified by sha256 at the small tiers).
//
// Real payload_json carries far more than the projected fields, and
// json_extract cost grows with payload size, so payloads are padded with
// invented filler fields of realistic size (command output, reasoning text,
// message bodies the reader never selects).
//
// Uniform tiers by agent count (agents = threads + subagents, history in proportion):
//   s    25 agents:    6 threads /   19 subs, 2 runs/thread,  30 items/run
//   m   256 agents:   64 threads /  192 subs, 3 runs/thread,  40 items/run
//   l  1000 agents:  250 threads /  750 subs, 4 runs/thread,  50 items/run
//   xl 4000 agents: 1000 threads / 3000 subs, 5 runs/thread,  60 items/run
//   xxl 16000 agents: 4000 threads / 12000 subs, 5 runs/thread, 45 items/run
//   real  skewed stress workload, T3 schema only (legacy tier name)
// Measured row counts per tier are printed by --stats.
import { DatabaseSync, type SQLInputValue, type StatementSync } from "node:sqlite"
import { copyFileSync, existsSync, mkdirSync, readFileSync, readSync, rmSync, statSync, writeFileSync, writeSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { PREFIX } from "../../oracle/src/source.ts"

export const EPOCH = 1794484800000 // fixed clock, checks/fixtures/workloads.json
const HOUR = 3_600_000
const DAY = 24 * HOUR
const WINDOW = 3 * HOUR // source.ts MAX_WINDOW_MS
const ENVIRONMENT = "6f1c2a9e-3b7d-4e58-9a10-d4c7e2b8f531" // demo.ts environment id
// The other machine the skewed tier delegates to and from. Every remote
// ref uses this id so a delegatedFrom parent can coincide with a remoteChild.
const REMOTE_ENV = "7d3f9a1c-5e2b-48c6-a4d9-1f6e8b2c7d50"

// DDL verbatim from oracle/src/demo.ts (the reader's schema; no secondary indexes).
const SCHEMA = `
CREATE TABLE projection_projects (project_id TEXT PRIMARY KEY, title TEXT, workspace_root TEXT);
CREATE TABLE ${PREFIX}threads (thread_id TEXT PRIMARY KEY, payload_json TEXT, deleted_at TEXT, archived_at TEXT);
CREATE TABLE ${PREFIX}runs (run_id TEXT PRIMARY KEY, thread_id TEXT, ordinal INTEGER, payload_json TEXT,
  requested_at TEXT, completed_at TEXT);
CREATE TABLE ${PREFIX}subagents (subagent_id TEXT PRIMARY KEY, thread_id TEXT, payload_json TEXT);
CREATE TABLE ${PREFIX}nodes (node_id TEXT PRIMARY KEY, thread_id TEXT, parent_node_id TEXT, run_id TEXT);
CREATE TABLE ${PREFIX}provider_threads (provider_thread_id TEXT PRIMARY KEY, thread_id TEXT, payload_json TEXT);
CREATE TABLE ${PREFIX}provider_turns (thread_id TEXT, provider_thread_id TEXT, ordinal INTEGER, payload_json TEXT,
  started_at TEXT, completed_at TEXT);
CREATE TABLE ${PREFIX}turn_items (item_id TEXT PRIMARY KEY, thread_id TEXT, run_id TEXT, node_id TEXT,
  provider_thread_id TEXT, type TEXT, status TEXT, updated_at TEXT, ordinal INTEGER, payload_json TEXT);
CREATE TABLE ${PREFIX}runtime_requests (request_id TEXT PRIMARY KEY, thread_id TEXT, node_id TEXT, kind TEXT,
  status TEXT, created_at TEXT, resolved_at TEXT);
CREATE TABLE scale_meta (key TEXT PRIMARY KEY, value TEXT);
`

// T3 Code's real schema, extracted from the program bundle (never a database).
const T3_SCHEMA = readFileSync(new URL("../fixtures/t3-schema.sql", import.meta.url), "utf8")
// A nonempty synthetic ledger lets the reader detect an added migration.
const LEDGER_ROWS = 64

type Schema = "t3" | "demo"

// Column sets per schema. Generation builds one record per row carrying the
// union of both schemas' columns; the sink keeps only what its schema has, so
// the demo path writes exactly the rows the original generator wrote.
const DEMO_COLS: Record<string, string[]> = {
  projection_projects: ["project_id", "title", "workspace_root"],
  [`${PREFIX}threads`]: ["thread_id", "payload_json", "deleted_at", "archived_at"],
  [`${PREFIX}runs`]: ["run_id", "thread_id", "ordinal", "payload_json", "requested_at", "completed_at"],
  [`${PREFIX}subagents`]: ["subagent_id", "thread_id", "payload_json"],
  [`${PREFIX}nodes`]: ["node_id", "thread_id", "parent_node_id", "run_id"],
  [`${PREFIX}provider_threads`]: ["provider_thread_id", "thread_id", "payload_json"],
  [`${PREFIX}provider_turns`]: ["thread_id", "provider_thread_id", "ordinal", "payload_json", "started_at", "completed_at"],
  [`${PREFIX}turn_items`]: ["item_id", "thread_id", "run_id", "node_id", "provider_thread_id", "type", "status", "updated_at", "ordinal", "payload_json"],
  [`${PREFIX}runtime_requests`]: ["request_id", "thread_id", "node_id", "kind", "status", "created_at", "resolved_at"],
}
const T3_COLS: Record<string, string[]> = {
  projection_projects: ["project_id", "title", "workspace_root", "scripts_json", "created_at", "updated_at", "deleted_at",
    "default_model_selection_json", "default_thread_env_mode", "favicon_path", "auto_pull", "project_icon_json", "pinned_at", "archived_at"],
  [`${PREFIX}threads`]: ["thread_id", "project_id", "title", "default_provider", "runtime_mode", "interaction_mode",
    "active_provider_thread_id", "created_at", "updated_at", "archived_at", "deleted_at", "payload_json", "provider_instance_id"],
  [`${PREFIX}runs`]: ["run_id", "thread_id", "ordinal", "provider", "provider_thread_id", "status", "requested_at", "completed_at", "payload_json", "provider_instance_id"],
  [`${PREFIX}subagents`]: ["subagent_id", "thread_id", "run_id", "parent_node_id", "provider", "provider_thread_id", "child_thread_id",
    "origin", "status", "started_at", "completed_at", "updated_at", "payload_json", "driver", "provider_instance_id", "completion_delivery_state"],
  [`${PREFIX}nodes`]: ["node_id", "thread_id", "run_id", "parent_node_id", "root_node_id", "kind", "status", "provider_thread_id",
    "provider_turn_id", "runtime_request_id", "checkpoint_scope_id", "started_at", "completed_at", "payload_json"],
  [`${PREFIX}provider_threads`]: ["provider_thread_id", "thread_id", "owner_node_id", "provider", "provider_session_id", "status",
    "first_run_ordinal", "last_run_ordinal", "updated_at", "payload_json", "driver", "provider_instance_id"],
  [`${PREFIX}provider_turns`]: ["provider_turn_id", "thread_id", "provider_thread_id", "node_id", "run_attempt_id", "ordinal", "status",
    "started_at", "completed_at", "payload_json"],
  [`${PREFIX}turn_items`]: ["turn_item_id", "thread_id", "run_id", "node_id", "provider_thread_id", "provider_turn_id", "parent_item_id",
    "ordinal", "type", "status", "updated_at", "payload_json"],
  [`${PREFIX}runtime_requests`]: ["runtime_request_id", "thread_id", "node_id", "provider_turn_id", "kind", "status", "created_at", "resolved_at", "payload_json"],
}
const T3_PK: Record<string, string> = {
  projection_projects: "project_id",
  [`${PREFIX}threads`]: "thread_id",
  [`${PREFIX}runs`]: "run_id",
  [`${PREFIX}subagents`]: "subagent_id",
  [`${PREFIX}nodes`]: "node_id",
  [`${PREFIX}provider_threads`]: "provider_thread_id",
  [`${PREFIX}provider_turns`]: "provider_turn_id",
  [`${PREFIX}turn_items`]: "turn_item_id",
  [`${PREFIX}runtime_requests`]: "runtime_request_id",
}

interface Shape {
  threads: number // top-level threads
  subs: number // native subagents, nested to `depth`
  runs: number // runs per thread
  items: number // turn items per run; subagents add round(items*0.7) each under the host's latest run
  turns: number // provider turns per provider thread
  depth: number // maximum subagent nesting
  spanDays: number // how far back a thread's history reaches
  openRequests: number // fraction of threads with an open runtime request
  recent: number // target fraction of latest-run activity inside the last 3 hours
  running: number // fraction of threads whose latest run is still running
  seed: number
}

const TIERS: Record<string, Omit<Shape, "seed">> = {
  s: { threads: 6, subs: 19, runs: 2, items: 30, turns: 5, depth: 2, spanDays: 2, openRequests: 0.12, recent: 0.3, running: 0.3 },
  m: { threads: 64, subs: 192, runs: 3, items: 40, turns: 8, depth: 2, spanDays: 4, openRequests: 0.12, recent: 0.3, running: 0.3 },
  l: { threads: 250, subs: 750, runs: 4, items: 50, turns: 10, depth: 2, spanDays: 7, openRequests: 0.12, recent: 0.3, running: 0.3 },
  xl: { threads: 1000, subs: 3000, runs: 5, items: 60, turns: 12, depth: 3, spanDays: 10, openRequests: 0.12, recent: 0.3, running: 0.3 },
  xxl: { threads: 4000, subs: 12000, runs: 5, items: 45, turns: 14, depth: 3, spanDays: 14, openRequests: 0.12, recent: 0.3, running: 0.3 },
}

// ── Deterministic randomness ────────────────────────────────────────────────

function mulberry32(seed: number) {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6d2b79f5) >>> 0
    let t = a
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const WORDS = ("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau " +
  "upsilon phi chi psi omega cache buffer socket parser render codec vector matrix tensor stream ledger index queue").split(" ")

class Lab {
  readonly rng: () => number
  readonly shape: Shape
  private fillerPool: string[] = []
  ordinal = 0

  constructor(shape: Shape) {
    this.shape = shape
    this.rng = mulberry32(shape.seed)
    // Filler pool: fixed strings of growing size, picked per payload. Payload
    // sizes vary per row without paying generation-time string building.
    const words = mulberry32(shape.seed ^ 0x9e3779b9)
    for (let i = 0; i < 96; i++) {
      const target = 120 + Math.floor(words() * 2600)
      let s = ""
      while (s.length < target) s += WORDS[Math.floor(words() * WORDS.length)] + " "
      this.fillerPool.push(s)
    }
  }

  filler(min = 0): string {
    const s = this.fillerPool[Math.floor(this.rng() * this.fillerPool.length)]
    return s.length >= min ? s : s + this.fillerPool[0].repeat(Math.ceil((min - s.length) / this.fillerPool[0].length))
  }

  pick<T>(list: T[]): T {
    return list[Math.floor(this.rng() * list.length)]
  }
}

const iso = (t: number | null | undefined) => (t == null ? null : new Date(t).toISOString())

// ── Realistic invented content ──────────────────────────────────────────────

const MODELS: [string, number][] = [["claude-opus-5-5", 1_000_000], ["claude-sonnet-5-5", 1_000_000], ["claude-haiku-5-5", 200_000], ["gpt-6.1-sol", 400_000], ["grok-4.7", 2_000_000], ["grok-4.7-fast", 2_000_000]]
const PROJECTS = [
  { id: "atlas", title: "atlas-api", root: "/home/user/projects/atlas-api" },
  { id: "shop", title: "storefront-web", root: "/home/user/projects/storefront-web" },
  { id: "pipe", title: "pipeline-cli", root: "/home/user/projects/pipeline-cli" },
  { id: "book", title: "handbook", root: "/home/user/projects/handbook" },
  { id: "fleet", title: "fleet-operator", root: "/home/user/projects/fleet-operator" },
  { id: "lens", title: "lens-viewer", root: "/home/user/projects/lens-viewer" },
  { id: "mill", title: "mill-build", root: "/home/user/projects/mill-build" },
  { id: "ward", title: "warden-auth", root: "/home/user/projects/warden-auth" },
]
const TOOLS = ["Read", "Edit", "Write", "Grep", "Glob", "WebFetch", "NotebookEdit", "Bash", "Task"]
const DIRS = ["src", "test", "docs", "scripts", "packages/core", "packages/web", "config", "benches"]
const FILES = ["index.ts", "client.ts", "server.ts", "mod.rs", "lib.rs", "main.go", "app.py", "README.md", "config.toml", "worker.ts"]

/** Plausible provider name for a model id; the real schema denormalizes it. */
const providerOf = (model: string) => (model.startsWith("claude") ? "claude" : model.startsWith("gpt") ? "openai" : "xai")

// Weighted type mix over the reader's TOOLISH list plus the filtered types
// (checkpoint, todo_list) and user_message, roughly a busy coding agent.
const MIX: [string, number][] = [
  ["command_execution", 0.24], ["dynamic_tool", 0.18], ["file_change", 0.11], ["web_search", 0.04],
  ["reasoning", 0.16], ["assistant_message", 0.12], ["user_message", 0.05], ["todo_list", 0.012],
  ["checkpoint", 0.02], ["subagent", 0.01], ["notification", 0.008], ["user_input_request", 0.004],
  ["secret_request", 0.002], ["link_request", 0.002], ["handoff", 0.004],
]

function itemType(rng: () => number): string {
  let r = rng()
  for (const [type, w] of MIX) {
    if ((r -= w) < 0) return type
  }
  return "command_execution"
}

/** One invented payload with filler sized like a real T3 payload for that type. */
function payload(lab: Lab, type: string, started: number): Record<string, unknown> {
  const r = lab.rng
  const base: Record<string, unknown> = { startedAt: iso(started), schema: 2 }
  switch (type) {
    case "command_execution":
      return { ...base, input: `${lab.pick(["npm", "cargo", "git", "node", "rg"])} ${lab.pick(["test", "build", "check", "run", "status"])} ${lab.pick(DIRS)}/${lab.pick(FILES)}`, output: lab.filler(), exitCode: r() < 0.96 ? 0 : 1, durationMs: Math.round(400 + r() * 90_000), cwd: "/home/user/projects" }
    case "dynamic_tool": {
      const tool = lab.pick(TOOLS)
      return { ...base, toolName: tool, title: `${tool} ${lab.pick(DIRS)}/${lab.pick(FILES)}`, input: lab.filler().slice(0, 400), output: lab.filler(), callId: `call-${Math.floor(r() * 1e9)}` }
    }
    case "file_change":
      return { ...base, fileName: `${lab.pick(DIRS)}/${lab.pick(FILES)}`, diff: lab.filler().slice(0, 900), additions: Math.round(r() * 120), deletions: Math.round(r() * 40) }
    case "web_search":
      return { ...base, title: `${lab.pick(WORDS)} ${lab.pick(WORDS)} documentation`, query: `${lab.pick(WORDS)} ${lab.pick(WORDS)}`, results: lab.filler().slice(0, 700) }
    case "reasoning":
      return { ...base, text: lab.filler(), effort: lab.pick(["low", "medium", "high"]) }
    case "assistant_message":
      return { ...base, text: lab.filler(), phase: lab.pick(["update", "answer", "plan"]) }
    case "user_message":
      return { ...base, text: lab.filler().slice(0, 300), author: "user" }
    case "todo_list":
      return { ...base, steps: Array.from({ length: 4 + Math.floor(r() * 4) }, (_, i) => ({ text: `${lab.pick(WORDS)} ${lab.pick(WORDS)} step ${i}`, status: i === 0 ? "in_progress" : r() < 0.4 ? "completed" : "pending" })) }
    case "checkpoint":
      return { ...base, ref: lab.filler().slice(0, 120), files: Math.round(r() * 30) }
    case "subagent":
      return { ...base, title: `${lab.pick(WORDS)} ${lab.pick(WORDS)} investigation`, description: lab.filler().slice(0, 350) }
    case "notification":
      return { ...base, title: `${lab.pick(WORDS)} notice`, text: lab.filler().slice(0, 220) }
    case "user_input_request":
      return { ...base, title: `${lab.pick(WORDS)} decision`, question: lab.filler().slice(0, 260) }
    case "secret_request":
      return { ...base, title: `${lab.pick(WORDS)} credential`, question: lab.filler().slice(0, 180) }
    case "link_request":
      return { ...base, title: `${lab.pick(WORDS)} link`, question: lab.filler().slice(0, 180) }
    case "handoff":
      return { ...base, title: `handoff to ${lab.pick(WORDS)}`, summary: lab.filler().slice(0, 450) }
    default:
      return base
  }
}

// ── Writers ─────────────────────────────────────────────────────────────────

const serialize = (v: unknown): SQLInputValue =>
  (v === undefined ? null : typeof v === "object" && v !== null ? JSON.stringify(v) : v) as SQLInputValue

/** demo-schema writer: plain INSERTs of the demo column set, one big transaction run by the caller. */
class DemoSink {
  private statements = new Map<string, StatementSync>()
  private db: DatabaseSync
  constructor(db: DatabaseSync) {
    this.db = db
  }
  put(table: string, values: Record<string, unknown>) {
    const cols = DEMO_COLS[table]
    if (!cols) throw new Error(`no demo columns for ${table}`)
    const sql = `INSERT INTO ${table} (${cols.join(",")}) VALUES (${cols.map(() => "?").join(",")})`
    let statement = this.statements.get(sql)
    if (!statement) this.statements.set(sql, (statement = this.db.prepare(sql)))
    statement.run(...cols.map((c) => serialize(values[c])))
  }
  // Lab tables that are not part of either schema (scale_meta).
  raw(sql: string, ...args: SQLInputValue[]) {
    this.db.prepare(sql).run(...args)
  }
  close() {
    this.statements.clear()
  }
}

interface EventRef {
  thread: string
  type: string
  at: string | null
  run?: string | null
  node?: string | null
  provider?: string | null
}

/**
 * t3-schema writer. A projection write that names an event is one transaction:
 * append a version-2 row to orchestration_events (the log a real installation
 * writes), upsert the projection row (INSERT ... ON CONFLICT DO UPDATE, like
 * T3), and set thread-projections.last_sequence to that sequence. Mutator
 * events are thread events. The initial fill also records project events and
 * the two prune watermarks; those are not part of the mutation stream.
 * `hold`/`release` wrap the initial fill in one transaction. `lab.filler()`
 * still runs once per thread event, in the same places as before, so the
 * seeded stream that builds s/m/l projection rows does not move.
 */
class T3Sink {
  private upserts = new Map<string, StatementSync>()
  private eventStmt: StatementSync
  private eventExplicit: StatementSync
  private metaStmt: StatementSync
  private rowidStmt: StatementSync
  private eventCounter: number
  private versions = new Map<string, number>()
  private depth = 0
  private db: DatabaseSync
  private lab: Lab
  constructor(db: DatabaseSync, lab: Lab) {
    this.db = db
    this.lab = lab
    this.eventStmt = db.prepare(`INSERT INTO orchestration_events
      (event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, actor_kind, payload_json, metadata_json, application_event_version)
      VALUES (?, ?, ?, ?, ?, ?, 'system', ?, '{}', 2)`)
    this.eventExplicit = db.prepare(`INSERT INTO orchestration_events
      (sequence, event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, actor_kind, payload_json, metadata_json, application_event_version)
      VALUES (?, ?, ?, ?, ?, ?, ?, 'system', ?, '{}', 2)`)
    this.metaStmt = db.prepare(`UPDATE orchestration_v2_projection_metadata SET last_sequence=?, updated_at=? WHERE projection_name='thread-projections'`)
    this.rowidStmt = db.prepare("SELECT last_insert_rowid() AS s")
    this.eventCounter = (db.prepare("SELECT COALESCE(MAX(sequence),0) AS s FROM orchestration_events").get() as { s: number }).s
    for (const row of db.prepare("SELECT aggregate_kind AS k, stream_id AS s, MAX(stream_version) AS v FROM orchestration_events GROUP BY aggregate_kind, stream_id").all() as { k: string; s: string; v: number }[]) {
      this.versions.set(`${row.k}\0${row.s}`, row.v)
    }
  }
  /** One transaction around many writes. Mutations do not use this. */
  hold() {
    this.begin()
  }
  release() {
    this.end()
  }
  private begin() {
    if (this.depth++ === 0) this.db.exec("BEGIN")
  }
  private end() {
    if (this.depth === 0) throw new Error("transaction underflow")
    if (--this.depth === 0) this.db.exec("COMMIT")
  }
  private nextVersion(kind: string, stream: string): number {
    const key = `${kind}\0${stream}`
    const version = (this.versions.get(key) ?? 0) + 1
    this.versions.set(key, version)
    return version
  }
  /** Append one version-2 event. An explicit sequence is for the skewed tier's pruned tail. */
  private writeEvent(kind: string, stream: string, type: string, at: string, payload: string, sequence?: number): number {
    const version = this.nextVersion(kind, stream)
    if (sequence != null) {
      this.eventExplicit.run(sequence, `e-${sequence}`, kind, stream, version, type, at, payload)
      this.eventCounter = Math.max(this.eventCounter, sequence)
      return sequence
    }
    this.eventStmt.run(`e-${this.eventCounter + 1}`, kind, stream, version, type, at, payload)
    const seq = (this.rowidStmt.get() as { s: number }).s
    this.eventCounter = seq
    return seq
  }
  /** A project event during the initial fill. Does not draw from the row PRNG. */
  projectEvent(id: string, at: string) {
    this.begin()
    const seq = this.writeEvent("project", id, "project.updated", at, '{"note":"project"}')
    this.metaStmt.run(seq, at)
    this.end()
  }
  /**
   * A project event as T3 commits one (`commitProjectCommand`): appended to the log, and
   * the cursor left where it is, since only thread events move it. `projectEvent` above
   * moves it; it is kept as it was for the initial fill and the rename-project shape.
   */
  projectEventKeepCursor(id: string, at: string) {
    this.begin()
    this.writeEvent("project", id, "project.meta-updated", at, '{"note":"project"}')
    this.end()
  }
  /**
   * Model T3's migration ledger with one row per migration. The lab supplies its own
   * IDs and names without advancing the row PRNG.
   */
  ledger(count: number, at: string) {
    this.begin()
    const stmt = this.db.prepare("INSERT INTO effect_sql_migrations (migration_id, created_at, name) VALUES (?, ?, ?)")
    for (let id = 1; id <= count; id++) stmt.run(id, at, `LabMigration${String(id).padStart(3, "0")}`)
    this.end()
  }
  /** Prune watermarks. `floor` must be ≤ the thread-projections cursor. */
  prune(floor: number, at: string) {
    this.begin()
    const stmt = this.db.prepare("INSERT INTO orchestration_v2_projection_metadata (projection_name, schema_version, last_sequence, updated_at) VALUES (?, 1, ?, ?)")
    stmt.run("event-log-prune-mark", floor, at)
    stmt.run("event-log-pruned-through", floor, at)
    this.end()
  }
  /** Gap-free tail with explicit sequences. The last row is the cursor. */
  tail(rows: { sequence: number; kind: string; stream: string; type: string; at: string }[]) {
    this.begin()
    for (const row of rows) {
      const payload = row.kind === "project" ? '{"note":"project"}' : '{"note":"lab"}'
      this.writeEvent(row.kind, row.stream, row.type, row.at, payload, row.sequence)
    }
    const last = rows[rows.length - 1]
    this.metaStmt.run(last.sequence, last.at)
    this.end()
  }
  private upsert(table: string, values: Record<string, unknown>) {
    const cols = T3_COLS[table]
    if (!cols) throw new Error(`no t3 columns for ${table}`)
    // The real projection denormalizes status into a column AND keeps it in
    // the payload; keep the two consistent for turn items.
    if (table === `${PREFIX}turn_items` && typeof values.payload_json === "object" && values.payload_json !== null) {
      values = { ...values, payload_json: { ...(values.payload_json as Record<string, unknown>), status: values.status ?? null } }
    }
    const pk = T3_PK[table]
    const sql = `INSERT INTO ${table} (${cols.join(",")}) VALUES (${cols.map(() => "?").join(",")})
      ON CONFLICT(${pk}) DO UPDATE SET ${cols.filter((c) => c !== pk).map((c) => `${c}=excluded.${c}`).join(",")}`
    let statement = this.upserts.get(sql)
    if (!statement) this.upserts.set(sql, (statement = this.db.prepare(sql)))
    statement.run(...cols.map((c) => serialize(values[c])))
  }
  put(table: string, values: Record<string, unknown>, event?: EventRef) {
    this.begin()
    if (event) {
      const at = event.at ?? iso(EPOCH)!
      // One filler draw per thread event, same as the previous v2 writer.
      const seq = this.writeEvent("thread", event.thread, event.type, at, JSON.stringify({ note: this.lab.filler().slice(0, 160) }))
      this.upsert(table, values)
      this.metaStmt.run(seq, at)
    } else {
      this.upsert(table, values)
    }
    this.end()
  }
  /** One event transaction covering several projection writes (T3's multi-row events). */
  event(event: EventRef, writes: () => void) {
    const at = event.at ?? iso(EPOCH)!
    this.begin()
    const seq = this.writeEvent("thread", event.thread, event.type, at, JSON.stringify({ note: this.lab.filler().slice(0, 160) }))
    writes()
    this.metaStmt.run(seq, at)
    this.end()
  }
  upsertRow(table: string, values: Record<string, unknown>) {
    this.upsert(table, values)
  }
  raw(sql: string, ...args: SQLInputValue[]) {
    this.db.prepare(sql).run(...args)
  }
  close() {
    this.upserts.clear()
  }
}

type Sink = DemoSink | T3Sink

// ── Generation ──────────────────────────────────────────────────────────────

interface Ctx {
  lab: Lab
  w: Sink
  shape: Shape
  schema: Schema
}

/** Run boundaries for one thread: quadratic so recent runs are shorter and busier. */
function runBounds(shape: Shape): number[] {
  const span = shape.spanDays * DAY
  return Array.from({ length: shape.runs + 1 }, (_, k) => EPOCH - span * Math.pow((shape.runs - k) / shape.runs, 2.2))
}

/** Timestamps for one run's items: `recent` share inside the last 3 hours. */
function itemTimes(lab: Lab, shape: Shape, count: number, lo: number, hi: number, latest: boolean): number[] {
  const times: number[] = []
  const recentLo = EPOCH - WINDOW
  for (let i = 0; i < count; i++) {
    if (latest && lab.rng() < shape.recent) times.push(recentLo + lab.rng() * (EPOCH - recentLo))
    else times.push(lo + lab.rng() * Math.max(0, Math.min(hi, recentLo) - lo))
  }
  return times.sort((a, b) => a - b)
}

function writeItems(ctx: Ctx, where: { thread: string; run: string | null; provider: string | null }, times: number[], live: boolean) {
  const { lab, w } = ctx
  times.forEach((t, i) => {
    const last = live && i === times.length - 1
    const type = last ? lab.pick(["command_execution", "dynamic_tool", "file_change"]) : itemType(lab.rng)
    const failed = !last && lab.rng() < 0.045
    const status = last ? "running" : failed ? "failed" : "completed"
    const dur = Math.round(2000 + lab.rng() * 240_000)
    const id = `i-${ctx.lab.ordinal}`
    const updated = last ? t : Math.min(t + dur, EPOCH)
    w.put(`${PREFIX}turn_items`, {
      turn_item_id: id, item_id: id, thread_id: where.thread, run_id: where.run, node_id: null,
      provider_thread_id: where.provider, provider_turn_id: null, parent_item_id: null,
      type, status, updated_at: iso(updated), ordinal: ctx.lab.ordinal, payload_json: payload(lab, type, t),
    }, { thread: where.thread, type: "turn_item.upserted", at: iso(updated), run: where.run })
    ctx.lab.ordinal++
  })
}

function writeTurns(ctx: Ctx, where: { thread: string; provider: string }, lo: number, hi: number, model: string, max: number, open: boolean) {
  const { lab, w, shape } = ctx
  const r = lab.rng
  for (let k = 0; k < shape.turns; k++) {
    const start = lo + ((hi - lo) * (k + r() * 0.6)) / shape.turns
    const end = k === shape.turns - 1 ? hi : lo + ((hi - lo) * (k + 1)) / shape.turns
    const complete = !(open && k === shape.turns - 1)
    const used = Math.round(max * Math.min(0.97, 0.05 + (0.9 * (k + 1)) / shape.turns))
    const output = Math.round(800 + 5200 * r())
    const input = Math.max(0, used - output)
    const cached = Math.round(input * 0.85)
    w.put(`${PREFIX}provider_turns`, {
      provider_turn_id: `turn-${where.provider}-${k + 1}`, thread_id: where.thread, provider_thread_id: where.provider,
      node_id: `n-${where.thread}`, run_attempt_id: null, ordinal: k + 1, status: complete ? "completed" : "running",
      started_at: iso(start), completed_at: complete ? iso(end) : null,
      payload_json: {
        providerThreadId: where.provider, model, status: complete ? "completed" : "running",
        tokenUsage: { usedTokens: used, maxTokens: max, inputTokens: input, cachedInputTokens: cached, outputTokens: output, reasoningOutputTokens: Math.round(output * 0.5), updatedAt: iso(end) },
        ...(complete ? { turnTokenUsage: { inputTokens: input * 4, cachedInputTokens: cached * 4, outputTokens: output * 2, reasoningOutputTokens: output, usageStatus: "complete", hasSubagents: false } } : {}),
        trace: lab.filler().slice(0, 260),
      },
    }, { thread: where.thread, type: "provider_turn.upserted", at: iso(end), provider: providerOf(model) })
  }
}

function plan(ctx: Ctx, where: { thread: string; run: string }, when: number) {
  const { lab, w } = ctx
  const steps = Array.from({ length: 5 }, (_, i) => ({
    text: `${lab.pick(WORDS)} ${lab.pick(WORDS)} phase ${i}`,
    status: i < 2 ? "completed" : i === 2 ? "in_progress" : "pending",
  }))
  const id = `i-${ctx.lab.ordinal}`
  w.put(`${PREFIX}turn_items`, {
    turn_item_id: id, item_id: id, thread_id: where.thread, run_id: where.run, node_id: null,
    provider_thread_id: null, provider_turn_id: null, parent_item_id: null,
    type: "todo_list", status: "completed", updated_at: iso(when), ordinal: ctx.lab.ordinal,
    payload_json: { steps, startedAt: iso(when) },
  }, { thread: where.thread, type: "turn_item.upserted", at: iso(when), run: where.run })
  ctx.lab.ordinal++
}

function generate(path: string, shape: Shape, schema: Schema) {
  rmSync(path, { force: true })
  rmSync(path + "-wal", { force: true })
  rmSync(path + "-shm", { force: true })
  mkdirSync(dirname(path), { recursive: true })
  const db = new DatabaseSync(path)
  const ctx: Ctx = { lab: new Lab(shape), w: null as unknown as Sink, shape, schema }
  try {
    db.exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL")
    db.exec(schema === "t3" ? T3_SCHEMA : SCHEMA)
    if (schema === "t3") db.exec("CREATE TABLE scale_meta (key TEXT PRIMARY KEY, value TEXT)")
    const lab = ctx.lab
    const rng = lab.rng
    const w: Sink = schema === "t3" ? new T3Sink(db, lab) : new DemoSink(db)
    ctx.w = w
    if (schema === "demo") db.exec("BEGIN")
    if (schema === "t3") (w as T3Sink).hold()
    // Project rows are not thread-scoped. The initial fill records one project
    // event each (no row-PRNG draw). The mutator does not.
    for (const p of PROJECTS) {
      w.put("projection_projects", {
        project_id: p.id, title: p.title, workspace_root: p.root, scripts_json: {}, created_at: iso(EPOCH - shape.spanDays * DAY),
        updated_at: iso(EPOCH), deleted_at: null, default_model_selection_json: null, default_thread_env_mode: null,
        favicon_path: null, auto_pull: 0, project_icon_json: null, pinned_at: null, archived_at: null,
      })
      if (schema === "t3") (w as T3Sink).projectEvent(p.id, iso(EPOCH - shape.spanDays * DAY)!)
    }
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('seed', ?)", String(shape.seed))
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('shape', ?)", JSON.stringify(shape))
    if (schema === "t3") {
      ;(w as T3Sink).ledger(LEDGER_ROWS, iso(EPOCH - shape.spanDays * DAY)!)
      w.raw("INSERT INTO scale_meta (key, value) VALUES ('schema', 't3')")
      w.raw("INSERT INTO orchestration_v2_projection_metadata (projection_name, schema_version, last_sequence, updated_at) VALUES ('thread-projections', 1, 0, ?)", iso(EPOCH - shape.spanDays * DAY))
    }
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('mutations', '0')")

    // Subagent plan per thread: `subs` split across threads, some nested.
    const perThread: { id: string; depth: number; parent: string | null }[][] = []
    let left = shape.subs
    for (let t = 0; t < shape.threads; t++) {
      const remainingThreads = shape.threads - t
      const count = Math.min(left, Math.ceil(left / remainingThreads))
      left -= count
      const subs: { id: string; depth: number; parent: string | null }[] = []
      for (let s = 0; s < count; s++) {
        const nest = shape.depth > 1 && subs.length > 0 && rng() < 0.28
        const parent = nest ? lab.pick(subs.filter((x) => x.depth < shape.depth)) ?? null : null
        subs.push({ id: `s-${t.toString().padStart(5, "0")}-${s.toString().padStart(3, "0")}`, depth: parent ? subs.find((x) => x.id === parent.id)!.depth + 1 : 1, parent: parent?.id ?? null })
      }
      perThread.push(subs)
    }

    const bounds = runBounds(shape)
    for (let t = 0; t < shape.threads; t++) {
      const tid = `t-${t.toString().padStart(5, "0")}`
      const pt = `pt-${tid}`
      const project = PROJECTS[t % PROJECTS.length]
      const [model, max] = MODELS[Math.floor(rng() * MODELS.length)]
      const provider = providerOf(model)
      const running = rng() < shape.running
      const failed = !running && rng() < 0.07
      const openRequest = rng() < shape.openRequests
      const latestRun = `r-${tid}-${shape.runs}`
      const runStatus = running ? "running" : failed ? "failed" : rng() < 0.1 ? "interrupted" : "completed"
      let lastActivity = bounds[0]

      w.put(`${PREFIX}provider_threads`, {
        provider_thread_id: pt, thread_id: tid, owner_node_id: null, provider, provider_session_id: null, status: "active",
        first_run_ordinal: 1, last_run_ordinal: shape.runs, updated_at: iso(EPOCH), driver: null, provider_instance_id: null,
        payload_json: { id: pt, contextUsage: { usedTokens: Math.round(max * (0.1 + rng() * 0.7)), maxTokens: max, inputTokens: Math.round(max * 0.4), cachedInputTokens: Math.round(max * 0.3), outputTokens: Math.round(max * 0.05), reasoningOutputTokens: Math.round(max * 0.02), updatedAt: iso(EPOCH) }, goal: lab.filler().slice(0, 300) },
      }, { thread: tid, type: "provider_thread.attached", at: iso(bounds[0]), provider })
      w.put(`${PREFIX}nodes`, {
        node_id: `n-${tid}`, thread_id: tid, parent_node_id: null, run_id: latestRun, root_node_id: `n-${tid}`,
        kind: "agent", status: runStatus, provider_thread_id: pt, provider_turn_id: null, runtime_request_id: null,
        checkpoint_scope_id: null, started_at: iso(bounds[0]), completed_at: running ? null : iso(EPOCH), payload_json: {},
      }, { thread: tid, type: "node.upserted", at: iso(bounds[0]), run: latestRun })

      for (let k = 1; k <= shape.runs; k++) {
        const run = `r-${tid}-${k}`
        const lo = bounds[k - 1]
        const hi = k === shape.runs ? EPOCH : bounds[k]
        const latest = k === shape.runs
        const status = latest ? runStatus : "completed"
        const times = itemTimes(lab, shape, shape.items, lo, hi, latest)
        writeItems(ctx, { thread: tid, run, provider: pt }, times, latest && running)
        if (times.length) lastActivity = Math.max(lastActivity, times[times.length - 1])
        const requested = lo - 1500
        const completed = latest && runStatus === "running" ? null : Math.min(hi, (times[times.length - 1] ?? lo) + 4000)
        w.put(`${PREFIX}runs`, {
          run_id: run, thread_id: tid, ordinal: k, provider, provider_thread_id: pt, status,
          requested_at: iso(requested), completed_at: iso(completed), provider_instance_id: null,
          payload_json: { id: run, threadId: tid, status, providerThreadId: pt, requestedAt: iso(requested), startedAt: iso(lo), completedAt: iso(completed), modelSelection: { model, options: [{ id: "effort", value: lab.pick(["low", "medium", "high"]) }] }, summary: lab.filler().slice(0, 220) },
        }, { thread: tid, type: "run.upserted", at: iso(completed ?? requested), run, provider })
        if (latest && rng() < 0.6) plan(ctx, { thread: tid, run }, lastActivity)
        if (latest && openRequest) {
          const rid = `q-${tid}`
          w.put(`${PREFIX}runtime_requests`, {
            runtime_request_id: rid, request_id: rid, thread_id: tid, node_id: schema === "t3" ? `n-${tid}` : null,
            provider_turn_id: null, kind: "user_input", status: "pending", created_at: iso(EPOCH - Math.round(rng() * HOUR)),
            resolved_at: null,
            // demo has no payload column on requests; do not consume filler rng there.
            payload_json: schema === "t3" ? { kind: "user_input", question: lab.filler().slice(0, 160) } : undefined,
          }, { thread: tid, type: "runtime_request.opened", at: iso(EPOCH - 1000) })
          writeItems(ctx, { thread: tid, run, provider: pt }, [EPOCH - Math.round(rng() * HOUR)], false)
          // The ask item stays pending.
          const id = `i-${ctx.lab.ordinal}`
          w.put(`${PREFIX}turn_items`, {
            turn_item_id: id, item_id: id, thread_id: tid, run_id: run, node_id: null, provider_thread_id: pt,
            provider_turn_id: null, parent_item_id: null, type: "user_input_request", status: "pending",
            updated_at: iso(EPOCH - 1000), ordinal: ctx.lab.ordinal, payload_json: payload(lab, "user_input_request", EPOCH - 1000),
          }, { thread: tid, type: "turn_item.upserted", at: iso(EPOCH - 1000), run })
          ctx.lab.ordinal++
        }
      }
      // Resolved requests: history the reader's open-request filter must skip.
      if (rng() < 0.1) {
        const rid = `q-${tid}-old`
        w.put(`${PREFIX}runtime_requests`, {
          runtime_request_id: rid, request_id: rid, thread_id: tid, node_id: schema === "t3" ? `n-${tid}` : null,
          provider_turn_id: null, kind: "permission", status: "resolved", created_at: iso(EPOCH - 2 * DAY),
          resolved_at: iso(EPOCH - 2 * DAY + 60_000),
          payload_json: schema === "t3" ? { kind: "permission", question: lab.filler().slice(0, 120) } : undefined,
        }, { thread: tid, type: "runtime_request.resolved", at: iso(EPOCH - 2 * DAY + 60_000) })
      }

      writeTurns(ctx, { thread: tid, provider: pt }, bounds[0], EPOCH, model, max, running)

      // Native subagents: activity inside the host's latest run, own provider conversation.
      const latestLo = bounds[shape.runs - 1]
      for (const sub of perThread[t]) {
        const spt = rng() < 0.02 ? null : `pt-${sub.id}` // a few "remote" subs have no local provider
        const subRunning = rng() < 0.35
        const subStatus = subRunning ? "running" : rng() < 0.08 ? "failed" : "completed"
        const subStart = latestLo + rng() * (EPOCH - latestLo) * 0.5
        const subEnd = subRunning ? null : subStart + rng() * (EPOCH - subStart) * 0.9
        let subLast = subEnd ?? EPOCH
        if (spt) {
          w.put(`${PREFIX}provider_threads`, {
            provider_thread_id: spt, thread_id: tid, owner_node_id: `n-${sub.id}`, provider, provider_session_id: null,
            status: subRunning ? "active" : "closed", first_run_ordinal: shape.runs, last_run_ordinal: shape.runs,
            updated_at: iso(subLast), driver: null, provider_instance_id: null,
            payload_json: { id: spt, contextUsage: { usedTokens: Math.round(200_000 * rng()), maxTokens: 1_000_000, updatedAt: iso(subLast) } },
          }, { thread: tid, type: "provider_thread.attached", at: iso(subStart), provider })
          const times = itemTimes(lab, shape, Math.round(shape.items * 0.7), subStart, subEnd ?? EPOCH, subRunning)
          writeItems(ctx, { thread: tid, run: latestRun, provider: spt }, times, subRunning)
          if (times.length) subLast = Math.max(subLast, times[times.length - 1])
          writeTurns(ctx, { thread: tid, provider: spt }, subStart, subLast, model, max, subRunning)
        }
        w.put(`${PREFIX}nodes`, {
          node_id: `n-${sub.id}`, thread_id: tid, parent_node_id: sub.id, run_id: latestRun, root_node_id: `n-${tid}`,
          kind: "subagent", status: subStatus, provider_thread_id: spt, provider_turn_id: null, runtime_request_id: null,
          checkpoint_scope_id: null, started_at: iso(subStart), completed_at: iso(subEnd), payload_json: {},
        }, { thread: tid, type: "node.upserted", at: iso(subStart), run: latestRun })
        w.put(`${PREFIX}subagents`, {
          subagent_id: sub.id, thread_id: tid, run_id: latestRun, parent_node_id: sub.parent ? `n-${sub.parent}` : `n-${tid}`,
          provider, provider_thread_id: spt, child_thread_id: null, origin: "provider_native", status: subStatus,
          started_at: iso(subStart), completed_at: iso(subEnd), updated_at: iso(subLast),
          driver: null, provider_instance_id: null, completion_delivery_state: null,
          payload_json: {
            id: sub.id, threadId: tid, parentNodeId: sub.parent ? `n-${sub.parent}` : `n-${tid}`, runId: latestRun,
            providerThreadId: spt, status: subStatus, title: `${lab.pick(WORDS)} ${lab.pick(WORDS)} ${sub.id}`, model,
            startedAt: iso(subStart), completedAt: iso(subEnd), updatedAt: iso(subLast),
            ...(spt ? {} : { remoteChild: { label: `${lab.pick(WORDS)} sandbox` } }),
            progress: lab.filler().slice(0, 260),
          },
        }, { thread: tid, type: "subagent.upserted", at: iso(subLast), run: latestRun, provider })
      }

      const title = `${lab.pick(WORDS)} ${lab.pick(WORDS)} ${t}`
      w.put(`${PREFIX}threads`, {
        thread_id: tid, project_id: project.id, title, default_provider: provider,
        runtime_mode: "auto", interaction_mode: "default", active_provider_thread_id: pt,
        created_at: iso(bounds[0] - 60_000), updated_at: iso(lastActivity), archived_at: null, deleted_at: null,
        provider_instance_id: null,
        payload_json: {
          id: tid, projectId: project.id, title, branch: rng() < 0.6 ? `scale/${lab.pick(WORDS)}-${t}` : null,
          activeProviderThreadId: pt, worktreePath: project.root, modelSelection: { model, options: [] },
          updatedAt: iso(lastActivity), settledOverride: null, snoozedUntil: null,
          context: lab.filler().slice(0, 240),
        },
      }, { thread: tid, type: "thread.upserted", at: iso(lastActivity), provider })
    }
    if (schema === "demo") db.exec("COMMIT")
    if (schema === "t3") {
      // Floor 0: the uniform tiers' log starts at sequence 1 and has no gap.
      ;(w as T3Sink).prune(0, iso(EPOCH)!)
      ;(w as T3Sink).release()
    }
    w.close()
    db.exec("PRAGMA wal_checkpoint(TRUNCATE)")
  } finally {
    db.close()
  }
  rmSync(path + "-shm", { force: true })
  rmSync(path + "-wal", { force: true })
  // T3 keeps its environment id beside the database; thread links need one.
  writeFileSync(join(dirname(path), "environment-id"), ENVIRONMENT + "\n")
}

// ── Skewed stress tier (--tier real) ─────────────────────────────────────────

const STRESS_SEED = 0x51e55
const STRESS_PRUNE_FLOOR = 2 ** 18
const STRESS_EVENT_ROWS = 2 ** 15

interface StressSubagent {
  child: string | null
  providerThread: string | null
  origin: "app_owned" | "provider_native"
  remoteId: string | null
  hasNode: boolean
  status: "completed" | "failed" | "interrupted"
}

interface StressThread {
  id: string
  runs: number[] // item counts in run order
  activity: "hot" | "warm" | "cold"
  subagents: StressSubagent[]
  parent: string | null
  remoteParent: string | null
}

function stressThreads(rng: () => number): StressThread[] {
  // Short tasks dominate the list, but a small set of long-running projects
  // dominates the scan. Each band draws its own run and item counts.
  const bands = [
    { threads: 192, minRuns: 1, runSpread: 2, minItems: 4, itemSpread: 28 },
    { threads: 48, minRuns: 3, runSpread: 4, minItems: 40, itemSpread: 120 },
    { threads: 16, minRuns: 8, runSpread: 5, minItems: 160, itemSpread: 320 },
  ]
  const [short, busy, heavy] = bands.map((band, group) =>
    Array.from({ length: band.threads }, (_, index): StressThread => ({
      id: `t-${group}-${String(index).padStart(3, "0")}`,
      runs: Array.from({ length: band.minRuns + Math.floor(rng() * band.runSpread) },
        () => band.minItems + Math.floor(rng() * band.itemSpread)),
      activity: group === 1 && index < 16 ? "hot" : group === 1 && index < 32 ? "warm" : "cold",
      subagents: [],
      parent: null,
      remoteParent: null,
    })))
  // A few especially long runs stress reader caps and per-run scans.
  let nextChild = 0
  heavy.forEach((host, index) => {
    host.runs[0] *= 4
    let lastChild: StressThread | undefined
    // Mix local ownership, incomplete provider linkage, retries, and
    // remote-only children. Each local retry points to a child of this host.
    const count = 8 + (index % 4) * 4
    for (let slot = 0; slot < count; slot++) {
      const kind = slot % 8
      const remote = kind === 0 || kind === 4
      const native = kind === 2 || kind === 3 || kind === 6
      const remoteId = remote ? `remote-${index}-${slot}` : null
      const child = remote ? null : kind === 7 ? lastChild! : short[nextChild++]
      if (child) {
        child.parent = host.id
        lastChild = child
      }
      host.subagents.push({
        child: child?.id ?? null,
        providerThread: child && kind !== 3 ? `pt-${child.id}` : null,
        origin: native ? "provider_native" : "app_owned",
        remoteId,
        hasNode: kind !== 3,
        status: kind === 5 ? "failed" : kind === 7 ? "interrupted" : "completed",
      })
      // A remote child can itself delegate back to this machine.
      if (slot === 0) busy[index * 2].remoteParent = remoteId
    }
  })
  return [...short, ...busy, ...heavy]
}

const PAD_UNIT = `${WORDS.join(" ")} `
const PAD = PAD_UNIT.repeat(Math.ceil((512 * 1024) / PAD_UNIT.length))

function sized(fields: Record<string, unknown>, bytes: number): Record<string, unknown> {
  const payload = { ...fields, pad: "" }
  payload.pad = PAD.slice(0, Math.max(0, bytes - JSON.stringify(payload).length))
  return payload
}

// Mix small messages with larger command and tool output, plus rare oversized
// results. Sizes vary within each family without pinning a total or percentile.
const STRESS_ITEMS = [
  ["command_execution", 4, 16], ["command_execution", 2, 8],
  ["dynamic_tool", 12, 48], ["reasoning", 1, 4],
  ["reasoning", 1, 2], ["assistant_message", 1, 2],
  ["file_change", 2, 8], ["user_message", 1, 2],
  ["checkpoint", 1, 2], ["file_search", 2, 4],
  ["web_search", 2, 6], ["subagent", 2, 6],
  ["notification", 1, 2],
] as const

function stressItem(rng: () => number, ordinal: number, todo: boolean) {
  if (todo) return { type: "todo_list", bytes: 1024 }
  if (ordinal % 512 === 0) return { type: "dynamic_tool", bytes: 512 * 1024 }
  const [type, min, max] = STRESS_ITEMS[Math.floor(rng() * STRESS_ITEMS.length)]
  return { type, bytes: 1024 * (min + Math.floor(rng() * (max - min + 1))) }
}

function stressProjects() {
  return Array.from({ length: 16 }, (_, i) => ({
    id: `p${String(i).padStart(2, "0")}`,
    title: `lab-${WORDS[i % WORDS.length]}`,
    root: `/home/user/projects/lab-${WORDS[i % WORDS.length]}-${i}`,
  }))
}

function stressEvents(threads: StressThread[], projects: ReturnType<typeof stressProjects>) {
  const active = threads.filter((thread) => thread.activity !== "cold")
  const cold = threads.filter((thread) => thread.activity === "cold")
  const kinds = ["turn-item.updated", "node.updated", "turn-item.updated", "run.updated", "provider-turn.updated", "subagent.updated"]
  const projectInterval = STRESS_EVENT_ROWS / projects.length
  let activeIndex = 0
  let coldIndex = 0
  return Array.from({ length: STRESS_EVENT_ROWS }, (_, i) => {
    const sequence = STRESS_PRUNE_FLOOR + 1 + i
    const at = iso(EPOCH - (STRESS_EVENT_ROWS - i) * 1000)!
    if (i % projectInterval === 0) {
      return { sequence, kind: "project", stream: projects[i / projectInterval].id, type: "project.updated", at }
    }
    const thread = i % 4 === 0 ? cold[coldIndex++ % cold.length] : active[activeIndex++ % active.length]
    return { sequence, kind: "thread", stream: thread.id, type: kinds[i % kinds.length], at }
  })
}

function generateStress(path: string) {
  const rng = mulberry32(STRESS_SEED)
  const threads = stressThreads(rng)
  const projects = stressProjects()
  rmSync(path, { force: true })
  rmSync(path + "-wal", { force: true })
  rmSync(path + "-shm", { force: true })
  mkdirSync(dirname(path), { recursive: true })
  const db = new DatabaseSync(path)
  const lab = new Lab({ ...TIERS.s, seed: STRESS_SEED })
  try {
    db.exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-200000")
    db.exec(T3_SCHEMA)
    db.exec("CREATE TABLE scale_meta (key TEXT PRIMARY KEY, value TEXT)")
    const w = new T3Sink(db, lab)
    w.hold()
    const opened = iso(EPOCH - 21 * DAY)!
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('seed', ?)", String(STRESS_SEED))
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('schema', 't3')")
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('shape', ?)", JSON.stringify({ tier: "real", workload: "skewed-v1", seed: STRESS_SEED }))
    w.raw("INSERT INTO scale_meta (key, value) VALUES ('mutations', '0')")
    w.raw("INSERT INTO orchestration_v2_projection_metadata (projection_name, schema_version, last_sequence, updated_at) VALUES ('thread-projections', 1, 0, ?)", opened)
    w.ledger(LEDGER_ROWS, opened)
    for (const p of projects) {
      w.upsertRow("projection_projects", {
        project_id: p.id, title: p.title, workspace_root: p.root, scripts_json: {}, created_at: opened,
        updated_at: iso(EPOCH), deleted_at: null, default_model_selection_json: null, default_thread_env_mode: null,
        favicon_path: null, auto_pull: 0, project_icon_json: null, pinned_at: null, archived_at: null,
      })
    }
    const running = threads.filter((thread) => thread.activity === "hot")
    const nodeKinds = ["tool_call", "reasoning", "tool_call", "assistant_message"]
    let itemNo = 0
    let nodeNo = 0
    for (const [index, t] of threads.entries()) {
      const project = projects[index % projects.length]
      const model = MODELS[index % MODELS.length][0]
      const max = MODELS[index % MODELS.length][1]
      const provider = providerOf(model)
      const pt = `pt-${t.id}`
      const live = t.activity === "hot"
      const latestRun = `r-${t.id}-${t.runs.length}`
      const created = iso(EPOCH - 21 * DAY + index * 1000)!
      w.upsertRow(`${PREFIX}provider_threads`, {
        provider_thread_id: pt, thread_id: t.id, owner_node_id: null, provider, provider_session_id: null,
        status: live ? "active" : "closed", first_run_ordinal: 1, last_run_ordinal: t.runs.length, updated_at: iso(EPOCH),
        driver: null, provider_instance_id: null,
        payload_json: sized({ id: pt, contextUsage: { usedTokens: Math.round(max * 0.4), maxTokens: max, inputTokens: Math.round(max * 0.3), cachedInputTokens: Math.round(max * 0.2), outputTokens: Math.round(max * 0.05), reasoningOutputTokens: Math.round(max * 0.02), updatedAt: iso(EPOCH) } }, 1400),
      })
      const latestAt = EPOCH - (live ? 2 * HOUR : t.activity === "warm" ? 12 * HOUR : 2 * DAY)
      const updated = iso(live ? latestAt + HOUR : latestAt + 2 * HOUR)
      for (const [runIndex, nItems] of t.runs.entries()) {
        const k = runIndex + 1
        const run = `r-${t.id}-${k}`
        const latest = k === t.runs.length
        const root = `root-${run}`
        const runLive = live && latest
        const requested = latestAt - (t.runs.length - k) * DAY
        const completed = runLive ? null : iso(requested + 2 * HOUR)
        const nNodes = 1 + Math.floor(nItems * 0.75)
        const reserveTodo = latest && index % 4 === 0
        w.upsertRow(`${PREFIX}runs`, {
          run_id: run, thread_id: t.id, ordinal: k, provider, provider_thread_id: pt,
          status: runLive ? "running" : "completed", requested_at: iso(requested),
          completed_at: completed, provider_instance_id: null,
          payload_json: sized({
            id: run, threadId: t.id, status: runLive ? "running" : "completed", providerThreadId: pt,
            requestedAt: iso(requested), startedAt: iso(requested + 1000),
            completedAt: completed,
            modelSelection: { model, options: [{ id: "effort", value: "medium" }] },
          }, nItems >= 160 ? 2048 : 768),
        })
        for (let n = 0; n < nNodes; n++) {
          const id = n === 0 ? root : `nd-${nodeNo}`
          const kind = n === 0 ? "root_turn" : reserveTodo && n === 1 ? "todo_list" : nodeKinds[n % nodeKinds.length]
          nodeNo++
          w.upsertRow(`${PREFIX}nodes`, {
            node_id: id, thread_id: t.id, run_id: run, parent_node_id: n === 0 ? null : root, root_node_id: root,
            kind, status: runLive ? "running" : "completed", provider_thread_id: pt, provider_turn_id: null,
            runtime_request_id: null, checkpoint_scope_id: null, started_at: iso(requested),
            completed_at: completed, payload_json: sized({ node: id }, kind === "root_turn" ? 2048 : 768),
          })
        }
        for (let j = 0; j < nItems; j++) {
          const spec = stressItem(rng, itemNo, reserveTodo && j === 0)
          const status = runLive && j === nItems - 1 ? "running"
            : j % 97 === 11 ? "failed" : j % 97 === 35 ? "interrupted"
            : j % 97 === 62 ? "cancelled" : "completed"
          const when = requested + Math.floor((j + 1) * HOUR / nItems)
          const id = `i-${itemNo}`
          w.upsertRow(`${PREFIX}turn_items`, {
            turn_item_id: id, thread_id: t.id, run_id: run, node_id: root, provider_thread_id: pt,
            provider_turn_id: null, parent_item_id: null, ordinal: itemNo, type: spec.type, status,
            updated_at: iso(when), payload_json: sized({
              startedAt: iso(when - 1000), status, schema: 2,
              toolName: spec.type === "dynamic_tool" ? "Read" : undefined,
              input: spec.type === "command_execution" ? "cargo check" : undefined,
              fileName: spec.type === "file_change" ? "src/lib.rs" : undefined,
              exitCode: spec.type === "command_execution" && status !== "running" ? (status === "failed" ? 1 : 0) : undefined,
              steps: spec.type === "todo_list" ? [{ text: "Check synthetic output", status: "in_progress" }] : undefined,
              title: spec.type,
            }, spec.bytes),
          })
          itemNo++
        }
      }
      // Running threads keep the newest turn open.
      const turns = Math.min(t.runs.length, 4)
      for (let k = 1; k <= turns; k++) {
        const open = live && k === turns
        const start = latestAt - (turns - k) * HOUR
        w.upsertRow(`${PREFIX}provider_turns`, {
          provider_turn_id: `turn-${pt}-${k}`, thread_id: t.id, provider_thread_id: pt, node_id: `root-${latestRun}`,
          run_attempt_id: null, ordinal: k, status: open ? "running" : "completed",
          started_at: iso(start), completed_at: open ? null : iso(start + 30 * 60_000),
          payload_json: sized({
            providerThreadId: pt, model, status: open ? "running" : "completed",
            tokenUsage: { usedTokens: 40_000 + index * 10, maxTokens: max, inputTokens: 20_000, cachedInputTokens: 15_000, outputTokens: 4_000, reasoningOutputTokens: 2_000, updatedAt: iso(start) },
          }, open ? 2048 : 1024),
        })
      }
      for (const [s, spec] of t.subagents.entries()) {
        const sid = `s-${t.id}-${String(s).padStart(3, "0")}`
        const root = `root-${latestRun}`
        const ended = updated
        if (spec.hasNode) {
          w.upsertRow(`${PREFIX}nodes`, {
            node_id: sid, thread_id: t.id, run_id: latestRun, parent_node_id: root, root_node_id: root,
            kind: "subagent", status: spec.status, provider_thread_id: pt, provider_turn_id: null,
            runtime_request_id: null, checkpoint_scope_id: null, started_at: opened, completed_at: ended,
            payload_json: sized({ node: sid }, 2048),
          })
        }
        w.upsertRow(`${PREFIX}subagents`, {
          subagent_id: sid, thread_id: t.id, run_id: latestRun, parent_node_id: root, provider,
          provider_thread_id: spec.providerThread, child_thread_id: spec.child, origin: spec.origin,
          status: spec.status, started_at: opened, completed_at: ended,
          updated_at: ended, driver: null, provider_instance_id: null, completion_delivery_state: null,
          payload_json: sized({
            id: sid, threadId: t.id, parentNodeId: root, runId: latestRun,
            providerThreadId: spec.providerThread, childThreadId: spec.child, origin: spec.origin,
            status: spec.status, title: `${WORDS[s % WORDS.length]} ${WORDS[(s + 3) % WORDS.length]}`,
            model, startedAt: opened, completedAt: ended, updatedAt: ended,
            createdBy: spec.origin === "app_owned" ? "app" : "user",
            driver: null, modelSelection: { model, options: [] }, providerInstanceId: null,
            nativeTaskRef: null, cancelledRunId: null, completionDelivery: null, completionWake: null,
            progress: spec.status === "completed" ? null : "synthetic progress note",
            ...(spec.remoteId ? { remoteChild: { environmentId: REMOTE_ENV, threadId: spec.remoteId, label: `${WORDS[s % WORDS.length]} sandbox` } } : {}),
          }, spec.remoteId ? 8192 : 2048),
        })
      }
      const remoteParent = t.remoteParent
      const lineage = t.parent == null ? undefined : { relationshipToParent: "subagent", parentThreadId: t.parent }
      w.upsertRow(`${PREFIX}threads`, {
        thread_id: t.id, project_id: project.id, title: `${WORDS[index % WORDS.length]} ${WORDS[(index * 3) % WORDS.length]} ${index}`,
        default_provider: provider, runtime_mode: "auto", interaction_mode: "default", active_provider_thread_id: pt,
        created_at: created, updated_at: updated, archived_at: null, deleted_at: null, provider_instance_id: null,
        payload_json: sized({
          id: t.id, projectId: project.id, title: `${WORDS[index % WORDS.length]} work ${index}`,
          branch: index % 3 === 0 ? null : `scale/${WORDS[index % WORDS.length]}-${index}`,
          activeProviderThreadId: pt, worktreePath: project.root, modelSelection: { model, options: [] },
          updatedAt: updated, settledOverride: null, snoozedUntil: null, lineage,
          // Child threads carry the fork pointer back to the host; unread filler.
          ...(t.parent != null ? { forkedFrom: t.parent } : {}),
          // Delegated-in threads name their remote parent (not in this
          // database) and carry the link marker. linkOrigin's shape is invented.
          ...(remoteParent
            ? { delegatedFrom: { environmentId: REMOTE_ENV, threadId: remoteParent, title: `remote ${WORDS[index % WORDS.length]} ${index}` }, linkOrigin: "delegation" }
            : {}),
        }, t.subagents.length ? 3072 : 1024),
      })
    }
    // Pending and resolved requests coexist on active threads.
    for (const [i, host] of running.entries()) {
      const open = i % 2 === 0
      const rid = `q-${host.id}`
      const at = EPOCH - (open ? HOUR / 2 : HOUR)
      w.upsertRow(`${PREFIX}runtime_requests`, {
        runtime_request_id: rid, thread_id: host.id, node_id: `root-r-${host.id}-${host.runs.length}`, provider_turn_id: null,
        kind: "user_input", status: open ? "pending" : "resolved", created_at: iso(at),
        resolved_at: open ? null : iso(at + 60_000),
        payload_json: { kind: "user_input", question: "continue with the synthetic check" },
      })
    }
    w.tail(stressEvents(threads, projects))
    w.prune(STRESS_PRUNE_FLOOR, iso(EPOCH)!)
    w.release()
    w.close()
    db.exec("PRAGMA wal_checkpoint(TRUNCATE)")
  } finally {
    db.close()
  }
  rmSync(path + "-shm", { force: true })
  rmSync(path + "-wal", { force: true })
  writeFileSync(join(dirname(path), "environment-id"), ENVIRONMENT + "\n")
}

// ── Mutation stepper ────────────────────────────────────────────────────────

interface MutateInfo {
  db: DatabaseSync
  schema: Schema
  seed: number
  step: number
  rng: () => number
  lab: Lab
  at: (ms: number) => string | null
  running: { run_id: string; thread_id: string; payload_json: string }[]
}

function openMutation(path: string, step: number): MutateInfo {
  const db = new DatabaseSync(path)
  db.exec("PRAGMA busy_timeout=2000")
  const meta = (key: string) => (db.prepare("SELECT value FROM scale_meta WHERE key=?").get(key) as { value: string } | undefined)?.value
  const seed = Number(meta("seed") ?? "1")
  const applied = Number(meta("mutations") ?? "0")
  if (step !== applied + 1) throw new Error(`step ${step} requested but ${applied} applied; steps are sequential`)
  const schema = (meta("schema") ?? "demo") as Schema
  const rng = mulberry32((seed ^ Math.imul(step + 1, 0x9e3779b9)) >>> 0)
  const lab = new Lab({ ...TIERS.s, seed: (seed ^ step) >>> 0 })
  lab.ordinal = 0
  const at = (ms: number) => iso(EPOCH + step * 60_000 + ms)
  const running = db.prepare(`SELECT run_id, thread_id, payload_json FROM ${PREFIX}runs WHERE completed_at IS NULL ORDER BY run_id`).all() as { run_id: string; thread_id: string; payload_json: string }[]
  return { db, schema, seed, step, rng, lab, at, running }
}

/**
 * Apply step K of a deterministic write stream ("T3 keeps writing"). Steps must
 * be applied in order: the stream position is kept in scale_meta. Step K uses a
 * PRNG seeded from (seed, K), so the stream is reproducible for replay.
 *
 * demo schema: the original six shapes (K % 6), one big transaction, plain
 * inserts — unchanged so the old numbers stay reproducible.
 *
 * t3 schema: ten shapes (K % 10), each write its own event transaction like
 * T3's writer; `--touch N` replaces the shape with a burst touching N
 * different threads in one step. Writes that change a thread's content also
 * upsert the thread row's updated_at/updatedAt, the way T3's projection bumps
 * thread recency on activity — without that, no ordinary write ever touches
 * the threads table and a graph poll never runs. Shapes 8 and 9 exercise the
 * cross-machine structure of the skewed tier (activity on a delegated-in
 * thread, progress on a remote child); on databases without those rows they
 * fall back to the append-items and flip-item shapes.
 */
function mutate(path: string, step: number, touch: number) {
  const info = openMutation(path, step)
  try {
    const out = info.schema === "t3" ? mutateT3(info, touch) : mutateDemo(info, touch)
    info.db.prepare("UPDATE scale_meta SET value=? WHERE key='mutations'").run(String(step))
    info.db.exec("PRAGMA wal_checkpoint(PASSIVE)")
    return out
  } finally {
    info.db.close()
  }
}

function mutateDemo({ db, step, rng, lab, at, running }: MutateInfo, touch: number) {
  if (touch) throw new Error("--touch needs --schema t3 databases")
  const w = new DemoSink(db)
  const maxOrdinal = () => (db.prepare(`SELECT COALESCE(MAX(ordinal),0) AS o FROM ${PREFIX}turn_items`).get() as { o: number }).o
  let ordinal = maxOrdinal()
  const next = () => ++ordinal
  const kind = step % 6
  let detail = ""
  db.exec("BEGIN")
  if (kind === 0) {
    // Append turn items for running agents.
    if (!running.length) throw new Error("no running runs to extend")
    const n = Math.min(running.length, 8)
    for (let i = 0; i < n; i++) {
      const run = running[Math.floor(rng() * running.length)]
      const items = 3 + Math.floor(rng() * 4)
      for (let j = 0; j < items; j++) {
        const type = itemType(rng)
        const o = next()
        w.put(`${PREFIX}turn_items`, { turn_item_id: `i-m${step}-${o}`, item_id: `i-m${step}-${o}`, thread_id: run.thread_id, run_id: run.run_id, node_id: null, provider_thread_id: `pt-${run.thread_id}`, type, status: j === items - 1 ? "running" : "completed", updated_at: at(j * 1000), ordinal: o, payload_json: payload(lab, type, EPOCH + step * 60_000) })
      }
    }
    detail = `appended items to ${n} running agents`
  } else if (kind === 1) {
    // Complete a run.
    if (!running.length) throw new Error("no running runs to complete")
    const run = running[Math.floor(rng() * running.length)]
    const payloadRow = JSON.parse(run.payload_json)
    payloadRow.status = "completed"
    payloadRow.completedAt = at(0)
    db.prepare(`UPDATE ${PREFIX}runs SET completed_at=?, payload_json=? WHERE run_id=?`).run(at(0), JSON.stringify(payloadRow), run.run_id)
    db.prepare(`UPDATE ${PREFIX}turn_items SET status='completed', updated_at=? WHERE run_id=? AND status='running'`).run(at(0), run.run_id)
    detail = `completed ${run.run_id}`
  } else if (kind === 2) {
    // Start a subagent on a running thread.
    if (!running.length) throw new Error("no running threads for a subagent")
    const run = running[Math.floor(rng() * running.length)]
    const sid = `s-mut-${step}`
    const spt = `pt-${sid}`
    const model = "claude-sonnet-5-5"
    w.put(`${PREFIX}provider_threads`, { provider_thread_id: spt, thread_id: run.thread_id, payload_json: { id: spt, contextUsage: { usedTokens: 4000, maxTokens: 1_000_000, updatedAt: at(0) } } })
    w.put(`${PREFIX}nodes`, { node_id: `n-${sid}`, thread_id: run.thread_id, parent_node_id: sid, run_id: run.run_id })
    w.put(`${PREFIX}subagents`, { subagent_id: sid, thread_id: run.thread_id, payload_json: { id: sid, threadId: run.thread_id, parentNodeId: `n-${run.thread_id}`, runId: run.run_id, providerThreadId: spt, status: "running", title: `mutation subagent ${step}`, model, startedAt: at(0), completedAt: null, updatedAt: at(0) } })
    const o = next()
    w.put(`${PREFIX}turn_items`, { turn_item_id: `i-m${step}-${o}`, item_id: `i-m${step}-${o}`, thread_id: run.thread_id, run_id: run.run_id, node_id: null, provider_thread_id: spt, type: "dynamic_tool", status: "running", updated_at: at(0), ordinal: o, payload_json: payload(lab, "dynamic_tool", EPOCH + step * 60_000) })
    detail = `started ${sid} under ${run.thread_id}`
  } else if (kind === 3) {
    // Add a provider turn to running provider threads.
    const pts = db.prepare(`SELECT DISTINCT provider_thread_id AS pt, thread_id FROM ${PREFIX}provider_turns WHERE completed_at IS NULL ORDER BY provider_thread_id`).all() as { pt: string; thread_id: string }[]
    const n = Math.min(pts.length, 4)
    for (let i = 0; i < n; i++) {
      const p = pts[Math.floor(rng() * pts.length)]
      db.prepare(`UPDATE ${PREFIX}provider_turns SET completed_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE provider_thread_id=? AND completed_at IS NULL`).run(at(0), p.pt)
      const o = (db.prepare(`SELECT COALESCE(MAX(ordinal),0) AS o FROM ${PREFIX}provider_turns WHERE provider_thread_id=?`).get(p.pt) as { o: number }).o + 1
      w.put(`${PREFIX}provider_turns`, { thread_id: p.thread_id, provider_thread_id: p.pt, ordinal: o, started_at: at(0), completed_at: null, payload_json: { providerThreadId: p.pt, model: "claude-sonnet-5-5", status: "running", tokenUsage: { usedTokens: 500_000 + Math.round(rng() * 100_000), maxTokens: 1_000_000, inputTokens: 400_000, cachedInputTokens: 350_000, outputTokens: 12_000, reasoningOutputTokens: 6_000, updatedAt: at(0) } } })
    }
    detail = `added a provider turn to ${n} provider threads`
  } else if (kind === 4) {
    // Open a request on a running thread that has none.
    const busy = new Set((db.prepare(`SELECT thread_id FROM ${PREFIX}runtime_requests WHERE resolved_at IS NULL`).all() as { thread_id: string }[]).map((r) => r.thread_id))
    const free = running.filter((r) => !busy.has(r.thread_id))
    if (!free.length) throw new Error("no running thread without an open request")
    const run = free[Math.floor(rng() * free.length)]
    w.put(`${PREFIX}runtime_requests`, { runtime_request_id: `q-mut-${step}`, request_id: `q-mut-${step}`, thread_id: run.thread_id, node_id: null, kind: "user_input", status: "pending", created_at: at(0), resolved_at: null })
    const o = next()
    w.put(`${PREFIX}turn_items`, { turn_item_id: `i-m${step}-${o}`, item_id: `i-m${step}-${o}`, thread_id: run.thread_id, run_id: run.run_id, node_id: null, provider_thread_id: `pt-${run.thread_id}`, type: "user_input_request", status: "pending", updated_at: at(0), ordinal: o, payload_json: payload(lab, "user_input_request", EPOCH + step * 60_000) })
    detail = `opened a request on ${run.thread_id}`
  } else {
    // Resolve the oldest open request.
    const open = db.prepare(`SELECT request_id, thread_id FROM ${PREFIX}runtime_requests WHERE resolved_at IS NULL ORDER BY created_at LIMIT 1`).get() as { request_id: string; thread_id: string } | undefined
    if (!open) throw new Error("no open request to resolve")
    db.prepare(`UPDATE ${PREFIX}runtime_requests SET status='resolved', resolved_at=? WHERE request_id=?`).run(at(0), open.request_id)
    db.prepare(`UPDATE ${PREFIX}turn_items SET status='completed', updated_at=? WHERE thread_id=? AND type IN ('user_input_request','secret_request','link_request') AND status='pending'`).run(at(0), open.thread_id)
    detail = `resolved ${open.request_id}`
  }
  db.exec("COMMIT")
  w.close()
  return { step, kind: ["append-items", "complete-run", "start-subagent", "provider-turn", "open-request", "resolve-request"][kind], detail }
}

/** t3-schema mutation: every shape is event transactions like T3's writer. */
function mutateT3(info: MutateInfo, touch: number) {
  const { db, step, rng, lab, at, running } = info
  const w = new T3Sink(db, lab)
  const maxOrdinal = () => (db.prepare(`SELECT COALESCE(MAX(ordinal),0) AS o FROM ${PREFIX}turn_items`).get() as { o: number }).o
  let ordinal = maxOrdinal()
  const next = () => ++ordinal
  const model = "claude-sonnet-5-5"
  const provider = providerOf(model)

  const appendItem = (thread: string, run: string | null, pt: string | null, ms: number, status: string, type = itemType(rng)) => {
    const o = next()
    w.put(`${PREFIX}turn_items`, {
      turn_item_id: `i-m${step}-${o}`, item_id: `i-m${step}-${o}`, thread_id: thread, run_id: run, node_id: null,
      provider_thread_id: pt, provider_turn_id: null, parent_item_id: null, type, status,
      updated_at: at(ms), ordinal: o, payload_json: payload(lab, type, EPOCH + step * 60_000 + ms),
    }, { thread, type: "turn_item.upserted", at: at(ms), run, provider })
  }

  // T3's thread projection tracks recency: a write on a thread bumps the
  // thread row's updated_at (and the payload's updatedAt) in the same
  // transaction. The readers see a threads-table change on ordinary writes.
  const threadRow = (tid: string) =>
    db.prepare(`SELECT ${T3_COLS[`${PREFIX}threads`].join(",")} FROM ${PREFIX}threads WHERE thread_id=?`).get(tid) as Record<string, unknown> | undefined
  const bumpThreadRow = (tid: string, ms: number) => {
    const full = threadRow(tid)
    if (!full) return
    const body = { ...JSON.parse(full.payload_json as string), updatedAt: at(ms) }
    w.upsertRow(`${PREFIX}threads`, { ...full, updated_at: at(ms), payload_json: body })
  }
  const bumpThread = (tid: string, ms: number) => w.event({ thread: tid, type: "thread.updated", at: at(ms) }, () => bumpThreadRow(tid, ms))

  const appendItemsToRunning = () => {
    if (!running.length) throw new Error("no running runs to extend")
    const n = Math.min(running.length, 8)
    const touched = new Set<string>()
    for (let i = 0; i < n; i++) {
      const run = running[Math.floor(rng() * running.length)]
      const items = 3 + Math.floor(rng() * 4)
      for (let j = 0; j < items; j++) appendItem(run.thread_id, run.run_id, `pt-${run.thread_id}`, j * 1000, j === items - 1 ? "running" : "completed")
      touched.add(run.thread_id)
    }
    for (const tid of touched) bumpThread(tid, 2000)
    return `appended items to ${n} running agents`
  }

  const flipOneItem = () => {
    // Flip an existing running item to completed in place (T3 updates rows; the rowid stays).
    const cols = T3_COLS[`${PREFIX}turn_items`]
    const item = db.prepare(`SELECT ${cols.join(",")} FROM ${PREFIX}turn_items WHERE status='running' ORDER BY turn_item_id LIMIT 1`).get() as Record<string, unknown> | undefined
    if (!item) throw new Error("no running item to flip")
    const body = { ...JSON.parse(item.payload_json as string), status: "completed" }
    w.event({ thread: item.thread_id as string, type: "turn_item.completed", at: at(0), run: item.run_id as string | null, provider }, () => {
      w.upsertRow(`${PREFIX}turn_items`, {
        ...item, status: "completed", updated_at: at(0), payload_json: body,
      })
      bumpThreadRow(item.thread_id as string, 0)
    })
    return `flipped ${item.turn_item_id} on ${item.thread_id}`
  }

  let kind = ""
  let detail = ""
  if (touch > 0) {
    // Burst: touch N different threads in one step (one appended item and the
    // thread recency bump on each, in that thread's event transaction).
    const all = (db.prepare(`SELECT thread_id FROM ${PREFIX}threads ORDER BY thread_id`).all() as { thread_id: string }[]).map((r) => r.thread_id)
    if (!all.length) throw new Error("no threads to touch")
    const n = Math.min(touch, all.length)
    const chosen = new Set<string>()
    let i = Math.floor(rng() * all.length)
    while (chosen.size < n) {
      chosen.add(all[i % all.length])
      i += 7919
    }
    for (const tid of chosen) {
      const run = (db.prepare(`SELECT run_id FROM ${PREFIX}runs WHERE thread_id=? ORDER BY ordinal DESC LIMIT 1`).get(tid) as { run_id: string } | undefined)?.run_id ?? null
      const o = next()
      const type = itemType(rng)
      w.event({ thread: tid, type: "turn_item.upserted", at: at(0), run, provider }, () => {
        w.upsertRow(`${PREFIX}turn_items`, {
          turn_item_id: `i-m${step}-${o}`, item_id: `i-m${step}-${o}`, thread_id: tid, run_id: run, node_id: null,
          provider_thread_id: `pt-${tid}`, provider_turn_id: null, parent_item_id: null, type, status: "completed",
          updated_at: at(0), ordinal: o, payload_json: payload(lab, type, EPOCH + step * 60_000),
        })
        bumpThreadRow(tid, 0)
      })
    }
    w.close()
    return { step, kind: `burst-${n}`, detail: `touched ${n} threads: one item each` }
  }

  switch (step % 10) {
    case 0: {
      // Append turn items for running agents.
      kind = "append-items"
      detail = appendItemsToRunning()
      break
    }
    case 1: {
      kind = "flip-item"
      detail = flipOneItem()
      break
    }
    case 2: {
      // Complete a run: one event covering the run upsert, the item flips and
      // the thread recency bump.
      if (!running.length) throw new Error("no running runs to complete")
      const run = running[Math.floor(rng() * running.length)]
      const full = db.prepare(`SELECT ${T3_COLS[`${PREFIX}runs`].join(",")} FROM ${PREFIX}runs WHERE run_id=?`).get(run.run_id) as Record<string, unknown>
      const payloadRow = JSON.parse(run.payload_json)
      payloadRow.status = "completed"
      payloadRow.completedAt = at(0)
      kind = "complete-run"
      w.event({ thread: run.thread_id, type: "run.completed", at: at(0), run: run.run_id, provider }, () => {
        w.upsertRow(`${PREFIX}runs`, { ...full, status: "completed", completed_at: at(0), payload_json: payloadRow })
        db.prepare(`UPDATE ${PREFIX}turn_items SET status='completed', updated_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE run_id=? AND status='running'`).run(at(0), run.run_id)
        bumpThreadRow(run.thread_id, 0)
      })
      detail = `completed ${run.run_id}`
      break
    }
    case 3: {
      // Start a subagent with its own child thread: the subagent row and its
      // spawning node live under the parent thread, the child thread and its
      // provider conversation are new thread-scoped rows of their own. The
      // subagent's provider thread sits ON the child thread, its provider turn
      // lands on the child thread, and the child carries lineage + forkedFrom.
      if (!running.length) throw new Error("no running threads for a subagent")
      const run = running[Math.floor(rng() * running.length)]
      const host = run.thread_id
      const sid = `s-mut-${step}`
      const child = `t-mut-${step}`
      const spt = `pt-${sid}`
      const project = (db.prepare(`SELECT project_id, title FROM ${PREFIX}threads WHERE thread_id=?`).get(host) as { project_id: string; title: string } | undefined) ?? { project_id: "atlas", title: host }
      kind = "start-subagent-child"
      w.put(`${PREFIX}threads`, {
        thread_id: child, project_id: project.project_id, title: `mutation child ${step}`, default_provider: provider,
        runtime_mode: "auto", interaction_mode: "default", active_provider_thread_id: spt,
        created_at: at(0), updated_at: at(0), archived_at: null, deleted_at: null, provider_instance_id: null,
        payload_json: { id: child, projectId: project.project_id, title: `mutation child ${step}`, branch: null, activeProviderThreadId: spt, worktreePath: null, modelSelection: { model, options: [] }, updatedAt: at(0), settledOverride: null, snoozedUntil: null, lineage: { relationshipToParent: "subagent", parentThreadId: host }, forkedFrom: host },
      }, { thread: child, type: "thread.created", at: at(0), provider })
      w.event({ thread: host, type: "subagent.started", at: at(0), run: run.run_id, provider }, () => {
        w.upsertRow(`${PREFIX}nodes`, {
          node_id: `n-${sid}`, thread_id: host, run_id: run.run_id, parent_node_id: sid, root_node_id: `n-${host}`,
          kind: "subagent", status: "running", provider_thread_id: spt, provider_turn_id: null, runtime_request_id: null,
          checkpoint_scope_id: null, started_at: at(0), completed_at: null, payload_json: {},
        })
        w.upsertRow(`${PREFIX}subagents`, {
          subagent_id: sid, thread_id: host, run_id: run.run_id, parent_node_id: `n-${host}`, provider,
          provider_thread_id: spt, child_thread_id: child, origin: "app_owned", status: "running",
          started_at: at(0), completed_at: null, updated_at: at(0), driver: null, provider_instance_id: null, completion_delivery_state: null,
          payload_json: { id: sid, threadId: host, parentNodeId: `n-${host}`, runId: run.run_id, providerThreadId: spt, childThreadId: child, origin: "app_owned", status: "running", title: `mutation subagent ${step}`, model, startedAt: at(0), completedAt: null, updatedAt: at(0) },
        })
        bumpThreadRow(host, 0)
      })
      w.event({ thread: child, type: "provider_turn.appended", at: at(0), provider }, () => {
        w.upsertRow(`${PREFIX}provider_threads`, {
          provider_thread_id: spt, thread_id: child, owner_node_id: `n-${sid}`, provider, provider_session_id: null,
          status: "active", first_run_ordinal: null, last_run_ordinal: null, updated_at: at(0), driver: null, provider_instance_id: null,
          payload_json: { id: spt, contextUsage: { usedTokens: 4000, maxTokens: 1_000_000, updatedAt: at(0) } },
        })
        w.upsertRow(`${PREFIX}provider_turns`, {
          provider_turn_id: `turn-${spt}-1`, thread_id: child, provider_thread_id: spt, node_id: `n-${sid}`,
          run_attempt_id: null, ordinal: 1, status: "running", started_at: at(0), completed_at: null,
          payload_json: {
            providerThreadId: spt, model, status: "running",
            tokenUsage: { usedTokens: 4000, maxTokens: 1_000_000, inputTokens: 3000, cachedInputTokens: 2000, outputTokens: 800, reasoningOutputTokens: 400, updatedAt: at(0) },
          },
        })
      })
      appendItem(child, null, spt, 0, "running", "dynamic_tool")
      detail = `started ${sid} under ${host} with child thread ${child}`
      break
    }
    case 4: {
      // Append a provider turn with token usage to running provider threads.
      // The turn lands on the provider thread's own thread — for a child agent
      // conversation that is the child thread.
      const pts = db.prepare(`SELECT DISTINCT provider_thread_id AS pt, thread_id FROM ${PREFIX}provider_turns WHERE completed_at IS NULL ORDER BY provider_thread_id`).all() as { pt: string; thread_id: string }[]
      const n = Math.min(pts.length, 4)
      kind = "provider-turn"
      for (let i = 0; i < n; i++) {
        const p = pts[Math.floor(rng() * pts.length)]
        const o = (db.prepare(`SELECT COALESCE(MAX(ordinal),0) AS o FROM ${PREFIX}provider_turns WHERE provider_thread_id=?`).get(p.pt) as { o: number }).o + 1
        const used = 500_000 + Math.round(rng() * 100_000)
        w.event({ thread: p.thread_id, type: "provider_turn.appended", at: at(0), provider }, () => {
          db.prepare(`UPDATE ${PREFIX}provider_turns SET completed_at=?, status='completed', payload_json=json_set(payload_json,'$.status','completed') WHERE provider_thread_id=? AND completed_at IS NULL`).run(at(0), p.pt)
          w.upsertRow(`${PREFIX}provider_turns`, {
            provider_turn_id: `turn-${p.pt}-${o}`, thread_id: p.thread_id, provider_thread_id: p.pt, node_id: `n-${p.thread_id}`,
            run_attempt_id: null, ordinal: o, status: "running", started_at: at(0), completed_at: null,
            payload_json: {
              providerThreadId: p.pt, model, status: "running",
              tokenUsage: { usedTokens: used, maxTokens: 1_000_000, inputTokens: 400_000, cachedInputTokens: 350_000, outputTokens: 12_000, reasoningOutputTokens: 6_000, updatedAt: at(0) },
              turnTokenUsage: { inputTokens: 1_600_000, cachedInputTokens: 1_400_000, outputTokens: 24_000, reasoningOutputTokens: 12_000, usageStatus: "in_progress", hasSubagents: false },
            },
          })
          bumpThreadRow(p.thread_id, 0)
        })
      }
      detail = `added a provider turn to ${n} provider threads`
      break
    }
    case 5: {
      // Open a request on a running thread that has none.
      const busy = new Set((db.prepare(`SELECT thread_id FROM ${PREFIX}runtime_requests WHERE resolved_at IS NULL`).all() as { thread_id: string }[]).map((r) => r.thread_id))
      const free = running.filter((r) => !busy.has(r.thread_id))
      if (!free.length) throw new Error("no running thread without an open request")
      const run = free[Math.floor(rng() * free.length)]
      const rid = `q-mut-${step}`
      kind = "open-request"
      w.event({ thread: run.thread_id, type: "runtime_request.opened", at: at(0), run: run.run_id, provider }, () => {
        w.upsertRow(`${PREFIX}runtime_requests`, {
          runtime_request_id: rid, thread_id: run.thread_id, node_id: `n-${run.thread_id}`, provider_turn_id: null,
          kind: "user_input", status: "pending", created_at: at(0), resolved_at: null,
          payload_json: { kind: "user_input", question: lab.filler().slice(0, 160) },
        })
        bumpThreadRow(run.thread_id, 0)
      })
      appendItem(run.thread_id, run.run_id, `pt-${run.thread_id}`, 0, "pending", "user_input_request")
      detail = `opened a request on ${run.thread_id}`
      break
    }
    case 6: {
      // Resolve the oldest open request.
      const open = db.prepare(`SELECT runtime_request_id, thread_id FROM ${PREFIX}runtime_requests WHERE resolved_at IS NULL ORDER BY created_at LIMIT 1`).get() as { runtime_request_id: string; thread_id: string } | undefined
      if (!open) throw new Error("no open request to resolve")
      kind = "resolve-request"
      w.event({ thread: open.thread_id, type: "runtime_request.resolved", at: at(0), provider }, () => {
        db.prepare(`UPDATE ${PREFIX}runtime_requests SET status='resolved', resolved_at=? WHERE runtime_request_id=?`).run(at(0), open.runtime_request_id)
        db.prepare(`UPDATE ${PREFIX}turn_items SET status='completed', updated_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE thread_id=? AND type IN ('user_input_request','secret_request','link_request') AND status='pending'`).run(at(0), open.thread_id)
        bumpThreadRow(open.thread_id, 0)
      })
      detail = `resolved ${open.runtime_request_id}`
      break
    }
    case 7: {
      // Archive one thread, or (every other visit) delete one: T3 "deletes" by
      // setting deleted_at, so the feed carries it as an ordinary thread event.
      const candidates = (db.prepare(`SELECT thread_id FROM ${PREFIX}threads WHERE archived_at IS NULL AND deleted_at IS NULL ORDER BY thread_id`).all() as { thread_id: string }[]).map((r) => r.thread_id)
      if (!candidates.length) throw new Error("no live thread to archive or delete")
      const picked = candidates[Math.floor(rng() * candidates.length)]
      const full = threadRow(picked)!
      const del = Math.floor(step / 10) % 2 === 1
      const body = { ...JSON.parse(full.payload_json as string), updatedAt: at(0) }
      kind = del ? "delete-thread" : "archive-thread"
      w.put(`${PREFIX}threads`, {
        ...full, updated_at: at(0),
        archived_at: del ? null : at(0), deleted_at: del ? at(0) : null, payload_json: body,
      }, { thread: picked, type: del ? "thread.deleted" : "thread.archived", at: at(0), provider })
      detail = `${del ? "deleted" : "archived"} ${picked}`
      break
    }
    case 8: {
      // Activity on a thread delegated in from another machine: append an
      // item and bump the thread recency. The
      // delegatedFrom on that thread sends the poll down the full structural
      // walk. Without delegated threads, fall back to append-items.
      const delegated = (db.prepare(`SELECT thread_id FROM ${PREFIX}threads WHERE archived_at IS NULL AND deleted_at IS NULL AND json_extract(payload_json,'$.delegatedFrom.environmentId') IS NOT NULL ORDER BY thread_id`).all() as { thread_id: string }[])
        .map((r) => r.thread_id)
      if (!delegated.length) {
        kind = "append-items"
        detail = appendItemsToRunning() + " (no delegated-in thread)"
        break
      }
      const tid = delegated[step % delegated.length]
      const run = (db.prepare(`SELECT run_id FROM ${PREFIX}runs WHERE thread_id=? ORDER BY ordinal DESC LIMIT 1`).get(tid) as { run_id: string } | undefined)?.run_id ?? null
      kind = "delegated-activity"
      const o = next()
      const type = itemType(rng)
      w.event({ thread: tid, type: "turn_item.upserted", at: at(0), run, provider }, () => {
        w.upsertRow(`${PREFIX}turn_items`, {
          turn_item_id: `i-m${step}-${o}`, item_id: `i-m${step}-${o}`, thread_id: tid, run_id: run, node_id: null,
          provider_thread_id: `pt-${tid}`, provider_turn_id: null, parent_item_id: null, type, status: "completed",
          updated_at: at(0), ordinal: o, payload_json: payload(lab, type, EPOCH + step * 60_000),
        })
        bumpThreadRow(tid, 0)
      })
      detail = `appended an item on delegated-in ${tid}`
      break
    }
    default: {
      // Progress on a remote child: the subagent row on the host thread gets a
      // new progress note and updated_at. Without remote children, fall back
      // to flip-item.
      const remotes = (db.prepare(`SELECT subagent_id, thread_id, payload_json FROM ${PREFIX}subagents WHERE json_extract(payload_json,'$.remoteChild.environmentId') IS NOT NULL ORDER BY subagent_id`).all() as { subagent_id: string; thread_id: string; payload_json: string }[])
      if (!remotes.length) {
        kind = "flip-item"
        detail = flipOneItem() + " (no remote child)"
        break
      }
      const sub = remotes[step % remotes.length]
      const body = { ...JSON.parse(sub.payload_json), progress: `synthetic progress at step ${step}: ${lab.filler().slice(0, 120)}`, updatedAt: at(0) }
      kind = "remote-progress"
      w.event({ thread: sub.thread_id, type: "subagent.updated", at: at(0), provider }, () => {
        db.prepare(`UPDATE ${PREFIX}subagents SET updated_at=?, payload_json=? WHERE subagent_id=?`).run(at(0), JSON.stringify(body), sub.subagent_id)
        bumpThreadRow(sub.thread_id, 0)
      })
      detail = `progress on remote child ${sub.subagent_id} (${sub.thread_id})`
      break
    }
  }
  w.close()
  return { step, kind, detail }
}

// ── Randomized, endless write stream ────────────────────────────────────────
//
// `--stream SEED --count N` applies N writes chosen at random from a seeded
// generator, on a t3-schema database. Unlike `--mutate` (ten fixed shapes that
// run dry once the running threads are used up) the stream looks at what the
// database holds before it chooses, starts runs and threads as others finish,
// and so keeps every shape possible for as long as it is asked to run.
//
// Every write is one transaction, like T3's writer: the event(s) appended to
// orchestration_events, the projection rows changed, the thread's own row
// bumped, thread-projections.last_sequence moved. Exceptions are the same
// ones T3 has: a project event touches no thread; a commit without an event
// (a table the dashboard does not read, or the log being pruned) adds none.
// Time passing is a step that writes nothing: the clock moves, and the poll
// that follows sees only that.
//
// What T3 does when it starts or updates is here too. A migration is one commit
// with its ledger row (effect_sql_migrations) and no event: data only, data and
// schema, a ledger row alone, or followed by an ordinary thread event before the
// next poll (two commits, writes: 2). A T3-shaped project event leaves the cursor
// behind (rename-project, older, moves it). restore-older-file models a failed
// update: the first draw copies the database's three files aside (writes 0, the
// launcher's snapshot), a later one stops the writer, copies them back over the live
// files in place (same inode, as `cp --reflink=auto` does on Linux) and starts it
// again. Half the snapshots leave -shm out and the restore deletes the live one, as
// T3 does where it cannot clone the file. The stream's own step, clock and ordinal
// keep going and are written back in a commit with no event. The snapshot sits
// beside the database (<db>.restore-snapshot/), so a replay from a copy of the file
// alone starts without one.
//
// Reproducibility: step K draws from a PRNG seeded by (SEED, K) alone and reads
// the rest from the database, so the same starting file plus the same seed
// gives the same stream, and a step can be replayed from a copy of the file as
// it stood before it. The position (stream_step), the virtual clock
// (stream_clock, milliseconds) and the turn-item ordinal live in scale_meta and
// move inside each write's own transaction. Do not mix with --mutate.
//
// The clock starts at EPOCH. A step's rows are stamped between its clock and
// clock + 2.5 s, and the record it prints names now_ms (clock + 3 s) as the time
// a reader should poll at, so no stamp is ever after the reader's clock and the
// clock never moves backwards. The harness passes now_ms to the reader.
//
// Output: one JSON line per step on stdout. `--serve` reads a step count from
// each line of stdin, prints that many step lines and then a {"batch": ...}
// line; the batch mode (`--count N`) applies N writes, counting the time-passes
// steps between them as free, and ends with a {"stream_done": ...} line.

const STREAM_SHAPES: [string, number][] = [
  ["append-items", 17], ["flip-item", 12], ["start-run", 5], ["complete-run", 5], ["provider-turn", 5],
  ["open-request", 3], ["resolve-request", 3], ["create-thread", 2], ["archive-thread", 1.2], ["unarchive-thread", 1],
  ["delete-thread", 0.8], ["rename-project", 0.7], ["edit-thread", 2], ["start-subagent-child", 3.5],
  ["progress-subagent", 4], ["complete-subagent", 3], ["start-remote-child", 1.5], ["remote-progress", 2],
  ["delegated-activity", 2.5], ["retry-subagent", 1], ["reparent-child", 1], ["unlink-child", 0.8],
  ["burst", 3], ["silent-commit", 5], ["prune-log", 0.5], ["time-passes", 4], ["append-tied", 1.5], ["sparse-write", 1.5],
  // What T3 does when it starts or updates, and its project events (see above). The restore's weight is
  // ten times this while a snapshot is held, so a trial lasts a few dozen steps.
  ["migrate-data", 0.4], ["migrate-schema", 0.15], ["migrate-then-event", 0.4], ["ledger-only", 0.2],
  ["project-event", 0.7], ["restore-older-file", 0.3],
  // Off unless asked for with --include: a command_execution item with no `input` (or a file_change with
  // no fileName) is read by the original but makes the Rust full reader fail ("invalid type: null").
  ["sparse-null-text", 0],
  // A variant, not a shape of its own: start-run on a thread that has no run yet but has items with a NULL
  // run_id. The record names it so; --exclude first-run-of-runless keeps start-run off such threads. The
  // incremental reader serves the poll after one of these wrong while the 3,000-item cap binds (soak finding 1).
  ["first-run-of-runless", 0],
]
const REQUEST_TYPES = ["user_input_request", "secret_request", "link_request"]
const OPEN_ITEM = "('pending','running','waiting')"
const LIVE_NODE = "('pending','starting','running','waiting')"
const OPEN_REQUEST_STATUS = ["pending", "pending", "pending", "open", "waiting", "requested"]
const T = (name: string) => `${PREFIX}${name}`

type StreamRow = Record<string, unknown>
interface StreamRecord {
  step: number
  shape: string
  requested?: string
  writes: number
  detail: string
  now_ms: number
  seq: [number, number] | null
  events: number
  threads: string[]
  tables: string[]
}

class Stream {
  db: DatabaseSync
  readonly path: string
  readonly seed: number
  private exclude: Set<string>
  private w: T3Sink
  private lab: Lab
  private stmts = new Map<string, StatementSync>()
  step = 0
  clock = EPOCH
  private ordinal = 0
  private live0 = 0
  // per step
  private rng: () => number = () => 0
  private n = 0
  private t0 = EPOCH
  private touched = new Set<string>()
  private tables = new Set<string>()
  private runs: { run_id: string; thread_id: string }[] = []
  private relabel: string | null = null
  private forceDelegated = false
  private commits = 1
  /** The launcher's copy of the database, held between restore-older-file's two draws. */
  private snapshot: { step: number; shm: boolean } | null = null

  private include: Set<string>

  constructor(path: string, seed: number, exclude: string[], include: string[] = []) {
    this.path = path
    this.db = new DatabaseSync(path)
    this.db.exec("PRAGMA busy_timeout=5000")
    this.seed = seed
    this.exclude = new Set(exclude)
    this.include = new Set(include)
    for (const name of [...exclude, ...include]) if (!STREAM_SHAPES.some(([s]) => s === name)) throw new Error(`unknown shape ${name} in --exclude/--include`)
    if (this.meta("schema") !== "t3") throw new Error("--stream needs a t3-schema lab database")
    const stored = this.meta("stream_seed")
    if (stored !== undefined && Number(stored) !== seed) throw new Error(`this database was streamed with seed ${stored}, not ${seed}`)
    this.lab = new Lab({ ...TIERS.s, seed: (seed ^ 0x51ed270b) >>> 0 })
    this.w = new T3Sink(this.db, this.lab)
    if (stored === undefined) {
      // Tables T3 has that the dashboard never opens. The silent commits write to them; they
      // are created here, before any reader starts, as a real installation already has them.
      this.db.exec("BEGIN")
      this.db.exec(`CREATE TABLE IF NOT EXISTS orchestration_command_receipts (command_id TEXT PRIMARY KEY, status TEXT NOT NULL, updated_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS provider_session_runtime (provider_session_id TEXT PRIMARY KEY, status TEXT NOT NULL, payload_json TEXT NOT NULL, updated_at TEXT NOT NULL);`)
      const maxOrdinal = (this.db.prepare(`SELECT COALESCE(MAX(ordinal),0) AS o FROM ${T("turn_items")}`).get() as { o: number }).o
      const live = (this.db.prepare(`SELECT COUNT(*) AS c FROM ${T("threads")} WHERE archived_at IS NULL AND deleted_at IS NULL`).get() as { c: number }).c
      this.setMeta("stream_seed", String(seed))
      this.setMeta("stream_step", "0")
      this.setMeta("stream_clock", String(EPOCH))
      this.setMeta("stream_ordinal", String(maxOrdinal))
      this.setMeta("stream_live0", String(live))
      this.db.exec("COMMIT")
    }
    this.step = Number(this.meta("stream_step"))
    this.clock = Number(this.meta("stream_clock"))
    this.ordinal = Number(this.meta("stream_ordinal"))
    this.live0 = Number(this.meta("stream_live0"))
    // A snapshot left by an earlier run on this file belongs to that run.
    rmSync(this.snapshotDir(), { recursive: true, force: true })
  }

  private meta(key: string): string | undefined {
    return (this.db.prepare("SELECT value FROM scale_meta WHERE key=?").get(key) as { value: string } | undefined)?.value
  }
  private setMeta(key: string, value: string) {
    this.db.prepare("INSERT OR REPLACE INTO scale_meta (key, value) VALUES (?, ?)").run(key, value)
  }
  close() {
    this.w.close()
    this.stmts.clear()
    this.db.close()
  }

  // ── helpers ──
  private all(sql: string, ...args: SQLInputValue[]): StreamRow[] {
    let s = this.stmts.get(sql)
    if (!s) this.stmts.set(sql, (s = this.db.prepare(sql)))
    return s.all(...args) as StreamRow[]
  }
  private get(sql: string, ...args: SQLInputValue[]): StreamRow | undefined {
    return this.all(sql, ...args)[0]
  }
  private chance(p: number) {
    return this.rng() < p
  }
  private pick<X>(list: X[]): X {
    return list[Math.floor(this.rng() * list.length)]
  }
  private between(lo: number, hi: number) {
    return lo + Math.floor(this.rng() * (hi - lo + 1))
  }
  private at(off = 0): string {
    return iso(this.t0 + off)!
  }
  private uid(prefix: string) {
    return `${prefix}-x${this.step}-${++this.n}`
  }
  private touch(table: string, thread?: string) {
    this.tables.add(table)
    if (thread) this.touched.add(thread)
  }
  private row(table: string, id: string): StreamRow | undefined {
    return this.get(`SELECT ${T3_COLS[table].join(",")} FROM ${table} WHERE ${T3_PK[table]}=?`, id)
  }
  /** Upsert one row with its changes applied; `body` is merged into the JSON payload. */
  private upd(table: string, id: string, patch: StreamRow, body?: StreamRow, thread?: string): boolean {
    const full = this.row(table, id)
    if (!full) return false
    const next: StreamRow = { ...full, ...patch }
    if (body) next.payload_json = { ...JSON.parse(full.payload_json as string), ...body }
    this.w.upsertRow(table, next)
    this.touch(table.replace(PREFIX, ""), thread)
    return true
  }
  private put(table: string, values: StreamRow, thread?: string) {
    this.w.upsertRow(table, values)
    this.touch(table.replace(PREFIX, ""), thread)
  }
  private bump(tid: string, off = 0) {
    this.upd(T("threads"), tid, { updated_at: this.at(off) }, { updatedAt: this.at(off) }, tid)
  }
  /** One thread event: `writes` run inside it, in the transaction the step already holds. */
  private ev(thread: string, type: string, off: number, run: string | null, writes: () => void) {
    this.touched.add(thread)
    this.w.event({ thread, type, at: this.at(off), run, provider: "claude" }, writes)
  }
  private rootNode(thread: string, run: string): string | null {
    const r = this.get(`SELECT node_id FROM ${T("nodes")} WHERE thread_id=? AND run_id=? AND parent_node_id IS NULL ORDER BY node_id LIMIT 1`, thread, run)
    return (r?.node_id as string | undefined) ?? null
  }
  private liveThreads(): string[] {
    return this.all(`SELECT thread_id FROM ${T("threads")} WHERE archived_at IS NULL AND deleted_at IS NULL ORDER BY thread_id`).map((r) => r.thread_id as string)
  }
  private runningRuns(): { run_id: string; thread_id: string }[] {
    return this.all(`SELECT r.run_id, r.thread_id FROM ${T("runs")} r JOIN ${T("threads")} t ON t.thread_id=r.thread_id
      WHERE r.completed_at IS NULL AND t.archived_at IS NULL AND t.deleted_at IS NULL ORDER BY r.run_id`) as { run_id: string; thread_id: string }[]
  }
  private model(): [string, number] {
    return this.pick(MODELS)
  }
  private count(sql: string): number {
    return (this.get(sql)!.c as number) ?? 0
  }

  private newItem(thread: string, run: string | null, node: string | null, pt: string | null, off: number, status?: string, type?: string, sameStamp?: string, ask = false) {
    let t = type ?? itemType(this.rng)
    // An ask item belongs with a request row; only openRequest writes one.
    if (!ask && REQUEST_TYPES.includes(t)) t = "notification"
    const s = status ?? (this.chance(0.3) ? "running" : this.chance(0.9) ? "completed" : this.chance(0.7) ? "failed" : this.pick(["interrupted", "cancelled"]))
    const o = ++this.ordinal
    const id = `i-x${this.step}-${o}`
    this.put(T("turn_items"), {
      turn_item_id: id, thread_id: thread, run_id: run, node_id: node, provider_thread_id: pt, provider_turn_id: null,
      parent_item_id: null, ordinal: o, type: t, status: s, updated_at: sameStamp ?? this.at(off), payload_json: payload(this.lab, t, this.t0 + off),
    }, thread)
    return { id, ordinal: o, type: t, status: s }
  }

  private weights(): Map<string, number> {
    this.runs = this.runningRuns()
    const live = this.liveThreads().length
    const archived = this.count(`SELECT COUNT(*) AS c FROM ${T("threads")} WHERE archived_at IS NOT NULL AND deleted_at IS NULL`)
    const requests = this.count(`SELECT COUNT(*) AS c FROM ${T("runtime_requests")} WHERE resolved_at IS NULL`)
    const subs = this.count(`SELECT COUNT(*) AS c FROM ${T("subagents")} WHERE status='running'`)
    const w = new Map(STREAM_SHAPES)
    const scale = (name: string, f: number) => w.set(name, (w.get(name) ?? 0) * f)
    const running = this.runs.length
    // Keep the pools every shape draws from non-empty, and bounded.
    if (running < 12) { scale("start-run", 8); scale("complete-run", 0.1); scale("create-thread", 3) }
    if (running < 4) { for (const name of ["flip-item", "append-items", "open-request", "start-subagent-child", "burst"]) scale(name, 0.3) }
    if (running > 45) { scale("complete-run", 4); scale("start-run", 0.1); scale("create-thread", 0.2) }
    const floor = Math.max(20, Math.floor(this.live0 * 0.4))
    if (live < floor) { scale("create-thread", 10); for (const name of ["archive-thread", "delete-thread"]) w.set(name, 0) }
    if (live > this.live0 + 120) { scale("archive-thread", 4); scale("delete-thread", 3); scale("create-thread", 0.1) }
    if (archived === 0) w.set("unarchive-thread", 0)
    if (archived > 80) scale("unarchive-thread", 4)
    if (requests === 0) w.set("resolve-request", 0)
    if (requests > 40) scale("resolve-request", 4)
    if (requests < 3) scale("open-request", 3)
    if (subs === 0) for (const name of ["progress-subagent", "complete-subagent"]) w.set(name, 0)
    if (subs < 6) scale("start-subagent-child", 3)
    if (subs > 70) { scale("complete-subagent", 4); scale("start-subagent-child", 0.2); scale("start-remote-child", 0.2) }
    if (this.snapshot) scale("restore-older-file", 10)
    for (const name of this.include) w.set(name, 1.5)
    for (const name of this.exclude) w.set(name, 0)
    return w
  }

  // ── the step ──
  next(): StreamRecord {
    const step = ++this.step
    this.rng = mulberry32((this.seed ^ Math.imul(step + 1, 0x9e3779b9)) >>> 0)
    ;(this.lab as { rng: () => number }).rng = mulberry32((this.seed ^ Math.imul(step + 1, 0x85ebca6b) ^ 0x27d4eb2f) >>> 0)
    this.n = 0
    this.touched = new Set()
    this.tables = new Set()
    const weights = this.weights()
    let total = 0
    for (const v of weights.values()) total += v
    let r = this.rng() * total
    let requested = "append-items"
    for (const [name, v] of weights) {
      if ((r -= v) < 0) { requested = name; break }
    }
    // The clock: a few seconds between writes, now and then a long gap.
    if (requested === "time-passes") {
      const gap = this.rng()
      const minutes = gap < 0.7 ? this.between(1, 10) : gap < 0.95 ? this.between(10, 45) : this.between(60, 200)
      this.clock += minutes * 60_000 + this.between(0, 59_000)
      return { step, shape: "time-passes", writes: 0, detail: `the clock moved ${minutes} min; nothing written`, now_ms: this.clock + 3000, seq: null, events: 0, threads: [], tables: [] }
    }
    this.clock += 3500 + Math.round(-Math.log(1 - this.rng()) * 8000)
    this.t0 = this.clock
    if (requested === "restore-older-file") return this.restoreStep(step)
    // A shape that finds nothing to work on hands the step to the shape that makes it, so the
    // pools refill instead of the stream drifting to the shapes that always have work.
    const feeds: Record<string, string[]> = {
      "delegated-activity": ["create-thread"], "reparent-child": ["start-subagent-child"], "unlink-child": ["start-subagent-child"],
      "retry-subagent": ["start-subagent-child", "start-remote-child"], "progress-subagent": ["start-subagent-child"], "complete-subagent": ["start-subagent-child"],
      "remote-progress": ["start-remote-child"], "open-request": ["start-run"], "resolve-request": ["open-request"], "flip-item": ["append-items"],
      "unarchive-thread": ["archive-thread"], "prune-log": ["append-items"],
    }
    this.forceDelegated = requested === "delegated-activity"
    const order = [requested, ...(feeds[requested] ?? []), ...["append-items", "start-run"], "create-thread"]
      .filter((name, i, all) => all.indexOf(name) === i && (i === 0 || !this.exclude.has(name) || name === "create-thread"))
    const before = (this.get("SELECT COALESCE(MAX(sequence),0) AS s FROM orchestration_events")!.s as number) ?? 0
    for (const shape of order) {
      // The sink counts transaction depth, so the step's transaction is the sink's: every event
      // and row the shape writes nests inside it and one COMMIT ends it. A shape that finds
      // nothing to do returns null before it writes, and its empty transaction changes nothing.
      this.w.hold()
      try {
        this.touched = new Set()
        this.tables = new Set()
        this.relabel = null
        this.commits = 1
        const detail = this.apply(shape)
        if (detail === null) {
          this.w.release()
          continue
        }
        this.setMeta("stream_step", String(step))
        this.setMeta("stream_clock", String(this.clock))
        this.setMeta("stream_ordinal", String(this.ordinal))
        this.w.release()
        const after = (this.get("SELECT COALESCE(MAX(sequence),0) AS s FROM orchestration_events")!.s as number) ?? 0
        const threads = [...this.touched]
        const label = this.relabel ?? shape
        return {
          step, shape: label, ...(shape !== requested ? { requested } : {}), writes: this.commits, detail, now_ms: this.clock + 3000,
          seq: after > before ? [before + 1, after] : null, events: after - before,
          threads: threads.length > 8 ? [...threads.slice(0, 8), `+${threads.length - 8} more`] : threads, tables: [...this.tables].sort(),
        }
      } catch (e) {
        try { this.db.exec("ROLLBACK") } catch { /* no transaction left */ }
        throw new Error(`step ${step} (${shape}) failed: ${(e as Error).message}`)
      }
    }
    throw new Error(`step ${step}: no shape was possible`)
  }

  private apply(shape: string): string | null {
    switch (shape) {
      case "append-items": return this.appendItems()
      case "flip-item": return this.flipItem()
      case "start-run": return this.startRun()
      case "complete-run": return this.completeRun()
      case "provider-turn": return this.providerTurn()
      case "open-request": return this.openRequest()
      case "resolve-request": return this.resolveRequest()
      case "create-thread": return this.createThread()
      case "archive-thread": return this.archiveThread("archive")
      case "unarchive-thread": return this.archiveThread("unarchive")
      case "delete-thread": return this.archiveThread("delete")
      case "rename-project": return this.renameProject()
      case "edit-thread": return this.editThread()
      case "start-subagent-child": return this.startSubagentChild()
      case "progress-subagent": return this.progressSubagent()
      case "complete-subagent": return this.completeSubagent()
      case "start-remote-child": return this.startRemoteChild()
      case "remote-progress": return this.remoteProgress()
      case "delegated-activity": return this.delegatedActivity()
      case "retry-subagent": return this.retrySubagent()
      case "reparent-child": return this.relinkChild("reparent")
      case "unlink-child": return this.relinkChild("unlink")
      case "burst": return this.burst()
      case "silent-commit": return this.silentCommit()
      case "prune-log": return this.pruneLog()
      case "append-tied": return this.appendTied()
      case "sparse-write": return this.sparseWrite(false)
      case "sparse-null-text": return this.sparseWrite(true)
      case "migrate-data": return this.migrateData()
      case "migrate-schema": return this.migrateSchema()
      case "migrate-then-event": return this.migrateThenEvent()
      case "ledger-only": return this.ledgerOnly()
      case "project-event": return this.projectEvent()
      default: throw new Error(`no shape ${shape}`)
    }
  }

  // ── items ──
  private appendItems(): string | null {
    if (!this.runs.length) return null
    const run = this.pick(this.runs)
    const thread = run.thread_id
    const info = this.get(`SELECT provider_thread_id AS pt FROM ${T("runs")} WHERE run_id=?`, run.run_id)!
    const hostPt = (info.pt as string | null) ?? null
    const root = this.rootNode(thread, run.run_id)
    const subs = this.all(`SELECT subagent_id, provider_thread_id FROM ${T("subagents")} WHERE thread_id=? AND status='running' ORDER BY subagent_id LIMIT 20`, thread)
    const mode = subs.length ? this.pick(["run", "run", "run", "run", "run", "run", "sub-node", "native", "bare"]) : this.pick(["run", "run", "run", "run", "run", "run", "run", "bare"])
    const n = this.between(1, 6)
    let node: string | null = root
    let pt = hostPt
    let how = ""
    if (mode === "sub-node") {
      const sub = this.pick(subs)
      node = (this.get(`SELECT node_id FROM ${T("nodes")} WHERE node_id IN (?, ?) ORDER BY node_id LIMIT 1`, sub.subagent_id as string, `n-${sub.subagent_id}`)?.node_id as string | undefined) ?? root
      how = `under subagent node ${node}`
    } else if (mode === "native") {
      const sub = this.pick(subs)
      pt = (sub.provider_thread_id as string | null) ?? hostPt
      node = null
      how = `by provider thread ${pt}`
    } else if (mode === "bare") {
      node = null
      pt = null
      how = "with no node or provider thread"
    }
    const kinds: string[] = []
    for (let j = 0; j < n; j++) {
      this.ev(thread, "turn-item.updated", j * 400, run.run_id, () => {
        const item = this.newItem(thread, run.run_id, node, pt, j * 400)
        kinds.push(`${item.type}/${item.status}`)
        this.bump(thread, j * 400)
      })
    }
    return `${n} items${how ? " " + how : ""} on run ${run.run_id} of ${thread}: ${kinds.join(", ")}`
  }

  private flipItem(): string | null {
    for (let tries = 0; tries < 6 && this.runs.length; tries++) {
      const run = this.pick(this.runs)
      const items = this.all(`SELECT turn_item_id, type, status FROM ${T("turn_items")} WHERE thread_id=? AND run_id=? AND status IN ${OPEN_ITEM}
        AND type NOT IN ('user_input_request','secret_request','link_request') ORDER BY ordinal, turn_item_id`, run.thread_id, run.run_id)
      if (!items.length) continue
      const item = this.pick(items)
      const roll = this.rng()
      const status = roll < 0.84 ? "completed" : roll < 0.94 ? "failed" : roll < 0.97 ? "interrupted" : "cancelled"
      this.ev(run.thread_id, status === "completed" ? "turn-item.completed" : "turn-item.failed", 0, run.run_id, () => {
        this.db.prepare(`UPDATE ${T("turn_items")} SET status=?, updated_at=?, payload_json=json_set(payload_json,'$.status',?) WHERE turn_item_id=?`).run(status, this.at(0), status, item.turn_item_id as string)
        this.touch("turn_items", run.thread_id)
        this.bump(run.thread_id)
      })
      if (status !== "completed") this.relabel = "fail-item"
      return `${item.turn_item_id} (${item.type}) ${item.status} -> ${status} on ${run.thread_id}`
    }
    return null
  }
  private appendTied(): string | null {
    if (!this.runs.length) return null
    const run = this.pick(this.runs)
    const thread = run.thread_id
    const pt = (this.get(`SELECT provider_thread_id AS pt FROM ${T("runs")} WHERE run_id=?`, run.run_id)!.pt as string | null) ?? null
    const root = this.rootNode(thread, run.run_id)
    const n = this.between(2, 4)
    const stamp = this.at(0)
    this.ev(thread, "turn-item.updated", 0, run.run_id, () => {
      for (let j = 0; j < n; j++) this.newItem(thread, run.run_id, root, pt, 0, this.pick(["completed", "completed", "running"]), undefined, stamp)
      // An older item of the thread lands on the same instant.
      const old = this.get(`SELECT turn_item_id FROM ${T("turn_items")} WHERE thread_id=? AND status IN ${OPEN_ITEM} AND type NOT IN ('user_input_request','secret_request','link_request') ORDER BY ordinal LIMIT 1`, thread)
      if (old && this.chance(0.6)) {
        this.db.prepare(`UPDATE ${T("turn_items")} SET updated_at=? WHERE turn_item_id=?`).run(stamp, old.turn_item_id as string)
      }
      this.bump(thread)
    })
    return `${n} items with one shared stamp ${stamp} on ${thread}`
  }

  // ── runs, turns, requests ──
  private startRun(threadHint?: string, off = 0): string | null {
    let thread = threadHint
    if (!thread) {
      const busy = new Set(this.runs.map((r) => r.thread_id))
      let free = this.liveThreads().filter((t) => !busy.has(t))
      if (this.exclude.has("first-run-of-runless")) free = free.filter((t) => !this.runlessWithItems(t))
      if (!free.length) return null
      thread = this.pick(free)
    }
    if (this.runlessWithItems(thread)) this.relabel = "first-run-of-runless"
    const row = this.row(T("threads"), thread)!
    const body = JSON.parse(row.payload_json as string)
    const [fallbackModel] = this.model()
    const model: string = body.modelSelection?.model ?? fallbackModel
    const provider = providerOf(model)
    const ordinal = (this.get(`SELECT COALESCE(MAX(ordinal),0)+1 AS o FROM ${T("runs")} WHERE thread_id=?`, thread)!.o as number)
    const rid = this.uid("r")
    const root = `root-${rid}`
    let pt = row.active_provider_thread_id as string | null
    const max = MODELS.find((m) => m[0] === model)?.[1] ?? 1_000_000
    this.ev(thread, "run.started", off, rid, () => {
      if (!pt) {
        pt = `pt-${thread}-x${this.step}`
        this.put(T("provider_threads"), {
          provider_thread_id: pt, thread_id: thread, owner_node_id: null, provider, provider_session_id: null, status: "active",
          first_run_ordinal: ordinal, last_run_ordinal: ordinal, updated_at: this.at(off), driver: null, provider_instance_id: null,
          payload_json: { id: pt, contextUsage: { usedTokens: Math.round(max * 0.1), maxTokens: max, updatedAt: this.at(off) } },
        }, thread)
        this.upd(T("threads"), thread, { active_provider_thread_id: pt }, { activeProviderThreadId: pt }, thread)
      } else {
        this.db.prepare(`UPDATE ${T("provider_threads")} SET status='active', last_run_ordinal=?, updated_at=? WHERE provider_thread_id=?`).run(ordinal, this.at(off), pt)
        this.touch("provider_threads", thread)
      }
      this.put(T("runs"), {
        run_id: rid, thread_id: thread, ordinal, provider, provider_thread_id: pt, status: "running", requested_at: this.at(off),
        completed_at: null, provider_instance_id: null,
        payload_json: { id: rid, threadId: thread, status: "running", providerThreadId: pt, requestedAt: this.at(off), startedAt: this.at(off + 200), completedAt: null, modelSelection: { model, options: [{ id: "effort", value: this.pick(["low", "medium", "high"]) }] } },
      }, thread)
      this.put(T("nodes"), {
        node_id: root, thread_id: thread, run_id: rid, parent_node_id: null, root_node_id: root, kind: "root_turn", status: "running",
        provider_thread_id: pt, provider_turn_id: null, runtime_request_id: null, checkpoint_scope_id: null, started_at: this.at(off), completed_at: null, payload_json: { node: root },
      }, thread)
      this.openTurn(thread, pt!, model, max, off)
      const items = this.between(0, 3)
      for (let j = 0; j < items; j++) this.newItem(thread, rid, root, pt, off + 300 + j * 200, j === items - 1 ? "running" : "completed", j === 0 ? "user_message" : undefined)
      if (this.chance(0.3)) this.newItem(thread, rid, root, pt, off + 900, "completed", "todo_list")
      this.bump(thread, off + 1000)
    })
    return `run ${rid} (#${ordinal}) started on ${thread} with ${model}`
  }

  /** A thread with no run of its own whose activity (items with a NULL run_id) is shown as its latest. */
  private runlessWithItems(thread: string): boolean {
    if (this.get(`SELECT 1 AS x FROM ${T("runs")} WHERE thread_id=? LIMIT 1`, thread)) return false
    return this.get(`SELECT 1 AS x FROM ${T("turn_items")} WHERE thread_id=? AND run_id IS NULL LIMIT 1`, thread) !== undefined
  }

  /** Close the provider thread's open turns and open the next one. */
  private openTurn(thread: string, pt: string, model: string, max: number, off: number, used?: number) {
    this.db.prepare(`UPDATE ${T("provider_turns")} SET status='completed', completed_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE provider_thread_id=? AND completed_at IS NULL`).run(this.at(off), pt)
    const ordinal = (this.get(`SELECT COALESCE(MAX(ordinal),0)+1 AS o FROM ${T("provider_turns")} WHERE provider_thread_id=?`, pt)!.o as number)
    const u = used ?? Math.round(max * (0.05 + this.rng() * 0.85))
    const output = this.between(800, 6000)
    const node = this.get(`SELECT node_id FROM ${T("nodes")} WHERE thread_id=? AND parent_node_id IS NULL ORDER BY node_id DESC LIMIT 1`, thread)?.node_id as string | undefined
    this.put(T("provider_turns"), {
      provider_turn_id: `turn-${pt}-${ordinal}-x${this.step}`, thread_id: thread, provider_thread_id: pt, node_id: node ?? `n-${thread}`, run_attempt_id: null,
      ordinal, status: "running", started_at: this.at(off), completed_at: null,
      payload_json: {
        providerThreadId: pt, model, status: "running",
        tokenUsage: { usedTokens: u, maxTokens: max, inputTokens: Math.max(0, u - output), cachedInputTokens: Math.round(u * 0.6), outputTokens: output, reasoningOutputTokens: Math.round(output / 2), updatedAt: this.at(off) },
        ...(this.chance(0.5) ? { turnTokenUsage: { inputTokens: u * 3, cachedInputTokens: u * 2, outputTokens: output * 2, reasoningOutputTokens: output, usageStatus: "in_progress", hasSubagents: false } } : {}),
      },
    }, thread)
  }

  /** End a run: its row, its open nodes, items, turns and requests, as one event's writes. */
  private finishRun(thread: string, run: string, status: string, off: number) {
    const full = this.row(T("runs"), run)
    if (!full) return
    const itemStatus = status === "completed" ? "completed" : status === "failed" ? "failed" : "interrupted"
    this.upd(T("runs"), run, { status, completed_at: this.at(off) }, { status, completedAt: this.at(off) }, thread)
    this.db.prepare(`UPDATE ${T("nodes")} SET status=?, completed_at=? WHERE thread_id=? AND run_id=? AND status IN ${LIVE_NODE}`).run(status, this.at(off), thread, run)
    this.db.prepare(`UPDATE ${T("turn_items")} SET status=?, updated_at=?, payload_json=json_set(payload_json,'$.status',?) WHERE thread_id=? AND run_id=? AND status IN ${OPEN_ITEM}`).run(itemStatus, this.at(off), itemStatus, thread, run)
    this.db.prepare(`UPDATE ${T("runtime_requests")} SET status='cancelled', resolved_at=? WHERE thread_id=? AND resolved_at IS NULL`).run(this.at(off), thread)
    const pt = full.provider_thread_id as string | null
    if (pt) {
      this.db.prepare(`UPDATE ${T("provider_turns")} SET status='completed', completed_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE provider_thread_id=? AND completed_at IS NULL`).run(this.at(off), pt)
      if (this.chance(0.3)) this.db.prepare(`UPDATE ${T("provider_threads")} SET status='closed', updated_at=? WHERE provider_thread_id=?`).run(this.at(off), pt)
    }
    for (const t of ["nodes", "turn_items", "runtime_requests", "provider_turns"]) this.touch(t, thread)
    this.bump(thread, off)
  }

  private completeRun(): string | null {
    if (!this.runs.length) return null
    const run = this.pick(this.runs)
    const roll = this.rng()
    const status = roll < 0.8 ? "completed" : roll < 0.9 ? "failed" : "interrupted"
    this.ev(run.thread_id, `run.${status}`, 0, run.run_id, () => this.finishRun(run.thread_id, run.run_id, status, 0))
    return `${status} ${run.run_id} on ${run.thread_id}`
  }

  private providerTurn(): string | null {
    const open = this.all(`SELECT DISTINCT provider_thread_id AS pt, thread_id FROM ${T("provider_turns")} WHERE completed_at IS NULL ORDER BY provider_thread_id`) as { pt: string; thread_id: string }[]
    const live = new Set(this.liveThreads())
    const pool = open.filter((p) => live.has(p.thread_id))
    if (!pool.length) return null
    const n = Math.min(pool.length, this.between(1, 3))
    const chosen = new Set<string>()
    const out: string[] = []
    for (let i = 0; i < n; i++) {
      const p = this.pick(pool)
      if (chosen.has(p.pt)) continue
      chosen.add(p.pt)
      const [model, max] = this.chance(0.2) ? this.model() : [null, 0]
      const last = this.get(`SELECT payload_json FROM ${T("provider_turns")} WHERE provider_thread_id=? AND completed_at IS NULL ORDER BY ordinal DESC LIMIT 1`, p.pt)
      const prev = last ? JSON.parse(last.payload_json as string) : {}
      const useModel: string = model ?? prev.model ?? "claude-sonnet-5-5"
      const useMax: number = model ? max : prev.tokenUsage?.maxTokens ?? 1_000_000
      const closeOnly = this.chance(0.15)
      this.ev(p.thread_id, "provider-turn.appended", i * 300, null, () => {
        if (closeOnly) {
          this.db.prepare(`UPDATE ${T("provider_turns")} SET status='completed', completed_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE provider_thread_id=? AND completed_at IS NULL`).run(this.at(i * 300), p.pt)
          this.touch("provider_turns", p.thread_id)
        } else {
          this.openTurn(p.thread_id, p.pt, useModel, useMax, i * 300, 500_000 + Math.round(this.rng() * 100_000))
          this.db.prepare(`UPDATE ${T("provider_threads")} SET updated_at=?, payload_json=json_set(payload_json,'$.contextUsage.usedTokens',?,'$.contextUsage.updatedAt',?) WHERE provider_thread_id=?`).run(this.at(i * 300), 400_000 + Math.round(this.rng() * 200_000), this.at(i * 300), p.pt)
          this.touch("provider_threads", p.thread_id)
        }
        this.bump(p.thread_id, i * 300)
      })
      out.push(`${p.pt}${closeOnly ? " closed" : model ? ` on ${model}` : ""}`)
    }
    return `provider turns: ${out.join(", ")}`
  }

  private openRequest(): string | null {
    if (!this.runs.length) return null
    const withOpen = new Map<string, number>()
    for (const r of this.all(`SELECT thread_id FROM ${T("runtime_requests")} WHERE resolved_at IS NULL`)) withOpen.set(r.thread_id as string, (withOpen.get(r.thread_id as string) ?? 0) + 1)
    const free = this.runs.filter((r) => !withOpen.has(r.thread_id))
    // A second request on a thread that already waits is rare.
    const pool = free.length && !this.chance(0.06) ? free : this.runs.filter((r) => withOpen.has(r.thread_id))
    if (!pool.length) return null
    const run = this.pick(pool)
    const pt = (this.get(`SELECT provider_thread_id AS pt FROM ${T("runs")} WHERE run_id=?`, run.run_id)!.pt as string | null) ?? null
    const root = this.rootNode(run.thread_id, run.run_id)
    const kind = this.pick(["user_input", "user_input", "permission", "secret", "link"])
    const itemType = kind === "secret" ? "secret_request" : kind === "link" ? "link_request" : "user_input_request"
    const rid = this.uid("q")
    const status = this.pick(OPEN_REQUEST_STATUS)
    this.ev(run.thread_id, "runtime-request.opened", 0, run.run_id, () => {
      this.put(T("runtime_requests"), {
        runtime_request_id: rid, thread_id: run.thread_id, node_id: root ?? `n-${run.thread_id}`, provider_turn_id: null, kind, status,
        created_at: this.at(0), resolved_at: null, payload_json: { kind, question: this.lab.filler().slice(0, 160) },
      }, run.thread_id)
      this.bump(run.thread_id)
    })
    this.ev(run.thread_id, "turn-item.updated", 300, run.run_id, () => {
      this.newItem(run.thread_id, run.run_id, root, pt, 300, itemType === "link_request" ? "waiting" : "pending", itemType, undefined, true)
    })
    return `${kind} request ${rid} (${status}) opened on ${run.thread_id}`
  }

  private resolveRequest(): string | null {
    const open = this.all(`SELECT runtime_request_id, thread_id, kind FROM ${T("runtime_requests")} WHERE resolved_at IS NULL ORDER BY created_at, runtime_request_id`)
    if (!open.length) return null
    const req = this.pick(open)
    const status = this.chance(0.85) ? "resolved" : "cancelled"
    const thread = req.thread_id as string
    this.ev(thread, "runtime-request.resolved", 0, null, () => {
      this.db.prepare(`UPDATE ${T("runtime_requests")} SET status=?, resolved_at=? WHERE runtime_request_id=?`).run(status, this.at(0), req.runtime_request_id as string)
      this.db.prepare(`UPDATE ${T("turn_items")} SET status='completed', updated_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE thread_id=? AND type IN ('user_input_request','secret_request','link_request') AND status IN ('pending','waiting')`).run(this.at(0), thread)
      this.touch("runtime_requests", thread)
      this.touch("turn_items", thread)
      this.bump(thread)
    })
    return `${req.runtime_request_id} on ${thread} ${status}`
  }

  // ── threads and projects ──
  private createThread(): string | null {
    const projects = this.all("SELECT project_id FROM projection_projects WHERE deleted_at IS NULL ORDER BY project_id").map((r) => r.project_id as string)
    if (!projects.length) return null
    const tid = `t-x${this.step}`
    const project = this.pick(projects)
    const [model, max] = this.model()
    const provider = providerOf(model)
    const pt = `pt-${tid}`
    const delegated = this.forceDelegated || this.chance(0.1)
    let remoteRef: string | null = null
    if (delegated) {
      // Usually the same remote thread a remote child already names; sometimes one nothing here names.
      const taken = new Set(this.all(`SELECT json_extract(payload_json,'$.delegatedFrom.threadId') AS r FROM ${T("threads")} WHERE json_extract(payload_json,'$.delegatedFrom.threadId') IS NOT NULL`).map((r) => r.r as string))
      const refs = (this.all(`SELECT json_extract(payload_json,'$.remoteChild.threadId') AS r FROM ${T("subagents")} WHERE json_extract(payload_json,'$.remoteChild.environmentId')=? ORDER BY subagent_id`, REMOTE_ENV)
        .map((r) => r.r as string).filter((r) => r && !taken.has(r)))
      remoteRef = refs.length && this.chance(0.7) ? this.pick(refs) : `rt-x${this.step}`
    }
    const withRun = this.chance(0.3)
    const title = `${this.pick(WORDS)} ${this.pick(WORDS)} ${this.step}`
    this.ev(tid, "thread.created", 0, null, () => {
      this.put(T("threads"), {
        thread_id: tid, project_id: project, title, default_provider: provider, runtime_mode: "auto", interaction_mode: "default",
        active_provider_thread_id: pt, created_at: this.at(0), updated_at: this.at(0), archived_at: null, deleted_at: null, provider_instance_id: null,
        payload_json: {
          id: tid, projectId: project, title, branch: this.chance(0.5) ? `scale/${this.pick(WORDS)}-${this.step}` : null, activeProviderThreadId: pt,
          worktreePath: this.chance(0.7) ? `/home/user/projects/${project}` : null, modelSelection: { model, options: [] }, updatedAt: this.at(0), settledOverride: null, snoozedUntil: null,
          ...(remoteRef ? { delegatedFrom: { environmentId: REMOTE_ENV, threadId: remoteRef, title: `remote ${this.pick(WORDS)} ${this.step}` }, linkOrigin: "delegation" } : {}),
        },
      }, tid)
    })
    this.ev(tid, "provider-thread.attached", 300, null, () => {
      this.put(T("provider_threads"), {
        provider_thread_id: pt, thread_id: tid, owner_node_id: null, provider, provider_session_id: null, status: "active", first_run_ordinal: null, last_run_ordinal: null,
        updated_at: this.at(300), driver: null, provider_instance_id: null,
        payload_json: { id: pt, contextUsage: { usedTokens: 0, maxTokens: max, updatedAt: this.at(300) } },
      }, tid)
    })
    if (withRun) this.startRun(tid, 600)
    return `thread ${tid} in ${project}${delegated ? ` delegated from ${remoteRef}` : ""}${withRun ? " with a run" : ""}`
  }

  private archiveThread(mode: "archive" | "unarchive" | "delete"): string | null {
    let pool: string[]
    if (mode === "unarchive") pool = this.all(`SELECT thread_id FROM ${T("threads")} WHERE archived_at IS NOT NULL AND deleted_at IS NULL ORDER BY thread_id`).map((r) => r.thread_id as string)
    else pool = this.liveThreads()
    if (!pool.length) return null
    const tid = this.pick(pool)
    const full = this.row(T("threads"), tid)!
    const body = { ...JSON.parse(full.payload_json as string), updatedAt: this.at(0) }
    const patch = mode === "archive" ? { archived_at: this.at(0), deleted_at: null } : mode === "unarchive" ? { archived_at: null, deleted_at: null } : { archived_at: null, deleted_at: this.at(0) }
    this.touched.add(tid)
    this.w.put(T("threads"), { ...full, ...patch, updated_at: this.at(0), payload_json: body }, { thread: tid, type: `thread.${mode === "unarchive" ? "unarchived" : mode === "archive" ? "archived" : "deleted"}`, at: this.at(0), provider: "claude" })
    this.touch("threads")
    return `${mode === "unarchive" ? "un-archived" : mode === "archive" ? "archived" : "deleted"} ${tid}`
  }

  private renameProject(): string | null {
    const projects = this.all("SELECT project_id FROM projection_projects WHERE deleted_at IS NULL ORDER BY project_id").map((r) => r.project_id as string)
    if (!projects.length) return null
    const id = this.pick(projects)
    const full = this.row("projection_projects", id)!
    const patch: StreamRow = { updated_at: this.at(0) }
    if (this.chance(0.7)) patch.title = `${this.pick(WORDS)}-${this.pick(WORDS)}-${this.step}`
    else patch.workspace_root = `/home/user/projects/${this.pick(WORDS)}-${this.step}`
    this.w.projectEvent(id, this.at(0))
    this.w.upsertRow("projection_projects", { ...full, ...patch })
    this.touch("projection_projects")
    return `project ${id} ${patch.title ? `renamed to ${patch.title}` : `moved to ${patch.workspace_root}`} (a project event)`
  }

  private editThread(): string | null {
    const live = this.liveThreads()
    if (!live.length) return null
    const tid = this.pick(live)
    const full = this.row(T("threads"), tid)!
    const body: StreamRow = { updatedAt: this.at(0) }
    const patch: StreamRow = { updated_at: this.at(0) }
    const what = this.pick(["title", "branch", "settled", "snooze", "model"])
    if (what === "title") { patch.title = `${this.pick(WORDS)} ${this.pick(WORDS)} e${this.step}`; body.title = patch.title }
    else if (what === "branch") body.branch = this.chance(0.7) ? `scale/e${this.step}` : null
    else if (what === "settled") body.settledOverride = this.pick(["settled", null])
    else if (what === "snooze") body.snoozedUntil = this.chance(0.5) ? this.at(this.between(1, 400) * 60_000) : null
    else { const [m] = this.model(); body.modelSelection = { model: m, options: [] } }
    this.ev(tid, "thread.updated", 0, null, () => {
      this.w.upsertRow(T("threads"), { ...full, ...patch, payload_json: { ...JSON.parse(full.payload_json as string), ...body } })
      this.touch("threads", tid)
    })
    return `edited ${what} of ${tid}`
  }

  // ── subagents ──
  private hostRun(): { run_id: string; thread_id: string; root: string } | null {
    const hosts = this.runs.filter((r) => this.rootNode(r.thread_id, r.run_id))
    if (!hosts.length) return null
    const run = this.pick(hosts)
    return { ...run, root: this.rootNode(run.thread_id, run.run_id)! }
  }

  private subagentNode(host: string, run: string, sid: string, root: string, pt: string | null, off: number) {
    this.put(T("nodes"), {
      node_id: sid, thread_id: host, run_id: run, parent_node_id: root, root_node_id: root, kind: "subagent", status: "running",
      provider_thread_id: pt, provider_turn_id: null, runtime_request_id: null, checkpoint_scope_id: null, started_at: this.at(off), completed_at: null, payload_json: { node: sid },
    }, host)
  }

  private subagentRow(sid: string, host: string, run: string, root: string, provider: string, spt: string | null, child: string | null, origin: string, model: string, off: number, extra: StreamRow = {}, updated?: string) {
    this.put(T("subagents"), {
      subagent_id: sid, thread_id: host, run_id: run, parent_node_id: root, provider, provider_thread_id: spt, child_thread_id: child, origin, status: "running",
      started_at: this.at(off), completed_at: null, updated_at: updated ?? this.at(off), driver: null, provider_instance_id: null, completion_delivery_state: null,
      payload_json: {
        id: sid, threadId: host, parentNodeId: root, runId: run, providerThreadId: spt, childThreadId: child, origin, status: "running",
        title: `${this.pick(WORDS)} ${this.pick(WORDS)} ${sid}`, model, startedAt: this.at(off), completedAt: null, updatedAt: updated ?? this.at(off),
        createdBy: origin === "app_owned" ? "app" : "user", progress: null, ...extra,
      },
    }, host)
  }

  private startSubagentChild(): string | null {
    const host = this.hostRun()
    if (!host) return null
    const sid = `s-x${this.step}`
    const child = `t-x${this.step}c`
    const spt = this.chance(0.1) ? null : `pt-${sid}`
    const [model, max] = this.model()
    const provider = providerOf(model)
    const hostRow = this.row(T("threads"), host.thread_id)!
    const hostPt = (this.get(`SELECT provider_thread_id AS pt FROM ${T("runs")} WHERE run_id=?`, host.run_id)!.pt as string | null) ?? null
    const origin = this.pick(["app_owned", "app_owned", "provider_native"])
    const childRun = this.chance(0.6)
    const crid = `r-x${this.step}c`
    const croot = `root-${crid}`
    this.w.put(T("threads"), {
      thread_id: child, project_id: hostRow.project_id, title: `child ${this.step}`, default_provider: provider, runtime_mode: "auto", interaction_mode: "default",
      active_provider_thread_id: spt, created_at: this.at(0), updated_at: this.at(0), archived_at: null, deleted_at: null, provider_instance_id: null,
      payload_json: {
        id: child, projectId: hostRow.project_id, title: `child ${this.step}`, branch: null, activeProviderThreadId: spt, worktreePath: null, modelSelection: { model, options: [] },
        updatedAt: this.at(0), settledOverride: null, snoozedUntil: null, lineage: { relationshipToParent: "subagent", parentThreadId: host.thread_id }, forkedFrom: host.thread_id,
      },
    }, { thread: child, type: "thread.created", at: this.at(0), provider })
    this.touch("threads", child)
    this.ev(host.thread_id, "subagent.started", 300, host.run_id, () => {
      this.subagentNode(host.thread_id, host.run_id, sid, host.root, hostPt, 300)
      this.subagentRow(sid, host.thread_id, host.run_id, host.root, provider, spt, child, origin, model, 300)
      this.bump(host.thread_id, 300)
    })
    if (spt) {
      this.ev(child, "provider-turn.appended", 600, null, () => {
        this.put(T("provider_threads"), {
          provider_thread_id: spt, thread_id: child, owner_node_id: sid, provider, provider_session_id: null, status: "active", first_run_ordinal: null, last_run_ordinal: null,
          updated_at: this.at(600), driver: null, provider_instance_id: null, payload_json: { id: spt, contextUsage: { usedTokens: 4000, maxTokens: max, updatedAt: this.at(600) } },
        }, child)
        this.openTurn(child, spt, model, max, 600, 4000)
      })
    }
    if (childRun) {
      this.ev(child, "run.started", 900, crid, () => {
        const ordinal = 1
        this.put(T("runs"), {
          run_id: crid, thread_id: child, ordinal, provider, provider_thread_id: spt, status: "running", requested_at: this.at(900), completed_at: null, provider_instance_id: null,
          payload_json: { id: crid, threadId: child, status: "running", providerThreadId: spt, requestedAt: this.at(900), startedAt: this.at(1000), completedAt: null, modelSelection: { model, options: [] } },
        }, child)
        this.put(T("nodes"), {
          node_id: croot, thread_id: child, run_id: crid, parent_node_id: null, root_node_id: croot, kind: "root_turn", status: "running",
          provider_thread_id: spt, provider_turn_id: null, runtime_request_id: null, checkpoint_scope_id: null, started_at: this.at(900), completed_at: null, payload_json: { node: croot },
        }, child)
        this.newItem(child, crid, croot, spt, 1000, "running", this.pick(["dynamic_tool", "command_execution", "file_change"]))
        this.bump(child, 1000)
      })
    } else {
      this.ev(child, "turn-item.updated", 900, null, () => {
        this.newItem(child, null, null, spt, 900, "running", "dynamic_tool")
        this.bump(child, 900)
      })
    }
    return `subagent ${sid} (${origin}) under ${host.thread_id} with child thread ${child}${childRun ? " and a run" : ", runless"}${spt ? "" : ", no provider thread"}`
  }

  private runningSubs(): StreamRow[] {
    const live = new Set(this.liveThreads())
    return this.all(`SELECT subagent_id, thread_id, run_id, child_thread_id, provider_thread_id, payload_json FROM ${T("subagents")} WHERE status='running' ORDER BY subagent_id`)
      .filter((s) => live.has(s.thread_id as string))
  }

  private progressSubagent(): string | null {
    const subs = this.runningSubs()
    if (!subs.length) return null
    const sub = this.pick(subs)
    const host = sub.thread_id as string
    const child = (sub.child_thread_id as string | null) ?? null
    const spt = (sub.provider_thread_id as string | null) ?? null
    this.ev(host, "subagent.updated", 0, sub.run_id as string | null, () => {
      this.upd(T("subagents"), sub.subagent_id as string, { updated_at: this.at(0) }, { updatedAt: this.at(0), progress: `step ${this.step}: ${this.lab.filler().slice(0, 80)}` }, host)
      this.bump(host)
    })
    if (child && this.row(T("threads"), child)) {
      const crun = this.get(`SELECT run_id FROM ${T("runs")} WHERE thread_id=? AND completed_at IS NULL ORDER BY ordinal DESC LIMIT 1`, child)?.run_id as string | undefined
      const croot = crun ? this.rootNode(child, crun) : null
      this.ev(child, "turn-item.updated", 300, crun ?? null, () => {
        const n = this.between(1, 3)
        for (let j = 0; j < n; j++) this.newItem(child, crun ?? null, croot, spt, 300 + j * 100)
        if (spt) {
          this.db.prepare(`UPDATE ${T("provider_turns")} SET payload_json=json_set(payload_json,'$.tokenUsage.usedTokens',?,'$.tokenUsage.updatedAt',?) WHERE provider_thread_id=? AND completed_at IS NULL`).run(10_000 + Math.round(this.rng() * 900_000), this.at(300), spt)
          this.touch("provider_turns", child)
        }
        this.bump(child, 300)
      })
    }
    return `progress on ${sub.subagent_id} of ${host}${child ? ` and its child ${child}` : ""}`
  }

  private completeSubagent(): string | null {
    const subs = this.runningSubs()
    if (!subs.length) return null
    const sub = this.pick(subs)
    const host = sub.thread_id as string
    const roll = this.rng()
    const status = roll < 0.8 ? "completed" : roll < 0.92 ? "failed" : "interrupted"
    const sid = sub.subagent_id as string
    const child = (sub.child_thread_id as string | null) ?? null
    const spt = (sub.provider_thread_id as string | null) ?? null
    this.ev(host, `subagent.${status}`, 0, sub.run_id as string | null, () => {
      this.upd(T("subagents"), sid, { status, completed_at: this.at(0), updated_at: this.at(0) }, { status, completedAt: this.at(0), updatedAt: this.at(0), progress: null }, host)
      this.db.prepare(`UPDATE ${T("nodes")} SET status=?, completed_at=? WHERE node_id IN (?, ?) AND status IN ${LIVE_NODE}`).run(status, this.at(0), sid, `n-${sid}`)
      this.touch("nodes", host)
      this.bump(host)
    })
    if (child && this.row(T("threads"), child)) {
      this.ev(child, "run.completed", 300, null, () => {
        for (const r of this.all(`SELECT run_id FROM ${T("runs")} WHERE thread_id=? AND completed_at IS NULL ORDER BY run_id`, child)) this.finishRun(child, r.run_id as string, status === "completed" ? "completed" : status, 300)
        // Runless child: its open items and turns end with it.
        this.db.prepare(`UPDATE ${T("turn_items")} SET status=?, updated_at=?, payload_json=json_set(payload_json,'$.status',?) WHERE thread_id=? AND status IN ${OPEN_ITEM}`).run(status === "completed" ? "completed" : "interrupted", this.at(300), status === "completed" ? "completed" : "interrupted", child)
        if (spt) {
          this.db.prepare(`UPDATE ${T("provider_turns")} SET status='completed', completed_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE provider_thread_id=? AND completed_at IS NULL`).run(this.at(300), spt)
          this.db.prepare(`UPDATE ${T("provider_threads")} SET status='closed', updated_at=? WHERE provider_thread_id=?`).run(this.at(300), spt)
        }
        this.touch("turn_items", child)
        this.bump(child, 300)
      })
    }
    return `subagent ${sid} of ${host} ${status}${child ? ` with its child ${child}` : ""}`
  }

  private startRemoteChild(): string | null {
    const host = this.hostRun()
    if (!host) return null
    const sid = `s-x${this.step}`
    const hostPt = (this.get(`SELECT provider_thread_id AS pt FROM ${T("runs")} WHERE run_id=?`, host.run_id)!.pt as string | null) ?? null
    const [model] = this.model()
    const ref = `rt-x${this.step}`
    this.ev(host.thread_id, "subagent.started", 0, host.run_id, () => {
      this.subagentNode(host.thread_id, host.run_id, sid, host.root, hostPt, 0)
      this.subagentRow(sid, host.thread_id, host.run_id, host.root, providerOf(model), null, null, "app_owned", model, 0, {
        remoteChild: { environmentId: REMOTE_ENV, threadId: ref, label: `${this.pick(WORDS)} sandbox` }, progress: "synthetic: started on the other machine",
      })
      this.bump(host.thread_id)
    })
    return `remote child ${sid} (${ref}) under ${host.thread_id}`
  }

  private remoteProgress(): string | null {
    const remotes = this.all(`SELECT subagent_id, thread_id FROM ${T("subagents")} WHERE json_extract(payload_json,'$.remoteChild.environmentId') IS NOT NULL ORDER BY subagent_id`)
    if (!remotes.length) return null
    const sub = this.pick(remotes)
    const host = sub.thread_id as string
    const done = this.chance(0.15)
    this.ev(host, "subagent.updated", 0, null, () => {
      this.upd(T("subagents"), sub.subagent_id as string, done ? { updated_at: this.at(0), status: "completed", completed_at: this.at(0) } : { updated_at: this.at(0) },
        { updatedAt: this.at(0), progress: `synthetic progress at step ${this.step}: ${this.lab.filler().slice(0, 100)}`, ...(done ? { status: "completed", completedAt: this.at(0) } : {}) }, host)
      this.bump(host)
    })
    return `progress${done ? " (done)" : ""} on remote child ${sub.subagent_id} of ${host}`
  }

  private delegatedActivity(): string | null {
    const pool = this.all(`SELECT thread_id FROM ${T("threads")} WHERE archived_at IS NULL AND deleted_at IS NULL AND json_extract(payload_json,'$.delegatedFrom.environmentId') IS NOT NULL ORDER BY thread_id`).map((r) => r.thread_id as string)
    if (!pool.length) return null
    const tid = this.pick(pool)
    const run = this.get(`SELECT run_id, provider_thread_id AS pt FROM ${T("runs")} WHERE thread_id=? ORDER BY ordinal DESC LIMIT 1`, tid)
    const rid = (run?.run_id as string | undefined) ?? null
    const pt = (run?.pt as string | undefined) ?? null
    const root = rid ? this.rootNode(tid, rid) : null
    const n = this.between(1, 3)
    this.ev(tid, "turn-item.updated", 0, rid, () => {
      for (let j = 0; j < n; j++) this.newItem(tid, rid, root, pt, j * 200)
      this.bump(tid, n * 200)
    })
    return `${n} items on delegated-in ${tid}`
  }

  private retrySubagent(): string | null {
    const pool = this.all(`SELECT subagent_id, thread_id, run_id, provider_thread_id, child_thread_id, origin, updated_at, payload_json FROM ${T("subagents")}
      WHERE child_thread_id IS NOT NULL OR json_extract(payload_json,'$.remoteChild.environmentId') IS NOT NULL ORDER BY subagent_id`)
    const live = new Set(this.liveThreads())
    const cands = pool.filter((s) => live.has(s.thread_id as string))
    if (!cands.length) return null
    const old = this.pick(cands)
    const host = old.thread_id as string
    const latest = this.get(`SELECT run_id, provider_thread_id AS pt FROM ${T("runs")} WHERE thread_id=? ORDER BY ordinal DESC LIMIT 1`, host)
    if (!latest) return null
    const run = latest.run_id as string
    const root = this.rootNode(host, run)
    if (!root) return null
    const body = JSON.parse(old.payload_json as string)
    const sid = `s-x${this.step}r`
    const tie = this.chance(0.4)
    const stamp = tie ? (old.updated_at as string) : undefined
    const child = (old.child_thread_id as string | null) ?? null
    let spt = (old.provider_thread_id as string | null) ?? null
    const [model, max] = this.model()
    this.ev(host, "subagent.started", 0, run, () => {
      this.subagentNode(host, run, sid, root, (latest.pt as string | null) ?? null, 0)
      if (child && this.chance(0.5)) {
        // A fresh provider conversation on the same child thread.
        spt = `pt-${sid}`
        this.put(T("provider_threads"), {
          provider_thread_id: spt, thread_id: child, owner_node_id: sid, provider: providerOf(model), provider_session_id: null, status: "active", first_run_ordinal: null, last_run_ordinal: null,
          updated_at: this.at(0), driver: null, provider_instance_id: null, payload_json: { id: spt, contextUsage: { usedTokens: 1000, maxTokens: max, updatedAt: this.at(0) } },
        }, child)
      }
      this.subagentRow(sid, host, run, root, providerOf(model), spt, child, (old.origin as string) ?? "app_owned", model, 0,
        body.remoteChild ? { remoteChild: body.remoteChild, progress: "retry" } : { progress: "retry" }, stamp)
      this.bump(host)
    })
    return `retry ${sid} of ${old.subagent_id} (${child ? `child ${child}` : "remote"})${tie ? ", updated_at tied with the original" : ""}`
  }

  private relinkChild(mode: "reparent" | "unlink"): string | null {
    const kids = this.all(`SELECT thread_id, json_extract(payload_json,'$.lineage.parentThreadId') AS parent FROM ${T("threads")}
      WHERE archived_at IS NULL AND deleted_at IS NULL AND json_extract(payload_json,'$.lineage.relationshipToParent')='subagent' ORDER BY thread_id`)
    if (!kids.length) return null
    const kid = this.pick(kids)
    const tid = kid.thread_id as string
    const full = this.row(T("threads"), tid)!
    const body = JSON.parse(full.payload_json as string)
    if (mode === "unlink") {
      const subs = this.all(`SELECT subagent_id, thread_id FROM ${T("subagents")} WHERE child_thread_id=? ORDER BY subagent_id`, tid)
      this.ev(tid, "thread.updated", 0, null, () => {
        const next = { ...body, updatedAt: this.at(0) }
        delete next.lineage
        delete next.forkedFrom
        this.w.upsertRow(T("threads"), { ...full, updated_at: this.at(0), payload_json: next })
        this.touch("threads", tid)
      })
      if (subs.length && this.chance(0.6)) {
        for (const s of subs) this.ev(s.thread_id as string, "subagent.updated", 300, null, () => {
          this.upd(T("subagents"), s.subagent_id as string, { child_thread_id: null, updated_at: this.at(300) }, { childThreadId: null, updatedAt: this.at(300) }, s.thread_id as string)
          this.bump(s.thread_id as string, 300)
        })
      }
      return `unlinked child ${tid} from ${kid.parent}${subs.length ? ` (${subs.length} subagent row${subs.length > 1 ? "s" : ""} name it)` : ""}`
    }
    // Re-parent: any other live thread that does not descend from this one.
    const live = this.liveThreads().filter((t) => t !== tid && t !== kid.parent)
    const parentOf = new Map<string, string>()
    for (const k of kids) parentOf.set(k.thread_id as string, k.parent as string)
    const ok = (candidate: string) => {
      let at: string | undefined = candidate
      for (let hop = 0; at && hop < 40; hop++) {
        if (at === tid) return false
        at = parentOf.get(at)
      }
      return true
    }
    const choices = live.filter(ok)
    if (!choices.length) return null
    const parent = this.pick(choices)
    this.ev(tid, "thread.updated", 0, null, () => {
      this.w.upsertRow(T("threads"), { ...full, updated_at: this.at(0), payload_json: { ...body, updatedAt: this.at(0), lineage: { ...body.lineage, parentThreadId: parent }, forkedFrom: parent } })
      this.touch("threads", tid)
    })
    return `re-parented child ${tid} from ${kid.parent} to ${parent}`
  }

  // ── many threads, nothing, and the log itself ──
  private burst(): string | null {
    const live = this.liveThreads()
    if (live.length < 2) return null
    const n = Math.min(live.length, this.chance(0.3) ? this.between(20, 60) : this.between(2, 20))
    const tie = this.chance(0.5)
    const chosen = new Set<string>()
    let i = this.between(0, live.length - 1)
    // A stride that shares a factor with the count would walk a subset forever.
    const gcd = (a: number, b: number): number => (b === 0 ? a : gcd(b, a % b))
    let stride = this.pick([1, 7, 13, 101])
    while (gcd(stride, live.length) !== 1) stride++
    while (chosen.size < n) {
      chosen.add(live[i % live.length])
      i += stride
    }
    let flips = 0
    let j = 0
    for (const tid of chosen) {
      const off = tie ? 0 : j * 50
      const run = this.get(`SELECT run_id, provider_thread_id AS pt, completed_at FROM ${T("runs")} WHERE thread_id=? ORDER BY ordinal DESC LIMIT 1`, tid)
      const rid = (run?.run_id as string | undefined) ?? null
      const pt = (run?.pt as string | undefined) ?? null
      const root = rid ? this.rootNode(tid, rid) : null
      this.ev(tid, "turn-item.updated", off, rid, () => {
        this.newItem(tid, rid, root, pt, off, this.chance(0.2) ? "running" : "completed")
        if (this.chance(0.25)) {
          const open = this.get(`SELECT turn_item_id FROM ${T("turn_items")} WHERE thread_id=? AND status='running' ORDER BY ordinal LIMIT 1`, tid)
          if (open) {
            this.db.prepare(`UPDATE ${T("turn_items")} SET status='completed', updated_at=?, payload_json=json_set(payload_json,'$.status','completed') WHERE turn_item_id=?`).run(this.at(off), open.turn_item_id as string)
            flips++
          }
        }
        this.bump(tid, off)
      })
      j++
    }
    return `touched ${n} threads in one transaction${tie ? ", every stamp the same" : ""}${flips ? `, flipped ${flips} items` : ""}`
  }

  private silentCommit(): string | null {
    const n = this.between(1, 4)
    for (let i = 0; i < n; i++) {
      const id = `cmd-x${this.step}-${i}`
      this.db.prepare("INSERT OR REPLACE INTO orchestration_command_receipts (command_id, status, updated_at) VALUES (?, ?, ?)").run(id, this.pick(["accepted", "completed", "rejected"]), this.at(i * 100))
    }
    const session = this.chance(0.5)
    if (session) {
      this.db.prepare("INSERT OR REPLACE INTO provider_session_runtime (provider_session_id, status, payload_json, updated_at) VALUES (?, ?, ?, ?)")
        .run(`ps-${this.step % 17}`, this.pick(["ready", "busy", "stopped"]), JSON.stringify({ note: this.lab.filler().slice(0, 120) }), this.at(0))
    }
    this.tables.add("orchestration_command_receipts")
    if (session) this.tables.add("provider_session_runtime")
    return `${n} command receipts${session ? " and a provider session" : ""} written: no event, no table the dashboard reads`
  }

  private pruneLog(): string | null {
    const mark = this.get("SELECT last_sequence AS s FROM orchestration_v2_projection_metadata WHERE projection_name='event-log-prune-mark'")?.s as number | undefined
    const cursor = this.get("SELECT last_sequence AS s FROM orchestration_v2_projection_metadata WHERE projection_name='thread-projections'")?.s as number
    if (mark === undefined) return null
    const room = cursor - mark - 3000
    if (room < 200) return null
    const floor = mark + this.between(100, Math.min(room, 4000))
    const deleted = this.db.prepare("DELETE FROM orchestration_events WHERE sequence <= ?").run(floor).changes
    for (const name of ["event-log-prune-mark", "event-log-pruned-through"]) {
      this.db.prepare("UPDATE orchestration_v2_projection_metadata SET last_sequence=?, updated_at=? WHERE projection_name=?").run(floor, this.at(0), name)
    }
    this.tables.add("orchestration_events")
    return `pruned the log through ${floor} (${deleted} events deleted), both prune rows moved`
  }

  // ── what T3 does when it starts or updates ──
  /** The migration's ledger row, as T3's migrator writes it: in the migration's own transaction. */
  private ledgerRow(name: string) {
    this.db.prepare("INSERT INTO effect_sql_migrations (migration_id, created_at, name) VALUES ((SELECT COALESCE(MAX(migration_id),0)+1 FROM effect_sql_migrations), ?, ?)").run(this.at(0), name)
    this.tables.add("effect_sql_migrations")
  }
  /**
   * Rewrite some threads' titles in their payload (like migrations 064, 077 and 096 rewrite
   * payload_json), now and then a project's title too (044, 087), with no event.
   */
  private rewriteRows(): string | null {
    const live = this.liveThreads()
    if (!live.length) return null
    const n = Math.min(live.length, this.between(1, 12))
    const chosen = new Set<string>()
    while (chosen.size < n) chosen.add(this.pick(live))
    const title = `repaired ${this.step}`
    for (const tid of chosen) {
      this.db.prepare(`UPDATE ${T("threads")} SET title=?, payload_json=json_set(payload_json,'$.title',?) WHERE thread_id=?`).run(title, title, tid)
      this.touch("threads", tid)
    }
    let project = ""
    if (this.chance(0.3)) {
      const projects = this.all("SELECT project_id FROM projection_projects WHERE deleted_at IS NULL ORDER BY project_id").map((r) => r.project_id as string)
      if (projects.length) {
        const id = this.pick(projects)
        this.db.prepare("UPDATE projection_projects SET title=? WHERE project_id=?").run(`${this.pick(WORDS)}-repaired-${this.step}`, id)
        this.touch("projection_projects")
        project = ` and project ${id}`
      }
    }
    return `${n} thread titles${project}`
  }
  private migrateData(): string | null {
    const what = this.rewriteRows()
    if (what === null) return null
    this.ledgerRow(`LabRepair${this.step}`)
    return `a data-only migration rewrote ${what}, with its ledger row and no event`
  }
  private migrateSchema(): string | null {
    const table = this.chance(0.5) ? T("threads") : "projection_projects"
    this.db.exec(`ALTER TABLE ${table} ADD COLUMN lab_m${this.step} INTEGER`)
    this.tables.add(table.replace(PREFIX, ""))
    const what = this.rewriteRows() ?? "no rows"
    this.ledgerRow(`LabColumn${this.step}`)
    return `a migration added a column to ${table} and rewrote ${what}, with its ledger row and no event`
  }
  private migrateThenEvent(): string | null {
    const what = this.rewriteRows()
    if (what === null) return null
    this.ledgerRow(`LabRepair${this.step}`)
    // The migration commits; an ordinary write follows before the next poll.
    this.w.release()
    this.w.hold()
    this.commits = 2
    const then = this.appendItems() ?? this.editThread()
    return `a data-only migration rewrote ${what}, then ${then ?? "nothing"}`
  }
  private ledgerOnly(): string | null {
    this.ledgerRow(`LabNoop${this.step}`)
    return "a ledger row and nothing else"
  }
  private projectEvent(): string | null {
    const projects = this.all("SELECT project_id FROM projection_projects WHERE deleted_at IS NULL ORDER BY project_id").map((r) => r.project_id as string)
    if (!projects.length) return null
    const id = this.pick(projects)
    const full = this.row("projection_projects", id)!
    const patch: StreamRow = { updated_at: this.at(0) }
    if (this.chance(0.7)) patch.title = `${this.pick(WORDS)}-${this.pick(WORDS)}-p${this.step}`
    else patch.workspace_root = `/home/user/projects/${this.pick(WORDS)}-p${this.step}`
    this.w.projectEventKeepCursor(id, this.at(0))
    this.w.upsertRow("projection_projects", { ...full, ...patch })
    this.touch("projection_projects")
    return `project ${id} ${patch.title ? `renamed to ${patch.title}` : `moved to ${patch.workspace_root}`} (a project event; the cursor stays)`
  }

  // ── a failed update: snapshot, then restore ──
  private snapshotDir() {
    return `${this.path}.restore-snapshot`
  }
  private restoreStep(step: number): StreamRecord {
    const dir = this.snapshotDir()
    const base = { step, requested: "restore-older-file", seq: null, events: 0, threads: [] as string[] }
    if (!this.snapshot) {
      // The launcher's snapshot, taken while nothing writes: the stream is between steps.
      const shm = this.rng() < 0.5
      rmSync(dir, { recursive: true, force: true })
      mkdirSync(dir)
      for (const suffix of ["", "-wal", ...(shm ? ["-shm"] : [])]) {
        if (existsSync(this.path + suffix)) copyFileSync(this.path + suffix, join(dir, `db${suffix}`))
      }
      this.snapshot = { step, shm }
      return { ...base, shape: "snapshot-for-restore", writes: 0, detail: `copied the database aside for a later restore${shm ? "" : " (without -shm)"}`, now_ms: this.clock + 3000, tables: [] }
    }
    const { step: from, shm } = this.snapshot
    const before = (this.get("SELECT COALESCE(MAX(sequence),0) AS s FROM orchestration_events")!.s as number) ?? 0
    const inode = statSync(this.path).ino
    this.w.close()
    this.stmts.clear()
    this.db.close()
    for (const suffix of ["", "-wal", "-shm"]) {
      const saved = join(dir, `db${suffix}`)
      if (existsSync(saved)) copyFileSync(saved, this.path + suffix)
      else rmSync(this.path + suffix, { force: true })
    }
    if (statSync(this.path).ino !== inode) throw new Error(`step ${step}: the restore replaced the file instead of copying over it`)
    rmSync(dir, { recursive: true, force: true })
    this.snapshot = null
    this.db = new DatabaseSync(this.path)
    this.db.exec("PRAGMA busy_timeout=5000")
    this.w = new T3Sink(this.db, this.lab)
    const after = (this.get("SELECT COALESCE(MAX(sequence),0) AS s FROM orchestration_events")!.s as number) ?? 0
    this.setMetaNow()
    return {
      ...base, shape: "restore-older-file", writes: 1, now_ms: this.clock + 3000, tables: ["scale_meta"],
      detail: `restored the file as of step ${from} in place${shm ? "" : ", deleting -shm"} (log ${before} -> ${after}); the stream's position written back with no event`,
    }
  }
  /** The stream's position, in a commit of its own with no event. */
  private setMetaNow() {
    this.db.exec("BEGIN")
    this.setMeta("stream_step", String(this.step))
    this.setMeta("stream_clock", String(this.clock))
    this.setMeta("stream_ordinal", String(this.ordinal))
    this.db.exec("COMMIT")
  }

  // ── rare, legal oddities ──
  private sparseWrite(nullText: boolean): string | null {
    const projects = this.all("SELECT project_id FROM projection_projects WHERE deleted_at IS NULL ORDER BY project_id").map((r) => r.project_id as string)
    if (!projects.length) return null
    const tid = `t-x${this.step}s`
    const project = this.pick(projects)
    const rid = `r-x${this.step}s`
    // Everything optional is left out: no title text, no provider thread, no node, no payload fields.
    this.ev(tid, "thread.created", 0, null, () => {
      this.put(T("threads"), {
        thread_id: tid, project_id: project, title: "", default_provider: "claude", runtime_mode: "auto", interaction_mode: "default", active_provider_thread_id: null,
        created_at: this.at(0), updated_at: this.at(0), archived_at: null, deleted_at: null, provider_instance_id: null, payload_json: { id: tid, projectId: project },
      }, tid)
    })
    const withRun = this.chance(0.6)
    if (withRun) {
      this.ev(tid, "run.started", 300, rid, () => {
        this.put(T("runs"), {
          run_id: rid, thread_id: tid, ordinal: 1, provider: "claude", provider_thread_id: null, status: "running", requested_at: this.at(300), completed_at: null,
          provider_instance_id: null, payload_json: { id: rid, threadId: tid, status: "running" },
        }, tid)
      })
    }
    this.ev(tid, "turn-item.updated", 600, withRun ? rid : null, () => {
      for (let j = 0; j < 2; j++) {
        const o = ++this.ordinal
        const type = nullText ? this.pick(["command_execution", "file_change"]) : this.pick(["dynamic_tool", "reasoning", "assistant_message"])
        this.put(T("turn_items"), {
          turn_item_id: `i-x${this.step}-${o}`, thread_id: tid, run_id: withRun ? rid : null, node_id: null, provider_thread_id: null, provider_turn_id: null, parent_item_id: null,
          ordinal: o, type, status: j ? "running" : "completed", updated_at: this.at(600 + j * 100), payload_json: {},
        }, tid)
      }
      this.bump(tid, 800)
    })
    return `bare thread ${tid}${withRun ? " with a bare run" : " with no run"} and two items with empty payloads${nullText ? " (command and file items with no text)" : ""}`
  }
}

/** Blocking line reader for stdin: the serve loop answers one request at a time. */
function* stdinLines(): Generator<string> {
  const buf = Buffer.alloc(65536)
  let pending = ""
  for (;;) {
    let n = 0
    try {
      n = readSync(0, buf, 0, buf.length, null)
    } catch (e) {
      if ((e as { code?: string }).code === "EAGAIN") {
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5)
        continue
      }
      if ((e as { code?: string }).code === "EOF") return
      throw e
    }
    if (n === 0) return
    pending += buf.toString("utf8", 0, n)
    let at: number
    while ((at = pending.indexOf("\n")) >= 0) {
      yield pending.slice(0, at)
      pending = pending.slice(at + 1)
    }
  }
}

function streamMain(path: string, seed: number, count: number, exclude: string[], include: string[], serve: boolean) {
  const stream = new Stream(path, seed, exclude, include)
  const shapes: Record<string, number> = {}
  // writeSync, not console.log: the serve loop blocks on stdin, and a buffered pipe write would wait with it.
  const out = (value: unknown) => writeSync(1, JSON.stringify(value) + "\n")
  const step = () => {
    const started = performance.now()
    const record = stream.next()
    // Opt-in: how long the step took to generate (not deterministic, so not in the default record).
    if (process.env.T3_STREAM_TIMING) (record as StreamRecord & { ms?: number }).ms = Math.round((performance.now() - started) * 10) / 10
    shapes[record.shape] = (shapes[record.shape] ?? 0) + 1
    out(record)
    return record
  }
  try {
    if (!serve) {
      let writes = 0
      let last: StreamRecord | null = null
      while (writes < count) {
        last = step()
        writes += last.writes
      }
      out({ stream_done: true, seed, steps: stream.step, writes, now_ms: last?.now_ms ?? null, shapes })
      return
    }
    out({ ready: true, seed, step: stream.step, now_ms: stream.clock + 3000 })
    for (const line of stdinLines()) {
      const k = Math.max(1, Number.parseInt(line.trim() || "1", 10) || 1)
      let writes = 0
      let last: StreamRecord | null = null
      for (let i = 0; i < k; i++) {
        last = step()
        writes += last.writes
      }
      out({ batch: k, writes, step: stream.step, now_ms: last!.now_ms })
    }
  } finally {
    stream.close()
  }
}

// ── CLI ─────────────────────────────────────────────────────────────────────

function nearest(sorted: number[], p: number): number | null {
  if (!sorted.length) return null
  return sorted[Math.ceil((p / 100) * sorted.length) - 1]
}

function stats(path: string) {
  const db = new DatabaseSync(path, { readOnly: true })
  try {
    const tables = ["projection_projects", ...["threads", "runs", "subagents", "nodes", "provider_threads", "provider_turns", "turn_items", "runtime_requests"].map((t) => PREFIX + t)]
    const existing = new Set((db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all() as { name: string }[]).map((r) => r.name))
    if (existing.has("orchestration_v2_events")) tables.push("orchestration_v2_events")
    if (existing.has("orchestration_events")) tables.push("orchestration_events")
    const rows: Record<string, number> = {}
    for (const t of tables) rows[t] = (db.prepare(`SELECT COUNT(*) AS n FROM ${t}`).get() as { n: number }).n
    const meta: Record<string, number> = {}
    if (existing.has("orchestration_v2_projection_metadata")) {
      for (const row of db.prepare("SELECT projection_name AS name, last_sequence AS n FROM orchestration_v2_projection_metadata").all() as { name: string; n: number }[]) meta[row.name] = row.n
      rows["metadata_last_sequence"] = meta["thread-projections"] ?? -1
    }
    let shape: Record<string, unknown> | undefined
    if (existing.has(`${PREFIX}turn_items`) && rows[`${PREFIX}turn_items`] > 0) {
      const pct = (sql: string) => {
        const values = (db.prepare(sql).all() as { n: number }[]).map((r) => r.n).sort((a, b) => a - b)
        return { n: values.length, sum: values.reduce((a, b) => a + b, 0), p50: nearest(values, 50), p75: nearest(values, 75), p90: nearest(values, 90), p99: nearest(values, 99), max: values.at(-1) ?? null }
      }
      const since3 = iso(EPOCH - WINDOW)
      const since24 = iso(EPOCH - DAY)
      shape = {
        items_per_thread: pct(`SELECT COUNT(*) AS n FROM ${PREFIX}turn_items GROUP BY thread_id`),
        nodes_per_thread: pct(`SELECT COUNT(*) AS n FROM ${PREFIX}nodes GROUP BY thread_id`),
        runs_per_thread: pct(`SELECT COUNT(*) AS n FROM ${PREFIX}runs GROUP BY thread_id`),
        items_per_run: pct(`SELECT COUNT(*) AS n FROM ${PREFIX}turn_items WHERE run_id IS NOT NULL GROUP BY run_id`),
        nodes_per_run: pct(`SELECT COUNT(*) AS n FROM ${PREFIX}nodes WHERE run_id IS NOT NULL GROUP BY run_id`),
        item_bytes: pct(`SELECT length(payload_json) AS n FROM ${PREFIX}turn_items`),
        recent_3h: db.prepare(`SELECT COUNT(*) AS n, COUNT(DISTINCT thread_id) AS threads FROM ${PREFIX}turn_items WHERE updated_at>=?`).get(since3),
        recent_24h: db.prepare(`SELECT COUNT(*) AS n, COUNT(DISTINCT thread_id) AS threads FROM ${PREFIX}turn_items WHERE updated_at>=?`).get(since24),
      }
    }
    let events: Record<string, unknown> | undefined
    if (existing.has("orchestration_events")) {
      const tip = db.prepare("SELECT MAX(sequence) AS n, COUNT(*) AS c, MIN(sequence) AS lo FROM orchestration_events").get() as { n: number; c: number; lo: number }
      events = {
        rows: tip.c, min: tip.lo, max: tip.n, gap_free: tip.n - tip.lo + 1 === tip.c,
        by_kind: db.prepare("SELECT aggregate_kind AS k, COUNT(*) AS n FROM orchestration_events GROUP BY 1").all(),
        by_type: db.prepare("SELECT event_type AS k, COUNT(*) AS n FROM orchestration_events GROUP BY 1 ORDER BY n DESC").all(),
        versions: db.prepare("SELECT application_event_version AS v, COUNT(*) AS n FROM orchestration_events GROUP BY 1").all(),
        meta,
      }
    }
    const wal = existsSync(path + "-wal") ? statSync(path + "-wal").size : 0
    return { rows, events, shape, bytes: statSync(path).size + wal, journal: (db.prepare("PRAGMA journal_mode").get() as { journal_mode: string }).journal_mode }
  } finally {
    db.close()
  }
}

const args = process.argv.slice(2)
function arg(name: string, fallback?: string): string {
  const i = args.indexOf(name)
  if (i < 0 && fallback !== undefined) return fallback
  if (i < 0 || args[i + 1] === undefined) throw new Error(`missing ${name}`)
  return args[i + 1]
}
const has = (name: string) => args.includes(name)

const schemaArg = arg("--schema", "t3")
if (schemaArg !== "t3" && schemaArg !== "demo") throw new Error(`bad --schema ${schemaArg}; expected t3 or demo`)
const schema: Schema = schemaArg
const defaultDir = schema === "t3" ? join("checks", "generated", "scale", "t3") : join("checks", "generated", "scale")
const dbPath = resolve(arg("--db", join(defaultDir, `${arg("--tier", "s")}.sqlite`)))
if (has("--stream")) {
  const exclude = has("--exclude") ? arg("--exclude").split(",").filter(Boolean) : []
  const include = has("--include") ? arg("--include").split(",").filter(Boolean) : []
  streamMain(dbPath, Number(arg("--stream")), has("--count") ? Number(arg("--count")) : 0, exclude, include, has("--serve"))
} else if (has("--mutate")) {
  console.log(JSON.stringify({ path: dbPath, ...mutate(dbPath, Number(arg("--mutate")), has("--touch") ? Number(arg("--touch")) : 0) }))
} else if (has("--stats")) {
  console.log(JSON.stringify({ path: dbPath, ...stats(dbPath) }))
} else if (arg("--tier", "s") === "real") {
  if (schema !== "t3") throw new Error("--tier real needs --schema t3")
  generateStress(dbPath)
  console.log(JSON.stringify({ tier: "real", schema, path: dbPath, epoch: EPOCH, ...stats(dbPath) }))
} else {
  const tier = arg("--tier", "s")
  const base = TIERS[tier]
  if (!base) throw new Error(`unknown tier ${tier}; expected one of ${Object.keys(TIERS).join(", ")}, real`)
  const num = (name: string, value: number) => (has(name) ? Number(arg(name)) : value)
  const shape: Shape = {
    threads: num("--threads", base.threads), subs: num("--subs", base.subs), runs: num("--runs", base.runs),
    items: num("--items", base.items), turns: num("--turns", base.turns), depth: num("--depth", base.depth),
    spanDays: num("--span-days", base.spanDays), openRequests: num("--open-requests", base.openRequests),
    recent: num("--recent", base.recent), running: num("--running", base.running), seed: num("--seed", 42),
  }
  for (const [k, v] of Object.entries(shape)) if (!Number.isFinite(v) || v < 0) throw new Error(`bad ${k}: ${v}`)
  generate(dbPath, shape, schema)
  console.log(JSON.stringify({ tier, schema, path: dbPath, epoch: EPOCH, shape, ...stats(dbPath) }))
}
