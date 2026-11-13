// Agent attribution and read-only source behavior (ported from the v1 suite).
import assert from "node:assert/strict"
import { mkdtempSync, readFileSync, existsSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { DatabaseSync } from "node:sqlite"
import { test } from "node:test"
import { clean } from "../src/canvas.ts"
import { Source, buildAgents, category, tree, type Agent } from "../src/source.ts"

function snapshot(childThread = false): Agent[] {
  const threads: any[] = [{ id: "root", projectId: "p", title: "Root", activeProviderThreadId: "pr" }]
  const runs = new Map<string, any>([["root", { id: "r", status: "running", providerThreadId: "pr" }]])
  const subs: any[] = [{ id: "sub", threadId: "root", parentNodeId: "nroot", runId: "r", providerThreadId: "ps", status: "running" }]
  if (childThread) {
    threads.push({ id: "child", title: "Child", projectId: "p" })
    runs.set("child", { id: "rc", status: "running", providerThreadId: "ps" })
    subs[0].childThreadId = "child"
  }
  const nodes = new Map<string, any>([["nroot", { parent_node_id: null }], ["ntool", { parent_node_id: "sub" }]])
  const providers = new Map<string, any>([["pr", { contextUsage: { usedTokens: 10, maxTokens: 100 } }],
    ["ps", { contextUsage: { usedTokens: 20, maxTokens: 200 } }]])
  const turns = new Map<string, any>([["pr", { tokenUsage: { usedTokens: 30, maxTokens: 100 } }],
    ["ps", { tokenUsage: { usedTokens: 40, maxTokens: 200 } }]])
  const items: any[] = [{ node_id: "ntool", provider_thread_id: "ps", thread_id: "root", run_id: childThread ? "rc" : "r",
    updated_at: "2026-11-12T00:00:00Z", type: "command_execution", status: "completed", label: "", command: "ls", file: "", tool: null, started_at: null }]
  const requests = [{ node_id: "ntool", thread_id: "root", kind: "user_input" }]
  return buildAgents(threads, runs, subs, providers, turns, nodes, new Map(), requests, items)
}

test("native agent context, activity and requests do not bleed to the parent", () => {
  const [root, sub] = snapshot()
  assert.equal(root.context.usedTokens, 30)
  assert.equal(sub.context.usedTokens, 40)
  assert.deepEqual(root.items, [])
  assert.deepEqual(root.requests, [])
  assert.deepEqual(sub.requests, ["user_input"])
  assert.equal(sub.status, "waiting")
  assert.deepEqual(sub.mix, { shell: 1 })
})

test("an app-owned child is shown once with the correct activity", () => {
  const agents = snapshot(true)
  assert.equal(agents.length, 2)
  const child = agents.find((a) => a.id === "child")!
  assert.equal(child.parent, "root")
  assert.equal(child.items.length, 1)
  assert.deepEqual(child.requests, ["user_input"])
})

test("the active filter keeps a completed ancestor", () => {
  const agents = snapshot()
  agents[0].status = "completed"
  assert.deepEqual(tree(agents, new Set(), true).map((r) => r.agent.id), ["root", "sub"])
  assert.equal(tree(agents, new Set(["root"]), true).length, 1)
})

test("tree guides mark the last child", () => {
  const rows = tree(snapshot(), new Set())
  assert.equal(rows[1].depth, 1)
  assert.equal(rows[1].last, true)
})

test("control sequences and bidi overrides are removed", () => {
  const value = clean("hello\x1b]0;window\x07\n\u202eunsafe")
  assert.ok(!value.includes("\x1b"))
  assert.ok(!value.includes("\u202e"))
  assert.ok(!value.includes("\n"))
  assert.equal(clean("  > child"), "  > child")
})

test("tool categories", () => {
  assert.equal(category("dynamic_tool", "Read"), "read")
  assert.equal(category("dynamic_tool", "Edit"), "edit")
  assert.equal(category("command_execution"), "shell")
  assert.equal(category("user_message"), "you")
})

test("a missing database is not created", () => {
  const db = join(mkdtempSync(join(tmpdir(), "t3a-")), "missing.sqlite")
  assert.throws(() => new Source(db).snapshot())
  assert.equal(existsSync(db), false)
})

test("an unsupported database is left unchanged", () => {
  const db = join(mkdtempSync(join(tmpdir(), "t3a-")), "state.sqlite")
  const conn = new DatabaseSync(db)
  conn.exec("CREATE TABLE untouched (value TEXT)")
  conn.close()
  const before = readFileSync(db)
  assert.throws(() => new Source(db).snapshot(), /Unsupported T3 schema/)
  assert.deepEqual(readFileSync(db), before)
})

test("prompts, results and other message text never leave the database", () => {
  // Plant marker text in every payload field that can hold message content.
  const dir = mkdtempSync(join(tmpdir(), "t3a-"))
  const db = join(dir, "statev2.sqlite")
  const conn = new DatabaseSync(db)
  const P = "orchestration_v2_projection_"
  conn.exec(`CREATE TABLE ${P}threads (thread_id TEXT, payload_json TEXT, deleted_at TEXT, archived_at TEXT);
    CREATE TABLE ${P}runs (thread_id TEXT, ordinal INTEGER, payload_json TEXT, requested_at TEXT, completed_at TEXT);
    CREATE TABLE ${P}subagents (thread_id TEXT, payload_json TEXT);
    CREATE TABLE ${P}nodes (thread_id TEXT, node_id TEXT, parent_node_id TEXT, run_id TEXT);
    CREATE TABLE ${P}provider_threads (thread_id TEXT, payload_json TEXT);
    CREATE TABLE ${P}provider_turns (thread_id TEXT, provider_thread_id TEXT, ordinal INTEGER, payload_json TEXT, started_at TEXT, completed_at TEXT);
    CREATE TABLE ${P}turn_items (thread_id TEXT, run_id TEXT, node_id TEXT, provider_thread_id TEXT, type TEXT, status TEXT, updated_at TEXT, ordinal INTEGER, payload_json TEXT);
    CREATE TABLE ${P}runtime_requests (thread_id TEXT, node_id TEXT, kind TEXT, status TEXT, resolved_at TEXT);
    CREATE TABLE projection_projects (project_id TEXT, title TEXT, workspace_root TEXT);`)
  const put = (sql: string, ...v: string[]) => conn.prepare(sql).run(...v)
  const secret = (field: string) => `SECRET-${field}`
  put(`INSERT INTO ${P}threads VALUES ('t', ?, NULL, NULL)`, JSON.stringify({ id: "t", projectId: "p", title: "Thread",
    activeProviderThreadId: "pt", delegatedFrom: { prompt: secret("delegatedFrom") }, forkedFrom: secret("forkedFrom") }))
  put(`INSERT INTO ${P}runs VALUES ('t', 1, ?, NULL, NULL)`, JSON.stringify({ id: "r", threadId: "t", status: "running",
    providerThreadId: "pt", delegatedCompletion: { result: secret("delegatedCompletion") } }))
  put(`INSERT INTO ${P}subagents VALUES ('t', ?)`, JSON.stringify({ id: "s", threadId: "t", runId: "r", status: "running",
    title: "Sub", prompt: secret("prompt"), result: secret("result"), progress: secret("progress"),
    remoteChild: { label: "other machine", title: secret("remoteTitle") } }))
  put(`INSERT INTO ${P}provider_threads VALUES ('t', ?)`, JSON.stringify({ id: "pt", goal: secret("goal"),
    nativeMetadata: { note: secret("nativeMetadata") }, contextUsage: { usedTokens: 1, maxTokens: 10 } }))
  put(`INSERT INTO ${P}provider_turns VALUES ('t', 'pt', 1, ?, NULL, NULL)`, JSON.stringify({ providerThreadId: "pt",
    model: "m", nativeTurnRef: secret("nativeTurnRef"), tokenUsage: { usedTokens: 2, maxTokens: 10 } }))
  conn.close()
  try {
    const snap = new Source(db).snapshot()
    assert.equal(snap.agents.length, 2)
    assert.equal(snap.agents.find((a) => a.id === "s")?.remote?.label, "other machine")
    assert.equal(snap.agents.find((a) => a.id === "t")?.context.usedTokens, 2)
    assert.doesNotMatch(JSON.stringify(snap), /SECRET/)
  } finally {
    rmSync(dirname(db), { recursive: true, force: true })
  }
})
