// No hidden content without a cue: what is scrolled or folded out of view
// says so, and at "beside T3" sizes the agent list gets the rows first.
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import type { KeyEvent } from "@opentui/core"
import { App } from "../src/app.ts"
import { Canvas } from "../src/canvas.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, needsYou, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { LIMITS_FULL_MIN_H, draw, initialState, type ViewState } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const EPOCH = Date.parse("2026-11-12T12:00:00Z")
const NOW = EPOCH + 2.5 * 60_000
const palette = derivePalette(THEMES.solitude)
const usage = demoUsage(NOW, EPOCH)
const LANE = /[▁▂▃▄▅▆▇█┃×]/

function demo(now = NOW): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-cues-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), now, EPOCH)
    return new Source(join(dir, "statev2.sqlite")).snapshot(now)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

const byTitle = (snap: Snapshot, title: string) => snap.agents.find((a) => a.title === title)!

function render(snap: Snapshot, w: number, h: number, setup: (st: ViewState) => void = () => {}, st = initialState(2)) {
  st.bootAt = 0
  st.calm = true
  setup(st)
  const c = new Canvas(w, h)
  const frame = draw(c, st, snap, usage, palette, NOW)
  return { c, st, frame, lines: c.toText().split("\n") }
}

test("the detail panel says when there is more below or above", () => {
  const snap = demo()
  const billing = byTitle(snap, "Migrate billing to the v2 API").id
  const { frame, lines, st, c } = render(snap, 100, 30, (s) => (s.selected = billing))
  const d = frame.detail
  assert.ok(frame.detailMax > 0, "the details overflow at this size")
  assert.match(lines[d.y + d.h - 1], new RegExp(`▼ ${frame.detailMax} more$`))
  assert.doesNotMatch(lines[d.y], /▲/, "nothing above yet")
  st.detailScroll = 4
  const again = draw(c, st, snap, usage, palette, NOW)
  const text = c.toText().split("\n")
  assert.match(text[d.y], /▲ 4 more$/)
  assert.match(text[d.y + d.h - 1], new RegExp(`▼ ${again.detailMax - 4} more$`))
  st.detailScroll = again.detailMax
  draw(c, st, snap, usage, palette, NOW)
  assert.doesNotMatch(c.toText().split("\n")[d.y + d.h - 1], /▼/, "scrolled to the end")
})

test("beside T3, the agent list gets the rows first and LIMITS shrinks to one line", () => {
  const snap = demo()
  for (const [w, h, least] of [[80, 24, 8], [100, 30, 11], [120, 40, 14]]) {
    const { frame, lines } = render(snap, w, h)
    const shown = new Set(frame.hits.filter((x) => x.area === "list").map((x) => x.id)).size
    assert.ok(shown >= least, `${w}x${h}: ${shown} agents listed`)
    assert.ok(frame.detail.h >= 7, `${w}x${h}: the details keep a few rows`)
    const limitRows = lines.filter((l) => /^ LIMITS/.test(l) || /Session|Weekly/.test(l)).length
    if (h < LIMITS_FULL_MIN_H) {
      assert.equal(limitRows, 1, `${w}x${h}: one LIMITS line`)
      const line = lines.find((l) => l.startsWith(" LIMITS"))!
      assert.ok(line.indexOf("92%") < line.indexOf("47%"), "the fullest window first")
    } else assert.ok(limitRows > 1, `${w}x${h}: every window`)
  }
})

test("the list's overflow marker has its own row and says what is hidden", () => {
  const snap = demo()
  const { frame, lines, c } = render(snap, 100, 30)
  const y = lines.findIndex((l) => /^ {2}▼ \d+ more/.test(l))
  assert.ok(y > 0, "an overflow row")
  assert.doesNotMatch(lines[y], LANE, "no lane on it")
  assert.ok(!frame.hits.some((x) => x.y === y), "and no agent")
  const hidden = frame.rows.length - new Set(frame.hits.filter((x) => x.area === "list").map((x) => x.id)).size
  assert.match(lines[y], new RegExp(`▼ ${hidden} more`))
  assert.match(lines[y], /1 ✗/, "the failed agent below")
  assert.equal(c.fg[y * c.w + 2], palette.text2, "not hot while nobody waiting is hidden")
})

test("a waiting agent scrolled out of the list turns the marker hot with a ◆", () => {
  const snap = demo()
  const last = snap.agents.find((a) => a.title === "Upgrade the image pipeline")!.id
  const { lines, c } = render(snap, 100, 30, (s) => (s.selected = last))
  const y = lines.findIndex((l) => /^ {2}▲ \d+/.test(l))
  assert.ok(y > 0, "the rows above are counted")
  assert.match(lines[y], /◆1/)
  assert.equal(c.fg[y * c.w + 2], palette.hot)
  assert.equal(c.fg[y * c.w + lines[y].indexOf("◆")], palette.hot)
  assert.ok(!snap.agents.filter(needsYou).some((a) => lines.some((l) => l.includes(a.title))), "the waiting agent is out of view")
})

test("a folded parent with a subagent waiting on you shows a ◆", () => {
  const snap = structuredClone(demo())
  const callers = byTitle(snap, "Check rate-limit headers")
  callers.requests = ["user_input"]
  const parent = byTitle(snap, "Map callers of the legacy client")
  const { lines, frame, c } = render(snap, 150, 50, (s) => s.collapsed.add(parent.id))
  const y = frame.hits.find((x) => x.id === parent.id && x.area === "list")!.y
  assert.match(lines[y], /Map callers of the legacy client ▸ ◆/)
  assert.equal(c.fg[y * c.w + lines[y].indexOf("◆")], palette.hot)
})

test("one-row lanes leave the provider dot and the age alone", () => {
  const snap = demo()
  for (const w of [80, 100, 120]) {
    const { frame, lines, c } = render(snap, w, 40)
    for (const hit of frame.hits.filter((x) => x.area === "list")) {
      const line = lines[hit.y]
      const m = / ● (\d+[smhd]( \d+[smh])?)$/.exec(line)
      assert.ok(m, `${w} cols: "${line}" ends with the provider dot and age`)
      const dot = line.length - m[0].length + 1
      const fam = c.fg[hit.y * c.w + dot]
      assert.notEqual(fam, palette.faint, "the dot is the provider's, not a lane tick")
      assert.equal(line[dot - 1], " ", "a gap before the dot")
    }
  }
})

test("help that does not fit scrolls, and says so", () => {
  const snap = demo()
  const source = { path: "", snapshot: () => snap } as unknown as Source
  const app = new App(source, 2, { usage: { poll: () => usage }, demo: true, calm: true })
  const key = (name: string, sequence = name) => app.key({ name, sequence } as KeyEvent)
  const frame = () => {
    app.canvas.resize(80, 24)
    app.frame = draw(app.canvas, app.st, snap, usage, palette, NOW)
    return app.canvas.toText()
  }
  key("?", "?")
  let text = frame()
  assert.ok(app.frame!.helpMax > 0)
  assert.match(text, new RegExp(`▼ ${app.frame!.helpMax} more`))
  assert.doesNotMatch(text, /Unofficial/)
  const selected = app.st.selected
  for (let i = 0; i < 40; i++) key("down")
  text = frame()
  assert.equal(app.st.selected, selected, "scrolling the help leaves the selection alone")
  assert.equal(app.st.helpScroll, app.frame!.helpMax)
  assert.match(text, /Unofficial; not affiliated with T3 Code or Omarchy\./)
  assert.match(text, /▲ \d+ more/)
  assert.doesNotMatch(text, /▼ \d+ more · ↓/)
})
