// Process/model reference: imports the pinned dashboard implementation unchanged.
// This is a headless microbenchmark, never a proxy for terminal presentation.
import { readFileSync } from "node:fs"
import { performance } from "node:perf_hooks"
import { tree, type Snapshot } from "../../oracle/src/source.ts"

const args = process.argv.slice(2)
function arg(name: string, fallback?: string): string {
  const i = args.indexOf(name)
  if (i < 0 && fallback !== undefined) return fallback
  if (i < 0 || args[i + 1] === undefined) throw new Error(`missing ${name}`)
  return args[i + 1]
}
const n = Number(arg("--bench-model", "100"))
const drawN = Number(arg("--draw", "0"))
if (!Number.isSafeInteger(n) || n < 1 || !Number.isSafeInteger(drawN) || drawN < 0) throw new Error("positive operation count required")
const data = readFileSync(arg("--snapshot"), "utf8")
const snap: Snapshot = JSON.parse(data)
const collapsed = new Set<string>(), expanded = new Set<string>()
const build = () => tree(snap.agents, collapsed, "all", { now: snap.at, expanded })
let parse_checksum = 0, checksum = 0
// Identical count requested of native. Warm parser + tree/JIT, not file caches.
for (let i = 0; i < 20; i++) { JSON.parse(data); build() }
let start = performance.now()
for (let i = 0; i < n; i++) parse_checksum += (JSON.parse(data) as Snapshot).agents.length
const parse_ms = performance.now() - start
start = performance.now()
for (let i = 0; i < n; i++) checksum += build().length
const model_ms = performance.now() - start
const rows = build()
let presentation: Record<string, unknown> | undefined
if (drawN) {
  const { Canvas } = await import("../../oracle/src/canvas.ts")
  const { draw, initialState, observe } = await import("../../oracle/src/view.ts")
  const { derivePalette } = await import("../../oracle/src/theme.ts")
  const canvas = new Canvas(160, 50), palette = derivePalette({}), state = initialState()
  state.calm = true; state.demo = true; state.bootAt = snap.at - 10000
  observe(state, snap, snap.at)
  for (let i = 0; i < 20; i++) draw(canvas, state, snap, [], palette, snap.at)
  let draw_checksum = 0
  start = performance.now()
  for (let i = 0; i < drawN; i++) {
    const frame = draw(canvas, state, snap, [], palette, snap.at)
    draw_checksum += frame.rows.length + canvas.ch[canvas.ch.length - 1]
  }
  const draw_ms = performance.now() - start
  // Independent output validation happens after the timed region.
  const text = canvas.toText()
  if (!text.includes("T3")) throw new Error("draw produced no dashboard")
  presentation = { operations: drawN, draw_ms, checksum: draw_checksum, columns: 160, lines: 50,
    output_characters: text.length, errors: 0,
    scope: "Canvas draw only; excludes OpenTUI flush, terminal emulator, compositor and input-to-present" }
}
console.log(JSON.stringify({ implementation: "opentui-typescript", operations: n, warmup: 20,
  errors: 0, checksum, parse_checksum, parse_ms, model_ms, agents: snap.agents.length,
  rows: rows.length, row_ids: rows.map(r => r.id), presentation }))
