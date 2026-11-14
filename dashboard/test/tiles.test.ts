// The top row: big numbers only for news, a dash with a reason for what is
// missing, and never a calm-looking zero when T3 can't be read.
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { BIG_ROWS, Canvas, bigWidth, drawBig } from "../src/canvas.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, isRunning, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { SHADOW_MIN_H, draw, faceFor, initialState, trouble, type ViewState } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const EPOCH = Date.parse("2026-11-12T12:00:00Z")
const NOW = EPOCH + 2.5 * 60_000
const palette = derivePalette(THEMES.solitude)
const BIG = /[▀▄█]/ // big digits are drawn in blocks: half blocks, or full ones with a shadow

function demo(now = NOW): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-tiles-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), now, EPOCH)
    return new Source(join(dir, "statev2.sqlite")).snapshot(now)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

/** Everything finished cleanly: nobody waiting, nothing running, no failed calls. */
function calm(snap: Snapshot): Snapshot {
  const still = structuredClone(snap)
  for (const a of still.agents) {
    a.status = "completed"
    a.requests = []
    a.items = a.items.map((i) => ({ ...i, status: "completed" }))
    a.events = a.events.map((e) => ({ ...e, failed: false }))
    a.now = null
  }
  return still
}

function render(snap: Snapshot, w: number, h: number, setup: (st: ViewState) => void = () => {}, usage = demoUsage(NOW, EPOCH)) {
  const st = initialState(2)
  st.bootAt = 0
  st.calm = true
  setup(st)
  const c = new Canvas(w, h)
  const frame = draw(c, st, snap, usage, palette, NOW)
  const lines = c.toText().split("\n")
  /** A tile's rows, by its label. */
  const tile = (label: string) => {
    const x = lines[1].indexOf(label) - 2
    assert.ok(x >= 0, `${label} tile on screen`)
    const next = lines[1].slice(x + 2 + label.length).search(/[A-Z]/)
    const w = next < 0 ? c.w - x : x + 2 + label.length + next - 2 - x - 1 // up to the gap before the next tile
    const height = lines.findIndex((l, y) => y > 1 && /^ (LIMITS|AGENTS) /.test(l)) - 1 // tiles end where a section starts
    return { x, rows: lines.slice(1, 1 + height).map((l) => l.slice(x, x + w)) }
  }
  return { c, st, frame, text: c.toText(), lines, tile }
}

test("zero and empty values are small and dim; big digits mean look here", () => {
  const snap = calm(demo())
  const { tile, c } = render(snap, 150, 50)
  for (const label of ["ACTIVE", "NEEDS YOU", "FAILED CALLS"]) {
    const t = tile(label)
    assert.ok(!t.rows.slice(1).some((r) => BIG.test(r)), `${label} 0 is not drawn in big digits`)
    assert.ok(t.rows.some((r) => /^ {2}0\s*$/.test(r)), `${label} shows a small 0`)
    const y = t.rows.findIndex((r) => /^ {2}0\s*$/.test(r)) + 1
    assert.equal(c.fg[y * c.w + t.x + 2], palette.dim, `${label}'s 0 is dim`)
  }
  assert.ok(tile("NEEDS YOU").rows.some((r) => r.includes("nothing pending")))
  assert.ok(tile("ACTIVE").rows.some((r) => r.includes("nothing running")))
  // News stays big: the busy demo has agents running and one waiting.
  const busy = render(demo(), 150, 50)
  for (const label of ["ACTIVE", "NEEDS YOU"]) assert.ok(busy.tile(label).rows.slice(1).some((r) => BIG.test(r)), `${label} is big`)
})

test("missing values are a small dash with the reason", () => {
  const snap = calm(demo())
  for (const a of snap.agents) a.context = {}
  const { tile } = render(snap, 150, 50, () => {}, [])
  const tokens = tile("TOKENS TODAY").rows
  assert.ok(tokens.some((r) => /^ {2}—\s*$/.test(r)))
  assert.ok(tokens.some((r) => r.includes("no Omarchy usage data")))
  const peak = tile("PEAK CONTEXT").rows
  assert.ok(peak.some((r) => /^ {2}—\s*$/.test(r)))
  assert.ok(peak.some((r) => r.includes("not reported")))
})

test("tiles keep their places whatever their values", () => {
  const where = (snap: Snapshot) => {
    const { lines } = render(snap, 150, 50)
    return ["ACTIVE", "NEEDS YOU", "TOOLS / MIN", "PEAK CONTEXT", "TOKENS TODAY", "FAILED CALLS"].map((l) => lines[1].indexOf(l))
  }
  assert.deepEqual(where(calm(demo())), where(demo()))
})

test("ACTIVE counts agents the way --once and --status do", () => {
  const snap = demo()
  const { lines } = render(snap, 100, 24) // small tiles: the value is text
  const value = Number(/^ {2}(\d+)/.exec(lines[2])![1])
  assert.equal(value, snap.agents.filter(isRunning).length)
})

test("T3 never read: the tiles say so instead of showing zeros, and the list says what to try", () => {
  const path = join(tmpdir(), "t3a-nowhere", "statev2.sqlite")
  let err: unknown
  assert.throws(() => new Source(path).snapshot(), (e) => ((err = e), true))
  const { text, tile, lines } = render({ agents: [], at: 0, path: "" }, 150, 50, (st) => {
    st.error = (err as Error).message
    st.cause = trouble(err, path)
  })
  for (const label of ["ACTIVE", "NEEDS YOU", "TOOLS / MIN", "PEAK CONTEXT", "FAILED CALLS"]) {
    const t = tile(label).rows
    assert.ok(t.some((r) => /^ {2}—\s*$/.test(r)), `${label} is a dash`)
    assert.ok(t.some((r) => r.includes("can't read T3")), `${label} says why`)
    assert.ok(!t.some((r) => /\b0\b|nothing|clean/.test(r)), `${label} does not claim calm`)
  }
  assert.ok(text.includes(`No T3 database at ${path}.`), "names the path")
  assert.match(text, /T3 Code installed/)
  assert.match(text, /--db PATH/)
  assert.match(text, /T3CODE_HOME/)
  assert.match(text, /--demo/)
  assert.doesNotMatch(lines.at(-1)!, /last snapshot kept/, "there is no snapshot to keep")
  assert.match(lines.at(-1)!, /SOURCE ERROR/)
  assert.doesNotMatch(text, /PULSE|No unarchived/)
})

test("a failing source with a snapshot kept: agents stay listed, tiles and footer say it's stale", () => {
  const snap = demo()
  const { text, tile, lines } = render(snap, 150, 50, (st) => {
    st.error = "database is locked"
    st.cause = trouble(new Error("database is locked"), snap.path)
  })
  assert.ok(tile("NEEDS YOU").rows.some((r) => r.includes("can't read T3")))
  assert.match(text, /Migrate billing to the v2 API/)
  assert.match(lines.at(-1)!, /SOURCE ERROR · last snapshot kept · .*database is locked/)
})

test("unreadable and unknown databases get their own advice", () => {
  const cases = [
    [{ kind: "unreadable", path: "/x/statev2.sqlite", detail: "permission denied" }, /Can't read the T3 database at \/x\/statev2\.sqlite: permission denied\.[\s\S]*your user can read it/],
    [{ kind: "schema", path: "/x/statev2.sqlite", detail: "missing orchestration_v2_projection_runs" }, /doesn't know[\s\S]*newer T3 Agents/],
  ] as const
  for (const [cause, expected] of cases) {
    const { text } = render({ agents: [], at: 0, path: "" }, 150, 50, (st) => {
      st.error = "x"
      st.cause = { ...cause }
    })
    assert.match(text, expected)
    assert.match(text, /--demo/)
  }
})

test("tall, wide windows draw every big number shadowed; the rest draw them all plain", () => {
  const SHADOW = /[╔╗╚╝═║]/
  const PLAIN = /[▀▄]/
  for (const [w, h, face] of [[210, 52, "shadow"], [210, SHADOW_MIN_H - 1, "plain"], [150, 50, "plain"], [120, 34, "plain"]] as const) {
    const { lines, frame } = render(demo(), w, h)
    assert.equal(frame.face, face, `${w}×${h}`)
    const other = face === "shadow" ? PLAIN : SHADOW
    const mine = face === "shadow" ? SHADOW : PLAIN
    // The tiles' digit rows, under their labels.
    const tiles = lines.slice(2, 2 + BIG_ROWS[face]).join("\n")
    assert.match(tiles, mine, `${w}×${h} tiles are ${face}`)
    assert.doesNotMatch(tiles, other, `${w}×${h} tiles mix in the other face`)
    // The selected agent's context, under its CONTEXT heading in the details.
    const { x, y: top, h: dh } = frame.detail
    const at = lines.findIndex((l, y) => y >= top && y < top + dh && l.slice(x).startsWith("CONTEXT"))
    assert.ok(at > 0, `${w}×${h} details show CONTEXT`)
    const context = lines.slice(at + 1, at + 1 + BIG_ROWS[face]).map((l) => l.slice(x, x + 30)).join("\n")
    assert.match(context, mine, `${w}×${h} context is ${face}`)
    assert.doesNotMatch(context, other, `${w}×${h} context mixes in the other face`)
  }
})

test("the face comes from the window's size: rows for it, and tile room for a full context window", () => {
  for (const w of [100, 150, 190, 191, 210, 330]) {
    assert.equal(faceFor(w, SHADOW_MIN_H - 1), "plain", `${w} columns, too short`)
  }
  // The narrowest window that shadows has six tiles each wide enough for "100%".
  const narrowest = [...Array(400).keys()].find((w) => faceFor(w, 80) === "shadow")!
  assert.ok(narrowest > 120, `shadowed from ${narrowest} columns`)
  const tw = Math.floor((narrowest - 5) / 6)
  assert.ok(tw - 3 >= bigWidth("100%", "shadow"))
  assert.ok(Math.floor((narrowest - 1 - 5) / 6) - 3 < bigWidth("100%", "shadow"), "one column narrower is plain")
  for (let w = narrowest; w < 400; w++) assert.equal(faceFor(w, SHADOW_MIN_H), "shadow", `${w} columns`)
})

test("a decimal too wide for big digits rounds to a whole number instead of turning small", () => {
  // 27.6M is 32 columns shadowed; a 210-column window's tiles have 31.
  const usage = demoUsage(NOW, EPOCH).map((u, i) => ({ ...u, today: i === 0 ? 27_600_000 : 0 }))
  assert.ok(bigWidth("27.6M", "shadow") > 31)
  const { tile, frame } = render(demo(), 210, 52, () => {}, usage)
  assert.equal(frame.face, "shadow")
  const ref = new Canvas(40, BIG_ROWS.shadow)
  ref.clear(0, 0)
  drawBig(ref, 0, 0, "28M", 1, "shadow")
  const rows = tile("TOKENS TODAY").rows.slice(1, 1 + BIG_ROWS.shadow).map((r) => r.slice(2).trimEnd())
  assert.deepEqual(rows, ref.toText().split("\n"))
  // Where it fits, the decimal stays.
  const wide = render(demo(), 330, 52, () => {}, usage)
  assert.ok(wide.tile("TOKENS TODAY").rows.slice(1, 1 + BIG_ROWS.shadow).join("").includes("╗"))
  const ref2 = new Canvas(40, BIG_ROWS.shadow)
  ref2.clear(0, 0)
  drawBig(ref2, 0, 0, "27.6M", 1, "shadow")
  assert.deepEqual(wide.tile("TOKENS TODAY").rows.slice(1, 1 + BIG_ROWS.shadow).map((r) => r.slice(2).trimEnd()), ref2.toText().split("\n"))
})
