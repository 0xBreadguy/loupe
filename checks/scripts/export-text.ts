// Records the text and number helpers of `text-parity.ts` from the TypeScript original at main
// `2f1d553`, so `crates/core/tests/golden_text.rs` can check the Rust build with only `cargo test`.
//
// The gate feeds about 5.1 million generated inputs through the helpers, so the fixture keeps a
// digest per group of inputs and per block of outputs, never the data itself (see text-record.ts
// and checks/fixtures/golden/README.md). The gate runs once per timezone, twice each: the fixture
// is installed only if both runs of every zone are byte-identical. The groups where the zones
// agree are stored once; where they disagree (clock and when) each zone keeps its own digests.
//
//   node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/export-text.ts
import { spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { gunzipSync, gzipSync } from "node:zlib"
import { GOLDEN, ROOT } from "./golden.ts"

const NAME = "text.json.gz"
const GATE = join(ROOT, "scripts/text-parity.ts")
const ZONES = ["UTC", "Asia/Kolkata", "America/Los_Angeles"]
const NOTE = "The text and number helpers over about 5.1 million generated inputs, as FNV-1a digests: one per group of inputs (to prove the Rust port of the generators), the first 64 outputs of each group verbatim, and one digest per 64 outputs after that. Recorded under UTC, Asia/Kolkata and America/Los_Angeles; groups whose outputs differ by zone carry their own digests."

type Case = Record<string, any>
type Record_ = { version: number; dashboard: unknown; generator: string; exporter: string; note: string; cases: Case[]; assets: Record<string, unknown> }

const repo = resolve(ROOT, "..")
const home = mkdtempSync(join(tmpdir(), "t3-export-home-"))

function record(zone: string, run: number): Buffer {
  const target = join(ROOT, "generated", `record-text-${zone.replace(/\W/g, "-")}-${run}.json.gz`)
  mkdirSync(dirname(target), { recursive: true })
  const result = spawnSync(process.execPath, ["--experimental-ffi", "--disable-warning=ExperimentalWarning", GATE], {
    cwd: repo,
    encoding: "utf8",
    timeout: 3_600_000,
    maxBuffer: 256 * 1024 * 1024,
    // The same fixed environment `exportGate` uses, except for the timezone, which is the point.
    env: {
      PATH: process.env.PATH ?? "/usr/bin:/bin",
      HOME: home,
      LANG: "C.UTF-8",
      TZ: zone,
      T3CODE_HOME: home,
      ...(process.env.T3_NATIVE_BINARY ? { T3_NATIVE_BINARY: process.env.T3_NATIVE_BINARY } : {}),
      T3_RECORD: target,
      T3_RECORD_GENERATOR: relative(repo, GATE),
      T3_RECORD_EXPORTER: relative(repo, fileURLToPath(import.meta.url)),
      T3_RECORD_NOTE: NOTE,
      T3_RECORD_RUN: String(run),
    },
  })
  if (result.status !== 0) throw new Error(`${GATE} under TZ=${zone} (run ${run + 1}) exited ${result.status}\n${result.stdout.split("\n").filter((line) => !/ 0 differ$/.test(line)).join("\n")}\n${result.stderr}`)
  return readFileSync(target)
}

function sameAcross<T>(label: string, values: T[]): T {
  const first = JSON.stringify(values[0])
  values.forEach((value, index) => {
    if (JSON.stringify(value) !== first) throw new Error(`${label}: zone ${ZONES[index]} disagrees with ${ZONES[0]}`)
  })
  return values[0]!
}

try {
  const records: Record_[] = ZONES.map((zone) => {
    const first = record(zone, 0)
    const second = record(zone, 1)
    if (!first.equals(second)) throw new Error(`${zone}: two runs differ; the fixture is not deterministic`)
    return JSON.parse(gunzipSync(first).toString())
  })
  const base = records[0]!
  for (const other of records) {
    if (other.cases.length !== base.cases.length) throw new Error("the zones recorded different numbers of cases")
  }
  const cases: Case[] = base.cases.map((item, index) => {
    const peers = records.map((r) => r.cases[index]!)
    // Everything but a group's outputs must agree: its identity, its inputs, the oracle's own
    // decisions about which inputs it can run.
    const { head, blocks, ...identity } = item
    sameAcross(`case ${index} (${item.kind} ${item.name ?? item.label ?? ""})`, peers.map(({ head: _h, blocks: _b, ...rest }) => rest))
    if (item.kind !== "group") return item
    const differing: Record<string, { head: string[]; blocks: string[] }> = {}
    peers.forEach((peer, z) => {
      if (z > 0 && (JSON.stringify(peer.head) !== JSON.stringify(head) || JSON.stringify(peer.blocks) !== JSON.stringify(blocks))) {
        differing[ZONES[z]!] = { head: peer.head, blocks: peer.blocks }
      }
    })
    return Object.keys(differing).length ? { ...identity, head, blocks, zones: differing } : item
  })
  const body = { ...base, zones: ZONES, cases }
  const bytes = gzipSync(JSON.stringify(body), { level: 9, mtime: 0 })
  mkdirSync(GOLDEN, { recursive: true })
  writeFileSync(join(GOLDEN, NAME), bytes)
  const groups = cases.filter((c) => c.kind === "group")
  console.log(JSON.stringify({
    fixture: NAME,
    groups: groups.length,
    inputs: groups.reduce((n, g) => n + g.n, 0) + (cases.find((c) => c.kind === "hidden")?.count ?? 0),
    zoneDependent: groups.filter((g) => g.zones).map((g) => `${g.name}#${g.occurrence}`).length,
    gzipBytes: bytes.length,
    deterministic: true,
  }))
} finally {
  rmSync(home, { recursive: true, force: true })
}
