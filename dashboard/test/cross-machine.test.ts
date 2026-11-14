import assert from "node:assert/strict"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { DatabaseSync } from "node:sqlite"
import { test } from "node:test"
import { buildDemo } from "../src/demo.ts"
import { agentThreadLink, t3Thread } from "../src/focus.ts"
import { Source, PREFIX, buildAgents, tree, type Agent } from "../src/source.ts"

const NOW = Date.parse("2026-11-14T12:00:00Z")
const stamp = new Date(NOW).toISOString()
const ref = (environmentId = "machine-b", threadId = "parent") => ({ environmentId, threadId })
const root = { id: "root", title: "Root", settledOverride: "settled", updatedAt: stamp }
const leaf = { id: "leaf", title: "Local child", delegatedFrom: { ...ref(), title: "Remote parent" }, updatedAt: stamp }
const remote = { id: "remote-node", threadId: "root", title: "Remote parent", remoteChild: { ...ref(), label: "Machine B" },
  status: "completed", runId: "root-run", updatedAt: stamp }

function build(threads: any[], subs: any[] = [], extra: { runs?: Map<string, any>; requests?: any[]; items?: any[]; nodes?: Map<string, any>; env?: string | null } = {}): Agent[] {
  return buildAgents(threads, extra.runs ?? new Map(), subs, new Map(), new Map(), extra.nodes ?? new Map(),
    new Map(), extra.requests ?? [], extra.items ?? [], extra.env === undefined ? "machine-a" : extra.env)
}

const by = (agents: Agent[], id: string) => agents.find((a) => a.id === id)!
const rows = (agents: Agent[], scope: "all" | "active" | "t3" = "all") => tree(agents, new Set(), scope)

test("a local → remote → local chain keeps the true parents, native children and settlement values", () => {
  const threads = [root, { ...leaf, settledOverride: "settled", snoozedUntil: stamp }]
  const subs = [remote, { id: "native", threadId: "leaf", status: "running" }]
  const before = structuredClone({ threads, subs })
  const agents = build(threads, subs, { runs: new Map([["root", { id: "root-run", status: "completed" }], ["leaf", { status: "completed" }]]) })
  assert.deepEqual(rows(agents).map((r) => [r.id, r.depth]), [["root", 0], ["remote-node", 1], ["leaf", 2], ["native", 3]])
  assert.deepEqual(rows(agents, "active").map((r) => r.id), ["root", "remote-node", "leaf", "native"], "settled ancestors of live work stay observable")
  assert.equal(by(agents, "root").shelf?.settled, true)
  assert.deepEqual(by(agents, "leaf").shelf, { listed: true, settled: true, snoozedUntil: stamp })
  assert.equal(by(agents, "remote-node").shelf, undefined, "local delegation status does not claim remote settlement")
  assert.deepEqual({ threads, subs }, before, "no source payload is mutated")
  by(agents, "native").status = "completed"
  assert.deepEqual(rows(agents, "t3"), [], "a finished family follows its known root's shelf")
  by(agents, "root").shelf!.settled = false
  assert.equal(rows(agents, "t3").length, 4, "an open parent brings its completed children")
})

test("remote retries have one parent row and preserve activity and request attribution from every spawning node", () => {
  const old = { ...remote, id: "old", providerThreadId: "old-provider", updatedAt: "2026-11-13T12:00:00Z", status: "running" }
  const latest = { ...remote, providerThreadId: "new-provider" }
  const agents = build([root, leaf], [old, latest, { id: "grandchild", threadId: "root", parentNodeId: "tool", status: "completed" }], {
    nodes: new Map([["tool", { parent_node_id: "old" }]]),
    requests: [{ thread_id: "root", node_id: "old", kind: "approval" }, { thread_id: "root", provider_thread_id: "old-provider", kind: "user_input" }],
    items: [{ thread_id: "root", node_id: "old", run_id: "root-run", type: "command_execution", status: "completed", updated_at: stamp }],
  })
  assert.equal(agents.length, 4)
  assert.equal(by(agents, "leaf").parent, "remote-node")
  assert.equal(by(agents, "grandchild").parent, "remote-node")
  assert.equal(by(agents, "remote-node").status, "completed", "the newest delegation record supplies the status")
  assert.deepEqual(by(agents, "remote-node").requests, ["approval", "user_input"])
  assert.equal(by(agents, "remote-node").items.length, 1)
  assert.deepEqual(by(agents, "root").requests, [])
  assert.deepEqual(by(agents, "root").items, [])
})

test("identical thread ids on different machines never merge or borrow a local parent", () => {
  const agents = build([{ id: "parent", title: "Unrelated local thread" }, root, leaf,
    { id: "other-leaf", delegatedFrom: ref("machine-c") }], [remote, { ...remote, id: "other-remote", remoteChild: ref("machine-c") }])
  assert.equal(by(agents, "leaf").parent, "remote-node")
  assert.equal(by(agents, "other-leaf").parent, "other-remote")
  assert.equal(by(agents, "parent").parent, null)
  assert.equal(agentThreadLink(by(agents, "remote-node"), "machine-a"), "t3code://threads/machine-b/parent")
  assert.equal(agentThreadLink(by(agents, "other-remote"), "machine-a"), "t3code://threads/machine-c/parent")
})

test("an absent parent is one explicit reference with unknown state, and does not hide local work", () => {
  const agents = build([leaf, { ...leaf, id: "leaf-2", settledOverride: "settled" }])
  const parent = agents.find((a) => a.referenceOnly)!
  assert.equal(agents.length, 3)
  assert.equal(parent.status, "unknown")
  assert.equal(parent.shelf, undefined)
  assert.equal(parent.started, null)
  assert.equal(parent.updated, null)
  assert.deepEqual(parent.run, {})
  assert.equal(parent.title, "Remote parent")
  assert.equal(by(agents, "leaf").parent, parent.id)
  assert.equal(by(agents, "leaf-2").parent, parent.id)
  assert.equal(rows(agents, "t3").length, 3, "unknown remote settlement must not suppress local children")
  assert.equal(agentThreadLink(parent, null), "t3code://threads/machine-b/parent")
})

test("same-environment references resolve locally only when the database's environment is known", () => {
  const threads = [root, { ...leaf, delegatedFrom: ref("machine-a", "root") }]
  assert.equal(by(build(threads), "leaf").parent, "root")
  const unknown = build(threads, [], { env: null })
  assert.notEqual(by(unknown, "leaf").parent, "root")
  assert.equal(unknown.find((a) => a.referenceOnly)?.status, "unknown")
  const sameEnvironment = build(threads, [{ ...remote, remoteChild: ref("machine-a", "leaf") }])
  assert.equal(sameEnvironment.length, 2, "a local thread referenced by a spawning record is shown once")
})

test("cycles and incomplete references keep every real thread reachable", () => {
  const agents = build([{ id: "a", delegatedFrom: ref("machine-a", "b") },
    { id: "b", delegatedFrom: ref("machine-a", "a") },
    { id: "c", delegatedFrom: { environmentId: "machine-b" } }])
  assert.deepEqual(rows(agents).map((r) => r.id).sort(), ["a", "b", "c"])
  assert.equal(by(agents, "c").parent, null)
})

test("remote parents can be followed and opened; local native agents still open their owning thread", () => {
  const agents = build([root, leaf], [remote, { id: "native", threadId: "leaf", title: "Native agent" }])
  const parent = by(agents, "remote-node")
  assert.equal(t3Thread("Remote parent", agents), parent.id)
  assert.equal(t3Thread("Native agent", agents), null)
  assert.equal(agentThreadLink(parent, "machine-a"), "t3code://threads/machine-b/parent")
  assert.equal(agentThreadLink(by(agents, "leaf"), "machine-a"), "t3code://threads/machine-a/leaf")
  assert.equal(agentThreadLink(by(agents, "native"), "machine-a"), "t3code://threads/machine-a/leaf")
})

test("the SQLite adapter reads only parent identity metadata and leaves the database unchanged", () => {
  const dir = mkdtempSync(join(tmpdir(), "t3a-cross-machine-"))
  const path = join(dir, "statev2.sqlite")
  try {
    buildDemo(path, NOW, NOW)
    writeFileSync(join(dir, "environment-id"), "machine-a\n")
    const db = new DatabaseSync(path)
    const put = db.prepare(`INSERT INTO ${PREFIX}threads (thread_id,payload_json) VALUES (?,?)`)
    put.run("cross-root", JSON.stringify({ id: "cross-root", title: "Cross root" }))
    put.run("cross-leaf", JSON.stringify({ id: "cross-leaf", title: "Cross leaf", settledOverride: "settled",
      delegatedFrom: { ...ref(), title: "Parent", prompt: "SECRET-prompt" } }))
    db.prepare(`INSERT INTO ${PREFIX}subagents (subagent_id,thread_id,payload_json) VALUES (?,?,?)`).run("cross-node", "cross-root",
      JSON.stringify({ ...remote, id: "cross-node", threadId: "cross-root", remoteChild: { ...ref(), label: "Machine B", title: "SECRET-remote-title" } }))
    db.close()
    const before = readFileSync(path)
    const snap = new Source(path).snapshot(NOW)
    assert.equal(by(snap.agents, "cross-leaf").parent, "cross-node")
    assert.equal(by(snap.agents, "cross-node").parent, "cross-root")
    assert.equal(by(snap.agents, "cross-leaf").shelf?.settled, true)
    assert.doesNotMatch(JSON.stringify(snap), /SECRET/)
    assert.deepEqual(readFileSync(path), before)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
