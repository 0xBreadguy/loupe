// Demo mode: a made-up T3 database in a temporary directory that Source reads
// like the real one, made-up usage, and the CLI and app wiring around them.
import assert from "node:assert/strict"
import { spawn, spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { basename, dirname, join } from "node:path"
import { test } from "node:test"
import { fileURLToPath } from "node:url"
import type { KeyEvent } from "@opentui/core"
import { App } from "../src/app.ts"
import { CYCLE_MS, Demo, DemoUsage, buildDemo, demoUsage } from "../src/demo.ts"
import { environmentId } from "../src/focus.ts"
import { SLOW_TOOL_MS, Source, isLive, needsYou, pace, type Agent, type Pace } from "../src/source.ts"
import { family } from "../src/theme.ts"

const MIN = 60_000
const MAIN = fileURLToPath(new URL("../src/main.ts", import.meta.url))

/** Point TMPDIR at a fresh directory for the duration, so everything the demo writes can be inspected. */
function sandbox(fn: (tmp: string) => void | Promise<void>) {
  return async () => {
    const tmp = mkdtempSync(join(tmpdir(), "t3a-demo-test-"))
    const saved = process.env.TMPDIR
    process.env.TMPDIR = tmp
    try {
      await fn(tmp)
    } finally {
      if (saved === undefined) delete process.env.TMPDIR
      else process.env.TMPDIR = saved
      rmSync(tmp, { recursive: true, force: true })
    }
  }
}

/** Pace per running agent the way the view works it out: a parent with live subagents waits on them. */
function paces(agents: Agent[], now: number): Map<string, Pace> {
  const parents = new Set(agents.filter(isLive).map((a) => a.parent).filter((p): p is string => p !== null))
  const out = new Map<string, Pace>()
  for (const a of agents) {
    const pc = pace(a, now, parents.has(a.id))
    if (pc) out.set(a.id, pc)
  }
  return out
}

const ratio = (a: Agent) => (a.context.usedTokens ?? 0) / (a.context.maxTokens ?? Infinity)

test("the demo database builds in a temp dir and Source reads it into the expected tree", sandbox((tmp) => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const demo = new Demo({ now, refreshMs: 0 })
  try {
    assert.equal(dirname(demo.dir), tmp)
    const { agents } = new Source(demo.path).snapshot(now)
    const title = new Map(agents.map((a) => [a.id, a.title]))
    const shape = Object.fromEntries(agents.map((a) => [a.title, a.parent ? title.get(a.parent) : null]))
    assert.deepEqual(shape, {
      "Migrate billing to the v2 API": null,
      "Map callers of the legacy client": "Migrate billing to the v2 API",
      "Check rate-limit headers": "Map callers of the legacy client",
      "Write migration tests": "Migrate billing to the v2 API",
      "Fix the flaky checkout tests": null,
      "Bisect the checkout regression": "Fix the flaky checkout tests",
      "Plan the search index rebuild": null,
      "Survey index sizes": "Plan the search index rebuild",
      "Add streaming export to the CLI": null,
      "Profile the CSV writer": "Add streaming export to the CLI",
      "Run the integration suite": "Add streaming export to the CLI",
      "Refresh the onboarding guide": null,
      "Tune the retry backoff": null,
      "Upgrade the image pipeline": null,
    })
    const bisect = agents.find((a) => a.title === "Bisect the checkout regression")!
    assert.equal(bisect.kind, "subagent", "a delegated app thread nests under the thread that started it")
    assert.equal(bisect.id, bisect.thread_id)
    assert.deepEqual(new Set(agents.map((a) => a.project)), new Set(["atlas-api", "storefront-web", "pipeline-cli", "handbook"]))
    assert.deepEqual(new Set(agents.map((a) => family(a.model))), new Set(["claude", "openai", "grok"]))
    assert.equal(environmentId(demo.path), readFileSync(join(demo.dir, "environment-id"), "utf8").trim())
  } finally {
    demo.stop()
  }
}))

test("the scenario keeps every state on screen through its whole cycle", sandbox((tmp) => {
  const epoch = Date.parse("2026-11-12T12:00:00Z")
  const path = join(tmp, "demo.sqlite")
  for (const minutes of [0, 2.5, 5, 9, 14, 19.9, 20, 31]) {
    const now = epoch + minutes * MIN
    buildDemo(path, now, epoch)
    const { agents } = new Source(path).snapshot(now)
    const pcs = paces(agents, now)
    const at = `${minutes} minutes in`
    const byTitle = (t: string) => agents.find((a) => a.title === t)!

    const waiting = agents.filter(needsYou)
    assert.deepEqual(waiting.map((a) => [a.title, a.requests]), [["Plan the search index rebuild", ["user_input"]]], at)
    const stalled = agents.filter((a) => pcs.get(a.id)?.quiet && pcs.get(a.id)?.stalled)
    assert.deepEqual(stalled.map((a) => a.title), ["Write migration tests"], `one quiet, stalled agent ${at}`)
    const slow = agents.filter((a) => {
      const pc = pcs.get(a.id)
      return pc?.tool && !pc.delegating && pc.toolMs >= SLOW_TOOL_MS && pc.stalled
    })
    assert.deepEqual(slow.map((a) => [a.title, a.now?.type]), [["Fix the flaky checkout tests", "command_execution"]], at)
    assert.ok(!pcs.get(byTitle("Profile the CSV writer").id)?.stalled, `the bursty agent never stalls ${at}`)
    const fails = agents.flatMap((a) => a.events).filter((e) => e.failed && e.t >= now - 30 * MIN)
    assert.ok(fails.length > 0, `failed tool calls in the lane window ${at}`)
    assert.ok(agents.some((a) => isLive(a) && !needsYou(a) && ratio(a) >= 0.9), `a running agent past 90% ${at}`)
    assert.ok(agents.some((a) => ratio(a) >= 0.75 && ratio(a) < 0.9), `an agent past 75% ${at}`)
    assert.ok(agents.every((a) => ratio(a) <= 1), at)
    for (const status of ["completed", "interrupted", "failed"]) assert.ok(agents.some((a) => a.status === status), `${status} ${at}`)
    assert.equal(agents.filter((a) => isLive(a) && !needsYou(a)).length >= 6, true, at)
  }

  // At the start of a cycle: a quiet agent that has not stalled yet, and recent failures.
  buildDemo(path, epoch, epoch)
  const { agents } = new Source(path).snapshot(epoch)
  const pcs = paces(agents, epoch)
  const bench = pcs.get(agents.find((a) => a.title === "Profile the CSV writer")!.id)!
  assert.ok(bench.quiet && !bench.stalled)
  assert.ok(agents.flatMap((a) => a.events).some((e) => e.failed && e.t >= epoch - 10 * MIN))
  assert.ok(agents.some((a) => a.status === "queued"))
  // Detail panels: a plan, context growth with a provider handoff, token breakdowns and prompt caches.
  const billing = agents.find((a) => a.title === "Migrate billing to the v2 API")!
  assert.ok(billing.plan && billing.plan.some((s) => s.status === "in_progress"))
  assert.ok(billing.history.length >= 8)
  assert.ok(billing.turn.tokenUsage.cachedInputTokens > 0)
  assert.ok(Date.parse(billing.context.promptCacheExpiresAt!) > epoch)
  const search = agents.find((a) => a.title === "Plan the search index rebuild")!
  assert.equal(search.turn.turnTokenUsage?.hasSubagents, true)
  const handoff = agents.find((a) => a.title === "Add streaming export to the CLI")!
  assert.deepEqual(new Set(handoff.history.map((p) => family(p.model))), new Set(["openai", "grok"]))
  assert.ok(handoff.spans.length >= 2, "two runs in the lanes")
  assert.ok(agents.find((a) => a.title === "Run the integration suite")!.remote)
}))

test("demo usage: limits with a pace, one near its limit, tokens today by model", () => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const usage = demoUsage(now)
  assert.deepEqual(usage.map((u) => u.id), ["claude", "codex", "grok"])
  const limits = usage.flatMap((u) => u.limits)
  assert.ok(limits.every((l) => l.elapsed !== null && l.resetsAt! > now))
  assert.ok(limits.some((l) => l.percent >= 0.9), "near a limit")
  assert.ok(limits.some((l) => l.percent > l.elapsed! + 0.08), "ahead of pace")
  for (const u of usage) {
    assert.equal(u.today, u.models.reduce((n, m) => n + m.tokens, 0))
    assert.deepEqual(u.models, [...u.models].sort((a, b) => b.tokens - a.tokens))
  }
  const later = demoUsage(now + 10 * MIN, now)
  assert.ok(later[0].today > usage[0].today, "tokens keep coming in")
  assert.deepEqual(new DemoUsage(now).poll(now), usage)
})

test("the demo writes only inside its temp dir, rebuilds in place and removes it when stopped", sandbox((tmp) => {
  const listeners = process.listenerCount("exit")
  const now = Date.parse("2026-11-12T12:00:00Z")
  const demo = new Demo({ now, refreshMs: 0 })
  assert.deepEqual(readdirSync(tmp), [basename(demo.dir)])
  assert.deepEqual(readdirSync(demo.dir).sort(), ["environment-id", "statev2.sqlite"])
  assert.equal(process.listenerCount("exit"), listeners + 1, "cleans up on exit")
  const source = new Source(demo.path)
  const before = source.snapshot(now).agents.find((a) => a.title === "Migrate billing to the v2 API")!
  demo.rebuild(now + 2 * MIN)
  assert.deepEqual(readdirSync(demo.dir).sort(), ["environment-id", "statev2.sqlite"], "no partial files left behind")
  const after = source.snapshot(now + 2 * MIN).agents.find((a) => a.title === "Migrate billing to the v2 API")!
  assert.ok(after.updated! > before.updated!, "times move on with each rebuild")
  demo.stop()
  assert.deepEqual(readdirSync(tmp), [])
  assert.equal(process.listenerCount("exit"), listeners)
  demo.stop() // twice is harmless
}))

test("the timer keeps the demo database fresh", sandbox(async () => {
  const demo = new Demo({ refreshMs: 20 })
  try {
    const first = readFileSync(demo.path)
    await new Promise((resolve) => setTimeout(resolve, 120))
    assert.notDeepEqual(readFileSync(demo.path), first)
  } finally {
    demo.stop()
  }
}))

test("in demo mode the app uses the given usage and only says what it would open", sandbox(() => {
  const now = Date.now()
  const demo = new Demo({ now, refreshMs: 0 })
  try {
    const app = new App(new Source(demo.path), 2, { usage: new DemoUsage(demo.epoch), demo: true })
    assert.equal(app.t3, null, "no T3 window link")
    assert.equal(app.st.t3Link, false)
    assert.deepEqual(app.usage.poll(now).map((u) => u.id), ["claude", "codex", "grok"])
    app.st.selected = app.snap.agents.find((a) => a.title === "Survey index sizes")!.id
    app.key({ name: "return", sequence: "\r" } as KeyEvent)
    assert.equal(app.st.notice?.text, "Demo: would open Plan the search index rebuild in T3 Code")
    assert.equal(app.st.notice?.error, false)
  } finally {
    demo.stop()
  }
}))

test("--once --demo and --status --demo work without any T3 or Omarchy data and clean up", sandbox((tmp) => {
  // An empty home and temp dir: the CLI can only use what it makes itself.
  const home = join(tmp, "home")
  const temp = join(tmp, "temp")
  mkdirSync(home)
  mkdirSync(temp)
  const env = { ...process.env, HOME: home, TMPDIR: temp, T3CODE_HOME: join(home, ".t3"), XDG_STATE_HOME: join(home, ".local/state") }
  const run = (...args: string[]) => spawnSync(process.execPath, ["--disable-warning=ExperimentalWarning", MAIN, ...args],
    { env, encoding: "utf8", timeout: 20_000 })

  const once = run("--once", "--demo")
  assert.equal(once.status, 0, once.stderr)
  assert.match(once.stdout, /^T3 AGENTS · demo data · .* 1 waiting · 14 agents/)
  assert.match(once.stdout, /◆ Plan the search index rebuild/)

  const status = run("--status", "--demo")
  assert.equal(status.status, 0, status.stderr)
  const json = JSON.parse(status.stdout)
  assert.equal(json.waiting, 1)
  assert.ok(json.running >= 6 && json.failed >= 1)
  assert.equal(json.error, undefined)

  assert.deepEqual(readdirSync(temp), [], "the temporary directory is gone")
  assert.deepEqual(readdirSync(home), [], "nothing written to the home directory")

  const both = run("--demo", "--db", "/nonexistent.sqlite")
  assert.equal(both.status, 2)
  assert.match(both.stderr, /cannot be used together/)
}))

test("a watching demo removes its temp dir when it is terminated", sandbox(async (tmp) => {
  const temp = join(tmp, "temp")
  mkdirSync(temp)
  const child = spawn(process.execPath, ["--disable-warning=ExperimentalWarning", MAIN, "--status", "--watch", "--demo"],
    { env: { ...process.env, TMPDIR: temp }, stdio: ["ignore", "pipe", "inherit"] })
  const first = await new Promise<string>((resolve, reject) => {
    child.stdout.once("data", (d: Buffer) => resolve(d.toString()))
    child.once("exit", () => reject(new Error("exited before printing")))
  })
  assert.equal(JSON.parse(first).waiting, 1)
  assert.equal(readdirSync(temp).length, 1)
  const exited = new Promise<number | null>((resolve) => child.once("exit", (code) => resolve(code)))
  child.kill("SIGTERM")
  assert.equal(await exited, 143)
  assert.deepEqual(readdirSync(temp), [])
}))
