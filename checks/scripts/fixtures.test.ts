import assert from "node:assert/strict"
import { test } from "node:test"
import { edgeSnapshot, EPOCH, largeSnapshot, rapidSnapshot, snapshot, SPEC } from "./fixtures.ts"
import { actionName, facts, replay } from "./oracle.ts"

test("fixtures are synthetic, controlled-time and repeatable", () => {
  for (const name of ["empty", "error", "edge", "large", "rapid"]) {
    assert.deepEqual(snapshot(name), snapshot(name), name)
    assert.ok(snapshot(name).path.startsWith("synthetic:"))
  }
  assert.equal(snapshot("demo").agents.length > 10, true)
  assert.deepEqual(snapshot("demo"), snapshot("demo"))
  assert.equal(edgeSnapshot().at, EPOCH)
  assert.equal(largeSnapshot(32).agents.length, 32)
})

test("edge case exercises earlier descendants, quiet, needs-you, errors and Unicode", () => {
  const snap = edgeSnapshot()
  const result = replay(snap)
  const earlier = result.frame.rows.find((r) => r.id === "earlier:edge-root")!
  assert.ok(earlier.earlier!.some((a) => a.id === "edge-great"), "complete hidden subtree")
  assert.ok(facts(snap).quiet.includes("edge-quiet"))
  assert.ok(facts(snap).waiting.includes("edge-wait"))
  assert.match(snap.agents[0].title, /日本語.*🍞/u)
  assert.match(replay(snapshot("error"), [], 150, 70, "synthetic schema failure").text, /SOURCE ERROR/)
  assert.equal(replay(snapshot("empty")).frame.rows.length, 0)
})

test("scripted actions remain deterministic and restore all scope", () => {
  const snap = edgeSnapshot()
  const actions = SPEC.actions.map(actionName)
  const result = replay(snap, actions)
  assert.deepEqual(result.frames, replay(snap, actions).frames)
  assert.equal(result.state.scope, "all")
  assert.ok(result.state.expanded.has("edge-root"))
  assert.ok(result.frames.some((f) => f.scope === "active"))
  assert.ok(result.frames.some((f) => f.scope === "t3"))
  assert.ok(replay(snap, [], 60, 24).text.length > 0)
  assert.match(replay(snap, [], 30, 10).text, /Enlarge/)
})

test("rapid updates change data and time rather than reusing a no-op", () => {
  const first = rapidSnapshot(0)
  const last = rapidSnapshot(SPEC.rapid.updates - 1)
  assert.equal(last.at - first.at, (SPEC.rapid.updates - 1) * SPEC.rapid.stepMs)
  const working = (s: typeof first) => s.agents.find((a) => a.id === "edge-working")!
  assert.notEqual(working(first).context.usedTokens, working(last).context.usedTokens)
  assert.notDeepEqual(working(first).events, working(last).events)
})
