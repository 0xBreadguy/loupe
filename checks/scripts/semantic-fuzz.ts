// Deterministic differential navigation checks. Synthetic DTOs only; --semantic
// exits before Rune starts. No GUI, service, database, or performance measurement.
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { isDeepStrictEqual } from "node:util"
import type { Agent, Item, Snapshot } from "../../oracle/src/source.ts"
import { replay } from "./oracle.ts"
// Opt-in: with T3_RECORD set, export-fuzz.ts turns this run into the recorded fixture.
import { enabled as recording, recordAsset, recordCase } from "./record.ts"

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const args = process.argv.slice(2)
const option = (name: string, fallback: string) => {
  const index = args.indexOf(name)
  return index < 0 ? fallback : args[index + 1] ?? fallback
}
const integer = (name: string, fallback: number, min: number, max: number) => {
  const value = Number(option(name, String(fallback)))
  if (!Number.isSafeInteger(value) || value < min || value > max) throw new Error(`${name} must be ${min}..${max}`)
  return value
}
const seed = integer("--seed", 0xb4ead202, 0, 0xffffffff) >>> 0
const cases = integer("--cases", 45, 1, 45)
const caseOnly = args.includes("--case") ? integer("--case", 0, 0, 44) : null
const binary = resolve(option("--native", process.env.T3_NATIVE_BINARY ?? join(ROOT, "..", "target/release/t3-agents-core")))
const output = resolve(option("--output", join(ROOT, "generated/semantic-fuzz")))
const MAX_OPERATIONS = 1000
const ACTIONS_PER_CASE = 20
const EPOCH = 1_794_484_800_000
const templatePath = join(ROOT, "fixtures/demo.json")
const templates = JSON.parse(readFileSync(templatePath, "utf8")).agents as Agent[]
const hash = (path: string) => createHash("sha256").update(readFileSync(path)).digest("hex")
const nativeHash = hash(binary)
const templateHash = hash(templatePath)
const oracleHash = hash(join(ROOT, "scripts/oracle.ts"))
const stamp = (time: number) => new Date(time).toISOString()
mkdirSync(output, { recursive: true })

class Random {
  value: number
  constructor(value: number) { this.value = value >>> 0 }
  next() {
    let value = this.value
    value ^= value << 13; value ^= value >>> 17; value ^= value << 5
    this.value = value >>> 0
    return this.value
  }
  int(max: number) { return this.next() % max }
  chance(numerator: number, denominator = 10) { return this.int(denominator) < numerator }
  one<T>(values: readonly T[]): T { return values[this.int(values.length)] }
}
const caseSeed = (index: number) => (seed ^ Math.imul(index + 1, 0x9e3779b9)) >>> 0
function identifier(index: number, node: number) {
  const hex = (n: number, width: number) => (n >>> 0).toString(16).padStart(width, "0").slice(-width)
  return `${hex(caseSeed(index), 8)}-${hex(index, 4)}-4${hex(node, 3)}-a000-${hex(node + 1, 12)}`
}
function makeSnapshot(index: number): Snapshot {
  const random = new Random(caseSeed(index))
  const now = EPOCH + index * 137
  const count = index % 17 === 16 ? 0 : 10 + random.int(23)
  const agents: Agent[] = []
  const statuses = ["running", "active", "waiting", "queued", "starting", "completed", "completed", "completed", "failed", "interrupted"]
  for (let i = 0; i < count; i++) {
    const a = structuredClone(random.one(templates))
    a.id = identifier(index, i)
    a.parent = i === 0 || random.chance(2) ? null : agents[random.int(i)].id
    a.thread_id = a.parent ? agents.find(agent => agent.id === a.parent)!.thread_id : a.id
    a.kind = a.parent ? "subagent" : "thread"
    a.title = random.one(["Synthetic branch", "日本語 · e\u0301 · 🍞", "x", "label ".repeat(20), "مرحبا · nested"])+` ${i}`
    a.project = "synthetic-fuzz"
    a.cwd = "/synthetic/fuzz"
    a.branch = "fuzz-only"
    a.remote = null
    a.status = i === 0 ? "running" : random.one(statuses)
    const age = random.one([-90, -1, 0, 1, 2, 15, 30, 179, 180, 181, 350, 700]) * 60_000
    a.updated = stamp(now - age)
    a.started = stamp(now - age - (random.chance(2) ? -600_000 : random.int(50) * 60_000))
    a.ended = ["completed", "failed", "interrupted"].includes(a.status) ? stamp(now - age) : null
    a.requests = a.status === "waiting" || random.chance(1) ? [random.one(["approval", "user_input", "secret_request"])] : []
    a.asked = a.requests.length ? stamp(now - random.int(60) * 60_000) : null
    a.shelf = a.parent ? undefined : { listed: random.chance(8), settled: random.chance(3),
      snoozedUntil: random.chance(3) ? stamp(now + random.one([-1, 1, 60]) * 60_000) : null }
    a.context = random.chance(1) ? {} : { usedTokens: random.int(110_000), maxTokens: random.one([0, 100_000, 200_000]) }
    a.history = a.history.slice(-6)
    a.events = Array.from({ length: random.int(9) }, () => ({ t: now - random.one([-6_000, -5_000, 0, 1000, 30_000, 600_000, 12_000_000]),
      cat: random.one(["you", "shell", "edit", "read", "think", "reply", "ask", "agent", "web"]), failed: random.chance(1) }))
    a.spans = [[now - random.int(10) * 60_000, random.chance(5) ? null : now + random.one([-1, 1]) * random.int(20) * 60_000]]
    a.items = []
    a.now = null
    a.recentFailures = []
    a.mix = {}
    a.failures = 0
    a.run = { id: `synthetic-run-${index}-${i}` }
    a.turn = {}
    if (["running", "active"].includes(a.status) && random.chance(4)) {
      const item: Item = { thread_id: a.thread_id, run_id: `synthetic-run-${index}-${i}`, node_id: a.id,
        provider_thread_id: "synthetic-provider", type: random.one(["command_execution", "subagent", "dynamic_tool"]),
        status: "running", updated_at: stamp(now - 1000), started_at: stamp(now - random.int(15) * 60_000),
        label: "Synthetic in-flight call", command: "synthetic-check", file: "", tool: "read" }
      a.items = [item]; a.now = item
    }
    agents.push(a)
  }
  // Occasional isolated malformed topology is still a type-correct Snapshot DTO.
  // The original tree must bound cycles and exclude orphans without hanging.
  if (count > 4 && index % 5 === 2) agents[count - 1].parent = identifier(index, count + 20)
  if (count > 5 && index % 7 === 3) {
    agents[count - 1].parent = agents[count - 2].id
    agents[count - 2].parent = agents[count - 1].id
  }
  return { at: now, path: `synthetic:fuzz-${index}`, agents }
}
function actionsFor(snapshot: Snapshot, index: number) {
  const random = new Random(caseSeed(index) ^ 0x51a7e)
  const actions: string[] = []
  const keys = ["down", "down", "up", "home", "end", "space", "right", "left", "n", "a", "a", "t", "pagedown", "pageup"]
  for (let step = 0; step < ACTIONS_PER_CASE; step++) {
    if (step === 5 || step === 13) {
      const rows = replay(snapshot, actions).frames.at(-1)!.rows
      const earlier = rows.filter(row => row.id.startsWith("earlier:"))
      const candidates = earlier.length && random.chance(7) ? earlier : rows
      if (candidates.length) { actions.push(`pick:${random.one(candidates).id}`); continue }
    }
    if (step > 0 && actions[step - 1]?.startsWith("pick:earlier:")) actions.push("enter")
    else actions.push(random.one(keys))
  }
  return actions
}
let operations = 0
const input = join(output, "current.json")
type Difference = { field: string; expected: unknown; actual: unknown }
function compare(snapshot: Snapshot, actions: string[], expected?: Record<string, unknown>) {
  if (operations >= MAX_OPERATIONS) throw new Error("bounded operation budget exhausted")
  writeFileSync(input, JSON.stringify(snapshot)+"\n")
  const process = spawnSync(binary, ["--snapshot", input, "--semantic", "--actions", actions.join(",")],
    { encoding: "utf8", timeout: 15_000, maxBuffer: 8 * 1024 * 1024 })
  operations++
  if (process.status !== 0) return { differences: [{ field: "process", expected: 0, actual: process.error?.message ?? process.stderr ?? process.status }], actual: null }
  let actual: Record<string, unknown>
  try { actual = JSON.parse(process.stdout) } catch { return { differences: [{ field: "json", expected: "semantic JSON", actual: process.stdout.slice(0, 1000) }], actual: null } }
  const reference = expected ?? replay(snapshot, actions).frames.at(-1)!
  const differences: Difference[] = []
  for (const [field, value] of Object.entries(reference)) {
    if (field === "action") continue
    if (!isDeepStrictEqual(actual[field], value)) differences.push({ field, expected: value, actual: actual[field] })
  }
  return { differences, actual }
}
function minimize(snapshot: Snapshot, actions: string[]) {
  let snap = structuredClone(snapshot), steps = [...actions], attempts = 0
  const stillFails = (candidate: Snapshot, next: string[]) => {
    if (attempts >= 54 || operations >= MAX_OPERATIONS) return false
    attempts++
    try { return compare(candidate, next).differences.length > 0 } catch { return false }
  }
  // Delete action chunks, then agents. Keep every remaining DTO type-correct.
  for (let chunk = Math.max(1, Math.floor(steps.length / 2)); chunk >= 1; chunk = Math.floor(chunk / 2)) {
    for (let start = 0; start < steps.length;) {
      const next = steps.slice(0, start).concat(steps.slice(start + chunk))
      if (stillFails(snap, next)) steps = next
      else start += chunk
      if (attempts >= 54) break
    }
    if (chunk === 1 || attempts >= 54) break
  }
  for (let chunk = Math.max(1, Math.floor(snap.agents.length / 2)); chunk >= 1; chunk = Math.floor(chunk / 2)) {
    for (let start = 0; start < snap.agents.length;) {
      const agents = snap.agents.slice(0, start).concat(snap.agents.slice(start + chunk))
      const candidate = { ...snap, agents }
      if (stillFails(candidate, steps)) snap = candidate
      else start += chunk
      if (attempts >= 54) break
    }
    if (chunk === 1 || attempts >= 54) break
  }
  return { snapshot: snap, actions: steps, attempts }
}
const completed: { case: number; seed: number; agents: number; prefixes: number }[] = []
const selectedCases = caseOnly === null ? Array.from({ length: cases }, (_, i) => i) : [caseOnly]
let failure: unknown = null
for (const index of selectedCases) {
  const snapshot = makeSnapshot(index), actions = actionsFor(snapshot, index)
  const expected = replay(snapshot, actions).frames
  if (recording) {
    const path = join(output, `case-${index}.json`)
    writeFileSync(path, JSON.stringify(snapshot) + "\n")
    recordCase({ kind: "prefixes", case: index, seed: caseSeed(index), input: recordAsset(path, "text"), actions,
      frames: expected.map(({ action: _, ...semantic }) => semantic) })
  }
  for (let prefix = 0; prefix <= actions.length; prefix++) {
    const checked = compare(snapshot, actions.slice(0, prefix), expected[prefix])
    if (checked.differences.length) {
      const originalPath = join(output, `failure-case-${index}.json`)
      writeFileSync(originalPath, JSON.stringify(snapshot, null, 2)+"\n")
      const minimal = minimize(snapshot, actions.slice(0, prefix))
      const minimalPath = join(output, `minimal-case-${index}.json`)
      writeFileSync(minimalPath, JSON.stringify(minimal.snapshot, null, 2)+"\n")
      const final = compare(minimal.snapshot, minimal.actions)
      failure = { case: index, seed: caseSeed(index), prefix, actions: actions.slice(0, prefix), snapshot: originalPath,
        differences: checked.differences, minimal: { snapshot: minimalPath, actions: minimal.actions, attempts: minimal.attempts,
          differences: final.differences, method: "Bounded greedy action/agent deletion, not proof of global minimality" }, reproduce: { seed, case: index, native: binary, output } }
      break
    }
  }
  if (failure) break
  completed.push({ case: index, seed: caseSeed(index), agents: snapshot.agents.length, prefixes: expected.length })
}
const changedInputs = hash(binary) !== nativeHash || hash(templatePath) !== templateHash || hash(join(ROOT, "scripts/oracle.ts")) !== oracleHash
const report = { schema: 1, seed, requestedCases: selectedCases.length, actionsPerCase: ACTIONS_PER_CASE,
  operationLimit: MAX_OPERATIONS, operations, completed, errors: Number(Boolean(failure)) + Number(changedInputs), failure,
  invalid: changedInputs ? "Native binary, fixture template, or oracle changed during fuzz run" : null,
  native_sha256: nativeHash, template_sha256: templateHash, oracle_sha256: oracleHash,
  script_sha256: hash(fileURLToPath(import.meta.url)),
  normalization: "Only supplementary native diagnostic fields are ignored; selected, scope, sorted fold/expand sets, complete row order and every row's semantic fields compare exactly.",
  input: "Synthetic typed fixtures only; no accounts, databases, GUI, or services." }
writeFileSync(join(output, "report.json"), JSON.stringify(report, null, 2)+"\n")
console.log(JSON.stringify(report, null, 2))
if (failure || changedInputs) process.exitCode = 1
