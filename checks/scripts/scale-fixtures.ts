// Synthetic snapshots at swarm sizes, for measuring how frame cost grows with agent count.
// Invented data only: the same generator as the `large` case, at several sizes.
import { mkdirSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"
import { ROOT, largeSnapshot } from "./fixtures.ts"

export const SIZES = [25, 256, 1024, 4096, 16384]

const out = resolve(process.argv[2] ?? join(ROOT, "generated/scale"))
const sizes = process.argv.slice(3).map(Number).filter((n) => Number.isSafeInteger(n) && n > 0)
mkdirSync(out, { recursive: true })
writeFileSync(join(out, "usage.json"), "[]\n")
const manifest: Record<string, unknown>[] = []
for (const agents of sizes.length ? sizes : SIZES) {
  const snap = largeSnapshot(agents)
  const text = JSON.stringify(snap)
  writeFileSync(join(out, `agents-${agents}.json`), text + "\n")
  manifest.push({ agents, events: snap.agents.reduce((n, a) => n + a.events.length, 0),
    history: snap.agents.reduce((n, a) => n + a.history.length, 0), bytes: Buffer.byteLength(text) })
}
writeFileSync(join(out, "manifest.json"), JSON.stringify({ version: 1, sizes: manifest }, null, 2) + "\n")
console.log(JSON.stringify({ out, sizes: manifest }))
