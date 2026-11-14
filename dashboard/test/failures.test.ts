import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { DatabaseSync } from "node:sqlite"
import { test } from "node:test"
import { Canvas } from "../src/canvas.ts"
import { buildDemo } from "../src/demo.ts"
import { PREFIX as P, Source, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { draw, initialState } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const NOW = Date.parse("2026-11-12T12:00:00Z")
const iso = (minutes: number) => new Date(NOW - minutes * 60_000).toISOString()

function fixture(): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-failures-"))
  const path = join(dir, "state.sqlite")
  try {
    buildDemo(path, NOW, NOW)
    const db = new DatabaseSync(path)
    try {
      // A provider-created native child has a thread and items, but no app run.
      for (const [id, parent] of [["root", null], ["child", "root"], ["sibling", "root"]]) {
        db.prepare(`INSERT INTO ${P}threads VALUES (?, ?, NULL, NULL)`).run(id, JSON.stringify({ id, title: id,
          activeProviderThreadId: `${id}-provider`, projectId: "atlas",
          lineage: { relationshipToParent: parent ? "subagent" : null, parentThreadId: parent } }))
      }
      db.prepare(`INSERT INTO ${P}runs VALUES ('new-run', 'root', 2, ?, ?, NULL)`).run(
        JSON.stringify({ id: "new-run", threadId: "root", status: "running", providerThreadId: "root-provider" }), iso(5))
      // The spawning record is an alias for the native child, not a duplicate agent.
      db.prepare(`INSERT INTO ${P}subagents VALUES ('alias', 'root', ?)`).run(JSON.stringify({
        id: "alias", threadId: "root", childThreadId: "child", runId: "new-run", status: "completed" }))
      db.prepare(`INSERT INTO ${P}nodes VALUES ('child-tool', 'child', 'alias', NULL)`).run()
      const item = (id: string, thread: string, run: string | null, status: string, start: number, end: number, exit?: number) => {
        db.prepare(`INSERT INTO ${P}turn_items VALUES (?, ?, ?, ?, ?, 'command_execution', ?, ?, 1, ?)`).run(
          id, thread, run, thread === "child" ? "child-tool" : null, `${thread}-provider`, status, iso(end), JSON.stringify({
            startedAt: iso(start), input: `command-${id}`, exitCode: exit, output: "SECRET-OUTPUT", error: "SECRET-ERROR",
          }))
      }
      item("missing-path", "child", null, "failed", 12, 12, 2)
      item("no-match", "child", null, "failed", 11, 11, 1)
      for (let n = 0; n < 25; n++) item(`success-${n}`, "child", null, "completed", 3 + n / 10, 3 + n / 10, 0)
      item("sibling-call", "sibling", null, "failed", 10, 10, 3)
      item("old-run", "root", "old-run", "failed", 14, 14, 7)
      item("long-call", "root", "old-run", "failed", 240, 2, 9)
      item("new-success", "root", "new-run", "completed", 1, 1, 0)
      item("expired", "root", "old-run", "failed", 200, 200, 6)
    } finally { db.close() }
    return new Source(path).snapshot(NOW)
  } finally { rmSync(dir, { recursive: true, force: true }) }
}

const agent = (s: Snapshot, id: string) => s.agents.find((a) => a.id === id)!
function render(snap: Snapshot, id: string, w = 160, h = 55, minutes = 30) {
  const st = initialState()
  st.bootAt = 0
  st.calm = true
  st.selected = id
  st.windowIndex = [15, 30, 60, 180].indexOf(minutes)
  const c = new Canvas(w, h)
  const frame = draw(c, st, snap, [], derivePalette(THEMES.solitude), NOW)
  return { text: c.toText(), frame, st, c }
}

test("runless native child activity and failures are loaded without leaking to its parent or sibling", () => {
  const snap = fixture()
  const child = agent(snap, "child")
  assert.equal(snap.agents.some((a) => a.id === "alias"), false)
  assert.equal(child.items.length, 27)
  assert.equal(child.failures, 2)
  assert.deepEqual(child.recentFailures.map((i) => i.exit_code), [1, 2])
  assert.deepEqual(agent(snap, "sibling").recentFailures.map((i) => i.command), ["command-sibling-call"])
  assert.ok(!agent(snap, "root").recentFailures.some((i) => i.thread_id === "child"))
  assert.doesNotMatch(JSON.stringify(snap), /SECRET/)
})

test("failures survive a newer run and a long call is counted when its failure is reported", () => {
  const root = agent(fixture(), "root")
  assert.deepEqual(root.items.map((i) => i.command), ["command-new-success"])
  assert.equal(root.failures, 0, "the latest run did not fail")
  assert.deepEqual(root.recentFailures.map((i) => i.command), ["command-long-call", "command-old-run"])
  assert.deepEqual(root.events.filter((e) => e.failed).map((e) => e.t), [Date.parse(iso(14)), Date.parse(iso(2))])
})

test("failed calls stay above ordinary activity after many successes at narrow, normal and wide sizes", () => {
  for (const [w, h] of [[100, 30], [160, 55], [260, 65]]) {
    const snap = fixture()
    const { text, st, c } = render(snap, "child", w, h)
    assert.match(text, /RECENT FAILURES/)
    assert.match(text, /exit 1/)
    assert.match(text, /command-no-match/)
    assert.doesNotMatch(text, /No tool activity recorded/)
    st.detailScroll = 2
    draw(c, st, snap, [], derivePalette(THEMES.solitude), NOW)
    assert.match(c.toText(), /exit 2/)
    assert.match(c.toText(), /command-missing-path/)
  }
  const { text } = render(fixture(), "child", 160, 85)
  assert.match(text, /thread activity/)
  assert.match(text, /11 more calls/)
})

test("history labels earlier runs, obeys the lane window, and never claims recovery", () => {
  const snap = fixture()
  const { text } = render(snap, "root")
  assert.match(text, /earlier run/)
  assert.match(text, /command-long-call/)
  assert.match(text, /agent status is separate/)
  assert.doesNotMatch(text, /recovered|resolved|command-expired/)
  const child = agent(snap, "child")
  for (const i of child.recentFailures) i.updated_at = iso(20)
  assert.doesNotMatch(render(snap, "child", 160, 55, 15).text, /RECENT FAILURES/)
  assert.match(render(snap, "child", 160, 55, 30).text, /RECENT FAILURES/)
  child.recentFailures = []
  assert.doesNotMatch(render(snap, "child").text, /RECENT FAILURES/)
})

test("every failure remains reachable through detail scrolling", () => {
  const snap = fixture()
  const child = agent(snap, "child")
  child.recentFailures = Array.from({ length: 30 }, (_, n) => ({ ...child.recentFailures[0], command: `failure-number-${n}` }))
  const { text, frame, st, c } = render(snap, "child", 100, 30)
  assert.match(text, /▼ \d+ more/)
  assert.ok(frame.detailMax > 0)
  let found = false
  for (st.detailScroll = 0; st.detailScroll <= frame.detailMax; st.detailScroll++) {
    draw(c, st, snap, [], derivePalette(THEMES.solitude), NOW)
    found ||= c.toText().includes("failure-number-29")
  }
  assert.ok(found, "the last failure was not silently truncated")
})

test("failed tools without an exit code still have a useful label", () => {
  const snap = fixture()
  const child = agent(snap, "child")
  child.recentFailures = [{ ...child.recentFailures[0], type: "dynamic_tool", label: "ReadFile", exit_code: null }]
  const { text } = render(snap, "child")
  assert.match(text, /failed +ReadFile/)
  assert.doesNotMatch(text, /exit null|exit undefined/)
})
