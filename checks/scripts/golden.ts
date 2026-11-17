// Shared shapes for the recorded ("golden") fixtures the Rust tests replay without Node.
//
// A fixture is one gzip-compressed JSON object:
//
//   { version, dashboard: {commit, tree, src}, generator, note, cases: [...], assets: {...} }
//
// `dashboard` says which revision of the frozen oracle produced it. `cases` hold the oracle's
// output for one input each. `assets` hold the inputs the oracle built with its own code
// (synthetic SQLite databases as SQL text, whole snapshots, event scripts), so the Rust test can
// rebuild them and needs no Node, no generated/ directory and no T3 installation.
//
// gzip is written with mtime 0 and no name, so two runs over the same tree produce identical
// bytes. Do not add timestamps or host details to a fixture.
import { spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import { copyFileSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { gzipSync, gunzipSync } from "node:zlib"
import { DatabaseSync } from "node:sqlite"

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
export const GOLDEN = join(ROOT, "fixtures/golden")

/** The frozen oracle's identity: the repository commit, the oracle tree and its sources. */
export function dashboardRevision(): { commit: string; tree: string; src: string; dirty: boolean } {
  const git = (...args: string[]) => {
    const run = spawnSync("git", args, { cwd: resolve(ROOT, ".."), encoding: "utf8" })
    if (run.status !== 0) throw new Error(`git ${args.join(" ")}: ${run.stderr}`)
    return run.stdout.trim()
  }
  // The last commit that touched `oracle/`, not this branch's tip: the oracle's identity must
  // not change every time a commit lands elsewhere, or every fixture would churn for nothing.
  const commit = git("log", "-1", "--format=%H", "--", "oracle")
  const tree = git("rev-parse", "HEAD:oracle")
  const dirty = git("status", "--porcelain", "--", "oracle").length > 0
  const files: string[] = []
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      if (entry.name === "node_modules") continue
      const path = join(dir, entry.name)
      if (entry.isDirectory()) walk(path)
      else files.push(path)
    }
  }
  walk(join(ROOT, "..", "oracle", "src"))
  const hash = createHash("sha256")
  for (const file of files) {
    hash.update(relative(resolve(ROOT, ".."), file))
    hash.update("\0")
    hash.update(readFileSync(file))
    hash.update("\0")
  }
  return { commit, tree, src: hash.digest("hex"), dirty }
}

/**
 * A SQLite database as one SQL script, so another language can rebuild it byte-for-byte
 * (row ids included) without the original's code. Tables, indexes and triggers are written in
 * `sqlite_master` order, rows in rowid order.
 */
export function dumpDatabase(path: string): string {
  const db = new DatabaseSync(path, { readOnly: true })
  try {
    // Internal tables (`sqlite_sequence` and friends) cannot be created by name; SQLite recreates
    // them for the AUTOINCREMENT columns, and an explicit row id updates their counters.
    const objects = db.prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid").all() as
      { type: string; name: string; sql: string }[]
    const lines = ["BEGIN;"]
    for (const object of objects) {
      if (object.type === "table") {
        lines.push(object.sql + ";")
        const columns = (db.prepare(`PRAGMA table_info("${object.name}")`).all() as { name: string }[]).map((c) => c.name)
        const rows = db.prepare(`SELECT rowid AS __rowid__, * FROM "${object.name}" ORDER BY rowid`).all() as Record<string, unknown>[]
        for (const row of rows) {
          const values = [String(row.__rowid__)]
          for (const column of columns) {
            const value = row[column]
            if (value === null || value === undefined) values.push("NULL")
            else if (typeof value === "number") values.push(String(value))
            else if (value instanceof Uint8Array) values.push(`X'${Buffer.from(value).toString("hex")}'`)
            else values.push("'" + String(value).replace(/'/g, "''") + "'")
          }
          lines.push(`INSERT INTO "${object.name}"(rowid,${columns.map((c) => `"${c}"`).join(",")}) VALUES(${values.join(",")});`)
        }
      } else if (object.type === "index") {
        lines.push(object.sql + ";")
      } else {
        // Views and triggers keep their text; they read the same rows back.
        lines.push(object.sql + ";")
      }
    }
    lines.push("COMMIT;")
    return lines.join("\n") + "\n"
  } finally {
    db.close()
  }
}

/**
 * Runs a parity gate twice with the recorder on and installs the fixture only if both runs
 * produced identical bytes. The gate itself must pass (exit 0): a fixture recorded from a failing
 * gate would freeze a difference instead of a match.
 */
export function exportGate(gate: string, args: string[], name: string, note: string): void {
  const home = mkdtempSync(join(tmpdir(), "t3-export-home-"))
  const runs = [0, 1].map((i) => join(ROOT, "generated", `record-${name}-${i}.json.gz`))
  try {
    runs.forEach((record, index) => {
      mkdirSync(dirname(record), { recursive: true })
      const run = spawnSync(process.execPath, ["--experimental-ffi", "--disable-warning=ExperimentalWarning", gate, ...args], {
        cwd: resolve(ROOT, ".."),
        encoding: "utf8",
        timeout: 3_600_000,
        maxBuffer: 256 * 1024 * 1024,
        // A fixed environment: the original and the Rust build must not see the desktop's
        // theme or locale variables, or a recording would depend on the machine that made it.
        env: {
          PATH: process.env.PATH ?? "/usr/bin:/bin",
          HOME: home,
          LANG: "C.UTF-8",
          TZ: "UTC",
          T3CODE_HOME: home,
          T3_RECORD: record,
          T3_RECORD_GENERATOR: relative(resolve(ROOT, ".."), resolve(gate)),
          T3_RECORD_EXPORTER: relative(resolve(ROOT, ".."), fileURLToPath(import.meta.url)),
          T3_RECORD_NOTE: note,
          T3_RECORD_RUN: String(index),
        },
      })
      if (run.status !== 0) throw new Error(`${gate} (run ${index + 1}) exited ${run.status}\n${run.stdout}\n${run.stderr}`)
      if (!readFileSync(record).length) throw new Error(`${gate}: the recorder wrote nothing; is T3_RECORD supported there?`)
    })
    const first = readFileSync(runs[0])
    if (!first.equals(readFileSync(runs[1]))) throw new Error(`${name}: two runs differ; the fixture is not deterministic`)
    mkdirSync(GOLDEN, { recursive: true })
    copyFileSync(runs[0], join(GOLDEN, name))
    const fixture = JSON.parse(gunzipSync(first).toString())
    console.log(JSON.stringify({ fixture: name, cases: fixture.cases.length, inputs: Object.keys(fixture.assets).length, gzipBytes: first.length, deterministic: true }))
  } finally {
    rmSync(home, { recursive: true, force: true })
  }
}

/**
 * FNV-1a (64-bit) over UTF-8, the digest convention the Rust tests use (`golden_support::fnv1a`).
 * A digest only has to notice drift between a recording and a replay, so no crypto hash is needed.
 */
export function fnv1aText(text: string): string {
  let hash = 0xcbf29ce484222325n
  for (const byte of Buffer.from(text, "utf8")) {
    hash ^= BigInt(byte)
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn
  }
  return hash.toString(16).padStart(16, "0")
}

/** The digest of a number stream, matching `golden_support::numbers_digest`. */
export function numbersDigest(values: number[]): string {
  return fnv1aText(values.join(","))
}

/**
 * The digest of a whole JSON value, independent of object key order. The byte stream is:
 *
 *   null `n` · true `T` · false `F` · string `s` then UTF-8 bytes ·
 *   integer `i` then the decimal · other number `f` then 8 big-endian IEEE-754 bytes ·
 *   array `[` values separated by `,` then `]` ·
 *   object `{` then, for each key in UTF-8 byte order, `k` + key + `=` + value + `;`, then `}`
 *
 * A number counts as an integer when it is whole and within 2^53, so `0` and `0.0` agree.
 * `golden_support::canonical_digest` implements the same stream; both sides are checked against
 * the same vectors in `golden_full_read.rs`.
 */
export function canonicalDigest(value: unknown): string {
  const bytes: number[] = []
  const text = (s: string) => { for (const b of Buffer.from(s, "utf8")) bytes.push(b) }
  const emit = (v: unknown) => {
    if (v === null || v === undefined) { bytes.push(110); return }
    if (typeof v === "string") { bytes.push(115); text(v); return }
    if (typeof v === "boolean") { bytes.push(v ? 84 : 70); return }
    if (typeof v === "number") {
      if (Number.isInteger(v) && Math.abs(v) <= Number.MAX_SAFE_INTEGER) { bytes.push(105); text(String(v)) }
      else { bytes.push(102); const b = Buffer.alloc(8); b.writeDoubleBE(v); for (const x of b) bytes.push(x) }
      return
    }
    if (Array.isArray(v)) {
      bytes.push(91)
      v.forEach((item, i) => { if (i) bytes.push(44); emit(item) })
      bytes.push(93)
      return
    }
    const object = v as Record<string, unknown>
    bytes.push(123)
    for (const key of Object.keys(object).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))) {
      bytes.push(107); text(key); bytes.push(61); emit(object[key]); bytes.push(59)
    }
    bytes.push(125)
  }
  emit(value)
  let hash = 0xcbf29ce484222325n
  for (const byte of bytes) {
    hash ^= BigInt(byte)
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn
  }
  return hash.toString(16).padStart(16, "0")
}
