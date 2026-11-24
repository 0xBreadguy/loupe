// A separate correctness gate, never a timing benchmark. Native CLI must exit before Rune starts.
import assert from "node:assert/strict"
import { mkdirSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { spawnSync } from "node:child_process"
import { SPEC, ROOT, snapshot, rapidSnapshot } from "./fixtures.ts"
import { actionName, replay } from "./oracle.ts"
// Opt-in: with T3_RECORD set, export-semantic.ts turns this run into the recorded fixture.
import { enabled as recording, recordAsset, recordCase } from "./record.ts"

const binary = resolve(process.argv[2] ?? process.env.T3_NATIVE_BINARY ?? join(ROOT, "..", "target/release/loupe-core"))
const output = resolve(process.argv[3] ?? join(ROOT, "generated/parity.json"))
mkdirSync(join(ROOT, "generated"), { recursive: true })
let operations = 0
const cases: Record<string, unknown> = {}
for (const name of SPEC.cases) {
  const snap = snapshot(name)
  const input = join(ROOT, "generated", name + ".json")
  writeFileSync(input, JSON.stringify(snap) + "\n")
  const actions: string[] = SPEC.actions.map(actionName)
  const expected = replay(snap, actions).frames
  if (recording) recordCase({
    kind: "replay", case: name, input: recordAsset(input, "text"), actions,
    frames: expected.map(({ action: _, ...semantic }) => semantic),
  })
  for (let i = 0; i <= actions.length; i++) {
    const run = spawnSync(binary, ["--snapshot", input, "--semantic", "--actions", actions.slice(0, i).join(",")],
      { encoding: "utf8", timeout: 30000, maxBuffer: 16 * 1024 * 1024 })
    assert.equal(run.status, 0, `${name} step ${i}: ${run.error ?? run.stderr}`)
    const actual = JSON.parse(run.stdout)
    const { action: _, ...wanted } = expected[i]
    // Permit native to expose supplementary diagnostic fields without weakening these semantic assertions.
    for (const [field, value] of Object.entries(wanted)) {
      assert.deepEqual(actual[field], value, `${name} step ${i} ${i ? actions[i - 1] : "initial"}: ${field}`)
    }
    operations++
  }
  cases[name] = { agents: snap.agents.length, states: expected.length }
}
// Data really changes across the burst. Each sampled snapshot is compared independently.
for (const index of [0, 1, 59, 119]) {
  const snap = rapidSnapshot(index)
  const input = join(ROOT, "generated", `rapid-${index}.json`)
  writeFileSync(input, JSON.stringify(snap) + "\n")
  if (recording) recordCase({
    kind: "single", case: `rapid-${index}`, input: recordAsset(input, "text"), actions: [],
    frames: [(() => { const { action: _, ...semantic } = replay(snap).frames[0]; return semantic })()],
  })
  const run = spawnSync(binary, ["--snapshot", input, "--semantic"], { encoding: "utf8", timeout: 30000 })
  assert.equal(run.status, 0, run.stderr)
  const actual = JSON.parse(run.stdout)
  const { action: _, ...wanted } = replay(snap).frames[0]
  for (const [field, value] of Object.entries(wanted)) assert.deepEqual(actual[field], value, `rapid ${index}: ${field}`)
  operations++
}
const report = { version: 1, binary, operations, errors: 0, cases }
writeFileSync(output, JSON.stringify(report, null, 2) + "\n")
console.log(JSON.stringify(report))
