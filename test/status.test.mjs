// Status.js, the bar widget's only logic, checked without Quickshell.
// Run: node --test test/
import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"

const source = readFileSync(new URL("../Status.js", import.meta.url), "utf8").replace(/^\.pragma library\s*$/m, "")
const S = new Function(source + "\nreturn { plain, parse, unavailable, frameAt, dots, spinDots, litDots, working, cardSpins, SPIN }")()

const line = (extra = {}) => JSON.stringify({ v: 1, running: 2, waiting: 1, failed: 0,
  agents: [{ title: "Fix the build", state: "waiting", ctx: 0.364, model: "opus 5.5" },
    { title: "Refactor", state: "running", ctx: null, model: "gpt 6.1-sol" }], ...extra })

test("a status line parses into bounded values", () => {
  const s = S.parse(line())
  assert.equal(s.running, 2)
  assert.equal(s.waiting, 1)
  assert.equal(s.error, "")
  assert.equal(s.agents.length, 2)
  assert.deepEqual(s.agents.map(a => [a.title, a.state, a.model, a.ctx]),
    [["Fix the build", "waiting", "opus 5.5", 0.364], ["Refactor", "running", "gpt 6.1-sol", null]])
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
  assert.equal(e.cause, "watcher")
})

test("an unavailable status keeps why, bounded", () => {
  const launcher = S.unavailable("cannot start /home/u/.local/bin/t3-agents", "launcher")
  assert.equal(launcher.cause, "launcher")
  assert.equal(launcher.code, null)
  const exited = S.unavailable("exit code 127", "exited", 127)
  assert.equal(exited.cause, "exited")
  assert.equal(exited.code, 127)
  assert.equal(S.unavailable("x", "<b>").cause, "watcher", "unknown causes fall back")
  assert.equal(S.unavailable("x", "exited", "9").code, null, "only numeric codes")
  assert.ok(S.unavailable("y".repeat(500)).error.length <= 120)
  assert.ok(S.unavailable("").error, "always carries an error, so it never reads as idle")
})

test("counts and lists are bounded", () => {
  const many = Array.from({ length: 40 }, (_, i) => ({ title: "x".repeat(200) + i, state: "running", ctx: 0.1 }))
  const s = S.parse(line({ running: 1e9, waiting: -3, agents: many }))
  assert.equal(s.running, 999)
  assert.equal(s.waiting, 0)
  assert.equal(s.agents.length, 8)
  assert.ok(s.agents[0].title.length <= 60)
})

test("the spinner follows the shared 100 ms beat", () => {
  assert.equal(S.SPIN.join(""), "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
  assert.equal(S.frameAt(0), 0)
  assert.equal(S.frameAt(99), 0)
  assert.equal(S.frameAt(100), 1)
  assert.equal(S.frameAt(1_000), 0, "ten frames per second")
  const now = 1_763_000_000_123
  assert.equal(S.frameAt(now), Math.floor(now / 100) % 10)
})

test("provider family and activity are kept, and unknown values fall back", () => {
  const s = S.parse(line({ agents: [
    { title: "a", state: "running", family: "claude", now: "think" },
    { title: "b", state: "running", family: "<b>evil</b>", now: "<script>" },
    { title: "c", state: "running", family: "toString", now: "constructor" },
    { title: "d", state: "running", now: null },
    { title: "e", state: "running" },
  ] }))
  const [a, b, c, d, e] = s.agents
  assert.equal(a.family, "claude")
  assert.equal(a.now, "think")
  assert.equal(b.family, "other", "an unknown family takes the muted hue")
  assert.equal(b.now, "other", "an unknown activity is still activity, not quiet")
  assert.equal(c.family, "other", "inherited object keys are not families")
  assert.equal(c.now, "other")
  assert.equal(d.now, null, "null means the agent has gone quiet")
  assert.equal(e.family, "other")
  assert.equal(e.now, null)
  for (const f of ["claude", "openai", "grok", "other"])
    assert.equal(S.parse(line({ agents: [{ title: "x", family: f }] })).agents[0].family, f)
  for (const n of ["shell", "read", "edit", "web", "agent", "ask", "think", "reply", "you", "note", "tool", "other"])
    assert.equal(S.parse(line({ agents: [{ title: "x", now: n }] })).agents[0].now, n)
})

test("braille glyphs become dot masks", () => {
  assert.equal(S.dots("⠀"), 0)
  assert.equal(S.dots("⣿"), 0xff, "all eight")
  assert.equal(S.dots("⠿"), 0x3f, "six dots")
  assert.equal(S.dots("⠋"), 0b1011, "dots 1, 2 and 4")
  assert.equal(S.dots("x"), 0, "anything else lights nothing")
  assert.equal(S.dots(""), 0)
  for (const g of S.SPIN) {
    const n = S.dots(g).toString(2).replace(/0/g, "").length
    assert.ok(n >= 3 && n <= 4, `${g} lights a few dots, never the whole cell`)
  }
})

test("the bar's cell lights dots only while agents run and none need you", () => {
  const running = S.parse(line({ waiting: 0 }))
  assert.equal(S.litDots(running, 0, true), S.dots("⠋"))
  assert.equal(S.litDots(running, S.frameAt(250), true), S.dots("⠹"), "on the shared beat")
  assert.equal(S.litDots(S.parse(line({ running: 0, waiting: 0, agents: [] })), 3, true), 0, "idle lights nothing")
  assert.equal(S.litDots(S.parse(line()), 3, true), 0, "waiting shows the diamond, not the cell")
  assert.equal(S.litDots(null, 3, true), 0, "starting lights nothing")
  assert.equal(S.litDots(S.unavailable("x"), 3, true), 0)
})

test("with animations off a running cell holds one frame that still differs from idle", () => {
  const running = S.parse(line({ waiting: 0 }))
  const still = S.litDots(running, 0, false)
  assert.notEqual(still, 0, "not idle")
  assert.notEqual(still, 0xff, "not the whole cell either")
  for (let f = 0; f < 20; f++) assert.equal(S.litDots(running, f, false), still, "it doesn't move")
  assert.equal(S.spinDots(7, false), still)
})

test("only agents with a current activity count as working", () => {
  const s = S.parse(line({ agents: [
    { title: "a", state: "running", now: "think" },
    { title: "b", state: "running", now: null },
    { title: "c", state: "waiting", now: "ask" },
    { title: "d", state: "queued", now: "tool" },
    { title: "e", state: "pending", now: "tool" },
    { title: "f", state: "active", now: "teleport" },
  ] }))
  assert.deepEqual(s.agents.map(S.working), [true, false, false, false, false, true])
})

test("an open card ticks only when something in it spins", () => {
  assert.equal(S.cardSpins(S.parse(line({ waiting: 0 }))), true, "the header's cell spins while agents run")
  const waitingAndQuiet = S.parse(line({ agents: [{ title: "a", state: "waiting", now: "ask" }, { title: "b", state: "running", now: null }] }))
  assert.equal(S.cardSpins(waitingAndQuiet), false, "a diamond and a quiet ring don't move")
  const waitingAndWorking = S.parse(line({ agents: [{ title: "a", state: "waiting", now: "ask" }, { title: "b", state: "running", now: "edit" }] }))
  assert.equal(S.cardSpins(waitingAndWorking), true)
  assert.equal(S.cardSpins(S.parse(line({ running: 0, waiting: 0, agents: [] }))), false, "idle")
  assert.equal(S.cardSpins(null), false, "starting")
  assert.equal(S.cardSpins(S.unavailable("x")), false, "broken")
})
