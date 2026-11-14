// Made-up T3 data, for trying the dashboard and taking screenshots without a
// T3 Code install. A scratch copy of the tables Source reads, in a temporary
// directory, filled with a scenario that shows every state the dashboard draws.
// It never reads or writes real T3 or Omarchy state; everything is invented.
import { mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { DatabaseSync, type SQLInputValue, type StatementSync } from "node:sqlite"
import { PREFIX } from "./source.ts"
import { family } from "./theme.ts"
import { parseProvider, type Provider } from "./usage.ts"

const MIN = 60_000
/**
 * The scenario replays on this cycle. Within it time runs normally (a quiet
 * agent stays quiet longer, a slow call keeps running); each new cycle starts
 * over, so a long-open demo never drifts into nonsense.
 */
export const CYCLE_MS = 20 * MIN
export const REFRESH_MS = 3000
const ENVIRONMENT = "6f1c2a9e-3b7d-4e58-9a10-d4c7e2b8f531"
const GROWTH = 0.0025 // context added per minute by agents still working

// ── Scenario ────────────────────────────────────────────────────────────────

type RunStatus = "running" | "completed" | "interrupted" | "failed" | "queued"

interface Role {
  id: string
  title: string
  model: string
  max: number // context window, tokens
  ctx: [number, number] | null // context filled when the run began and at the cycle start; null: not reported
  status: RunStatus
  from: number // the run began (a queued agent: was queued), minutes before the cycle start
  to?: number // finished agents: the run ended, minutes before the cycle start
  until?: number // last tool call, for agents that stopped working; others work up to now
  queued?: number // a queued agent starts running this many minutes into the cycle
  rest?: number // works in bursts: 3 minutes on, this many off; the cycle starts 3 minutes into a rest
  every: number // seconds between tool calls
  work: string[] // tool calls it repeats; see call()
  tail?: [number, string, string, number?][] // one-off calls: [minutes before the cycle start, call, status, minutes it ran]
  plan?: string[] // "✓ done", "▸ in progress", "○ pending"
  cache?: number // prompt cache lifetime in minutes
  options?: { id: string; value: string }[]
  project?: string // threads
  branch?: string
  before?: { model: string; max: number; from: number; to: number; ctx: [number, number] } // an earlier run elsewhere: a handoff
  subs?: Role[]
  child?: boolean // an app thread delegated to, rather than a native subagent
  remote?: string // runs in another environment; nothing about it is local
}

const PROJECTS = [
  { id: "atlas", title: "atlas-api", root: "/home/user/projects/atlas-api" },
  { id: "shop", title: "storefront-web", root: "/home/user/projects/storefront-web" },
  { id: "pipe", title: "pipeline-cli", root: "/home/user/projects/pipeline-cli" },
  { id: "book", title: "handbook", root: "/home/user/projects/handbook" },
]

const OPUS = "claude-opus-5-5"
const SONNET = "claude-sonnet-5-5"
const HAIKU = "claude-haiku-5-5"
const GPT = "gpt-6.1-sol"
const GROK = "grok-4.7"
const GROK_FAST = "grok-4.7-fast"

const SCENARIO: Role[] = [
  {
    id: "demo-billing", project: "atlas", title: "Migrate billing to the v2 API", model: OPUS, max: 1_000_000,
    ctx: [0.18, 0.64], status: "running", from: 38, every: 34, cache: 60, branch: "billing-v2",
    options: [{ id: "effort", value: "high" }],
    work: ["Read src/billing/client.ts", "Grep LegacyBillingClient", "~ src/billing/v2/invoices.ts", "~ src/billing/v2/customers.ts",
      "$ npm run typecheck", "$ npm test -- billing", "Read docs/billing-v2.md", "Edit src/billing/index.ts",
      "Read src/billing/v2/types.ts", "$ npm run lint -- src/billing"],
    tail: [[31.1, "% Write migration tests", "completed", 0.1], [24.1, "% Map callers of the legacy client", "completed", 0.1]],
    plan: ["✓ Inventory legacy client calls", "✓ Add the v2 client behind a flag", "▸ Port invoices and customers",
      "○ Write migration tests", "○ Remove the legacy client"],
    subs: [
      {
        id: "demo-callers", title: "Map callers of the legacy client", model: SONNET, max: 1_000_000, ctx: [0.35, 0.91],
        status: "running", from: 24, every: 22,
        work: ["Grep billing.legacy", "Read src/orders/checkout.ts", "Read src/reports/revenue.ts", "Glob src/**/*.billing.ts",
          "$ rg -l 'LegacyBillingClient' src", "Read src/webhooks/invoice.ts"],
        subs: [{
          id: "demo-headers", title: "Check rate-limit headers", model: HAIKU, max: 200_000, ctx: [0.03, 0.03],
          status: "queued", from: 0.6, queued: 1, every: 26,
          work: ["? rate limit response headers", "Read src/http/retry.ts", "Grep Retry-After"],
        }],
      },
      {
        // Quiet: still running, nothing in flight and nothing new for about ten minutes.
        id: "demo-tests", title: "Write migration tests", model: SONNET, max: 1_000_000, ctx: [0.05, 0.27],
        status: "running", from: 31, until: 9.5, every: 40,
        work: ["~ test/billing/v2.test.ts", "!$ npm test -- billing/v2", "Read test/fixtures/invoices.json"],
      },
    ],
  },
  {
    // A slow call: an end-to-end run going for over six minutes, after recent failures.
    id: "demo-checkout", project: "shop", title: "Fix the flaky checkout tests", model: GPT, max: 400_000,
    ctx: [0.22, 0.78], status: "running", from: 44, until: 7.5, every: 45, branch: "fix/checkout-flake",
    options: [{ id: "reasoning", value: "high" }],
    work: ["Read src/checkout/cart.ts", "!$ npx playwright test checkout", "~ src/checkout/payment.ts", "Grep waitForSelector",
      "Read playwright.config.ts", "Read test/e2e/checkout.spec.ts", "~ test/e2e/checkout.spec.ts"],
    tail: [[21.1, "% Bisect the checkout regression", "completed", 0.1],
      [7.3, "$ npx playwright test checkout.spec.ts", "failed", 0.7], [6.4, "$ npm run test:e2e -- --repeat 20", "running"]],
    plan: ["✓ Reproduce the flake", "▸ Find the commit that broke it", "○ Fix the race", "○ Run the suite 20 times"],
    subs: [{
      // Delegated app thread: bisecting, where every bad commit fails its test run.
      id: "demo-bisect", child: true, project: "shop", title: "Bisect the checkout regression", model: GROK, max: 2_000_000,
      ctx: [0.04, 0.33], status: "running", from: 21, every: 30,
      work: ["$ git bisect good", "$ git bisect bad", "!$ npx playwright test checkout.spec.ts", "!$ npx playwright test checkout.spec.ts",
        "Read src/checkout/session.ts", "$ git log --oneline -5", "$ git show --stat HEAD"],
    }],
  },
  {
    // Needs you: asked a question and waits for the answer.
    id: "demo-search", project: "atlas", title: "Plan the search index rebuild", model: OPUS, max: 1_000_000,
    ctx: [0.12, 0.54], status: "running", from: 27, until: 1.8, every: 38, cache: 5,
    options: [{ id: "effort", value: "medium" }],
    work: ["Read src/search/indexer.ts", "$ npm run bench:search", "? inverted index compression trade-offs", "Grep buildIndex",
      "Read docs/search.md", "Read src/search/query.ts", "$ npm test -- search"],
    tail: [[22.1, "% Survey index sizes", "completed", 0.1], [1.7, "@ Which index layout should we keep?", "pending"]],
    plan: ["✓ Measure the current index", "✓ Compare layouts", "▸ Choose a layout", "○ Write the rebuild job"],
    subs: [{
      id: "demo-sizes", title: "Survey index sizes", model: HAIKU, max: 200_000, ctx: [0.05, 0.38],
      status: "completed", from: 22, to: 12, every: 25,
      work: ["$ du -sh data/index/*", "Read scripts/index-stats.ts", "$ node scripts/index-stats.ts"],
    }],
  },
  {
    // Handed off: started on one provider, continued on another.
    id: "demo-export", project: "pipe", title: "Add streaming export to the CLI", model: GROK, max: 2_000_000,
    ctx: [0.03, 0.29], status: "running", from: 29, every: 28, branch: "streaming-export",
    before: { model: GPT, max: 400_000, from: 55, to: 33, ctx: [0.1, 0.71] },
    work: ["~ src/export/stream.rs", "~ src/export/csv.rs", "$ cargo test export", "$ cargo clippy", "Read src/export/mod.rs",
      "Edit src/cli/args.rs", "Read Cargo.toml"],
    tail: [[17.1, "% Profile the CSV writer", "completed", 0.1], [26.1, "% Run the integration suite", "completed", 0.1]],
    plan: ["✓ Sketch the stream API", "▸ Write the CSV encoder", "○ Wire up --format", "○ Benchmark large exports"],
    subs: [
      {
        // Works in bursts: quiet for a few minutes at a time, never long enough to stall.
        id: "demo-bench", title: "Profile the CSV writer", model: GROK_FAST, max: 2_000_000, ctx: [0.02, 0.11],
        status: "running", from: 17, rest: 6, every: 20,
        work: ["$ cargo bench --bench csv", "Read benches/csv.rs", "~ benches/csv.rs"],
      },
      {
        id: "demo-remote", title: "Run the integration suite", model: GROK, max: 2_000_000, ctx: null,
        status: "completed", from: 26, to: 5, every: 60, work: [], remote: "cloud sandbox",
      },
    ],
  },
  {
    // Long-running: hours of subagents. The list keeps the running one and the
    // latest three finishes; the six before them fold into one earlier row.
    id: "demo-webhooks", project: "atlas", title: "Harden the webhook pipeline", model: GPT, max: 400_000,
    ctx: [0.06, 0.58], status: "running", from: 290, every: 50, branch: "harden-webhooks",
    options: [{ id: "reasoning", value: "medium" }],
    work: ["Read src/webhooks/receiver.ts", "$ npm test -- webhooks", "Grep verifySignature", "~ src/webhooks/retry.ts",
      "Read docs/webhooks.md", "$ npm run typecheck"],
    tail: [[30.1, "% Review the signature check", "completed", 0.1], [6.1, "% Replay the dead-letter queue", "completed", 0.1]],
    plan: ["✓ Inventory the endpoints", "✓ Pick a queue backend", "✓ Verify every signature", "▸ Replay what was dropped",
      "○ Alert on retry storms"],
    subs: [
      {
        id: "demo-wh-replay", title: "Replay the dead-letter queue", model: SONNET, max: 1_000_000, ctx: [0.03, 0.19],
        status: "running", from: 6, every: 24,
        work: ["$ npm run dlq:list", "Read src/webhooks/dlq.ts", "$ npm run dlq:replay -- --dry-run", "~ src/webhooks/dlq.ts"],
      },
      {
        id: "demo-wh-signature", title: "Review the signature check", model: OPUS, max: 1_000_000, ctx: [0.05, 0.31],
        status: "completed", from: 30, to: 16, every: 40,
        work: ["Read src/webhooks/verify.ts", "Grep timingSafeEqual", "Read test/webhooks/verify.test.ts"],
      },
      {
        id: "demo-wh-idempotency", title: "Add idempotency keys", model: GPT, max: 400_000, ctx: [0.07, 0.44],
        status: "completed", from: 70, to: 41, every: 45,
        work: ["~ src/webhooks/receiver.ts", "~ src/db/migrations/0042_idempotency.sql", "$ npm test -- receiver"],
      },
      {
        id: "demo-wh-load", title: "Load-test the receiver", model: GROK_FAST, max: 2_000_000, ctx: [0.01, 0.06],
        status: "completed", from: 95, to: 78, every: 30,
        work: ["!$ k6 run load/webhooks.js", "Read load/webhooks.js", "~ load/webhooks.js"],
      },
      {
        id: "demo-wh-headers", title: "Audit the retry headers", model: HAIKU, max: 200_000, ctx: [0.04, 0.27],
        status: "completed", from: 140, to: 128, every: 35, work: ["Grep Retry-After", "Read src/webhooks/retry.ts"],
      },
      {
        id: "demo-wh-senders", title: "Map the webhook senders", model: SONNET, max: 1_000_000, ctx: [0.04, 0.35],
        status: "completed", from: 175, to: 152, every: 50, work: ["Grep webhooks.send", "Read src/billing/events.ts", "Read src/orders/events.ts"],
      },
      {
        id: "demo-wh-schema", title: "Check the payload schemas", model: HAIKU, max: 200_000, ctx: [0.06, 0.4],
        status: "completed", from: 200, to: 188, every: 35, work: ["Read schemas/webhooks/*.json", "$ npm run schema:check"],
      },
      {
        id: "demo-wh-queue", title: "Compare queue backends", model: GPT, max: 400_000, ctx: [0.05, 0.52],
        status: "failed", from: 235, to: 214, every: 55, work: ["? sqs vs redis streams for webhooks", "!$ npm run bench:queue"],
      },
      {
        id: "demo-wh-logs", title: "Sample production logs", model: GROK_FAST, max: 2_000_000, ctx: [0.01, 0.04],
        status: "completed", from: 262, to: 247, every: 40, work: ["$ ./scripts/logs.sh webhooks --since 7d", "Read logs/sample.txt"],
      },
      {
        id: "demo-wh-inventory", title: "Inventory webhook endpoints", model: HAIKU, max: 200_000, ctx: [0.03, 0.21],
        status: "completed", from: 285, to: 272, every: 35, work: ["Glob src/**/webhooks/*.ts", "Read src/routes.ts"],
      },
    ],
  },
  {
    id: "demo-docs", project: "book", title: "Refresh the onboarding guide", model: SONNET, max: 1_000_000,
    ctx: [0.06, 0.41], status: "completed", from: 52, to: 18, every: 50, cache: 60,
    work: ["Read guides/onboarding.md", "~ guides/onboarding.md", "? markdown link checker", "$ npx markdownlint guides",
      "Edit guides/setup.md"],
  },
  {
    id: "demo-images", project: "shop", title: "Upgrade the image pipeline", model: GPT, max: 400_000,
    ctx: [0.08, 0.55], status: "interrupted", from: 49, to: 34, every: 42,
    work: ["Read src/images/resize.ts", "~ src/images/resize.ts", "$ npm run build", "$ npm ls sharp"],
  },
  {
    id: "demo-retry", project: "pipe", title: "Tune the retry backoff", model: GROK, max: 2_000_000,
    ctx: [0.02, 0.17], status: "failed", from: 40, to: 26, every: 36,
    work: ["Read src/net/retry.rs", "~ src/net/retry.rs", "!$ cargo test retry", "$ cargo build"],
    tail: [[26.6, "$ cargo test retry::backoff", "failed", 0.5]],
  },
]

// ── Building the database ───────────────────────────────────────────────────

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
`

/** A stable pseudo-random number in [0, 1) for the given keys, so rebuilds agree with each other. */
function rand(...keys: (string | number)[]): number {
  let h = 2166136261
  for (const ch of keys.join("|")) h = Math.imul(h ^ ch.charCodeAt(0), 16777619)
  h ^= h >>> 15
  h = Math.imul(h, 0x2c1b3c6d)
  h ^= h >>> 12
  return (h >>> 0) / 4294967296
}

const iso = (t: number | null | undefined) => (t == null ? null : new Date(t).toISOString())
const clamp = (v: number, lo = 0, hi = 1) => Math.max(lo, Math.min(hi, v))

/** A tool call written as "$ command", "~ file", "? search", "@ question", "% subagent" or "Tool argument"; "!" marks a flaky one. */
function call(spec: string): { type: string; payload: Record<string, string>; flaky: boolean } {
  const flaky = spec.startsWith("!")
  const s = flaky ? spec.slice(1) : spec
  const rest = s.slice(2)
  switch (s.slice(0, 2)) {
    case "$ ": return { type: "command_execution", payload: { input: rest }, flaky }
    case "~ ": return { type: "file_change", payload: { fileName: rest }, flaky }
    case "? ": return { type: "web_search", payload: { title: rest }, flaky }
    case "@ ": return { type: "user_input_request", payload: { title: rest }, flaky }
    case "% ": return { type: "subagent", payload: { title: rest }, flaky }
  }
  return { type: "dynamic_tool", payload: { toolName: s.split(" ")[0], title: s }, flaky }
}

interface Where { thread: string; run: string; provider: string | null }

class Writer {
  private db: DatabaseSync
  private statements = new Map<string, StatementSync>()
  ordinal = 0

  constructor(db: DatabaseSync) {
    this.db = db
  }

  row(table: string, values: Record<string, unknown>) {
    const keys = Object.keys(values)
    const sql = `INSERT INTO ${table} (${keys.join(",")}) VALUES (${keys.map(() => "?").join(",")})`
    let statement = this.statements.get(sql)
    if (!statement) {
      statement = this.db.prepare(sql)
      this.statements.set(sql, statement)
    }
    statement.run(...keys.map((k) => {
      const v = values[k]
      return (v === undefined ? null : typeof v === "object" && v !== null ? JSON.stringify(v) : v) as SQLInputValue
    }))
  }

  item(w: Where, type: string, status: string, started: number, updated: number, payload: Record<string, unknown> = {}) {
    const n = this.ordinal++
    this.row(`${PREFIX}turn_items`, {
      item_id: `item-${n}`, thread_id: w.thread, run_id: w.run, node_id: null, provider_thread_id: w.provider, type, status,
      updated_at: iso(updated), ordinal: n, payload_json: { ...payload, startedAt: iso(started) },
    })
  }
}

interface Life { status: RunStatus; start: number; ended: number | null; end: number; busy: boolean }

class Scene {
  readonly w: Writer
  readonly now: number
  readonly epoch: number
  readonly base: number // the start of the current cycle

  constructor(w: Writer, now: number, epoch: number) {
    this.w = w
    this.now = now
    this.epoch = epoch
    this.base = now - (Math.max(0, now - epoch) % CYCLE_MS)
  }

  at(minutes: number): number {
    return this.base - minutes * MIN
  }

  life(r: Role): Life {
    if (r.queued !== undefined) {
      const starts = this.base + r.queued * MIN
      const queued = this.now < starts
      return { status: queued ? "queued" : "running", start: queued ? this.at(r.from) : starts, ended: null,
        end: this.now, busy: !queued }
    }
    const ended = r.to !== undefined ? this.at(r.to) : null
    const until = r.until !== undefined ? this.at(r.until) : ended ?? this.now
    return { status: r.status, start: this.at(r.from), ended, end: Math.min(until, this.now),
      busy: r.status === "running" && ended === null && r.until === undefined }
  }

  /** Bursty agents work 3 minutes, then rest; each cycle starts 3 minutes into a rest. */
  private working(r: Role, t: number): boolean {
    if (!r.rest) return true
    const period = (3 + r.rest) * MIN
    return (((t - this.base + 6 * MIN) % period) + period) % period < 3 * MIN
  }

  /** Tool calls on a fixed time grid, so a rebuild keeps earlier calls and adds the new ones. Returns the last activity. */
  activity(r: Role, w: Where, life: Life, prompts: boolean): number {
    let last = life.start
    if (life.status === "queued") return last
    // Only top-level threads get prompts from you; subagents are prompted by their parent.
    if (prompts) {
      this.w.item(w, "user_message", "completed", life.start - 2000, life.start - 2000)
      if (life.end - life.start > 25 * MIN) {
        const t = life.start + Math.round((life.end - life.start) * 0.55)
        this.w.item(w, "user_message", "completed", t, t)
      }
    }
    const period = r.every * 1000
    const offset = Math.floor(rand(r.id, w.run, "offset") * period)
    let inFlight = false
    if (r.work.length) {
      for (let j = Math.ceil((life.start - this.epoch - offset) / period); ; j++) {
        const t = this.epoch + offset + j * period
        if (t > life.end) break
        if (t < life.start || !this.working(r, t)) continue
        // Activity ebbs and flows on a slow wave, so the pulse chart has a shape.
        const wave = 0.5 + 0.5 * Math.sin((2 * Math.PI * t) / (13 * MIN) + rand(r.id, "wave") * 2 * Math.PI)
        if (rand(r.id, j, "skip") < 0.08 + 0.32 * wave) continue
        const c = call(r.work[Math.floor(rand(r.id, j, "pick") * r.work.length)])
        // Busy agents spend about half their time inside a tool call; commands run longest.
        const share = c.type === "command_execution" ? 0.4 + 0.45 * rand(r.id, j, "dur") : 0.15 + 0.45 * rand(r.id, j, "dur")
        const dur = Math.round(period * share)
        const running = t + dur > life.end
        if (running && !life.busy) continue
        const status = running ? "running" : c.flaky && rand(r.id, j, "fail") < 0.4 ? "failed" : "completed"
        this.w.item(w, c.type, status, t, running ? t : t + dur, c.payload)
        inFlight ||= running
        last = Math.max(last, running ? t : t + dur)
      }
    }
    for (const [minutes, spec, status, ran = 0] of r.tail ?? []) {
      const t = this.at(minutes)
      if (t > this.now) continue
      const c = call(spec)
      const open = status === "running" || status === "pending"
      const done = Math.min(this.now, t + ran * MIN)
      this.w.item(w, c.type, status, t, open ? t : done, c.payload)
      if (open) inFlight = true
      last = Math.max(last, open ? t : done)
    }
    // Between tool calls a working agent is thinking (not while a bursty one rests: that is its quiet spell).
    if (life.busy && !inFlight && this.working(r, this.now)) this.w.item(w, "reasoning", "running", last, last)
    return last
  }

  /** One provider turn every few tool calls; the latest carries the context report. */
  turns(r: Role, w: Where & { provider: string }, model: string, max: number, ctx: [number, number], life: Life,
    last: number, open: boolean) {
    if (last <= life.start + 5000) return // just started: nothing reported yet
    // Reaches ctx[1] at the cycle start, or at the last activity for an agent that stopped before it.
    const anchor = Math.min(this.base, last)
    const fill = (t: number) => {
      if (t <= this.base) {
        const u = clamp((t - life.start) / Math.max(MIN, anchor - life.start))
        return ctx[0] + (ctx[1] - ctx[0]) * (u - 0.05 * Math.sin(2 * Math.PI * u))
      }
      return Math.min(Math.max(ctx[1], 0.97), ctx[1] + (GROWTH * (t - this.base)) / MIN)
    }
    const period = r.every * 5000
    const offset = Math.floor(rand(r.id, w.provider, "turns") * period)
    const starts = [life.start + 5000]
    for (let k = Math.ceil((life.start + 30_000 - this.epoch - offset) / period); ; k++) {
      const t = this.epoch + offset + k * period
      if (t >= last) break
      starts.push(t)
    }
    const fam = family(model)
    const ttl = r.cache ? r.cache * MIN : null
    starts.forEach((t, k) => {
      const latest = k === starts.length - 1
      const done = latest ? last : starts[k + 1]
      const used = Math.round(fill(done) * max)
      const output = Math.round(800 + 5200 * rand(r.id, k, "out"))
      const input = Math.max(0, used - output)
      const cached = Math.round(input * (fam === "claude" ? 0.94 : fam === "openai" ? 0.82 : 0.7))
      const reasoning = Math.round(output * (fam === "claude" ? 0.4 : 0.9))
      const calls = 3 + Math.floor(rand(r.id, k, "calls") * 7)
      const complete = !(latest && open)
      this.w.row(`${PREFIX}provider_turns`, {
        thread_id: w.thread, provider_thread_id: w.provider, ordinal: k + 1, started_at: iso(t),
        completed_at: complete ? iso(done) : null,
        payload_json: {
          providerThreadId: w.provider, model, status: complete ? "completed" : "running",
          tokenUsage: {
            usedTokens: used, maxTokens: max, inputTokens: input, cachedInputTokens: cached, outputTokens: output,
            reasoningOutputTokens: reasoning, updatedAt: iso(done),
            ...(ttl ? { promptCacheTtlMs: ttl, promptCacheExpiresAt: iso(done + ttl) } : {}),
          },
          ...(complete ? {
            turnTokenUsage: {
              inputTokens: input * calls, cachedInputTokens: cached * calls, outputTokens: output * 2, reasoningOutputTokens: reasoning * 2,
              usageStatus: "complete", hasSubagents: Boolean(r.subs?.length),
            },
          } : {}),
        },
      })
    })
  }

  plan(r: Role, w: Where, when: number) {
    if (!r.plan) return
    const steps = r.plan.map((s) => ({
      text: s.slice(2), status: s.startsWith("✓") ? "completed" : s.startsWith("▸") ? "in_progress" : "pending",
    }))
    this.w.item(w, "todo_list", "completed", when, when, { steps })
  }

  /** The working part shared by threads and native subagents: activity, turns and plan. Returns the last activity. */
  work(r: Role, w: Where & { provider: string }, life: Life, prompts: boolean): number {
    const last = this.activity(r, w, life, prompts)
    if (r.ctx && life.status !== "queued") {
      // A turn stays open while the agent works or waits on a tool; one waiting on you has finished its turn.
      const asking = (r.tail ?? []).some(([, spec, status]) => spec.startsWith("@") && status === "pending")
      this.turns(r, w, r.model, r.max, r.ctx, life, last, life.status === "running" && !asking)
    }
    this.plan(r, w, Math.min(last, life.start + (last - life.start) * 0.8))
    return last
  }

  thread(r: Role, host?: { thread: string; run: string; node: string }) {
    const pt = `pt-${r.id}`
    const run = `run-${r.id}`
    let ordinal = 1
    if (r.before) {
      const b = r.before
      const before: Life = { status: "completed", start: this.at(b.from), ended: this.at(b.to), end: this.at(b.to), busy: false }
      const w = { thread: r.id, run: `${run}-0`, provider: `${pt}-0` }
      this.w.row(`${PREFIX}provider_threads`, { provider_thread_id: w.provider, thread_id: r.id, payload_json: { id: w.provider } })
      this.runRow(r, w, before, ordinal++, b.model)
      const last = this.activity({ ...r, tail: [] }, w, before, true)
      this.turns(r, w, b.model, b.max, b.ctx, before, last, false)
    }
    const life = this.life(r)
    const w = { thread: r.id, run, provider: pt }
    this.w.row(`${PREFIX}provider_threads`, { provider_thread_id: pt, thread_id: r.id, payload_json: { id: pt } })
    this.w.row(`${PREFIX}nodes`, { node_id: `node-${r.id}`, thread_id: r.id, parent_node_id: null, run_id: run })
    this.runRow(r, w, life, ordinal, r.model)
    const last = this.work(r, w, life, !host)
    for (const [minutes, spec, status] of r.tail ?? []) {
      if (!spec.startsWith("@") || status !== "pending" || this.at(minutes) > this.now) continue
      this.w.row(`${PREFIX}runtime_requests`, { request_id: `req-${r.id}`, thread_id: r.id, node_id: null, kind: "user_input",
        status: "pending", created_at: iso(this.at(minutes)), resolved_at: null })
    }
    this.w.row(`${PREFIX}threads`, {
      thread_id: r.id, deleted_at: null, archived_at: null,
      payload_json: {
        id: r.id, projectId: r.project, title: r.title, branch: r.branch ?? null, activeProviderThreadId: pt,
        modelSelection: { model: r.model, options: r.options ?? [] }, updatedAt: iso(last),
        ...(host ? { lineage: { relationshipToParent: "subagent", parentThreadId: host.thread } } : {}),
      },
    })
    if (host) {
      // The delegating thread records the child; the dashboard nests the child thread under it.
      this.w.row(`${PREFIX}subagents`, {
        subagent_id: `sub-${r.id}`, thread_id: host.thread,
        payload_json: {
          id: `sub-${r.id}`, threadId: host.thread, parentNodeId: host.node, runId: host.run, childThreadId: r.id,
          providerThreadId: pt, status: life.status, title: r.title, model: r.model, startedAt: iso(life.start),
          completedAt: iso(life.ended), updatedAt: iso(last),
        },
      })
    }
    for (const s of r.subs ?? []) {
      if (s.child) this.thread({ ...s, project: s.project ?? r.project }, { thread: r.id, run, node: `node-${r.id}` })
      else this.sub(s, { thread: r.id, run, node: `node-${r.id}` })
    }
  }

  private runRow(r: Role, w: Where, life: Life, ordinal: number, model: string) {
    const requested = life.start - 1500
    this.w.row(`${PREFIX}runs`, {
      run_id: w.run, thread_id: w.thread, ordinal, requested_at: iso(requested), completed_at: iso(life.ended),
      payload_json: {
        id: w.run, threadId: w.thread, status: life.status, providerThreadId: w.provider, requestedAt: iso(requested),
        startedAt: iso(life.start), completedAt: iso(life.ended), modelSelection: { model, options: r.options ?? [] },
      },
    })
  }

  /** A native subagent: it works inside its thread's run, under its own provider conversation. */
  sub(r: Role, host: { thread: string; run: string; node: string }) {
    const life = this.life(r)
    const pt = r.remote ? null : `pt-${r.id}`
    let last = life.ended ?? life.start
    if (pt) {
      this.w.row(`${PREFIX}provider_threads`, { provider_thread_id: pt, thread_id: host.thread, payload_json: { id: pt } })
      last = this.work(r, { thread: host.thread, run: host.run, provider: pt }, life, false)
    }
    this.w.row(`${PREFIX}nodes`, { node_id: `node-${r.id}`, thread_id: host.thread, parent_node_id: r.id, run_id: host.run })
    this.w.row(`${PREFIX}subagents`, {
      subagent_id: r.id, thread_id: host.thread,
      payload_json: {
        id: r.id, threadId: host.thread, parentNodeId: host.node, runId: host.run, providerThreadId: pt, status: life.status,
        title: r.title, model: r.model, startedAt: iso(life.start), completedAt: iso(life.ended), updatedAt: iso(last),
        ...(r.remote ? { remoteChild: { label: r.remote } } : {}),
      },
    })
    for (const s of r.subs ?? []) this.sub(s, { thread: host.thread, run: host.run, node: `node-${r.id}` })
  }
}

/** Write the demo database to `path` (replacing it), with times as of `now` in a cycle that began at `epoch`. */
export function buildDemo(path: string, now = Date.now(), epoch = now) {
  rmSync(path, { force: true })
  const db = new DatabaseSync(path)
  try {
    // A scratch file that is renamed into place only once complete: no journal needed.
    db.exec("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF")
    db.exec(SCHEMA)
    db.exec("BEGIN")
    const scene = new Scene(new Writer(db), now, epoch)
    for (const p of PROJECTS) scene.w.row("projection_projects", { project_id: p.id, title: p.title, workspace_root: p.root })
    for (const r of SCENARIO) scene.thread(r)
    db.exec("COMMIT")
  } finally {
    db.close()
  }
}

// ── A live demo in a temporary directory ────────────────────────────────────

export class Demo {
  readonly dir: string
  readonly path: string
  readonly epoch: number
  private timer: ReturnType<typeof setInterval> | null = null

  /** Build the database now and, with refreshMs > 0, rebuild it on that period until stop(). */
  constructor(options: { now?: number; refreshMs?: number } = {}) {
    this.epoch = options.now ?? Date.now()
    this.dir = mkdtempSync(join(tmpdir(), "t3-agents-demo-"))
    this.path = join(this.dir, "statev2.sqlite")
    process.on("exit", this.stop)
    try {
      // T3 keeps its environment id beside the database; thread links need one.
      writeFileSync(join(this.dir, "environment-id"), ENVIRONMENT + "\n")
      this.rebuild(this.epoch)
    } catch (err) {
      this.stop()
      throw err
    }
    const refresh = options.refreshMs ?? REFRESH_MS
    if (refresh > 0) {
      this.timer = setInterval(() => this.rebuild(), refresh)
      this.timer.unref() // never what keeps the process alive
    }
  }

  /** Rebuild beside the database, then rename over it, so a reader never sees a half-written file. */
  rebuild(now = Date.now()) {
    const next = this.path + ".next"
    buildDemo(next, now, this.epoch)
    renameSync(next, this.path)
  }

  /** Stop rebuilding and remove the temporary directory. Runs on exit too. */
  stop = () => {
    if (this.timer) clearInterval(this.timer)
    this.timer = null
    process.off("exit", this.stop)
    rmSync(this.dir, { recursive: true, force: true })
  }
}

// ── Usage limits ────────────────────────────────────────────────────────────

/** Made-up Omarchy usage records: one provider ahead of pace, one model near its limit, tokens growing. */
export function demoUsage(now = Date.now(), epoch = now): Provider[] {
  const at = (hours: number) => new Date(epoch + hours * 3_600_000).toISOString()
  const grown = (n: number, perMin: number) => Math.round(n + (perMin * Math.max(0, now - epoch)) / MIN)
  const records: Record<string, any>[] = [
    {
      id: "claude", name: "Claude Code", tierLabel: "Max", ready: true,
      limits: [
        { label: "Session (5-hour)", percent: 0.68, resetsAt: at(2.6) },
        { label: "Weekly (7-day)", percent: 0.41, resetsAt: at(82) },
        { label: "Weekly (7-day)", title: "Opus", percent: 0.92, resetsAt: at(82) },
      ],
      todayTokensByModel: { [OPUS]: grown(11_200_000, 42_000), [SONNET]: grown(5_900_000, 31_000), [HAIKU]: grown(1_300_000, 4_000) },
    },
    {
      id: "codex", name: "Codex", tierLabel: "Pro", ready: true,
      limits: [
        { label: "Session (5-hour)", percent: 0.23, resetsAt: at(3.85) },
        { label: "Weekly (7-day)", percent: 0.47, resetsAt: at(68) },
      ],
      todayTokensByModel: { [GPT]: grown(6_100_000, 18_000) },
    },
    {
      id: "grok", name: "Grok", tierLabel: "SuperGrok", ready: true, limits: [],
      todayTokensByModel: { [GROK]: grown(2_400_000, 22_000), [GROK_FAST]: grown(700_000, 9_000) },
    },
  ]
  for (const r of records) r.todayTotalTokens = Object.values(r.todayTokensByModel as Record<string, number>).reduce((a, b) => a + b, 0)
  return records.map((r) => parseProvider(r, now)).filter((p): p is Provider => p !== null).sort((a, b) => b.today - a.today)
}

/** Stands in for UsageReader in demo mode. */
export class DemoUsage {
  private epoch: number
  constructor(epoch = Date.now()) {
    this.epoch = epoch
  }

  poll(now = Date.now()): Provider[] {
    return demoUsage(now, this.epoch)
  }
}
