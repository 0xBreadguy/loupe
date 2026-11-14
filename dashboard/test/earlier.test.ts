// Long-running threads stay compact: under each parent the list keeps what is
// running, anyone waiting on you and the latest finishes, and folds the rest
// into one earlier row that lists them on request. Top-level threads never fold.
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import type { KeyEvent, MouseEvent } from "@opentui/core"
import { App } from "../src/app.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { EARLIER, RECENT_MAX, Source, tree, type Agent, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { agoSpan, draw, type Frame } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const NOW = Date.parse("2026-11-12T12:00:00Z")
const ago = (min: number) => new Date(NOW - min * 60_000).toISOString()

/** A bare agent that last did anything `min` minutes ago; live ones have not ended. */
function agent(id: string, parent: string | null, status: string, min: number, extra: Partial<Agent> = {}): Agent {
  const live = status === "running"
  return {
    id, parent, title: id, kind: parent ? "subagent" : "thread", project: "p", cwd: "", model: "claude-opus-5-5", status,
    run: {}, provider_id: null, context: {}, turn: {}, updated: ago(min), started: ago(min + 10), ended: live ? null : ago(min),
    remote: null, items: [], requests: [], asked: null, thread_id: parent ?? id, branch: null, options: [], history: [],
    events: [], spans: [], plan: null, mix: {}, failures: 0, now: null, ...extra,
  }
}

/** A thread with hours of subagents: one running, one waiting on you, four done in the last 3h and three older ones. */
function longThread(): Agent[] {
  return [
    agent("root", null, "running", 0),
    agent("run", "root", "running", 1),
    agent("ask", "root", "completed", 500, { requests: ["user_input"] }), // waiting on you, however old
    agent("d5", "root", "completed", 5),
    agent("d20", "root", "completed", 20),
    agent("d40", "root", "failed", 40),
    agent("d90", "root", "completed", 90), // inside the window, but a fourth finish
    agent("d200", "root", "completed", 200),
    agent("d300", "root", "failed", 300),
    agent("d400", "root", "completed", 400),
  ]
}

const recent = (expanded: string[] = [], keep: string[] = []) => ({ now: NOW, expanded: new Set(expanded), keep: new Set(keep) })
const ids = (agents: Agent[], collapsed: string[] = [], r = recent()) => tree(agents, new Set(collapsed), false, r).map((x) => x.id)

test("a parent lists what runs, who waits and its latest finishes; the rest fold into one earlier row", () => {
  const rows = tree(longThread(), new Set(), false, recent())
  assert.deepEqual(rows.map((r) => r.id), ["root", "ask", "run", "d5", "d20", "d40", EARLIER + "root"])
  const root = rows[0]
  assert.deepEqual([root.stage, root.kept, root.tucked], ["recent", 5, 4])
  const earlier = rows.at(-1)!
  assert.deepEqual(earlier.earlier!.map((a) => a.id), ["d90", "d200", "d300", "d400"], "newest first")
  assert.deepEqual([earlier.depth, earlier.last, earlier.agent.id], [1, true, "root"], "the parent's last child row")
  assert.equal(rows.at(-2)!.last, false, "the earlier row closes the tree, not the last finish")
  assert.equal(RECENT_MAX, 3)
})

test("expanded lists every subagent, collapsed none, and without `recent` nothing folds", () => {
  const agents = longThread()
  const all = ids(agents, [], recent(["root"]))
  assert.equal(all.length, 10)
  assert.ok(!all.some((id) => id.startsWith(EARLIER)))
  assert.equal(tree(agents, new Set(), false, recent(["root"]))[0].stage, "all")
  assert.deepEqual(ids(agents, ["root"]), ["root"])
  assert.equal(tree(agents, new Set(["root"]), false, recent())[0].stage, "folded")
  assert.equal(tree(agents, new Set()).length, 10, "--once and n see every agent")
})

test("a finished subagent with one of its own still running stays listed, and so does a kept one", () => {
  const agents = [...longThread(), agent("grand", "d300", "running", 1)]
  const listed = ids(agents, [], recent([], ["d400"]))
  assert.ok(listed.includes("d300") && listed.includes("grand"), "above a running subagent")
  assert.ok(listed.includes("d400"), "the selection or the thread open in T3")
  const earlier = tree(agents, new Set(), false, recent([], ["d400"])).find((r) => r.earlier)!
  assert.deepEqual(earlier.earlier!.map((a) => a.id), ["d90", "d200"])
})

test("a parent with nothing recent reads as folded, and top-level threads never fold", () => {
  const old = [agent("t", null, "completed", 300), ...[200, 250, 400].map((m) => agent(`k${m}`, "t", "completed", m))]
  const rows = tree(old, new Set(), false, recent())
  assert.deepEqual(rows.map((r) => r.id), ["t"], "one row, not a lonely earlier row")
  assert.deepEqual([rows[0].stage, rows[0].kept, rows[0].tucked], ["folded", 0, 3])
  const roots = Array.from({ length: 12 }, (_, i) => agent(`r${i}`, null, "completed", 200 + i * 60))
  assert.equal(tree(roots, new Set(), false, recent()).length, 12)
})

test("the span says how long ago, newest to oldest, in whole units", () => {
  const agents = longThread()
  assert.equal(agoSpan(agents.filter((a) => ["d90", "d200", "d300", "d400"].includes(a.id)), NOW), "1h–6h ago")
  assert.equal(agoSpan([agents[3]], NOW), "5m ago")
  assert.equal(agoSpan([], NOW), "")
})

// ── In the dashboard, on the demo's long-running thread ─────────────────────

const EPOCH = Date.parse("2026-11-12T12:00:00Z")
const AT = EPOCH + 2.5 * 60_000
const palette = derivePalette(THEMES.solitude)
const usage = demoUsage(AT, EPOCH)
const LONG = "Harden the webhook pipeline"
const FOLDED = ["Audit the retry headers", "Map the webhook senders", "Check the payload schemas", "Compare queue backends",
  "Sample production logs", "Inventory webhook endpoints"]

function demo(): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-earlier-"))
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
const press = (a: App, name: string, sequence = name) => a.key({ name, sequence } as KeyEvent)
function frame(a: App, w = 150, h = 70): Frame {
  a.canvas.resize(w, h)
  a.frame = draw(a.canvas, a.st, a.snap, usage, palette, AT)
  return a.frame
}
const titles = (f: Frame, parent: Agent) => f.rows.filter((r) => r.agent.parent === parent.id).map((r) => r.agent.title)
const line = (a: App, f: Frame, id: string) => a.canvas.toText().split("\n")[f.hits.find((h) => h.id === id && h.area === "list")!.y]

test("the long thread shows what runs and its latest three, then an earlier row with a span and its failures", () => {
  const a = app(demo())
  const long = a.snap.agents.find((x) => x.title === LONG)!
  const f = frame(a)
  assert.deepEqual(titles(f, long), ["Replay the dead-letter queue", "Review the signature check", "Add idempotency keys",
    "Load-test the receiver"])
  const row = line(a, f, EARLIER + long.id)
  assert.match(row, /└─┄ 6 earlier ▸ · 2h–4h ago · 1 ✗/)
  assert.doesNotMatch(row, /show all/, "the hint waits for the selection")
  assert.ok(FOLDED.every((t) => !a.canvas.toText().includes(t)), "folded away")
  a.st.selected = EARLIER + long.id
  frame(a)
  const text = a.canvas.toText()
  assert.match(line(a, a.frame!, EARLIER + long.id), /⏎ show all │/, "a hint at the end of the row")
  assert.match(text, /6 earlier subagents · last active 2h–4h ago/)
  assert.match(text, /EARLIER ─+ {2}newest first · 1 ✗/)
  const order = FOLDED.map((t) => text.indexOf(t))
  assert.ok(order.every((i) => i > 0) && order.every((i, k) => !k || i > order[k - 1]), "every one in the details, newest first")
})

test("→ and ← step through folded, recent and all; Space folds and unfolds to recent", () => {
  const a = app(demo())
  const long = a.snap.agents.find((x) => x.title === LONG)!
  frame(a)
  a.st.selected = long.id
  const count = () => titles(frame(a), long).length
  assert.equal(count(), 4)
  press(a, "right")
  assert.equal(count(), 10, "all of them")
  assert.ok(!frame(a).rows.some((r) => r.earlier && r.agent.id === long.id))
  press(a, "right")
  assert.equal(count(), 10, "nothing further")
  press(a, "left")
  assert.equal(count(), 4, "back to the recent ones")
  press(a, "left")
  assert.equal(count(), 0, "folded")
  assert.match(line(a, frame(a), long.id), /Harden the webhook pipeline ▸/)
  press(a, "left")
  assert.equal(a.st.selected, long.id, "a top-level thread has no parent to go to")
  press(a, "space")
  assert.equal(count(), 4, "Space unfolds to the recent ones")
  press(a, "space")
  assert.equal(count(), 0)
})

test("Enter, → or a click on the earlier row lists them all and selects the newest; ← goes to the parent", () => {
  for (const act of ["return", "right", "space", "click"]) {
    const a = app(demo())
    const long = a.snap.agents.find((x) => x.title === LONG)!
    const f = frame(a)
    a.st.selected = EARLIER + long.id
    if (act === "click") {
      const hit = f.hits.find((h) => h.id === EARLIER + long.id)!
      a.click({ x: hit.x0 + 4, y: hit.y } as MouseEvent)
    } else press(a, act)
    assert.equal(titles(frame(a), long).length, 10, act)
    assert.equal(a.snap.agents.find((x) => x.id === a.st.selected)?.title, FOLDED[0], `${act}: the newest folded one`)
  }
  const a = app(demo())
  const long = a.snap.agents.find((x) => x.title === LONG)!
  frame(a)
  a.st.selected = EARLIER + long.id
  press(a, "left")
  assert.equal(a.st.selected, long.id)
})

test("a parent with nothing to fold away unfolds to its plain state, so later finishes still fold", () => {
  const a = app(demo())
  const billing = a.snap.agents.find((x) => x.title === "Migrate billing to the v2 API")!
  a.st.collapsed.add(billing.id)
  frame(a)
  a.st.selected = billing.id
  press(a, "right")
  assert.equal(titles(frame(a), billing).length, 2)
  assert.ok(!a.st.expanded.has(billing.id) && !a.st.collapsed.has(billing.id))
})

test("following T3 to the long thread unfolds it to the recent stage, not all of it", () => {
  const a = app(demo())
  const long = a.snap.agents.find((x) => x.title === LONG)!
  a.st.collapsed.add(long.id)
  a.st.t3Title = `${long.project} / ${long.title}`
  const f = frame(a)
  assert.equal(a.st.selected, long.id)
  assert.equal(titles(f, long).length, 4)
  assert.ok(f.rows.some((r) => r.id === EARLIER + long.id))
  assert.ok(f.linked.has(long.id))
  assert.equal(a.canvas.toText().split("\n")[f.hits.find((h) => h.id === EARLIER + long.id)!.y][0], "▎", "the earlier row is in the band")
})

test("a folded subagent picked in T3 or selected stays listed under its parent", () => {
  const a = app(demo())
  const long = a.snap.agents.find((x) => x.title === LONG)!
  const audit = a.snap.agents.find((x) => x.title === FOLDED[0])!
  a.st.selected = audit.id
  const f = frame(a)
  assert.equal(a.st.selected, audit.id, "the selection did not jump away")
  assert.ok(titles(f, long).includes(FOLDED[0]))
  assert.match(line(a, f, EARLIER + long.id), /5 earlier/)
})
