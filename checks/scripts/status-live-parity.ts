// Synthetic real-schema mutation stream against the production watcher state
// machine. Virtual clock + stdin acknowledgements avoid polling/sleep races and
// let us check silent ticks too. No event payloads or user database are read.
import { spawn, spawnSync } from "node:child_process"
import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { createInterface } from "node:readline"
import { line } from "../../oracle/src/status.ts"
import { Source } from "../../oracle/src/source.ts"

const root = resolve(import.meta.dirname, "..")
// Strip only additive numeric fields, preserving legacy JSON bytes and surrogate escapes.
const legacy = (s: string) => s.replace(/,"openAsks":\d+,"estimatedAsks":\d+/g, "")
const binary = resolve(process.env.T3_NATIVE_BINARY ?? join(root, "..", "target/release/loupe-core"))
const directory = mkdtempSync(join(tmpdir(), "t3-status-live-"))
const home = join(directory, "empty-home")
mkdirSync(home)
const env = { ...process.env, T3CODE_HOME: home }
const epoch = 1794484800000
const tiers = process.argv.slice(2).length ? process.argv.slice(2) : ["s", "m", "l"]
let checks = 0
let writes = 0
const errors: string[] = []

function scale(args: string[]) {
  const run = spawnSync(process.execPath, ["--experimental-ffi", "--disable-warning=ExperimentalWarning",
    join(root, "scripts/scale-db.ts"), ...args], { env, encoding: "utf8", timeout: 120_000 })
  if (run.status !== 0) throw new Error(`scale-db ${args.join(" ")}: ${run.status}: ${run.stderr}`)
  return JSON.parse(run.stdout)
}

try {
  for (const tier of tiers) {
    const path = join(directory, `${tier}.sqlite`)
    const generated = scale(["--tier", tier, "--schema", "t3", "--db", path])
    if (tier === "real") {
      const { rows, shape, events } = generated
      assert.ok(shape.items_per_thread.sum >= 50_000, "stress tier has a large item history")
      assert.ok(shape.items_per_thread.p99 > 10 * shape.items_per_thread.p50, "thread histories are skewed")
      assert.ok(shape.item_bytes.p99 > 10 * shape.item_bytes.p50, "item payloads are skewed")
      assert.ok(shape.recent_3h.n > 0 && shape.recent_24h.n > shape.recent_3h.n
        && shape.recent_24h.n < shape.items_per_thread.sum, "recent, intermediate and older activity")
      assert.equal(rows.orchestration_v2_events, 0)
      assert.ok(events.gap_free && events.min > 1, "pruned event log has a continuous surviving tail")
      assert.equal(events.max, events.meta["thread-projections"])
      assert.equal(events.min - 1, events.meta["event-log-pruned-through"])
      assert.equal(events.min - 1, events.meta["event-log-prune-mark"])
    }
    const watcher = spawn(binary, ["--status", "--db", path, "--probe-status-db"], {
      env, stdio: ["pipe", "pipe", "pipe"],
    })
    let stderr = ""
    watcher.stderr.on("data", chunk => { stderr += chunk })
    const closed = new Promise<number | null>((resolve, reject) => {
      watcher.on("error", reject)
      watcher.on("close", resolve)
    })
    const lines = createInterface({ input: watcher.stdout })[Symbol.asyncIterator]()
    const kinds = new Set<string>()
    async function check(label: string, now: number) {
      const expected = line(new Source(path), now)
      watcher.stdin.write(String(now) + "\n")
      const timeout = setTimeout(() => watcher.kill("SIGKILL"), 30_000)
      let result: IteratorResult<string>
      try {
        result = await lines.next()
      } finally {
        clearTimeout(timeout)
      }
      checks++
      if (result.done || legacy(result.value) !== expected) {
        errors.push(`${tier} ${label}\n  oracle: ${expected}\n  watcher: ${result.value}\n  stderr: ${stderr}`)
      }
    }
    try {
      await check("initial", epoch)
      for (let step = 1; step <= 24; step++) {
        const mutation = scale(["--db", path, "--mutate", String(step)])
        kinds.add(mutation.kind)
        writes++
        await check(`write ${step} ${mutation.kind}`, epoch + step * 1000)
      }
      for (let burst = 0; burst < 3; burst++) {
        const mutation = scale(["--db", path, "--mutate", String(25 + burst), "--touch", "10"])
        kinds.add(mutation.kind)
        writes++
        await check(`burst ${burst}`, epoch + 30_000 + burst * 2000)
      }
      await check("idle", epoch + 35_000)
      await check("failure-window-aged", epoch + 700_000)
      if (tier === "real") {
        assert.ok(kinds.has("delegated-activity") && kinds.has("remote-progress"), "both remote mutation paths ran")
      }
      console.log(JSON.stringify({ tier, checks: 30, writes: 27, kinds: [...kinds] }))
    } finally {
      watcher.stdin.end()
      const exit = await closed
      if (exit !== 0) errors.push(`${tier}: watcher exit ${exit}: ${stderr}`)
    }
  }
} finally {
  rmSync(directory, { recursive: true, force: true })
}
console.log(JSON.stringify({ checks, writes, errors: errors.length }))
for (const error of errors) console.error(error)
if (errors.length) process.exitCode = 1
