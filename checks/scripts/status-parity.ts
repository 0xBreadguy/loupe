// Differential gate: the native `--status` line must be byte-identical to
// JSON.stringify(status(snapshot, now)) in oracle/src/status.ts (the frozen oracle).
// Synthetic data only; every snapshot is written to a temporary directory and no T3
// database is discovered or opened implicitly.
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { spawnSync } from "node:child_process"
import { DatabaseSync } from "node:sqlite"
import { Demo } from "../../oracle/src/demo.ts"
import { line, status, watch } from "../../oracle/src/status.ts"
import { Source, type Agent, type Context, type Item, type Snapshot } from "../../oracle/src/source.ts"
import { EPOCH, ROOT, agent } from "./fixtures.ts"
// Opt-in: with T3_RECORD set, export-status.ts turns this run into the recorded fixture.
import { recordCase } from "./record.ts"

// Strip only additive numeric fields, preserving legacy JSON bytes and surrogate escapes.
const legacy = (s: string) => s.replace(/,"openAsks":\d+,"estimatedAsks":\d+/g, "")
const binary = resolve(process.argv[2] ?? process.env.T3_NATIVE_BINARY ?? join(ROOT, "..", "target/release/t3-agents-core"))
const dir = mkdtempSync(join(tmpdir(), "t3-status-parity-"))
const home = join(dir, "empty-t3-home")
mkdirSync(home)
const childEnv = { ...process.env, T3CODE_HOME: home }
const stamp = (t: number) => new Date(t).toISOString()
let cases = 0
const differences: string[] = []

function native(args: string[]): { status: number | null; stdout: string; stderr: string } {
  const run = spawnSync(binary, ["--status", ...(args.includes("--snapshot") ? ["--demo"] : []), ...args], { env: childEnv, encoding: "utf8", timeout: 120_000, maxBuffer: 64 * 1024 * 1024 })
  return { status: run.status, stdout: run.stdout, stderr: run.stderr }
}

function expect(label: string, expected: string, args: string[]) {
  cases++
  const run = native(args)
  if (run.status !== 0) {
    differences.push(`${label}: exit ${run.status}\n  stderr: ${run.stderr.trim()}`)
    return
  }
  const actual = run.stdout.endsWith("\n") ? run.stdout.slice(0, -1) : run.stdout
  if (legacy(actual) !== expected) differences.push(`${label}\n  ts:   ${expected}\n  rust: ${actual}`)
}

/** Compare one synthetic snapshot; `now` defaults to the snapshot's at, as `--snapshot` does. */
function compare(label: string, snap: Snapshot, now = snap.at, explicit = false) {
  const file = join(dir, `${cases}-${label.replace(/[^a-z0-9]+/gi, "-")}.json`)
  writeFileSync(file, JSON.stringify(snap) + "\n")
  const expected = JSON.stringify(status(snap, now))
  recordCase({ kind: "snapshot", label, now, explicit, snapshot: snap, expected })
  expect(label, expected, ["--snapshot", file, ...(explicit ? ["--now-ms", String(now)] : [])])
}

/** An error line from a database the Rust test rebuilds; the path is a placeholder. */
function compareDatabase(label: string, invalid: "missing" | "not-sqlite" | "folder" | "schema", path: string) {
  const expected = line(new Source(path)).split(path).join("{path}")
  recordCase({ kind: "database", label, invalid, expected })
  expect(label, line(new Source(path)), ["--db", path])
}

function snap(agents: Agent[], at = EPOCH): Snapshot {
  return { agents, at, path: "synthetic:status-parity" }
}

function item(type: string, tool: string | null = null): Item {
  return { thread_id: "synthetic", run_id: null, node_id: null, provider_thread_id: null, type, status: "running",
    updated_at: stamp(EPOCH), started_at: stamp(EPOCH), label: "", command: "", file: "", tool }
}

try {
  // ── The generated fixtures ────────────────────────────────────────────────
  for (const name of ["demo", "edge", "empty", "large", "rapid"]) {
    let text: string
    try {
      text = readFileSync(join(ROOT, "generated", name + ".json"), "utf8")
    } catch {
      throw new Error(`missing checks/generated/${name}.json; run checks/scripts/fixtures.ts first`)
    }
    const snap: Snapshot = JSON.parse(text)
    compare(`fixture:${name}`, snap)
    compare(`fixture:${name}:explicit-now`, snap, snap.at, true)
  }
  // The failure window ages with `now`: one more clock on the edge fixture.
  {
    const snap: Snapshot = JSON.parse(readFileSync(join(ROOT, "generated", "edge.json"), "utf8"))
    compare("fixture:edge:ten-minutes-later", snap, snap.at + 600_001, true)
  }

  // ── Emoji sliced through (UTF-16 units, lone surrogate in the output) ────
  compare("slice:title-emoji", snap([
    agent("emoji-title", null, "running", 0, { title: "t".repeat(79) + "🍞" + "tail" }),
    agent("emoji-exact", null, "running", 1, { title: "e".repeat(80) }),
    agent("emoji-one-over", null, "running", 2, { title: "o".repeat(81) }),
    agent("emoji-model", null, "running", 3, { title: "model slice", model: "claude-" + "a".repeat(30) + "🍞" + "zz" }),
  ]))

  // ── Control characters, quotes and backslashes ───────────────────────────
  compare("escape:controls", snap([
    agent("controls", null, "running", 0, {
      title: 'He said "hi" \\ and \u0000\u0001\u001f\u007f\u009f\n\t\r\b\f — done',
      status: 'running\u0000\u001f"\\',
      model: 'm\u0002"\\\u001f',
      provider_id: "p\u0001",
    }),
    agent("zwj", null, "running", 1, { title: "👨\u200d👩\u200d👧 family", model: "👨\u200d👩\u200d👧-model" }),
    agent("del-and-nonascii", null, "running", 2, { title: "\u007f\u0085\u009f ok é 日本語", status: "running\u0085" }),
  ]))

  // ── More than eight, waiting first then running ─────────────────────────
  {
    const agents: Agent[] = []
    for (let i = 0; i < 10; i++) agents.push(agent(`wait-${i}`, null, "waiting", i))
    for (let i = 0; i < 6; i++) agents.push(agent(`live-${i}`, null, "running", 20 + i))
    compare("order:ten-waiting-six-running", snap(agents))
  }

  // ── Ties and nulls in started (stable sort, null sorts as 0) ────────────
  {
    const tie = stamp(EPOCH - 600_000)
    const agents: Agent[] = [
      agent("tie-a", null, "waiting", 0, { started: tie }),
      agent("tie-b", null, "waiting", 0, { started: tie }),
      agent("tie-c", null, "waiting", 0, { started: tie }),
      agent("null-a", null, "waiting", 0, { started: null }),
      agent("null-b", null, "completed", 0, { started: null, requests: ["approval"] }),
      agent("newest", null, "running", 0, { started: stamp(EPOCH) }),
      agent("older", null, "running", 0, { started: stamp(EPOCH - 1_000) }),
      agent("invalid", null, "running", 0, { started: "not a date" }),
    ]
    // An absent `started` key is undefined in JS and the default in Rust.
    const missing = agent("absent", null, "running", 0)
    delete (missing as Partial<Agent>).started
    agents.push(missing)
    compare("order:ties-nulls-invalid", snap(agents))
  }

  // ── Context: null, zero, negative, above max, max 0, rounding ───────────
  const contexts: [string, number | null | undefined, number | null | undefined][] = [
    ["used-null", null, 100],
    ["max-null", 50, null],
    ["both-null", null, null],
    ["zero-of-zero", 0, 0],
    ["zero-of-hundred", 0, 100],
    ["negative", -50, 100],
    ["negative-half-cent-rounds-to-zero", -5, 1000],
    ["negative-six-tenths", -6, 1000],
    ["above-max", 150, 100],
    ["at-max", 100, 100],
    ["third", 1, 300],
    ["two-thirds", 2, 300],
    ["eighth", 1, 8],
    ["seventh", 1, 7],
    ["exact-half-percent", 0.005, 1],
    ["seven-percent", 7, 100],
    ["tiny", 1e-300, 1],
    ["large", 1e300, 1e299],
    ["small-ratio", 1, 1e9],
    ["max-negative", 50, -100],
    ["max-tiny", 5, 1e-9],
  ]
  for (const [name, usedTokens, maxTokens] of contexts) {
    const context: Record<string, number> = {}
    if (usedTokens !== null && usedTokens !== undefined) context.usedTokens = usedTokens
    if (maxTokens !== null && maxTokens !== undefined) context.maxTokens = maxTokens
    compare(`ctx:${name}`, snap([agent(`ctx-${name}`, null, "running", 0, { context: context as Context })]))
  }

  // ── A missing model, and family from the provider hint ──────────────────
  {
    const absent = agent("no-model", null, "running", 0)
    delete (absent as Partial<Agent>).model
    compare("model:absent", snap([
      absent,
      agent("empty-model", null, "running", 1, { model: "", provider_id: "anthropic" }),
      agent("hint-only", null, "running", 2, { model: "", provider_id: null, run: { provider: "openai-codex" } }),
      agent("model-wins", null, "running", 3, { model: "gpt-5", provider_id: "anthropic" }),
      agent("other", null, "running", 4, { model: "llama-3", provider_id: "meta" }),
    ]))
  }

  // ── Failures just inside and just outside the ten-minute window ─────────
  {
    const boundary = EPOCH - 600_000
    const events = (rows: [number, boolean][]) => rows.map(([t, failed]) => ({ t, cat: "shell", failed }))
    const finished = (id: string, rows: [number, boolean][], age = 30) =>
      agent(id, null, "completed", age, { events: events(rows) })
    compare("failed:window", snap([
      finished("inside-edge", [[boundary, true]]),
      finished("outside-edge", [[boundary - 1, true]]),
      finished("inside", [[boundary + 1, true], [EPOCH, true]]),
      finished("not-failed", [[boundary, false], [EPOCH, false]]),
      finished("mixed", [[boundary - 1, true], [boundary + 1, false], [EPOCH, true]]),
      agent("running-failure", null, "running", 0, { events: events([[EPOCH - 60_000, true]]) }),
      agent("waiting-failure", null, "waiting", 0, { events: events([[EPOCH - 120_000, true]]) }),
    ]), EPOCH)
    // A different now moves the boundary: the same snapshot at one minute later.
    const snapAt = snap([
      finished("inside-edge", [[boundary, true]]),
      finished("outside", [[boundary - 1, true]]),
    ])
    compare("failed:window-aged", snapAt, EPOCH + 60_000, true)
  }

  // ── Non-ASCII models longer than 32 UTF-16 units ────────────────────────
  compare("model:non-ascii", snap([
    agent("cjk", null, "running", 0, { title: "cjk", model: "claude-" + "日本語".repeat(12) }),
    agent("emoji-cut", null, "running", 1, { title: "emoji", model: "claude-" + "a".repeat(30) + "🍞" + "zz" }),
    agent("gpt-cjk", null, "running", 2, { title: "gpt", model: "gpt-" + "模型".repeat(10) }),
    agent("accented", null, "running", 3, { title: "accented", model: "claude-opus-4-5-" + "é".repeat(30) }),
    agent("combining", null, "running", 4, { title: "combining", model: "claude-" + "e\u0301".repeat(20) }),
    agent("fullwidth", null, "running", 5, { title: "fullwidth", model: "ｃｌａｕｄｅ-" + "Ａ".repeat(40) }),
  ]))

  // ── `now`: every tool category, and none ────────────────────────────────
  {
    const kinds: [string, string | null][] = [
      ["command_execution", null], ["file_change", null], ["web_search", null], ["subagent", null],
      ["handoff", null], ["user_input_request", null], ["secret_request", null], ["link_request", null],
      ["reasoning", null], ["assistant_message", null], ["user_message", null], ["notification", null],
      ["mystery", null],
    ]
    compare("now:static-categories", snap(kinds.map(([type, tool], i) =>
      agent(`now-${type}`, null, "running", i, { now: item(type, tool) }))))
    const tools = ["Read", "GLOB", "grep", "notebookread", "fuzzy_edit", "write_file", "web.fetch", "search", "agent", "task-run", "delegate", "spawn", "mystery"]
    compare("now:dynamic-tools", snap(tools.map((tool, i) =>
      agent(`tool-${i}`, null, "running", i, { now: item("dynamic_tool", tool) }))))
  }

  // ── Seeded fuzz: adversarial text and numbers the targeted cases did not think of ──
  {
    let seed = 0x7e3a9c15
    const rand = () => { // mulberry32
      seed = (seed + 0x6d2b79f5) | 0
      let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296
    }
    const pick = <T>(list: T[]): T => list[Math.floor(rand() * list.length)]
    const pieces = ["a", "Z", " ", "  ", "\t", "\n", "\r", "\u0000", "\u0001", "\u001b[31m", "\u007f", "\u0085", " ",
      "​", "‍", " ", " ", "‮", "﻿", "\"", "\\", "/", "<b>", "é", "é", "日本語", "模型",
      "🍞", "👩‍👩‍👧‍👦", "🇯🇵", "𝒳", "ｗｉｄｅ", "‹›", "…", "́", "0", "-", "claude-", "gpt-", "opus-4-5"]
    const text = (units: number) => {
      let out = ""
      while (out.length < units) out += pick(pieces)
      return out
    }
    const numbers = [null, 0, -0, 1, -1, 3, 7, 10, 100, 1e-7, 1e21, 0.1 + 0.2, 1 / 3, 199999, 200000, 123456.789, -50, 0.005, 2.5, 1.005]
    // Every form here is one Node's ISO parser reads; a time without a zone is local, so
    // run this gate under another TZ as well. Legacy forms ("Nov 12 2026") are out of scope.
    const stamps = [null, "", "garbage", "2026", "2026-11", "2026-11-12", stamp(EPOCH), stamp(EPOCH), stamp(EPOCH - 1),
      stamp(EPOCH - 60_000), stamp(EPOCH + 60_000), "2026-11-12T00:00:00Z", "2026-11-12T00:00:00.000+05:30", "1794484800000",
      "2026-11-12T00:00:00", "2026-11-12 00:00:01", "2026-11-12T00:00", "2026-11-12T05:30:00.250", "2026-02-30T00:00:00Z",
      "2026-11-11T24:00:00Z", "2026-11-11T23:59:59.9999Z", "2026-11-12t00:00:00z", "2026-11-12T00:00:00+0000"]
    const statuses = ["running", "starting", "preparing", "queued", "waiting", "pending", "active", "completed", "failed",
      "interrupted", "idle", "unknown", "", "RUNNING", "weird status"]
    const models = ["claude-opus-4-5", "claude-sonnet-4-5", "gpt-6.1-sol", "o3-mini", "grok-4.7", "--", "", "proto3", "kimi-k3"]
    const kinds = ["command_execution", "file_change", "web_search", "subagent", "dynamic_tool", "reasoning", "mystery"]
    for (let n = 0; n < 300; n++) {
      const agents: Agent[] = Array.from({ length: Math.floor(rand() * 14) }, (_, i) => {
        const used = pick(numbers), max = pick(numbers)
        const context: Context = {}
        if (used !== null || rand() < 0.5) context.usedTokens = used as never
        if (max !== null || rand() < 0.5) context.maxTokens = max as never
        return agent(`fuzz-${n}-${i}`, null, pick(statuses), i, {
          title: text(Math.floor(rand() * 30) + (rand() < 0.6 ? 66 : 0)),
          model: rand() < 0.5 ? pick(models) : pick(models) + text(Math.floor(rand() * 40)),
          context, started: pick(stamps), requests: rand() < 0.35 ? ["approval"] : [],
          provider_id: rand() < 0.3 ? pick(["anthropic", "openai", "xai", "", "codex"]) : null as never,
          now: rand() < 0.5 ? item(pick(kinds), rand() < 0.5 ? pick(["Read", "grep", "write_file", "task-run", "mystery"]) : null) : null,
          events: Array.from({ length: Math.floor(rand() * 6) }, () => ({
            t: EPOCH - pick([0, 1, 599_999, 600_000, 600_001, 3_600_000, -5_000]), cat: "shell", failed: rand() < 0.5 })),
        })
      })
      const now = EPOCH + pick([0, 1, -1, 30_000, 600_000, -600_000])
      compare(`fuzz:${n}`, snap(agents), now, true)
    }
  }

  // ── Error lines: a missing database and a file that is not SQLite ───────
  {
    const missing = join(dir, "does-not-exist.sqlite")
    compareDatabase("error:missing-db", "missing", missing)
    const garbage = join(dir, "not-a-database.sqlite")
    writeFileSync(garbage, "this is not a SQLite database\n")
    compareDatabase("error:not-sqlite", "not-sqlite", garbage)
    const folder = mkdtempSync(join(dir, "folder-"))
    compareDatabase("error:folder", "folder", folder)
    const empty = join(dir, "wrong-schema.sqlite")
    const database = new DatabaseSync(empty)
    database.exec("CREATE TABLE untouched(value TEXT)")
    database.close()
    compareDatabase("error:schema", "schema", empty)
  }
  // Replay the oracle's actual Demo + watch callbacks with virtual timers. No
  // wall-clock sleeps: refresh (3 s) and poll (2 s) preserve registration order,
  // including their shared 6 s deadlines, queued starts and the cycle restart.
  {
    const realNow = Date.now
    const realInterval = globalThis.setInterval
    const realClear = globalThis.clearInterval
    let now = EPOCH
    const timers: { callback: () => void; period: number; due: number; active: boolean }[] = []
    Date.now = () => now
    globalThis.setInterval = ((callback: () => void, period: number) => {
      const timer = { callback, period, due: now + period, active: true }
      timers.push(timer)
      return { unref() {}, timer }
    }) as unknown as typeof setInterval
    globalThis.clearInterval = ((handle: { timer: { active: boolean } }) => {
      handle.timer.active = false
    }) as unknown as typeof clearInterval
    const expected: string[] = []
    let demo: Demo | undefined
    try {
      demo = new Demo({ now: EPOCH })
      watch(new Source(demo.path), text => { expected.push(text) })
      const end = EPOCH + 1_206_000
      for (;;) {
        const due = Math.min(...timers.filter(t => t.active).map(t => t.due))
        if (due > end) break
        now = due
        for (const timer of timers) {
          if (timer.active && timer.due === due) {
            timer.due += timer.period
            timer.callback()
          }
        }
      }
    } finally {
      demo?.stop()
      Date.now = realNow
      globalThis.setInterval = realInterval
      globalThis.clearInterval = realClear
    }
    cases++
    const run = native(["--demo", "--probe-status-demo", "1206000", "--now-ms", String(EPOCH)])
    recordCase({ kind: "demo-watch", label: "demo:watch-timer-replay", args: ["--status", "--demo", "--probe-status-demo", "1206000", "--now-ms", String(EPOCH)], expected: expected.join("") })
    if (run.status !== 0 || legacy(run.stdout) !== expected.join("")) {
      differences.push(`demo:watch-timer-replay: exit ${run.status}; expected ${expected.length} lines, got ${run.stdout.split("\n").length - 1}; ${run.stderr}`)
    }
    if (new Set(expected).size < 2) throw new Error("demo replay did not evolve")
  }
} finally {
  rmSync(dir, { recursive: true, force: true })
}

const report = { version: 1, binary, cases, differences: differences.length, errors: differences.length }
mkdirSync(join(ROOT, "generated"), { recursive: true })
writeFileSync(join(ROOT, "generated", "status-parity.json"), JSON.stringify({ ...report, mismatches: differences }, null, 2) + "\n")
console.log(JSON.stringify(report))
for (const difference of differences) console.error(difference)
if (differences.length) process.exitCode = 1
