// Motion means work: what moves on screen, when, and at what frame rate.
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import type { KeyEvent } from "@opentui/core"
import { App, FPS, frameRate } from "../src/app.ts"
import { Canvas } from "../src/canvas.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, type Agent, type Item, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { FLASH_MS, SHIMMER_MS, WINDOWS, draw, initialState, itemKey, observe, slices, spinner, type Frame, type ViewState } from "../src/view.ts"

const MIN = 60_000
const EPOCH = Date.parse("2026-11-12T12:00:00Z")
const NOW = EPOCH + 2.5 * MIN // a whole second, with every demo state on screen
const SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
const palette = derivePalette({})
const usage = demoUsage(NOW, EPOCH)

const sec = (t: number) => Math.floor(t / 1000) * 1000
const ISO = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z$/

/**
 * The demo snapshot at `now`, with every timestamp on a whole second. Then two
 * frames drawn within one second differ only where something animates, not
 * because a seconds counter ("quiet 5m 22s") happened to tick in between.
 */
function demo(now = NOW): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-motion-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), now, EPOCH)
    const walk = (v: unknown): unknown => Array.isArray(v) ? v.map(walk)
      : v && typeof v === "object" ? Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x)]))
      : typeof v === "string" && ISO.test(v) ? new Date(sec(Date.parse(v))).toISOString() : v
    const snap = walk(new Source(join(dir, "statev2.sqlite")).snapshot(now)) as Snapshot
    for (const a of snap.agents) {
      a.events = a.events.map((e) => ({ ...e, t: sec(e.t) }))
      a.spans = a.spans.map(([s, e]) => [sec(s), e === null ? null : sec(e)])
      a.history = a.history.map((h) => ({ ...h, at: sec(h.at) }))
    }
    return { ...snap, at: sec(snap.at) }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

const byTitle = (snap: Snapshot, title: string): Agent => snap.agents.find((a) => a.title === title)!

/** A view state past the boot wipe with every context bar already at its value. */
function settled(snap: Snapshot, w: number, h: number, at: number, setup: (st: ViewState) => void = () => {}): { st: ViewState; c: Canvas } {
  const st = initialState(2)
  st.bootAt = 0
  setup(st)
  const c = new Canvas(w, h)
  const calm = st.calm
  st.calm = true // values jump straight to their targets
  draw(c, st, snap, usage, palette, at - 1000)
  st.calm = calm
  return { st, c }
}

interface Cells { ch: Uint32Array; fg: Uint32Array; bg: Uint32Array; w: number; text: string }
function frameAt(c: Canvas, st: ViewState, snap: Snapshot, now: number): { cells: Cells; frame: Frame } {
  const frame = draw(c, st, snap, usage, palette, now)
  return { frame, cells: { ch: c.ch.slice(), fg: c.fg.slice(), bg: c.bg.slice(), w: c.w, text: c.toText() } }
}

/** Cells that differ between two frames, apart from the header row (its refresh dot fades after each snapshot). */
function changes(a: Cells, b: Cells): { x: number; y: number; from: string; to: string }[] {
  const out = []
  for (let i = a.w; i < a.ch.length; i++) {
    if (a.ch[i] === b.ch[i] && a.fg[i] === b.fg[i] && a.bg[i] === b.bg[i]) continue
    out.push({ x: i % a.w, y: Math.floor(i / a.w), from: String.fromCodePoint(a.ch[i]), to: String.fromCodePoint(b.ch[i]) })
  }
  return out
}

test("time slices sit on the wall clock, whole seconds long", () => {
  for (const [cols, minutes] of [[47, 30], [60, 15], [140, 60], [9, 180]]) {
    const windowMs = minutes * MIN
    const { size, col } = slices(cols, windowMs, NOW)
    assert.equal(size % 1000, 0)
    assert.ok(Math.abs(size * cols - windowMs) <= cols * 500, "spans about the window")
    assert.equal(col(NOW), cols - 1, "now is in the last column")
    assert.equal(col(NOW - size * 3), cols - 4)
    // Between two frames in the same slice, every event keeps its column.
    const start = Math.floor(NOW / size) * size
    const later = slices(cols, windowMs, start + size - 1).col
    const early = slices(cols, windowMs, start).col
    for (const t of [NOW - 7_777, NOW - 123_456, start]) assert.equal(later(t), early(t))
    // When the next slice begins, everything moves left by exactly one column.
    const next = slices(cols, windowMs, start + size).col
    assert.equal(next(NOW - 7_777), early(NOW - 7_777) - 1)
  }
})

test("two frames a few hundred ms apart with no new data are identical apart from spinners", () => {
  const snap = demo()
  for (const [w, h] of [[200, 80], [140, 45], [100, 40], [60, 20]]) {
    for (let wi = 0; wi < WINDOWS.length; wi++) {
      const { st, c } = settled(snap, w, h, NOW, (s) => {
        s.windowIndex = wi
        s.selected = byTitle(snap, "Migrate billing to the v2 API").id
      })
      const a = frameAt(c, st, snap, NOW + 100).cells
      const b = frameAt(c, st, snap, NOW + 400).cells
      const moved = changes(a, b).filter((d) => !(SPIN.includes(d.from) && SPIN.includes(d.to)))
      assert.deepEqual(moved, [], `${w}x${h}, lane ${WINDOWS[wi]}m`)
      assert.ok(changes(a, b).length > 0, "the spinners did turn")
    }
  }
})

test("every spinner shows the same frame of one 100 ms beat", () => {
  assert.deepEqual(Array.from({ length: 11 }, (_, i) => spinner(i * 100)).join(""), SPIN + SPIN[0])
  assert.equal(spinner(1_000_050), spinner(1_000_099))
  const snap = demo()
  const { st, c } = settled(snap, 200, 80, NOW, (s) => (s.selected = byTitle(snap, "Fix the flaky checkout tests").id))
  for (const t of [NOW + 100, NOW + 250, NOW + 730]) {
    const shown = new Set([...frameAt(c, st, snap, t).cells.text].filter((ch) => SPIN.includes(ch)))
    assert.deepEqual([...shown], [spinner(t)], `all in phase at +${t - NOW}ms`)
  }
})

test("quiet, queued and waiting agents hold still; failures have their own shape", () => {
  const snap = demo(EPOCH) // the cycle start: a queued agent, two quiet ones and one waiting on you
  const { st, c } = settled(snap, 200, 80, EPOCH)
  const { frame, cells } = frameAt(c, st, snap, EPOCH + 100)
  const lines = cells.text.split("\n")
  const row = (title: string) => lines[frame.hits.find((x) => x.id === byTitle(snap, title).id && x.area === "list")!.y]
  assert.match(row("Check rate-limit headers"), /◔ Check rate-limit headers/, "queued: a still ◔")
  assert.match(row("Write migration tests"), /◌ Write migration tests/)
  const s = spinner(EPOCH + 100)
  const tiles = lines.slice(1, lines.findIndex((l) => /^ (LIMITS|AGENTS) /.test(l))) // the top row ends where a section starts
  assert.ok(tiles.join("\n").includes(`${s} ${s} ${s} ${s} ${s} ◔ ◌ ◌ 1 stalled`), "the ACTIVE tile matches the rows")
  const lane = lines[frame.hits.find((x) => x.id === byTitle(snap, "Write migration tests").id && x.area === "list")!.y + 1]
  assert.match(lane, /×/, "a failed call is a ×")
})

/** The activity rows in the detail panel, with whether each one is flashing. */
function activity(c: Canvas, frame: Frame, text: string): { label: string; flashing: boolean }[] {
  const lines = text.split("\n")
  const top = lines.findIndex((l, y) => y >= frame.detail.y && l.slice(frame.detail.x).startsWith("ACTIVITY"))
  assert.ok(top > 0, "the activity feed is on screen")
  const out = []
  for (let y = top + 1; y < c.h && lines[y].slice(frame.detail.x).trim(); y++) {
    const at = y * c.w + frame.detail.x + 2
    out.push({ label: lines[y].slice(frame.detail.x + 9).trim(), flashing: c.bg[at] !== palette.bg })
  }
  return out
}

/** A new tool call for `a`, started a moment after `now`. */
function newCall(a: Agent, now: number, file: string): Item {
  const call: Item = { thread_id: a.thread_id, run_id: a.run.id ?? null, node_id: a.items[0]?.node_id ?? null, provider_thread_id: null,
    type: "dynamic_tool", status: "running", tool: "Read", label: `Read ${file}`, command: "", file: "",
    started_at: new Date(now + 1000).toISOString(), updated_at: new Date(now + 1000).toISOString() }
  a.items.unshift(call)
  a.now = call
  return call
}

test("only what a snapshot newly brings flashes; selecting or following never does", () => {
  const first = demo()
  const st = initialState(2)
  st.bootAt = 0
  observe(st, first, NOW)
  assert.ok(st.firstSeen.size > 20)
  assert.ok([...st.firstSeen.values()].every((t) => t === 0), "the first snapshot is what was already there")
  assert.equal(st.shimmer.size, 0)

  const second = structuredClone(first)
  const callers = byTitle(second, "Map callers of the legacy client")
  const call = newCall(callers, NOW, "src/new-file.ts")
  // The same call, with its status and update time changed: still the same item.
  const earlier = callers.items.find((i, n) => n > 0 && i.status === "completed")!
  const key = itemKey(earlier)
  earlier.updated_at = new Date(NOW + 1500).toISOString()
  assert.equal(itemKey(earlier), key)
  const T = NOW + 2000
  observe(st, second, T)
  assert.equal(st.firstSeen.get(itemKey(call)), T)
  assert.deepEqual([...st.firstSeen.entries()].filter(([, t]) => t !== 0).map(([k]) => k), [itemKey(call)], "nothing else is new")
  assert.deepEqual([...st.shimmer.keys()], [callers.id])

  const c = new Canvas(200, 80)
  const look = (setup: () => void, at = T + 100) => {
    setup()
    const frame = draw(c, st, second, usage, palette, at)
    return activity(c, frame, c.toText())
  }
  // The selected thread (billing) has nothing new.
  assert.ok(look(() => (st.selected = byTitle(second, "Migrate billing to the v2 API").id)).every((r) => !r.flashing))
  // Selecting the agent that got the call shows that one row flashing, and only it.
  const rows = look(() => (st.selected = callers.id))
  assert.deepEqual(rows.filter((r) => r.flashing).map((r) => r.label), ["Read src/new-file.ts"])
  // Following T3 to another thread flashes nothing there.
  const checkout = byTitle(second, "Fix the flaky checkout tests")
  assert.ok(look(() => (st.t3Title = `${checkout.project} / ${checkout.title}`)).every((r) => !r.flashing))
  assert.equal(st.selected, checkout.id)
  // The flash is over after FLASH_MS, and the next snapshot does not bring it back.
  assert.ok(look(() => (st.selected = callers.id), T + FLASH_MS + 1).every((r) => !r.flashing))
  call.status = "completed"
  call.updated_at = new Date(T + 1000).toISOString()
  observe(st, second, T + 2000)
  assert.equal(st.firstSeen.get(itemKey(call)), T, "completing is not arriving")
  assert.ok(look(() => {}, T + 2100).every((r) => !r.flashing))
})

/** The colors of an agent's name in its list row: the cells that spell its title, as far as it fits. */
function nameColors(c: Canvas, frame: Frame, snap: Snapshot, a: Agent): number[] {
  const y = frame.hits.find((x) => x.id === a.id && x.area === "list")!.y
  const line = c.toText().split("\n")[y]
  const x = line.indexOf(a.title.slice(0, 12))
  assert.ok(x > 0, "the name is on screen")
  let n = 0
  while (n < a.title.length && line[x + n] === a.title[n]) n++
  return Array.from({ length: n }, (_, i) => c.fg[y * c.w + x + i])
}

test("a new tool call plays the name shimmer once, briefly; nothing shimmers otherwise", () => {
  const snap = demo()
  const callers = byTitle(snap, "Map callers of the legacy client")
  const billing = byTitle(snap, "Migrate billing to the v2 API")
  const { st, c } = settled(snap, 200, 80, NOW, (s) => observe(s, snap, NOW))
  const plain = (a: Agent, at: number) => {
    const frame = draw(c, st, snap, usage, palette, at)
    return { frame, uniform: new Set(nameColors(c, frame, snap, a)).size === 1 }
  }
  for (const t of [NOW + 100, NOW + 900, NOW + 1700]) {
    const f = plain(billing, t)
    assert.ok(f.uniform, "no shimmer without a new tool call")
    assert.equal(f.frame.smooth, false)
  }
  const next = structuredClone(snap)
  newCall(byTitle(next, callers.title), NOW, "src/orders/refund.ts")
  const T = NOW + 2000
  observe(st, next, T)
  const during = draw(c, st, next, usage, palette, T + 120)
  assert.equal(new Set(nameColors(c, during, next, callers)).size > 1, true, "the shimmer is playing")
  assert.equal(during.smooth, true, "a higher frame rate while it plays")
  const after = draw(c, st, next, usage, palette, T + SHIMMER_MS)
  assert.equal(new Set(nameColors(c, after, next, callers)).size, 1, "and it is over")
  assert.equal(after.smooth, false)
  assert.ok(SHIMMER_MS <= 500)
  // Calm mode: no shimmer at all.
  observe(st, structuredClone(snap), T + 3000) // the call is gone again
  const again = structuredClone(snap)
  newCall(byTitle(again, callers.title), T + 3000, "src/orders/void.ts")
  observe(st, again, T + 5000)
  st.calm = true
  const calm = draw(c, st, again, usage, palette, T + 5100)
  assert.equal(new Set(nameColors(c, calm, again, callers)).size, 1)
  assert.equal(calm.smooth, false)
})

test("calm mode: still running glyphs, values jump, no boot wipe, idle frame rate", () => {
  const snap = demo()
  const st = initialState(2)
  st.calm = true
  st.bootAt = NOW // the wipe would be under way
  st.selected = byTitle(snap, "Migrate billing to the v2 API").id
  const c = new Canvas(200, 80)
  const a = frameAt(c, st, snap, NOW + 100)
  assert.equal(a.frame.smooth, false)
  assert.equal(a.frame.beat, false)
  assert.equal(frameRate(a.frame, st), FPS.idle)
  for (const agent of snap.agents) {
    const { usedTokens: used, maxTokens: max } = agent.context
    if (typeof used === "number" && typeof max === "number" && max > 0) assert.equal(st.tween.get(agent.id), used / max, agent.title)
  }
  assert.ok(![...a.cells.text].some((ch) => SPIN.includes(ch)), "no spinner frames")
  assert.match(a.cells.text, /◉ Migrate billing to the v2 API/)
  assert.match(a.cells.text, /◌ Write migration tests/, "quiet stays distinct")
  const footer = (c.h - 1) * c.w + 1 // the first key hint
  assert.notEqual(a.cells.fg[footer], a.cells.bg[footer], "the footer is not wiped out")
  assert.deepEqual(changes(a.cells, frameAt(c, st, snap, NOW + 400).cells), [], "nothing at all moves")
})

test("frame rates: the beat while agents work, faster only for a tween or shimmer, idle otherwise", () => {
  const snap = demo()
  const st = initialState(2)
  st.bootAt = 0
  const c = new Canvas(200, 80)
  assert.equal(frameRate(null, st), FPS.idle)
  assert.equal(FPS.beat, 1000 / 100, "the spinners' beat")
  // First frame: bars sweep up from zero.
  assert.equal(frameRate(draw(c, st, snap, usage, palette, NOW), st), FPS.smooth)
  st.calm = true
  draw(c, st, snap, usage, palette, NOW + 50)
  st.calm = false
  assert.equal(frameRate(draw(c, st, snap, usage, palette, NOW + 100), st), FPS.beat)
  // Nothing working, nobody waiting: only the prompt cache drains, and that alone is not motion.
  const still = structuredClone(snap)
  for (const a of still.agents) {
    a.status = "completed"
    a.requests = []
    a.items = a.items.map((i) => ({ ...i, status: "completed" }))
    a.now = null
  }
  const billing = byTitle(still, "Migrate billing to the v2 API")
  assert.ok(Date.parse(billing.context.promptCacheExpiresAt!) > NOW)
  st.selected = billing.id
  const quiet = draw(c, st, still, usage, palette, NOW + 200)
  assert.match(c.toText(), /PROMPT CACHE .* warm/)
  assert.equal(frameRate(quiet, st), FPS.idle)
  // Calm mode and the help overlay stay idle whatever is on screen.
  st.calm = true
  assert.equal(frameRate({ ...quiet, beat: true, smooth: true }, st), FPS.idle)
  st.calm = false
  st.help = true
  assert.equal(frameRate({ ...quiet, beat: true, smooth: true }, st), FPS.idle)
})

test("the help overlay freezes everything underneath", () => {
  const snap = demo()
  const { st, c } = settled(snap, 120, 40, NOW, (s) => (s.help = true))
  const a = frameAt(c, st, snap, NOW + 100)
  const b = frameAt(c, st, snap, NOW + 5300)
  assert.equal(a.frame.beat || a.frame.smooth, false)
  assert.deepEqual(changes(a.cells, b.cells), [])
  assert.deepEqual([...a.cells.text.split("\n")[0]], [...b.cells.text.split("\n")[0]], "the clock stops too")
  st.help = false
  assert.notEqual(frameAt(c, st, snap, NOW + 5400).cells.text.split("\n")[0], a.cells.text.split("\n")[0], "and runs again once help closes")
})

test("the app: a calm option, m toggles it, and CRT effects stay off while calm", () => {
  const snap = demo()
  const source = { path: "", snapshot: () => snap } as unknown as Source
  const key = (app: App, sequence: string) => app.key({ name: sequence, sequence } as KeyEvent)
  const app = new App(source, 2, { usage: { poll: () => usage }, demo: true, calm: true })
  assert.equal(app.st.calm, true)
  assert.equal(app.st.seeded, true, "the first snapshot is observed")
  key(app, "x")
  assert.equal(app.st.fx, false)
  assert.match(app.st.notice!.text, /calm/i)
  key(app, "m")
  assert.equal(app.st.calm, false)
  key(app, "x")
  assert.equal(app.st.fx, true)
  key(app, "m")
  assert.equal(app.st.calm, true)
  assert.equal(app.st.fx, false, "turning calm on turns the CRT effects off")
  assert.equal(new App(source, 2, { usage: { poll: () => usage }, demo: true }).st.calm, false, "off by default")
})

test("a clock that steps back a day mid-boot draws a frame instead of hanging", () => {
  const snap = demo()
  const st = initialState(2)
  st.bootAt = NOW + 24 * 3_600_000 // the wall clock jumped back after start-up
  const c = new Canvas(120, 40)
  const frame = draw(c, st, snap, usage, palette, NOW)
  assert.equal(frame.smooth, true, "the wipe simply starts over")
})
