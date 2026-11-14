// How long a run took, and when: the list's compact run length, the detail
// pane's clock times, and the selected run's ticks on the lane axis.
process.env.TZ = "UTC" // clock times below are read on a known wall clock

import assert from "node:assert/strict"
import { test } from "node:test"
import { Canvas, textWidth } from "../src/canvas.ts"
import { clock, span, when } from "../src/format.ts"
import { buildAgents, type Agent } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { draw, initialState, placeMarks, runLength, runMarks } from "../src/view.ts"

const NOW = Date.parse("2026-11-12T14:30:00Z")
const MIN = 60_000

test("run lengths fit five cells at about two significant figures", () => {
  const cases: [number, string][] = [
    [0, "0s"], [36, "36s"], [59.9, "59s"], [60, "1m00s"], [392, "6m32s"], [599, "9m59s"], [600, "10m"], [1620, "27m"],
    [3600, "1h00m"], [17_400, "4h50m"], [35_999, "9h59m"], [36_000, "10h"], [86_400 + 5 * 3600, "1d5h"], [12 * 86_400, "12d"],
  ]
  for (const [sec, text] of cases) assert.equal(span(sec), text, `${sec}s`)
  assert.equal(span(null), "--")
  assert.equal(span(-5), "0s")
  for (let sec = 0; sec < 400 * 86_400; sec = sec * 1.07 + 1) assert.ok(span(sec).length <= 5, span(sec))
})

test("clock times are dated only off the reference day", () => {
  assert.equal(clock(Date.parse("2026-11-12T14:02:40Z"), NOW), "14:02")
  assert.equal(clock(Date.parse("2026-11-11T09:05:00Z"), NOW), "Nov 11 09:05")
  assert.equal(when(Date.parse("2026-11-12T13:51:00Z"), null, NOW), "since 13:51")
  assert.equal(when(Date.parse("2026-11-12T14:02:00Z"), Date.parse("2026-11-12T14:12:00Z"), NOW), "14:02 → 14:12")
  // Across midnight the end carries its own date; a run from another day carries the start's.
  assert.equal(when(Date.parse("2026-11-11T23:50:00Z"), Date.parse("2026-11-12T00:12:00Z"), NOW), "Nov 11 23:50 → Nov 12 00:12")
  assert.equal(when(Date.parse("2026-11-10T08:00:00Z"), Date.parse("2026-11-10T08:30:00Z"), NOW), "Nov 10 08:00 → 08:30")
})

test("the list counts a live run up and holds a finished one at its length", () => {
  const run = { started: new Date(NOW - 20 * MIN).toISOString(), ended: new Date(NOW - 20 * MIN + 572_000).toISOString() }
  assert.equal(runLength(run, NOW, false), "9m32s")
  assert.equal(runLength(run, NOW + 3_600_000, false), "9m32s", "a finished run does not keep counting")
  assert.equal(runLength(run, NOW, true), "20m")
  assert.equal(runLength({ started: run.started, ended: null }, NOW, false), "", "no end and not running: nothing to say")
  assert.equal(runLength({ started: null, ended: run.ended }, NOW, false), "")
})

test("axis labels sit outside the run, flip inward at the edges, and drop out when there is no room", () => {
  // Room on both sides: the start's label before its tick, the end's after.
  assert.deepEqual(placeMarks([{ at: 10, text: "14:02", side: -1 }, { at: 20, text: "14:12", side: 1 }], 40),
    [{ at: 10, text: "14:02", from: 5 }, { at: 20, text: "14:12", from: 21 }])
  // At the edges they flip inside the run.
  assert.deepEqual(placeMarks([{ at: 2, text: "14:02", side: -1 }, { at: 37, text: "14:12", side: 1 }], 40),
    [{ at: 2, text: "14:02", from: 3 }, { at: 37, text: "14:12", from: 32 }])
  // Off the axis, or squeezed out by the first label, a tick is left out.
  assert.deepEqual(placeMarks([{ at: -3, text: "14:02", side: -1 }, { at: 41, text: "14:12", side: 1 }], 40), [])
  assert.deepEqual(placeMarks([{ at: 2, text: "14:02", side: -1 }, { at: 6, text: "14:12", side: 1 }], 10),
    [{ at: 2, text: "14:02", from: 3 }])
})

function agent(over: Partial<Agent>): Agent {
  const [a] = buildAgents([{ id: "t", projectId: "p", title: "Survey index sizes", activeProviderThreadId: "pt" }],
    new Map([["t", { id: "r", status: "completed", providerThreadId: "pt" }]]), [], new Map(), new Map(), new Map(), new Map(), [], [])
  return Object.assign(a, over)
}

test("the selected run is ticked where it started and ended, on the clock", () => {
  const iso = (min: number) => new Date(NOW - min * MIN).toISOString()
  const window = 30 * MIN
  const done = agent({ status: "completed", started: iso(20), ended: iso(10) })
  const marks = runMarks(done, 60, window, NOW)
  assert.deepEqual(marks.map((m) => m.text), ["14:10", "14:20"])
  assert.deepEqual(marks.map((m) => m.at), [19, 39], "30 seconds a column, now in the last: 20 and 10 minutes back")
  // A live run has only its start; one that began before the window, nothing to tick.
  assert.deepEqual(runMarks(agent({ status: "running", started: iso(20), ended: null }), 60, window, NOW).map((m) => m.text), ["14:10"])
  assert.deepEqual(runMarks(agent({ status: "running", started: iso(45), ended: null }), 60, window, NOW), [])
  // A run that ended in the window but began before it keeps its end.
  assert.deepEqual(runMarks(agent({ status: "completed", started: iso(45), ended: iso(5) }), 60, window, NOW).map((m) => m.text), ["14:25"])
  // Shorter than a minute: one tick, one label.
  assert.deepEqual(runMarks(agent({ status: "completed", started: iso(10), ended: new Date(NOW - 10 * MIN + 20_000).toISOString() }),
    60, window, NOW).map((m) => m.text), ["14:20"])
})

test("finished rows show their length, the axis ticks the selection, and the detail pane says when", () => {
  const iso = (min: number) => new Date(NOW - min * MIN).toISOString()
  const done = agent({ id: "done", title: "Survey index sizes", status: "completed", started: iso(20), ended: iso(10) })
  done.spans = [[NOW - 20 * MIN, NOW - 10 * MIN]]
  const live = agent({ id: "live", title: "Add streaming export", status: "running", started: iso(27), ended: null, thread_id: "u" })
  const st = initialState(2)
  st.bootAt = 0
  st.calm = true
  st.selected = "done"
  const c = new Canvas(140, 40)
  draw(c, st, { agents: [done, live], at: NOW, path: "" }, [], derivePalette({}), NOW)
  const lines = c.toText().split("\n")
  // Two agents fit as cards: name and run length on the first line, context and lane on the second.
  const y = lines.findIndex((l) => /^▌ ✓ Survey index sizes/.test(l)) // the selected row, not the detail pane title
  assert.match(lines[y], /\s10m +│/)
  assert.doesNotMatch(lines[y], /ago/)
  assert.match(lines.find((l) => l.includes("Add streaming export"))!, /\s27m +│/)
  // The ticks stand over the run's band: the same columns on the axis and in the lane.
  const axis = lines.find((l) => l.includes("CONTEXT") && l.includes("╷"))!
  assert.match(axis, /14:10╷.*╷14:20/)
  const [startCol, endCol] = [...axis].flatMap((ch, i) => (ch === "╷" ? [i] : []))
  const bg = (x: number) => c.bg[(y + 1) * c.w + x]
  assert.notEqual(bg(startCol), bg(startCol - 1), "the band starts under the start tick")
  assert.equal(bg(startCol), bg(endCol), "and runs to the end tick")
  assert.notEqual(bg(endCol), bg(endCol + 1), "where it stops")
  assert.ok(lines.some((l) => /DONE +10m 00s +14:10 → 14:20 · 10m ago/.test(l)), "the detail pane has the clock times")
  assert.ok(lines.every((l) => textWidth(l) <= 140))
})
