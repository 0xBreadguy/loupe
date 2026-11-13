// "Needs you" one key away: n and the NEEDS YOU tile jump to whoever waits,
// the tile says what for and how long, and the blink stops once it is seen.
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import type { KeyEvent, MouseEvent } from "@opentui/core"
import { App, FPS, frameRate } from "../src/app.ts"
import { Canvas } from "../src/canvas.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, needsYou, waitingSince, type Agent, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { NEEDS_YOU_TILE, draw, initialState, nextWaiting, pick, type Frame, type ViewState } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const EPOCH = Date.parse("2026-11-12T12:00:00Z")
const NOW = EPOCH + 2.5 * 60_000 // a whole second: the blink's on phase; +600 ms is its off phase
const palette = derivePalette(THEMES.solitude)
const usage = demoUsage(NOW, EPOCH)

function demo(now = NOW): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-needs-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), now, EPOCH)
    return new Source(join(dir, "statev2.sqlite")).snapshot(now)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

const byTitle = (snap: Snapshot, title: string): Agent => snap.agents.find((a) => a.title === title)!
const PLAN = "Plan the search index rebuild" // the demo agent waiting on you
const HEADERS = "Check rate-limit headers" // nested two levels down, under "Map callers of the legacy client"

/** The demo with a second agent waiting, deep in the tree. */
function twoWaiting(): Snapshot {
  const snap = structuredClone(demo())
  const a = byTitle(snap, HEADERS)
  a.requests = ["command"]
  a.status = "waiting"
  return snap
}

/** Only the waiting agents are left doing anything: nothing else moves. */
function onlyWaiting(snap: Snapshot): Snapshot {
  const still = structuredClone(snap)
  for (const a of still.agents) {
    if (needsYou(a)) continue
    a.status = "completed"
    a.items = a.items.map((i) => ({ ...i, status: "completed" }))
    a.now = null
  }
  return still
}

function app(snap: Snapshot) {
  const source = { path: "", snapshot: () => snap } as unknown as Source
  const it = new App(source, 2, { usage: { poll: () => usage }, demo: true })
  it.st.bootAt = 0
  return it
}
const key = (a: App, sequence: string) => a.key({ name: sequence, sequence } as KeyEvent)
function frame(a: App, at = NOW, w = 150, h = 50): Frame {
  a.canvas.resize(w, h)
  a.frame = draw(a.canvas, a.st, a.snap, usage, palette, at)
  return a.frame
}

/** The status glyph on an agent's row. */
function mark(c: Canvas, f: Frame, a: Agent): string {
  const y = f.hits.find((h) => h.id === a.id && h.area === "list")!.y
  const line = c.toText().split("\n")[y]
  return [...line].find((ch) => ch === "◆" || ch === "◇")!
}

test("n selects the next agent waiting on you, unfolding its parents and wrapping around", () => {
  const snap = twoWaiting()
  const a = app(snap)
  const headers = byTitle(snap, HEADERS)
  a.st.collapsed.add(byTitle(snap, "Migrate billing to the v2 API").id)
  a.st.collapsed.add(byTitle(snap, "Map callers of the legacy client").id)
  frame(a)
  a.st.selected = byTitle(snap, "Fix the flaky checkout tests").id
  key(a, "n")
  assert.equal(a.st.selected, byTitle(snap, PLAN).id, "wraps to the top")
  key(a, "n")
  assert.equal(a.st.selected, headers.id)
  assert.equal(a.st.collapsed.size, 0, "its parents unfolded")
  assert.ok(frame(a).rows.some((r) => r.agent.id === headers.id), "so its row is listed")
  key(a, "n")
  assert.equal(a.st.selected, byTitle(snap, PLAN).id, "and around again")
})

test("n with nobody waiting says so", () => {
  const snap = structuredClone(demo())
  for (const x of snap.agents) {
    x.requests = []
    if (x.status === "waiting") x.status = "running"
  }
  const a = app(snap)
  frame(a)
  const before = a.st.selected
  key(a, "n")
  assert.equal(a.st.selected, before)
  assert.match(a.st.notice!.text, /Nobody is waiting on you/)
})

test("clicking the NEEDS YOU tile does what n does", () => {
  const snap = twoWaiting()
  const a = app(snap)
  const f = frame(a)
  a.st.selected = byTitle(snap, PLAN).id
  const tile = f.hits.find((h) => h.area === "tile" && h.id === NEEDS_YOU_TILE)!
  assert.ok(tile, "the tile is a click target")
  a.click({ x: tile.x0 + 3, y: tile.y + 1 } as MouseEvent)
  assert.equal(a.st.selected, byTitle(snap, HEADERS).id)
})

test("the NEEDS YOU tile says what is needed, for how long, and how many more", () => {
  const snap = demo()
  const plan = byTitle(snap, PLAN)
  const waited = Math.round((NOW - waitingSince(plan)!) / 60_000)
  assert.ok(plan.asked, "the request's own time, from T3's runtime_requests")
  const st = initialState(2)
  st.bootAt = 0
  const c = new Canvas(150, 50)
  draw(c, st, snap, usage, palette, NOW)
  const sub = (text: string) => text.split("\n").find((l) => l.includes("▌ ◆ "))!
  assert.match(sub(c.toText()), new RegExp(`◆ Which index lay… ${waited}m `), "the request's label and its wait")
  const two = twoWaiting()
  const st2 = initialState(2)
  st2.bootAt = 0
  draw(c, st2, two, usage, palette, NOW)
  assert.match(sub(c.toText()), /◆ \S.*… \d+m \+1 more/)
})

test("the blink stops once you have seen the wait, and starts again for a new one", () => {
  const snap = onlyWaiting(demo())
  const plan = byTitle(snap, PLAN)
  const a = app(snap)
  a.st.calm = true // context bars jump to their values, so no tween raises the frame rate
  frame(a, NOW - 1000)
  a.st.calm = false
  // Being the default selection is not seeing it.
  let f = frame(a)
  assert.equal(a.st.selected, plan.id)
  assert.equal(mark(a.canvas, f, plan), "◆")
  f = frame(a, NOW + 600)
  assert.equal(mark(a.canvas, f, plan), "◇", "blinking")
  assert.equal(f.beat, true)
  assert.equal(frameRate(f, a.st), FPS.beat, "10 fps for the blink")
  // Picking it after it began waiting is.
  pick(a.st, plan.id, NOW + 700)
  f = frame(a, NOW + 1600)
  assert.equal(mark(a.canvas, f, plan), "◆", "a steady ◆")
  assert.equal(f.beat, false)
  assert.equal(frameRate(f, a.st), FPS.idle, "nothing left blinking: back to idle")
  const edge = (fr: Frame) => a.canvas.fg[fr.hits.find((h) => h.area === "tile")!.y * a.canvas.w + fr.hits.find((h) => h.area === "tile")!.x0]
  assert.equal(edge(f), palette.hot, "the tile's edge holds steady too")
  // Another agent starts waiting: it blinks, and the tile with it.
  const next = structuredClone(snap)
  const other = byTitle(next, "Refresh the onboarding guide")
  other.requests = ["user_input"]
  a.snap = next
  f = frame(a, NOW + 2600)
  assert.equal(mark(a.canvas, f, plan), "◆", "the seen one stays steady")
  assert.equal(mark(a.canvas, f, other), "◇", "the new one blinks")
  assert.equal(f.beat, true)
  assert.notEqual(edge(f), palette.hot, "the tile blinks again")
  // Once a wait ends, the next wait by the same agent blinks again.
  const after = structuredClone(next)
  byTitle(after, PLAN).requests = []
  byTitle(after, PLAN).status = "completed"
  a.snap = after
  frame(a, NOW + 3000)
  a.snap = next
  f = frame(a, NOW + 3600)
  assert.equal(mark(a.canvas, f, plan), "◇")
})

test("a selection made before the wait began does not count; the thread open in T3 does", () => {
  const before = onlyWaiting(demo())
  const plan = byTitle(before, PLAN)
  const quiet = structuredClone(before)
  byTitle(quiet, PLAN).requests = []
  byTitle(quiet, PLAN).status = "running"
  const st: ViewState = initialState(2)
  st.bootAt = 0
  const c = new Canvas(150, 50)
  pick(st, plan.id, NOW - 5000) // selected while it was still working
  draw(c, st, quiet, usage, palette, NOW - 4000)
  const f = draw(c, st, before, usage, palette, NOW + 600)
  assert.equal(mark(c, f, plan), "◇", "still blinking: the pick came first")
  st.t3Title = `${plan.project} / ${plan.title}` // the user opened it in T3
  const g = draw(c, st, before, usage, palette, NOW + 1600)
  assert.equal(mark(c, g, plan), "◆")
  assert.equal(g.beat, false)
})

test("calm mode: the needs-you mark and the tile never blink", () => {
  const snap = onlyWaiting(demo())
  const plan = byTitle(snap, PLAN)
  const a = app(snap)
  a.st.calm = true
  const f = frame(a, NOW + 600)
  assert.equal(mark(a.canvas, f, plan), "◆")
  assert.equal(f.beat, false)
})

test("a pending request in the activity feed is a hot ◆", () => {
  const snap = demo()
  const a = app(snap)
  a.st.selected = byTitle(snap, PLAN).id
  const f = frame(a)
  const lines = a.canvas.toText().split("\n")
  const y = lines.findIndex((l, i) => i > f.detail.y && l.slice(f.detail.x).startsWith("◆ ") && l.includes("Which index layout"))
  assert.ok(y > 0, "the request is in the feed")
  const at = y * a.canvas.w + f.detail.x
  assert.equal(a.canvas.fg[at], palette.hot)
})
