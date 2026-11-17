// Full baseline Canvas/view work. No terminal, window, GPU or service is started.
// This is an elapsed draw+hash metric, never native input-to-present latency.
import { readFileSync } from "node:fs"
import { performance } from "node:perf_hooks"
import { Canvas } from "../../oracle/src/canvas.ts"
import { draw, initialState, observe } from "../../oracle/src/view.ts"
import { derivePalette } from "../../oracle/src/theme.ts"
import { THEMES } from "../../oracle/test/themes.ts"
import type { Snapshot } from "../../oracle/src/source.ts"
import type { Provider } from "../../oracle/src/usage.ts"

const args = process.argv.slice(2)
function option(name: string, fallback?: string): string {
  const index = args.indexOf(name)
  if (index < 0 && fallback !== undefined) return fallback
  if (index < 0 || !args[index + 1] || args[index + 1].startsWith("--")) throw new Error(`missing ${name}`)
  return args[index + 1]
}
const grid = /^(\d+)x(\d+)$/.exec(option("--grid", "233x55"))
if (!grid) throw new Error("--grid must be WxH")
const columns = Number(grid[1]), lines = Number(grid[2])
if (columns < 48 || lines < 14 || columns > 1000 || lines > 1000) throw new Error("grid outside supported bounds")
const snapshot: Snapshot = JSON.parse(readFileSync(option("--snapshot"), "utf8"))
const usage: Provider[] = JSON.parse(readFileSync(option("--usage"), "utf8"))
if (!Array.isArray(snapshot.agents) || !Number.isFinite(snapshot.at) || !Array.isArray(usage)) throw new Error("invalid snapshot/usage")
const canvas = new Canvas(columns, lines), palette = derivePalette(THEMES.tokyo)
const state = initialState(2)
state.calm = true; state.demo = true; state.follow = false; state.bootAt = 0
observe(state, snapshot, snapshot.at)
let frame = draw(canvas, state, snapshot, usage, palette, snapshot.at)

// DJB2 xor variant, row-major, fields interleaved cp/fg/bg/attr for EVERY cell.
// imul has the same wrapping low 32 bits as ((hash*33)^field) >>> 0.
function outputHash(): number {
  let hash = 5381
  for (let index = 0; index < canvas.ch.length; index++) {
    hash = (Math.imul(hash, 33) ^ canvas.ch[index]) >>> 0
    hash = (Math.imul(hash, 33) ^ canvas.fg[index]) >>> 0
    hash = (Math.imul(hash, 33) ^ canvas.bg[index]) >>> 0
    hash = (Math.imul(hash, 33) ^ canvas.at[index]) >>> 0
  }
  return hash
}
const hex = (value: number) => value.toString(16).padStart(8, "0")
if (args.includes("--frame-json")) {
  console.log(JSON.stringify({ w: columns, h: lines, selected: state.selected, rows: frame.rows.length,
    text: canvas.toText(), ch: [...canvas.ch], fg: [...canvas.fg], bg: [...canvas.bg], at: [...canvas.at],
    output_hash: hex(outputHash()) }))
} else {
  const operations = Number(option("--bench-draw", "100"))
  if (!Number.isSafeInteger(operations) || operations < 1 || operations > 1000) throw new Error("--bench-draw must be integer 1..1000")
  // One initial draw above establishes selection. Both sides then warm 20 full
  // draws and hashes. These are excluded from draw_ms but included in process wall.
  for (let i = 0; i < 20; i++) {
    frame = draw(canvas, state, snapshot, usage, palette, snapshot.at)
    outputHash()
  }
  let checksum = 0, hash = 0
  const start = performance.now()
  for (let i = 0; i < operations; i++) {
    frame = draw(canvas, state, snapshot, usage, palette, snapshot.at)
    hash = outputHash()
    checksum += hash
  }
  const draw_ms = performance.now() - start
  console.log(JSON.stringify({ implementation: "opentui-typescript-full-canvas", operations, warmup: 20,
    errors: 0, draw_ms, checksum, output_hash: hex(hash), rows: frame.rows.length, columns, lines }))
}
