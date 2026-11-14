// a steps the list through three scopes: every unarchived thread, only what is
// active, and what T3 Code's sidebar lists (threads not settled or snoozed).
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import type { KeyEvent } from "@opentui/core"
import { App } from "../src/app.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, inSidebar, tree, type Agent, type Shelf, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { draw, type Frame } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const NOW = Date.parse("2026-11-12T12:00:00Z")
const ago = (min: number) => new Date(NOW - min * 60_000).toISOString()
const open: Shelf = { listed: true, settled: false, snoozedUntil: null }

function agent(id: string, parent: string | null, status: string, extra: Partial<Agent> = {}): Agent {
  return {
    id, parent, title: id, kind: parent ? "subagent" : "thread", project: "p", cwd: "", model: "claude-opus-5-5", status,
    run: {}, provider_id: null, context: {}, turn: {}, updated: ago(5), started: ago(15), ended: null, remote: null,
    shelf: parent ? undefined : open, items: [], recentFailures: [], requests: [], asked: null, thread_id: parent ?? id,
    branch: null, options: [], history: [], events: [], spans: [], plan: null, mix: {}, failures: 0, now: null, ...extra,
  }
}

/** Threads in every state T3's sidebar distinguishes. */
function threads(): Agent[] {
  return [
    agent("open", null, "completed"),
    agent("open-sub", "open", "completed"),
    agent("settled", null, "completed", { shelf: { ...open, settled: true } }),
    agent("snoozed", null, "completed", { shelf: { ...open, snoozedUntil: ago(-60) } }),
    agent("woke", null, "completed", { shelf: { ...open, snoozedUntil: ago(1) } }), // the snooze ran out
    agent("asks", null, "completed", { shelf: { ...open, snoozedUntil: ago(-60) }, requests: ["user_input"] }),
    agent("orphan", null, "completed", { shelf: { ...open, listed: false } }), // a subagent thread whose parent is archived
    agent("parked", null, "completed", { shelf: { ...open, settled: true } }),
    agent("parked-live", "parked", "running"),
    agent("parked-done", "parked", "completed"),
    agent("running", null, "running"),
  ]
}
const ids = (agents: Agent[], scope: "all" | "active" | "t3", linked: string | null = null) =>
  tree(agents, new Set(), scope, { now: NOW, expanded: new Set(), linked }).map((r) => r.id).sort()

test("the T3 scope lists what T3's sidebar lists, plus anything active", () => {
  const agents = threads()
  assert.deepEqual(ids(agents, "all"), agents.map((a) => a.id).sort(), "all: every unarchived thread")
  assert.deepEqual(ids(agents, "active"), ["asks", "parked", "parked-live", "running"], "active: live or waiting, with their parents")
  assert.deepEqual(ids(agents, "t3"), ["asks", "open", "open-sub", "parked", "parked-live", "running", "woke"],
    "settled, snoozed and unlisted threads leave; an open thread's subagents come with it; active work stays")
})

test("the thread open in T3 stays in the T3 scope, settled or not, with its family", () => {
  const agents = threads()
  assert.ok(ids(agents, "t3", "settled").includes("settled"))
  assert.ok(ids(agents, "t3", "parked-live").includes("parked-done"), "a subagent open in T3 brings its whole thread")
  assert.ok(!ids(agents, "active", "settled").includes("settled"), "active only stays active only")
})

test("a snoozed thread is back early once it needs you, as T3 wakes it", () => {
  const snoozed = agent("t", null, "completed", { shelf: { ...open, snoozedUntil: ago(-60) } })
  assert.equal(inSidebar(snoozed, NOW), false)
  assert.equal(inSidebar({ ...snoozed, requests: ["approval"] }, NOW), true)
  assert.equal(inSidebar({ ...snoozed, shelf: { ...open, snoozedUntil: "not a date" } }, NOW), true, "an unreadable wake time hides nothing")
  assert.equal(inSidebar(agent("sub", "t", "completed"), NOW), true, "a native subagent follows its parent")
})

// ── The demo, from T3's tables ──────────────────────────────────────────────

const EPOCH = Date.parse("2026-11-12T12:00:00Z")
const AT = EPOCH + 2.5 * 60_000
const palette = derivePalette(THEMES.solitude)
const usage = demoUsage(AT, EPOCH)

function demo(): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-scope-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), AT, EPOCH)
    return new Source(join(dir, "statev2.sqlite")).snapshot(AT)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

function app(snap: Snapshot) {
  const source = { path: "", snapshot: () => snap } as unknown as Source
  const it = new App(source, 2, { usage: { poll: () => usage }, demo: true, calm: true })
  it.st.bootAt = 0
  return it
}
const press = (a: App, seq: string) => a.key({ name: seq, sequence: seq } as KeyEvent)
function frame(a: App, w = 150, h = 70): Frame {
  a.canvas.resize(w, h)
  a.frame = draw(a.canvas, a.st, a.snap, usage, palette, AT)
  return a.frame
}
const roots = (f: Frame) => f.rows.filter((r) => r.depth === 0).map((r) => r.agent.title)

test("the source reads where T3's sidebar files each thread", () => {
  const snap = demo()
  const by = (title: string) => snap.agents.find((a) => a.title === title)!
  assert.equal(by("Refresh the onboarding guide").shelf?.settled, true)
  assert.ok(Date.parse(by("Upgrade the image pipeline").shelf!.snoozedUntil!) > AT)
  assert.deepEqual(by("Migrate billing to the v2 API").shelf, open)
  assert.equal(by("Bisect the checkout regression").shelf?.listed, false, "a delegated thread lives inside its parent")
  assert.equal(snap.agents.find((a) => a.kind === "subagent" && a.thread_id !== a.id)?.shelf, undefined, "native subagents have none")
})

test("a steps all, active only, T3 sidebar and back, and says which", () => {
  const a = app(demo())
  let f = frame(a)
  const all = roots(f)
  assert.ok(all.includes("Refresh the onboarding guide") && all.includes("Upgrade the image pipeline"))
  assert.match(a.canvas.toText().split("\n").at(-1)!, /a all threads/)

  press(a, "a")
  f = frame(a)
  assert.equal(a.st.scope, "active")
  assert.ok(f.rows.every((r) => r.earlier || r.agent.status !== "completed" || f.rows.some((k) => k.agent.parent === r.agent.id)),
    "only live agents and the threads above them")
  assert.match(a.canvas.toText().split("\n")[0], /active only · lane/)
  assert.match(a.canvas.toText().split("\n").at(-1)!, /Active only: what is running or waiting on you/)

  press(a, "a")
  f = frame(a)
  assert.equal(a.st.scope, "t3")
  assert.deepEqual(roots(f).sort(), all.filter((t) => t !== "Refresh the onboarding guide" && t !== "Upgrade the image pipeline").sort(),
    "the settled and the snoozed thread leave; the failed one stays, since T3 still lists it")
  assert.match(a.canvas.toText().split("\n")[0], /T3 sidebar · lane/)
  assert.match(a.canvas.toText().split("\n").at(-1)!, /T3 sidebar: threads not settled or snoozed/)

  press(a, "a")
  f = frame(a)
  assert.equal(a.st.scope, "all")
  assert.deepEqual(roots(f), all)
  assert.doesNotMatch(a.canvas.toText().split("\n")[0], /active only|T3 sidebar/)
})

test("an empty T3 scope says why and how to see the rest", () => {
  const snap = demo()
  for (const x of snap.agents) {
    if (x.shelf) x.shelf.settled = true
    x.status = "completed"
    x.requests = []
  }
  const a = app(snap)
  a.st.scope = "t3"
  frame(a)
  assert.match(a.canvas.toText(), /Nothing in T3's sidebar: every thread is settled or snoozed\. Press a for all\./)
})
