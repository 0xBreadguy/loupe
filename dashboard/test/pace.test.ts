// Stall detection: busy in a tool, working, or quiet.
import assert from "node:assert/strict"
import { test } from "node:test"
import { QUIET_MS, SLOW_TOOL_MS, STALL_MS, pace, type Agent, type Item } from "../src/source.ts"

const NOW = Date.parse("2026-11-13T12:00:00Z")
const ago = (ms: number) => new Date(NOW - ms).toISOString()
const item = (type: string, status: string, startedAgo: number, extra: Partial<Item> = {}): Item =>
  ({ type, status, started_at: ago(startedAgo), updated_at: ago(startedAgo), tool: null, label: "", command: "", file: "",
    thread_id: "t", run_id: "r", node_id: null, provider_thread_id: null, ...extra }) as Item
const agent = (items: Item[], extra: Partial<Agent> = {}): Agent => {
  const now = items.find((i) => i.status === "running") ?? null
  return { id: "t", thread_id: "t", status: "running", requests: [], started: ago(60 * 60_000), items, now, events: [],
    ...extra } as Agent
}

test("a tool call in flight is timed and turns warm past the slow threshold", () => {
  const short = pace(agent([item("command_execution", "running", 60_000)]), NOW)!
  assert.equal(short.toolMs, 60_000)
  assert.equal(short.stalled, false)
  assert.equal(short.quiet, false)
  const long = pace(agent([item("command_execution", "running", SLOW_TOOL_MS + 1000)]), NOW)!
  assert.equal(long.stalled, true)
})

test("waiting on a subagent is delegation, never a stall", () => {
  const viaTool = pace(agent([item("dynamic_tool", "running", 30 * 60_000, { tool: "Agent" })]), NOW)!
  assert.equal(viaTool.delegating, true)
  assert.equal(viaTool.stalled, false)
  const viaChildren = pace(agent([item("command_execution", "completed", 30 * 60_000)]), NOW, true)!
  assert.equal(viaChildren.quiet, false)
})

test("no tool in flight and nothing new: quiet, then warm", () => {
  const working = pace(agent([item("command_execution", "completed", 30_000)]), NOW)!
  assert.equal(working.quiet, false)
  const quiet = pace(agent([item("command_execution", "completed", QUIET_MS + 1000)]), NOW)!
  assert.equal(quiet.quiet, true)
  assert.equal(quiet.stalled, false)
  const stalled = pace(agent([item("command_execution", "completed", STALL_MS + 1000)]), NOW)!
  assert.equal(stalled.stalled, true)
  // Recent activity anywhere (a prompt you sent, a run starting) resets the clock.
  assert.equal(pace(agent([item("command_execution", "completed", STALL_MS)], { events: [{ t: NOW - 5000, cat: "you", failed: false }] }), NOW)!.quiet, false)
  assert.equal(pace(agent([], { started: ago(10_000) }), NOW)!.quiet, false)
})

test("thinking is work, and only running agents have a pace", () => {
  assert.equal(pace(agent([item("reasoning", "running", STALL_MS * 2)]), NOW)!.quiet, false)
  assert.equal(pace(agent([], { status: "completed" }), NOW), null)
  assert.equal(pace(agent([], { requests: ["user_input"] }), NOW), null, "waiting on you is not a stall")
})
