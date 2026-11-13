// Status.js, the bar widget's only logic, checked without Quickshell.
// Run: node --test test/
import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"

const source = readFileSync(new URL("../Status.js", import.meta.url), "utf8").replace(/^\.pragma library\s*$/m, "")
const S = new Function(source + "\nreturn { plain, parse, glyph, agentLine, unavailable }")()

const line = (extra = {}) => JSON.stringify({ v: 1, running: 2, waiting: 1, failed: 0,
  agents: [{ title: "Fix the build", state: "waiting", ctx: 0.364, model: "opus 5.5" },
    { title: "Refactor", state: "running", ctx: null, model: "gpt 6.1-sol" }], ...extra })

test("a status line parses into bounded values", () => {
  const s = S.parse(line())
  assert.equal(s.running, 2)
  assert.equal(s.waiting, 1)
  assert.equal(s.error, "")
  assert.equal(s.agents.length, 2)
  assert.equal(S.agentLine(s.agents[0]), "◆ Fix the build · opus 5.5 · 36%")
  assert.equal(S.agentLine(s.agents[1]), "⠿ Refactor · gpt 6.1-sol")
})

test("markup-shaped and control text is neutralized before it reaches a tooltip", () => {
  const s = S.parse(line({ agents: [{ title: '<img src="https://example.invalid/t">\u202e\x1b[31mevil\n', state: "<b>", ctx: 9, model: "<i>m</i>" }] }))
  const a = s.agents[0]
  assert.ok(!/[<>]/.test(a.title + a.model), a.title)
  assert.ok(!/[\u0000-\u001f\u202e]/.test(a.title))
  assert.equal(a.state, "running", "unknown states fall back")
  assert.equal(a.ctx, 1, "context is clamped")
})

test("unusable lines are rejected and errors become an unavailable status", () => {
  assert.equal(S.parse("not json"), null)
  assert.equal(S.parse(JSON.stringify({ v: 2, running: 1 })), null)
  assert.equal(S.parse("null"), null)
  const e = S.parse(JSON.stringify({ v: 1, error: "Unsupported T3 schema" }))
  assert.equal(e.running, 0)
  assert.equal(e.error, "Unsupported T3 schema")
})

test("counts and lists are bounded", () => {
  const many = Array.from({ length: 40 }, (_, i) => ({ title: "x".repeat(200) + i, state: "running", ctx: 0.1 }))
  const s = S.parse(line({ running: 1e9, waiting: -3, agents: many }))
  assert.equal(s.running, 999)
  assert.equal(s.waiting, 0)
  assert.equal(s.agents.length, 8)
  assert.ok(s.agents[0].title.length <= 60)
})

test("the bar mark follows the most important state", () => {
  const s = S.parse(line())
  assert.equal(S.glyph(s, 3, true), "◆")
  const running = S.parse(line({ waiting: 0 }))
  assert.equal(S.glyph(running, 2, true), "⠹")
  assert.equal(S.glyph(running, 2, false), "⠿", "no spinner when animations are off")
  assert.equal(S.glyph(S.unavailable("x"), 0, true), "⠿")
  assert.equal(S.glyph(null, 0, true), "⠿")
})
