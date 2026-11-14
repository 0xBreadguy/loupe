// Read-only adapter over T3 Code's local orchestration v2 projection tables.
// Never writes. Reads only the fields it shows: never auth tables, secrets,
// prompts, message bodies, tool output or reasoning text.
import { accessSync, constants, existsSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { DatabaseSync } from "node:sqlite"

export const PREFIX = "orchestration_v2_projection_"
export const LIVE = new Set(["running", "starting", "preparing", "queued", "waiting", "pending", "active"])
export const BAD = new Set(["failed", "error", "interrupted"])
const TABLES = ["threads", "runs", "subagents", "nodes", "provider_threads", "provider_turns", "turn_items", "runtime_requests"]
export const MAX_WINDOW_MS = 3 * 3600_000
// The payload fields the dashboard uses. T3's payloads also hold prompts,
// results, goals and other message text; selecting fields in SQL keeps that
// text out of this process entirely. An object field lists its own fields.
type Fields = Record<string, true | string[]>
const FIELDS: Record<string, Fields> = {
  threads: { id: true, projectId: true, title: true, activeProviderThreadId: true, worktreePath: true, branch: true,
    updatedAt: true, lineage: ["relationshipToParent", "parentThreadId"], modelSelection: ["model", "options"] },
  runs: { id: true, threadId: true, status: true, providerThreadId: true, startedAt: true, requestedAt: true,
    completedAt: true, modelSelection: ["model", "options"] },
  subagents: { id: true, threadId: true, parentNodeId: true, runId: true, providerThreadId: true, childThreadId: true,
    status: true, title: true, model: true, startedAt: true, completedAt: true, updatedAt: true, remoteChild: ["label"] },
  provider_threads: { id: true, contextUsage: true },
  provider_turns: { providerThreadId: true, model: true, tokenUsage: true, turnTokenUsage: true },
}

/** SQL that builds a JSON object holding only the listed fields of payload_json. */
export function projection(fields: Fields): string {
  const parts = Object.entries(fields).map(([key, sub]) => {
    if (!/^\w+$/.test(key) || (sub !== true && !sub.every((k) => /^\w+$/.test(k)))) throw new Error("bad field " + key)
    if (sub === true) return `'${key}', json_extract(payload_json, '$.${key}')`
    const inner = sub.map((k) => `'${k}', json_extract(payload_json, '$.${key}.${k}')`).join(", ")
    return `'${key}', CASE WHEN json_type(payload_json, '$.${key}') = 'object' THEN json_object(${inner}) END`
  })
  return `json_object(${parts.join(", ")})`
}

const TOOLISH = ["command_execution", "dynamic_tool", "file_change", "web_search", "subagent", "handoff",
  "notification", "user_input_request", "secret_request", "link_request"]

type Json = Record<string, any>
type Row = Record<string, any>

export interface Context {
  usedTokens?: number
  maxTokens?: number
  inputTokens?: number
  cachedInputTokens?: number
  outputTokens?: number
  reasoningOutputTokens?: number
  promptCacheTtlMs?: number
  promptCacheExpiresAt?: string
  updatedAt?: string
}

export interface Item {
  thread_id: string
  run_id: string | null
  node_id: string | null
  provider_thread_id: string | null
  type: string
  status: string
  updated_at: string
  started_at: string | null
  label: string
  command: string
  file: string
  tool: string | null
}

export interface Event { t: number; cat: string; failed: boolean }
export interface Point { pct: number; used: number; max: number; at: number; provider: string; model: string }
export interface Step { text: string; status: string }

export interface Agent {
  id: string
  parent: string | null
  title: string
  kind: "thread" | "subagent"
  project: string
  cwd: string
  model: string
  status: string
  run: Json
  provider_id: string | null
  context: Context
  turn: Json
  updated: string | null
  started: string | null
  ended: string | null
  remote: Json | null
  items: Item[] // tool activity of the latest run, newest first
  requests: string[]
  asked: string | null // when its oldest open request was made, where T3 records it
  thread_id: string
  branch: string | null
  options: Json[]
  // Visual extras
  history: Point[] // context fill per provider turn, oldest first
  events: Event[] // activity in the lane window (all runs)
  spans: [number, number | null][] // run intervals overlapping the lane window
  plan: Step[] | null
  mix: Record<string, number> // tool categories in the latest run
  failures: number // failed tool items in the latest run
  now: Item | null // the item currently running, if any
}

export interface Snapshot { agents: Agent[]; at: number; path: string }

export const ms = (stamp: string | null | undefined): number | null => {
  if (!stamp) return null
  const t = Date.parse(stamp)
  return Number.isFinite(t) ? t : null
}

export function category(type: string, tool?: string | null): string {
  switch (type) {
    case "command_execution": return "shell"
    case "file_change": return "edit"
    case "web_search": return "web"
    case "subagent": case "handoff": return "agent"
    case "user_input_request": case "secret_request": case "link_request": return "ask"
    case "reasoning": return "think"
    case "assistant_message": return "reply"
    case "user_message": return "you"
    case "notification": return "note"
    case "dynamic_tool": {
      const t = (tool ?? "").toLowerCase()
      if (/^(read|glob|grep|ls|view|view_image|notebookread)$/.test(t)) return "read"
      if (/(edit|write|patch)/.test(t)) return "edit"
      if (/(web|fetch|search|browse)/.test(t)) return "web"
      if (/^(agent|task|delegate|spawn)/.test(t)) return "agent"
      return "tool"
    }
    default: return "other"
  }
}

export type SourceProblem = "missing" | "unreadable" | "schema"

/** A path with the home folder shortened to ~, as people type it. */
export function tilde(path: string): string {
  const home = homedir()
  return home && path.startsWith(home + "/") ? "~" + path.slice(home.length) : path
}

/**
 * T3's database could not be read. The message names the path and stays short
 * enough for the bar's tooltip; `--once` builds fuller advice from the fields.
 */
export class SourceError extends Error {
  readonly kind: SourceProblem
  readonly path: string
  readonly detail: string
  constructor(kind: SourceProblem, path: string, detail = "") {
    super(kind === "missing" ? `No T3 database at ${tilde(path)}. Is T3 Code installed and opened once?`
      : kind === "unreadable" ? `Can't read the T3 database at ${tilde(path)}: ${detail}`
      : `Unsupported T3 schema; ${detail}`)
    this.name = "SourceError"
    this.kind = kind
    this.path = path
    this.detail = detail
  }
}

function cannotOpen(path: string, err: unknown): SourceError {
  if (!existsSync(path)) return new SourceError("missing", path)
  // SQLite says "unable to open database file" for most causes; name the usual ones.
  let detail = err instanceof Error ? err.message : String(err)
  try {
    if (statSync(path).isDirectory()) detail = "it is a folder, not a file"
    else accessSync(path, constants.R_OK)
  } catch {
    detail = "permission denied"
  }
  return new SourceError("unreadable", path, detail)
}

export class Source {
  readonly path: string
  constructor(path: string) {
    this.path = path
  }

  snapshot(now = Date.now()): Snapshot {
    let db: DatabaseSync
    try {
      // readOnly: never creates a missing database, never writes or migrates.
      db = new DatabaseSync(this.path, { readOnly: true, timeout: 250 })
    } catch (err) {
      throw cannotOpen(this.path, err)
    }
    try {
      db.exec("PRAGMA query_only=ON")
      db.exec("BEGIN")
      try {
        return this.read(db, now)
      } finally {
        db.exec("ROLLBACK")
      }
    } catch (err) {
      // SQLITE_NOTADB: SQLite only finds out at the first read.
      if ((err as { errcode?: unknown }).errcode === 26) throw new SourceError("unreadable", this.path, "not a SQLite database")
      throw err
    } finally {
      db.close()
    }
  }

  private read(db: DatabaseSync, now: number): Snapshot {
    const all = (sql: string, ...args: (string | number)[]): Row[] => db.prepare(sql).all(...args) as Row[]
    const existing = new Set(all("SELECT name FROM sqlite_master WHERE type='table'").map((r) => r.name))
    const missing = TABLES.map((t) => PREFIX + t).filter((t) => !existing.has(t))
    if (missing.length) throw new SourceError("schema", this.path, "missing " + missing.sort().join(", "))
    const payloads = (table: string, clause = "", args: (string | number)[] = []): Json[] =>
      all(`SELECT ${projection(FIELDS[table])} AS payload FROM ${PREFIX}${table} ${clause}`, ...args)
        .map((r) => JSON.parse(r.payload))

    const threads = payloads("threads", "WHERE deleted_at IS NULL AND archived_at IS NULL")
    const ids: string[] = threads.map((t) => t.id)
    if (!ids.length) return { agents: [], at: now, path: this.path }
    const marks = ids.map(() => "?").join(",")
    const where = `WHERE thread_id IN (${marks})`
    const latest = new Map<string, Json>()
    for (const run of payloads("runs", where + " ORDER BY ordinal DESC", ids)) {
      if (!latest.has(run.threadId)) latest.set(run.threadId, run)
    }
    const subs = payloads("subagents", where, ids)
    const providers = new Map(payloads("provider_threads", where, ids).map((p) => [p.id, p]))
    // One turn per provider conversation, not one per app thread: native
    // subagents may share an app thread while having their own token counters.
    const turns = new Map<string, Json>()
    for (const r of all(`SELECT ${projection(FIELDS.provider_turns)} AS payload_json FROM (
        SELECT payload_json, ROW_NUMBER() OVER (
          PARTITION BY provider_thread_id ORDER BY ordinal DESC, rowid DESC) AS rank
        FROM ${PREFIX}provider_turns ${where}) WHERE rank=1`, ...ids)) {
      const p = JSON.parse(r.payload_json)
      turns.set(p.providerThreadId, p)
    }
    const nodes = new Map(all(`SELECT node_id,parent_node_id,run_id FROM ${PREFIX}nodes ${where}`, ...ids)
      .map((r) => [r.node_id as string, r]))
    const projects = new Map(all("SELECT project_id,title,workspace_root FROM projection_projects")
      .map((r) => [r.project_id as string, r]))
    // created_at says how long a request has waited; older databases may not have it.
    const asked = all(`SELECT name FROM pragma_table_info('${PREFIX}runtime_requests')`).some((r) => r.name === "created_at")
    const requests = all(`SELECT thread_id,node_id,kind,status,${asked ? "created_at" : "NULL AS created_at"}
        FROM ${PREFIX}runtime_requests ${where}
        AND resolved_at IS NULL AND status IN ('pending','open','waiting','requested')`, ...ids)

    const runIds = [...new Set([...latest.values()].map((r) => r.id).concat(subs.map((s) => s.runId)).filter(Boolean))]
    const runMarks = runIds.map(() => "?").join(",") || "NULL"
    // Labels are tool names, titles, file names and command excerpts only.
    const items = all(`SELECT thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,
        json_extract(payload_json,'$.startedAt') AS started_at,
        json_extract(payload_json,'$.toolName') AS tool,
        substr(COALESCE(json_extract(payload_json,'$.title'), json_extract(payload_json,'$.toolName'),
          json_extract(payload_json,'$.label'),''),1,240) AS label,
        CASE WHEN type='command_execution' THEN substr(json_extract(payload_json,'$.input'),1,240) ELSE '' END AS command,
        CASE WHEN type='file_change' THEN substr(json_extract(payload_json,'$.fileName'),1,240) ELSE '' END AS file
      FROM ${PREFIX}turn_items WHERE run_id IN (${runMarks})
        AND type IN (${[...TOOLISH, "reasoning", "assistant_message"].map((t) => `'${t}'`).join(",")})
      ORDER BY updated_at DESC, ordinal DESC LIMIT 3000`, ...runIds) as Item[]

    const since = new Date(now - MAX_WINDOW_MS).toISOString()
    const events = all(`SELECT thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,
        json_extract(payload_json,'$.startedAt') AS started_at,
        json_extract(payload_json,'$.toolName') AS tool
      FROM ${PREFIX}turn_items ${where} AND updated_at >= ? AND type NOT IN ('checkpoint','todo_list')
      ORDER BY updated_at DESC LIMIT 8000`, ...ids, since)
    const history = all(`SELECT provider_thread_id,thread_id,started_at,completed_at,
        json_extract(payload_json,'$.tokenUsage.usedTokens') AS used,
        json_extract(payload_json,'$.tokenUsage.maxTokens') AS max,
        json_extract(payload_json,'$.model') AS model
      FROM (SELECT *, ROW_NUMBER() OVER (PARTITION BY provider_thread_id ORDER BY ordinal DESC) AS rank
        FROM ${PREFIX}provider_turns ${where}) WHERE rank <= 160`, ...ids)
    const spans = all(`SELECT thread_id,requested_at,completed_at,json_extract(payload_json,'$.startedAt') AS started_at
      FROM ${PREFIX}runs ${where} AND (completed_at IS NULL OR completed_at >= ?)`, ...ids, since)
    const plans = all(`SELECT thread_id,run_id,node_id,provider_thread_id,updated_at,
        json_extract(payload_json,'$.steps') AS steps
      FROM ${PREFIX}turn_items WHERE type='todo_list' AND run_id IN (${runMarks})
      ORDER BY updated_at DESC`, ...runIds)

    const agents = buildAgents(threads, latest, subs, providers, turns, nodes, projects, requests, items)
    decorate(agents, { subs, nodes, events, history, spans, plans, now })
    return { agents, at: now, path: this.path }
  }
}

function attributor(agents: Map<string, Agent>, subs: Json[], nodes: Map<string, Row>) {
  const subIds = new Set(subs.map((s) => s.id))
  const aliases = new Map(subs.filter((s) => s.childThreadId).map((s) => [s.id, s.childThreadId]))
  const owner = (node: string | null | undefined): string | null => {
    const visited = new Set<string>()
    while (node && !visited.has(node)) {
      if (subIds.has(node)) return aliases.get(node) ?? node
      visited.add(node)
      node = nodes.get(node)?.parent_node_id
    }
    return null
  }
  const native = new Map<string, string>()
  for (const a of agents.values()) {
    if (a.kind === "subagent" && a.provider_id && !native.has(a.provider_id)) native.set(a.provider_id, a.id)
  }
  // A native subagent's provider conversation identifies activity even when
  // the node is not explicitly nested under its spawning node.
  const target = (row: { node_id?: string | null; thread_id: string; provider_thread_id?: string | null }) =>
    (row.provider_thread_id && native.get(row.provider_thread_id)) || owner(row.node_id) || row.thread_id
  return { owner, target }
}

export function buildAgents(threads: Json[], latest: Map<string, Json>, subs: Json[], providers: Map<string, Json>,
  turns: Map<string, Json>, nodes: Map<string, Row>, projects: Map<string, Row>, requests: Row[], items: Item[]): Agent[] {
  const agents = new Map<string, Agent>()
  const metrics = (pid: string | null | undefined): [Context, Json] => {
    const p = (pid && providers.get(pid)) || {}
    const turn = (pid && turns.get(pid)) || {}
    // A running turn can be newer than the provider-thread context projection.
    let ctx: Context = turn.tokenUsage || p.contextUsage || {}
    if (ctx.usedTokens == null) ctx = p.contextUsage || {}
    return [ctx, turn]
  }
  const blank = { items: [], requests: [], asked: null, history: [], events: [], spans: [], plan: null, mix: {}, failures: 0, now: null }
  for (const thread of threads) {
    const tid = thread.id
    const run = latest.get(tid) ?? {}
    const pid = run.providerThreadId || thread.activeProviderThreadId || null
    const [ctx, turn] = metrics(pid)
    const project = projects.get(thread.projectId) ?? {}
    const lineage = thread.lineage ?? {}
    const selection = run.modelSelection || thread.modelSelection || {}
    agents.set(tid, {
      ...structuredClone(blank), id: tid, title: thread.title ?? "Untitled", kind: "thread",
      parent: lineage.relationshipToParent === "subagent" ? lineage.parentThreadId ?? null : null,
      project: project.title ?? "Unknown project", cwd: thread.worktreePath || project.workspace_root || "",
      model: turn.model || selection.model || "--", status: run.status ?? "idle", run, provider_id: pid,
      context: ctx, turn, updated: thread.updatedAt ?? null, started: run.startedAt || run.requestedAt || null,
      ended: run.completedAt ?? null, remote: null, thread_id: tid, branch: thread.branch ?? null,
      options: selection.options ?? [],
    })
  }
  const { owner } = attributor(agents, subs, nodes)
  // A delegated app thread appears once, nested under its spawning agent.
  for (const sub of subs) {
    const child = sub.childThreadId
    const parent = owner(sub.parentNodeId) || sub.threadId
    const existing = child && agents.get(child)
    if (existing) {
      existing.parent = parent
      existing.kind = "subagent"
      continue
    }
    const [ctx, turn] = metrics(sub.providerThreadId)
    agents.set(sub.id, {
      ...structuredClone(blank), id: sub.id, parent, title: sub.title || "Subagent", kind: "subagent",
      project: agents.get(sub.threadId)?.project ?? "", cwd: "", model: turn.model || sub.model || "--",
      status: sub.status ?? "unknown", run: { id: sub.runId }, provider_id: sub.providerThreadId ?? null,
      context: ctx, turn, updated: sub.updatedAt ?? null, started: sub.startedAt ?? null,
      ended: sub.completedAt ?? null, remote: sub.remoteChild ?? null, thread_id: sub.threadId, branch: null, options: [],
    })
  }
  const { target } = attributor(agents, subs, nodes)
  for (const item of items) {
    const a = agents.get(target(item))
    if (a && item.run_id === a.run.id) a.items.push(item)
  }
  for (const req of requests) {
    const a = agents.get(target(req as never))
    if (!a) continue
    a.requests.push(req.kind)
    if (req.created_at && (!a.asked || req.created_at < a.asked)) a.asked = req.created_at
  }
  for (const a of agents.values()) {
    a.items.sort((x, y) => (x.updated_at < y.updated_at ? 1 : x.updated_at > y.updated_at ? -1 : 0))
    if (a.items.length && a.items[0].updated_at > (a.updated ?? "")) a.updated = a.items[0].updated_at
    if (a.requests.length && LIVE.has(a.status)) a.status = "waiting"
    if (!a.parent || !agents.has(a.parent) || a.parent === a.id) a.parent = null
    a.now = a.items.find((i) => LIVE.has(i.status) && !["reasoning", "assistant_message"].includes(i.type))
      ?? a.items.find((i) => LIVE.has(i.status)) ?? null
    for (const i of a.items) {
      if (i.type === "reasoning" || i.type === "assistant_message") continue
      const cat = category(i.type, i.tool)
      a.mix[cat] = (a.mix[cat] ?? 0) + 1
      if (i.status === "failed") a.failures++
    }
  }
  return [...agents.values()]
}

interface Extras {
  subs: Json[]
  nodes: Map<string, Row>
  events: Row[]
  history: Row[]
  spans: Row[]
  plans: Row[]
  now: number
}

function decorate(list: Agent[], x: Extras) {
  const agents = new Map(list.map((a) => [a.id, a]))
  const { target } = attributor(agents, x.subs, x.nodes)
  for (const row of x.events) {
    const a = agents.get(target(row as never))
    const t = ms(row.started_at) ?? ms(row.updated_at)
    if (a && t !== null) a.events.push({ t, cat: category(row.type, row.tool), failed: row.status === "failed" })
  }
  const nativeByProvider = new Map(list.filter((a) => a.kind === "subagent" && a.provider_id && a.thread_id !== a.id)
    .map((a) => [a.provider_id!, a]))
  for (const row of x.history) {
    const a = nativeByProvider.get(row.provider_thread_id) ?? agents.get(row.thread_id)
    const used = Number(row.used)
    const max = Number(row.max)
    if (!a || !Number.isFinite(used) || !Number.isFinite(max) || max <= 0) continue
    a.history.push({ pct: Math.min(1, used / max), used, max, at: ms(row.started_at) ?? ms(row.completed_at) ?? 0,
      provider: row.provider_thread_id, model: row.model ?? "" })
  }
  for (const row of x.spans) {
    const a = agents.get(row.thread_id)
    const start = ms(row.started_at) ?? ms(row.requested_at)
    if (a && start !== null) a.spans.push([start, ms(row.completed_at)])
  }
  for (const a of list) {
    a.history.sort((p, q) => p.at - q.at)
    a.events.sort((p, q) => p.t - q.t)
    // Native subagents have no runs of their own; their lifetime is the span.
    if (a.id !== a.thread_id) {
      const start = ms(a.started)
      if (start !== null) a.spans.push([start, ms(a.ended)])
    }
  }
  for (const row of x.plans) {
    const a = agents.get(target(row as never))
    if (!a || a.plan || row.run_id !== a.run.id) continue
    try {
      const steps = JSON.parse(row.steps ?? "[]")
      if (Array.isArray(steps) && steps.length) {
        a.plan = steps.map((s: Json) => ({ text: String(s.text ?? s.step ?? ""), status: String(s.status ?? "pending") }))
      }
    } catch {
      // Ignore malformed plan payloads.
    }
  }
}

// ── Tree ordering ───────────────────────────────────────────────────────────

/**
 * How much of a parent's subagents the list shows: none, the recent ones (the
 * default; older finished ones fold into one "earlier" row), or every one.
 */
export type Stage = "folded" | "recent" | "all"

export interface TreeRow {
  id: string // the agent's id; for an earlier row, EARLIER and the parent's id
  agent: Agent // for an earlier row, the parent whose subagents it holds
  depth: number
  hasChildren: boolean
  stage: Stage // what its subagents show as; "all" for an agent without any
  kept: number // subagents the recent stage lists
  tucked: number // subagents the recent stage folds into its earlier row
  guides: boolean[] // per ancestor level: whether a vertical guide continues
  last: boolean
  earlier?: Agent[] // only on an earlier row: the subagents folded into it, newest first
}

// ── Earlier subagents ───────────────────────────────────────────────────────
// Judgment values. A long-running thread can collect dozens of finished
// subagents; listed one by one, three such threads fill the list. Under each
// parent the list keeps its running subagents, anyone waiting on you, and its
// latest finishes; the rest fold into one "earlier" row. Top-level threads
// never fold this way.
export const RECENT_MS = 3 * 3600_000 // a finished subagent counts as recent for this long
export const RECENT_MAX = 3 // and at most this many of them per parent
export const EARLIER = "earlier:" // an earlier row's id: this, then its parent's id

/**
 * When an agent last did anything: when it finished, else the later of its last
 * update and its start (0 when unknown). A finished thread's update time also
 * moves when only its record changes, so its end is the truer time.
 */
export const lastSeen = (a: Agent): number => ms(a.ended) ?? Math.max(ms(a.updated) ?? 0, ms(a.started) ?? 0)

/** Fold older finished subagents into earlier rows, as of `now`. */
export interface Recent {
  now: number
  expanded: Set<string> // parents that list every subagent
  keep?: Set<string> // agents listed wherever they are: the selection, the thread open in T3
}

export const needsYou = (a: Agent) => a.requests.length > 0 || a.status === "waiting"

/** The question or approval an agent is waiting on, when its activity has one: a pending request item. */
export const request = (a: Agent): Item | null =>
  needsYou(a) ? a.items.find((i) => category(i.type, i.tool) === "ask" && LIVE.has(i.status)) ?? null : null

/** Since when an agent has waited on you: its request's time, else its pending request item's, else its last activity. */
export function waitingSince(a: Agent): number | null {
  if (!needsYou(a)) return null
  const item = request(a)
  return ms(a.asked) ?? ms(item?.started_at) ?? ms(item?.updated_at) ?? ms(a.updated)
}
export const isLive = (a: Agent) => LIVE.has(a.status)
/** Running, as the dashboard's ACTIVE tile, `--once` and the bar all count it: live and not waiting on you. */
export const isRunning = (a: Agent) => isLive(a) && !needsYou(a)

// ── Stall detection ─────────────────────────────────────────────────────────
// Judgment values. A model can think for a minute or two between items, and
// builds or test runs legitimately take minutes, so these flag "look at this",
// not "broken".
export const QUIET_MS = 2 * 60_000 // running, no tool in flight, nothing new
export const STALL_MS = 8 * 60_000 // quiet this long turns warm
export const SLOW_TOOL_MS = 5 * 60_000 // one tool call running this long turns warm

export interface Pace {
  tool: Item | null // the tool call in flight, if any
  toolMs: number // how long it has been running
  delegating: boolean // the call in flight is a subagent: waiting on it is normal
  quietMs: number // with no tool in flight: time since the agent last did anything
  quiet: boolean // quietMs past QUIET_MS
  stalled: boolean // quiet past STALL_MS, or a tool call past SLOW_TOOL_MS
}

/**
 * How a running agent is getting on, from its latest items: busy in a tool,
 * working, or quiet. Null when it is not running or is waiting on you. An agent
 * whose subagents are still running is waiting on them, not quiet.
 */
export function pace(a: Agent, now: number, childrenLive = false): Pace | null {
  if (needsYou(a) || (a.status !== "running" && a.status !== "active")) return null
  const tool = a.now && a.now.type !== "reasoning" && a.now.type !== "assistant_message" ? a.now : null
  if (tool) {
    const toolMs = Math.max(0, now - (ms(tool.started_at) ?? ms(tool.updated_at) ?? now))
    const delegating = category(tool.type, tool.tool) === "agent"
    return { tool, toolMs, delegating, quietMs: 0, quiet: false, stalled: !delegating && toolMs >= SLOW_TOOL_MS }
  }
  let last = Math.max(ms(a.started) ?? 0, ms(a.now?.updated_at) ?? 0, ms(a.items[0]?.updated_at) ?? 0)
  for (const e of a.events) if (e.t > last) last = e.t
  const quietMs = a.now || childrenLive || !last ? 0 : Math.max(0, now - last)
  return { tool: null, toolMs: 0, delegating: childrenLive, quietMs, quiet: quietMs >= QUIET_MS, stalled: quietMs >= STALL_MS }
}

/**
 * The agents as a tree, in list order. `collapsed` parents list no subagents.
 * With `recent`, every other parent lists only its recent subagents and one
 * earlier row for the rest, unless it is in `recent.expanded`; without it,
 * every subagent is listed.
 */
export function tree(agents: Agent[], collapsed: Set<string>, activeOnly = false, recent?: Recent): TreeRow[] {
  const byId = new Map(agents.map((a) => [a.id, a]))
  let allowed = new Set(byId.keys())
  if (activeOnly) {
    allowed = new Set()
    for (const start of agents) {
      if (!isLive(start) && !start.requests.length) continue
      const seen = new Set<string>()
      let a: Agent | undefined = start
      while (a && !seen.has(a.id)) {
        seen.add(a.id)
        allowed.add(a.id)
        a = a.parent ? byId.get(a.parent) : undefined
      }
    }
  }
  const children = new Map<string | null, Agent[]>()
  for (const a of agents) {
    if (!allowed.has(a.id)) continue
    const list = children.get(a.parent) ?? []
    list.push(a)
    children.set(a.parent, list)
  }
  const key = (a: Agent): [number, number, number, string] =>
    [a.requests.length ? 0 : 1, isLive(a) ? 0 : 1, -(ms(a.updated) ?? 0), a.id]
  const order = (list: Agent[]) => list.sort((p, q) => {
    const [a, b] = [key(p), key(q)]
    for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return (a[i] as number) - (b[i] as number)
    return a[3] < b[3] ? -1 : a[3] > b[3] ? 1 : 0
  })
  // Never folded into an earlier row: live, waiting on you, kept, or above one that is.
  const busy = new Map<string, boolean>()
  const stays = (a: Agent, path = new Set<string>()): boolean => {
    const known = busy.get(a.id)
    if (known !== undefined) return known
    if (path.has(a.id)) return false
    path.add(a.id)
    const v = isLive(a) || needsYou(a) || !!recent?.keep?.has(a.id) || (children.get(a.id) ?? []).some((k) => stays(k, path))
    busy.set(a.id, v)
    return v
  }
  // The recent stage: what stays, plus the latest finishes inside the window; the rest newest first.
  const split = (kids: Agent[]): [Agent[], Agent[]] => {
    if (!recent) return [kids, []]
    const done = kids.filter((k) => !stays(k)).sort((p, q) => lastSeen(q) - lastSeen(p))
    const latest = new Set(done.slice(0, RECENT_MAX).filter((k) => lastSeen(k) >= recent.now - RECENT_MS))
    return [kids.filter((k) => latest.has(k) || stays(k)), done.filter((k) => !latest.has(k))]
  }
  const rows: TreeRow[] = []
  const seen = new Set<string>()
  const visit = (a: Agent, depth: number, guides: boolean[], last: boolean) => {
    if (seen.has(a.id)) return
    seen.add(a.id)
    const kids = order(children.get(a.id) ?? [])
    const [kept, tucked] = split(kids)
    // Nothing recent to list reads as folded: one row, not a lonely earlier row.
    const stage: Stage = !kids.length ? "all" : collapsed.has(a.id) ? "folded"
      : !tucked.length || recent?.expanded.has(a.id) ? "all" : kept.length ? "recent" : "folded"
    rows.push({ id: a.id, agent: a, depth, hasChildren: kids.length > 0, stage, kept: kept.length, tucked: tucked.length, guides, last })
    const shown = stage === "all" ? kids : stage === "recent" ? kept : []
    const more = stage === "recent" ? tucked : []
    const inner = depth > 0 ? [...guides, !last] : guides
    shown.forEach((k, i) => visit(k, depth + 1, inner, i === shown.length - 1 && !more.length))
    if (more.length) {
      rows.push({ id: EARLIER + a.id, agent: a, depth: depth + 1, hasChildren: false, stage: "all", kept: 0, tucked: 0,
        guides: inner, last: true, earlier: more })
    }
  }
  const roots = order(children.get(null) ?? [])
  roots.forEach((a, i) => visit(a, 0, [], i === roots.length - 1))
  return rows
}
