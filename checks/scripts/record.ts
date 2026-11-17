// Opt-in recorder shared by the parity gates.
//
// A gate calls `record.case(...)` where it holds one input and the oracle's expected output, and
// `record.asset(...)` / `record.tree(...)` for every input it built with the original's own code
// (a synthetic SQLite database, a T3 home, an event script, a snapshot). With `T3_RECORD` pointing
// at a file, the process writes one golden fixture at exit; without it, every call is a no-op and
// the gate behaves exactly as before. `checks/scripts/export-*.ts` sets the variable, runs the
// gate twice and requires identical bytes, then installs the fixture under
// `checks/fixtures/golden/`.
//
// Assets are content-addressed, so the same database recorded twice costs one entry. A token is
// `@<kind>-<hash>`; a case refers to it in place of a path, and the Rust test rebuilds the input
// and puts the real path back.
//
// Nothing here reads the real T3 home: gates stay on their own temporary directories.
import { createHash } from "node:crypto"
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { gzipSync } from "node:zlib"
import { dashboardRevision, dumpDatabase } from "./golden.ts"

const target = process.env.T3_RECORD
const generator = process.env.T3_RECORD_GENERATOR ?? process.argv[1] ?? "unknown"
const exporter = process.env.T3_RECORD_EXPORTER ?? ""
const note = process.env.T3_RECORD_NOTE ?? ""
const cases: unknown[] = []
const assets = new Map<string, Record<string, unknown>>()
let flushed = false

export const enabled = Boolean(target)

/** Appends one recorded case. JSON-serializable values only. */
export function recordCase(value: unknown): void {
  if (target) cases.push(value)
}

function contentToken(kind: string, body: string): string {
  return `${kind}-${createHash("sha256").update(body).digest("hex").slice(0, 16)}`
}

function isSqlite(path: string): boolean {
  try {
    return statSync(path).isFile() && readFileSync(path).subarray(0, 16).toString("latin1").startsWith("SQLite format 3")
  } catch {
    return false
  }
}

/**
 * Registers one input file and returns the token that replaces its path in a case. A SQLite
 * database is kept as SQL text so the Rust test can rebuild it row for row; any other file is
 * kept verbatim.
 */
export function recordAsset(path: string, assetKind: "sql" | "text" = isSqlite(path) ? "sql" : "text"): string {
  if (!target) return "@unused"
  const body = assetKind === "sql" ? dumpDatabase(path) : readFileSync(path, "utf8")
  const token = contentToken(assetKind, body)
  if (!assets.has(token)) assets.set(token, assetKind === "sql" ? { kind: "sql", sql: body } : { kind: "text", text: body })
  return `@${token}`
}

/**
 * Registers a whole directory (a synthetic T3 home, for instance) and returns its token. Files
 * keep their relative paths, so a test can put the directory back together.
 */
export function recordTree(root: string): string {
  if (!target) return "@unused"
  const walk = (dir: string, prefix: string, out: Record<string, string>) => {
    for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const path = join(dir, entry.name)
      const name = prefix ? `${prefix}/${entry.name}` : entry.name
      if (entry.isDirectory()) walk(path, name, out)
      else out[name] = recordAsset(path)
    }
  }
  const files: Record<string, string> = {}
  walk(root, "", files)
  const token = `tree-${createHash("sha256").update(JSON.stringify(files)).digest("hex").slice(0, 16)}`
  if (!assets.has(token)) assets.set(token, { kind: "tree", files })
  return `@${token}`
}

function flush(): void {
  if (!target || flushed) return
  flushed = true
  const body: Record<string, unknown> = {
    version: 1,
    dashboard: dashboardRevision(),
    generator,
    exporter,
    note,
    cases: [...cases],
    assets: Object.fromEntries(assets),
  }
  writeFileSync(target, gzipSync(JSON.stringify(body), { level: 9, mtime: 0 }))
  // A short, stable summary on stderr; the gate's own stdout must stay untouched.
  process.stderr.write(`recorded ${cases.length} cases and ${assets.size} inputs to ${target}\n`)
}

process.on("exit", flush)

