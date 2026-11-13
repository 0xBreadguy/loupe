// Drawing primitives, palette derivation, usage parsing and whole-layout smoke tests.
import assert from "node:assert/strict"
import { test } from "node:test"
import { BIG_ROWS, Braille, Canvas, bigGlyph, bigWidth, drawBig, fit, fontWidth, hbar, spark, textWidth } from "../src/canvas.ts"
import { buildAgents, type Agent } from "../src/source.ts"
import { chroma, contrast, derivePalette, family, parseColorsToml } from "../src/theme.ts"
import { parseProvider, windowLength } from "../src/usage.ts"
import { draw, initialState, shortModel, tokens } from "../src/view.ts"

const SOLITUDE = `mode = "dark"
accent = "#798186"
selection = "#343d41"
background = "#101315"
foreground = "#cacccc"
red = "#565d60"
yellow = "#d9dbdc"
bright_red = "#de6145"
bright_yellow = "#c9c2b4"`

test("eighth-block meters are exact", () => {
  const c = new Canvas(10, 1)
  c.clear(0, 0xffffff)
  hbar(c, 0, 0, 10, 0.55, { track: 1, color: () => 2 })
  assert.equal(c.toText(), "█████▌")
  hbar(c, 0, 0, 10, 0, { track: 1, color: () => 2 })
  assert.equal(c.toText(), "")
})

test("sparklines, fitting and wide characters", () => {
  assert.deepEqual(spark([0, 1, 8]), [" ", "▁", "█"])
  assert.equal(fit("abcdefgh", 5), "abcd…")
  assert.equal(textWidth("漢字"), 4)
  const c = new Canvas(6, 1)
  c.clear(0, 0)
  c.text(0, 0, "漢字ab", 1)
  assert.equal(c.toText(), "漢字ab")
  c.text(1, 0, "x", 1) // overwrite half of a wide character
  assert.equal(c.toText(), " x字ab")
})

test("braille raster", () => {
  const b = new Braille(2, 1)
  b.column(0, 1)
  const c = new Canvas(2, 1)
  c.clear(0, 0)
  b.draw(c, 0, 0, () => 1)
  assert.equal(c.toText(), "⡇")
})

test("big font measurement covers the hero glyphs", () => {
  assert.ok(fontWidth("100%", "block") > 20)
})

// Decode a big glyph's half blocks back to pixels (6 rows: the top gap and 5 drawn).
function pixels(ch: string): boolean[][] {
  const g = bigGlyph(ch)!
  return g.flatMap((row) => [[...row].map((b) => b === "█" || b === "▀"), [...row].map((b) => b === "█" || b === "▄")])
}

// Enclosed background regions (4-connected, so a rounded corner does not leak).
function holes(px: boolean[][]): number {
  const h = px.length + 2
  const w = px[0].length + 2
  const lit = (y: number, x: number) => px[y - 1]?.[x - 1] ?? false
  const seen = new Set<number>()
  const fill = (y0: number, x0: number) => {
    const stack = [[y0, x0]]
    while (stack.length) {
      const [y, x] = stack.pop()!
      if (y < 0 || x < 0 || y >= h || x >= w || lit(y, x) || seen.has(y * w + x)) continue
      seen.add(y * w + x)
      stack.push([y + 1, x], [y - 1, x], [y, x + 1], [y, x - 1])
    }
  }
  fill(0, 0)
  let n = 0
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) if (!lit(y, x) && !seen.has(y * w + x)) { n++; fill(y, x) }
  return n
}

test("big digits are five pixels tall and each one is distinct", () => {
  const digits = [..."0123456789"]
  for (const d of digits) {
    const g = bigGlyph(d)!
    assert.equal(g.length, BIG_ROWS)
    assert.ok(g.every((row) => row.length === g[0].length), `${d} rows differ in width`)
    const px = pixels(d)
    assert.ok(px[0].every((on) => !on), `${d} must leave the top half row empty`)
    assert.ok(px[5].some(Boolean) && px[1].some(Boolean), `${d} must span all five pixel rows`)
  }
  // The shapes people tell digits apart by: 8 has two loops, 0 6 9 one, the rest none.
  assert.deepEqual(digits.map((d) => holes(pixels(d))), [1, 0, 0, 0, 0, 0, 1, 0, 2, 1])
  for (const a of digits) for (const b of digits) {
    if (a >= b) continue
    const pa = pixels(a).flat()
    const pb = pixels(b).flat()
    const diff = pa.reduce((n, on, i) => n + (on !== (pb[i] ?? false) ? 1 : 0), Math.abs(pa.length - pb.length))
    assert.ok(diff >= 2, `${a} and ${b} differ by only ${diff} pixel(s)`)
  }
})

test("big numbers draw digits as glyphs and units as text on the baseline", () => {
  const c = new Canvas(24, 3)
  c.clear(0, 0)
  const w = drawBig(c, 0, 0, "11.1M", 1)
  assert.equal(w, bigWidth("11.1M"))
  assert.equal(c.toText(), [" ▄   ▄     ▄", "▀█  ▀█    ▀█", "▄█▄ ▄█▄ ▄ ▄█▄ M"].join("\n"))
  assert.equal(bigWidth("100%"), 3 + 1 + 4 + 1 + 4 + 1 + 1)
})

test("the Solitude theme keeps its single ember as the hot color", () => {
  const p = derivePalette(parseColorsToml(SOLITUDE))
  assert.equal(p.hot, 0xde6145)
  assert.ok(chroma(p.warn) > 40, "warn should be a tint of the hot color, not gray")
  assert.ok(contrast(p.fg, p.bg) > 7)
  assert.ok(p.dark)
})

test("a missing theme falls back to a usable palette, light themes too", () => {
  const p = derivePalette({})
  assert.ok(contrast(p.fg, p.bg) > 4.5)
  const light = derivePalette({ mode: "light", background: "#fafafa", foreground: "#222222", accent: "#3366cc", red: "#cc3333" })
  assert.equal(light.dark, false)
  assert.equal(light.hot, 0xcc3333)
})

test("usage limits: percent, pace and expiry", () => {
  const now = Date.parse("2026-11-12T00:00:00Z")
  const p = parseProvider({ id: "claude", name: "Claude Code", ready: true, todayTotalTokens: 5,
    limits: [{ label: "Session (5-hour)", percent: 0.6, resetsAt: "2026-11-12T02:30:00Z" },
      { label: "Weekly (7-day)", percent: 0.4, resetsAt: "2026-11-11T00:00:00Z" }] }, now)!
  assert.equal(windowLength("Session (5-hour)"), 5 * 3_600_000)
  assert.equal(p.limits[0].percent, 0.6)
  assert.equal(p.limits[0].elapsed, 0.5)
  assert.equal(p.limits[1].percent, 0, "a window that already reset reads as unused")
  assert.equal(parseProvider({ id: "x", ready: true, limits: [] }), null)
})

test("formatting", () => {
  assert.equal(tokens(1_234_567), "1.2M")
  assert.equal(tokens(950), "950")
  assert.equal(shortModel("claude-opus-5-5"), "opus 5.5")
})

function fixture(now: number): Agent[] {
  const iso = (offset: number) => new Date(now - offset).toISOString()
  const threads = [
    { id: "a", projectId: "p", title: "Refactor the renderer with a very long descriptive title", activeProviderThreadId: "pa" },
    { id: "b", projectId: "p", title: "Waiting thread", activeProviderThreadId: "pb" },
    { id: "c", projectId: "p", title: "Done thread 漢字", activeProviderThreadId: "pc" },
  ]
  const runs = new Map<string, any>([
    ["a", { id: "ra", status: "running", providerThreadId: "pa", startedAt: iso(600_000) }],
    ["b", { id: "rb", status: "running", providerThreadId: "pb", startedAt: iso(60_000) }],
    ["c", { id: "rc", status: "completed", providerThreadId: "pc", startedAt: iso(9e6), completedAt: iso(8e6) }],
  ])
  const subs = [{ id: "s", threadId: "a", parentNodeId: "n", runId: "ra", providerThreadId: "ps", status: "running", title: "Sub", startedAt: iso(120_000) }]
  const turns = new Map<string, any>([
    ["pa", { model: "claude-opus-5-5", tokenUsage: { usedTokens: 820_000, maxTokens: 1_000_000, cachedInputTokens: 800_000, inputTokens: 810_000,
      outputTokens: 2000, promptCacheTtlMs: 3_600_000, promptCacheExpiresAt: iso(-1_800_000) } }],
    ["pc", { turnTokenUsage: { inputTokens: 5000, cachedInputTokens: 4000, outputTokens: 100, usageStatus: "complete" },
      tokenUsage: { usedTokens: 950, maxTokens: 1000 } }],
  ])
  const items: any[] = Array.from({ length: 30 }, (_, i) => ({
    thread_id: "a", run_id: "ra", node_id: null, provider_thread_id: "pa", type: i % 3 ? "command_execution" : "file_change",
    status: i === 0 ? "running" : i === 4 ? "failed" : "completed", updated_at: iso(i * 20_000), started_at: iso(i * 20_000 + 1000),
    label: "", command: "npm test --\x1b[31m color", file: "/home/x/file.ts", tool: null,
  }))
  const agents = buildAgents(threads, runs, subs, new Map(), turns, new Map(), new Map(),
    [{ thread_id: "b", node_id: null, kind: "user_input" }], items)
  const a = agents.find((x) => x.id === "a")!
  a.events = Array.from({ length: 80 }, (_, i) => ({ t: now - i * 15_000, cat: i % 7 ? "shell" : "you", failed: i === 9 }))
  a.spans = [[now - 600_000, null]]
  a.history = Array.from({ length: 12 }, (_, i) => ({ pct: i / 12, used: i * 80_000, max: 1e6, at: now - (12 - i) * 60_000,
    provider: i < 6 ? "old" : "pa", model: "claude-opus-5-5" }))
  a.plan = [{ text: "Port data layer", status: "completed" }, { text: "Draw lanes", status: "in_progress" }, { text: "Ship", status: "pending" }]
  return agents
}

test("the whole dashboard draws at every size without overflow", () => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const agents = fixture(now)
  const usage = [parseProvider({ id: "claude", name: "Claude Code", tierLabel: "Max", ready: true, todayTotalTokens: 8e6,
    limits: [{ label: "Session (5-hour)", percent: 0.93, resetsAt: new Date(now + 3_600_000).toISOString() }] }, now)!]
  const palette = derivePalette(parseColorsToml(SOLITUDE))
  for (const [w, h] of [[40, 10], [48, 14], [60, 20], [85, 60], [120, 40], [140, 45], [200, 50], [330, 70]]) {
    for (const help of [false, true]) {
      const st = initialState(2)
      st.bootAt = 0
      st.help = help
      st.selected = "a"
      const c = new Canvas(w, h)
      const frame = draw(c, st, { agents, at: now, path: "" }, usage, palette, now)
      const text = c.toText()
      assert.ok(text.split("\n").every((line) => textWidth(line) <= w), `${w}x${h} overflow`)
      assert.ok(!text.includes("\x1b"), "escape sequences must never reach the screen")
      if (w >= 48 && h >= 14 && !help) {
        assert.match(text, /AGENTS/)
        assert.ok(frame.rows.length === 4)
        assert.equal(frame.beat, true, "a working agent keeps the spinner beat")
      }
      if (w >= 85 && h >= 40 && !help) {
        assert.match(text, /NEEDS YOU/)
        assert.match(text, /CONTEXT/)
        assert.match(text, /ACTIVITY/)
        assert.match(text, /PLAN/)
      }
    }
  }
})

test("the thread open in T3 Code is selected when it changes and marked with its subagents", () => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const agents = fixture(now)
  const palette = derivePalette(parseColorsToml(SOLITUDE))
  const st = initialState(2)
  st.bootAt = 0
  st.collapsed.add("a")
  const c = new Canvas(140, 45)
  const frame = (title: string | null) => {
    st.t3Title = title
    return draw(c, st, { agents, at: now, path: "" }, [], palette, now)
  }
  const a = agents.find((x) => x.id === "a")!
  let f = frame(`${a.project} / ${a.title}`)
  assert.equal(st.selected, "a")
  assert.equal(st.collapsed.has("a"), false, "unfolded so its subagents show")
  assert.deepEqual([...f.linked].sort(), ["a", "s"])
  const subRow = f.hits.find((h) => h.id === "s" && h.area === "list")!
  assert.equal(c.toText().split("\n")[subRow.y][0], "▎", "the subagent carries the link rule")

  st.selected = "c" // picking another row here sticks while T3 stays on the same thread
  f = frame(`${a.project} / ${a.title}`)
  assert.equal(st.selected, "c")
  assert.deepEqual([...f.linked].sort(), ["a", "s"])

  f = frame("Waiting thread")
  assert.equal(st.selected, "b")
  st.follow = false
  f = frame(`${a.project} / ${a.title}`)
  assert.equal(st.selected, "b", "follow off: highlight only")
  assert.ok(f.linked.has("a"))
  f = frame("T3 Code (Nightly)")
  assert.equal(f.linked.size, 0)
})

test("quiet agents stop spinning and say how long; slow tool calls show their time", () => {
  const start = Date.parse("2026-11-12T12:00:00Z")
  const agents = fixture(start)
  const later = start + 10 * 60_000 // nothing new for ten minutes
  const palette = derivePalette(parseColorsToml(SOLITUDE))
  const st = initialState(2)
  st.bootAt = 0
  st.selected = "s"
  const c = new Canvas(140, 45)
  const frame = draw(c, st, { agents, at: later, path: "" }, [], palette, later)
  assert.equal(frame.paces.get("s")?.stalled, true, "the subagent has been quiet since it started")
  assert.equal(frame.paces.get("a")?.quiet, false, "its parent is waiting on it, not quiet")
  assert.ok(frame.paces.get("a")?.tool, "the parent has a tool call in flight")
  const lines = c.toText().split("\n")
  const row = (id: string) => lines[frame.hits.find((h) => h.id === id && h.area === "list")!.y]
  assert.match(row("s"), /◌ .*quiet 1\dm/)
  assert.match(c.toText(), /\d quiet/, "the ACTIVE tile counts quiet agents")
  st.selected = "a"
  draw(c, st, { agents, at: later, path: "" }, [], palette, later)
  assert.match(c.toText(), /now  ± .* · 1\dm \d\ds/, "the tool call's running time")
  assert.match(c.toText(), /one call running over 5m/)
})

test("provider families come from the model, then the provider id", () => {
  assert.equal(family("claude-opus-5-5"), "claude")
  assert.equal(family("gpt-6.1-sol"), "openai")
  assert.equal(family("codex-auto-review"), "openai")
  assert.equal(family("grok-4.7-build-fast"), "grok")
  assert.equal(family("--", "provider-thread:provider:claudeAgent:native"), "claude")
  assert.equal(family("kimi-k3"), "other")
})

test("monochrome themes get the validated provider hues; chromatic themes keep their own", () => {
  const mono = derivePalette(parseColorsToml(SOLITUDE))
  assert.deepEqual([mono.provider.claude, mono.provider.openai, mono.provider.grok], [0x8b5896, 0x1f9b82, 0x4c94ec])
  for (const c of [mono.provider.claude, mono.provider.openai, mono.provider.grok]) assert.ok(contrast(c, mono.bg) >= 3)
  const tokyo = derivePalette({ background: "#1a1b26", foreground: "#c0caf5", red: "#f7768e", green: "#9ece6a", blue: "#7aa2f7", magenta: "#bb9af7" })
  assert.deepEqual([tokyo.provider.claude, tokyo.provider.openai, tokyo.provider.grok], [0xbb9af7, 0x9ece6a, 0x7aa2f7])
})

test("nothing moves on a quiet agent's rows", () => {
  const start = Date.parse("2026-11-12T12:00:00Z")
  const agents = fixture(start)
  const later = start + 10 * 60_000 // the subagent "s" has been quiet since it started
  const palette = derivePalette(parseColorsToml(SOLITUDE))
  const st = initialState(2)
  st.bootAt = 0
  st.selected = "a"
  const frames = [later, later + 300].map((t) => {
    const c = new Canvas(140, 45)
    const frame = draw(c, st, { agents, at: t, path: "" }, [], palette, t)
    const y = frame.hits.find((h) => h.id === "s" && h.area === "list")!.y
    const cells = (row: number) => Array.from({ length: c.w }, (_, x) => `${c.ch[row * c.w + x]}:${c.fg[row * c.w + x]}`).join(" ")
    return { quiet: frame.paces.get("s")!.quiet, rows: cells(y) + "\n" + cells(y + 1) }
  })
  assert.equal(frames[0].quiet, true)
  assert.equal(frames[0].rows, frames[1].rows)
})

test("made-up data says so in the header", () => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const st = initialState(2)
  st.bootAt = 0
  const c = new Canvas(120, 40)
  const palette = derivePalette(parseColorsToml(SOLITUDE))
  draw(c, st, { agents: fixture(now), at: now, path: "" }, [], palette, now)
  assert.doesNotMatch(c.toText().split("\n")[0], /DEMO/)
  st.demo = true
  draw(c, st, { agents: fixture(now), at: now, path: "" }, [], palette, now)
  assert.match(c.toText().split("\n")[0], /DEMO/)
})

test("the header names the product in plain text and says when calm mode is on", () => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const palette = derivePalette(parseColorsToml(SOLITUDE))
  const st = initialState(2)
  st.bootAt = 0
  const c = new Canvas(120, 40)
  draw(c, st, { agents: fixture(now), at: now, path: "" }, [], palette, now)
  const top = () => c.toText().split("\n")[0]
  assert.match(top(), /^ T3 Agents {2}● LIVE 2s/)
  for (let x = 0; x < 11; x++) assert.equal(c.bg[x], palette.panel, "no badge behind the name")
  assert.doesNotMatch(top(), /calm/)
  st.calm = true
  draw(c, st, { agents: fixture(now), at: now, path: "" }, [], palette, now)
  assert.match(top(), /LIVE 2s {2}calm/)
  st.help = true
  draw(c, st, { agents: fixture(now), at: now, path: "" }, [], palette, now)
  assert.match(c.toText(), /T3 Agents · help/)
  assert.match(c.toText(), /Unofficial; not affiliated with T3 Code or Omarchy\./)
})
