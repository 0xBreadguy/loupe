// The status contract the Omarchy bar widget reads.
import assert from "node:assert/strict"
import { test } from "node:test"
import { buildAgents, type Snapshot, type Source } from "../src/source.ts"
import { line, status, watch } from "../src/status.ts"

function snapshot(now: number): Snapshot {
  const iso = (offset: number) => new Date(now - offset).toISOString()
  const threads = [
    { id: "a", projectId: "p", title: "Running\x1b[31m one", activeProviderThreadId: "pa" },
    { id: "b", projectId: "p", title: "Waiting one", activeProviderThreadId: "pb" },
    { id: "c", projectId: "p", title: "Done", activeProviderThreadId: "pc" },
  ]
  const runs = new Map<string, any>([
    ["a", { id: "ra", status: "running", providerThreadId: "pa", startedAt: iso(60_000) }],
    ["b", { id: "rb", status: "running", providerThreadId: "pb", startedAt: iso(30_000) }],
    ["c", { id: "rc", status: "completed", providerThreadId: "pc" }],
  ])
  const turns = new Map<string, any>([["pa", { model: "claude-opus-5-5", tokenUsage: { usedTokens: 123_456, maxTokens: 1_000_000 } }]])
  const agents = buildAgents(threads, runs, [], new Map(), turns, new Map(), new Map(),
    [{ thread_id: "b", node_id: null, kind: "user_input" }], [])
  agents[0].events = [{ t: now - 60_000, cat: "shell", failed: true }, { t: now - 3_600_000, cat: "shell", failed: true }]
  return { agents, at: now, path: "" }
}

test("status counts agents and lists the ones that need you first", () => {
  const now = Date.parse("2026-11-12T12:00:00Z")
  const s = status(snapshot(now), now)
  assert.equal(s.v, 1)
  assert.equal(s.running, 1)
  assert.equal(s.waiting, 1)
  assert.equal(s.failed, 1, "only failures in the last ten minutes count")
  assert.deepEqual(s.agents.map((a) => a.state), ["waiting", "running"])
  assert.equal(s.agents[1].ctx, 0.12)
  assert.equal(s.agents[1].model, "opus 5.5")
  assert.equal(s.agents[1].family, "claude")
  assert.ok(!s.agents[1].title.includes("\x1b"))
})

test("a source error becomes an error line instead of a crash", () => {
  const broken = { path: "/nonexistent", snapshot: () => { throw new Error("database is locked") } } as unknown as Source
  assert.deepEqual(JSON.parse(line(broken)), { v: 1, running: 0, waiting: 0, failed: 0, agents: [], error: "database is locked" })
})

test("watch prints only when the status changes", () => {
  let calls = 0
  const now = Date.now()
  const fake = { path: "/nonexistent/statev2.sqlite", snapshot: () => (calls++, snapshot(now)) } as unknown as Source
  const out: string[] = []
  const timer = watch(fake, (s) => void out.push(s), 60_000)
  clearInterval(timer)
  assert.equal(calls, 1)
  assert.equal(out.length, 1)
  assert.ok(out[0].endsWith("\n"))
  assert.equal(JSON.parse(out[0]).waiting, 1)
})
