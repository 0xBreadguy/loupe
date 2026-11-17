// Compare the original and the Rust build on a real T3 database without revealing what is in it.
//
//   node --experimental-ffi --disable-warning=ExperimentalWarning \
//     checks/scripts/own-data-check.mjs --db ~/.t3/userdata/statev2.sqlite [--rounds 5] [--polls 120]
//     [--only schema,snapshot,once,status,incremental]
//
// Everything this prints is a count, a hash, a duration, or the name of something in T3's
// schema. It never prints a value read from a row: where the two sides differ it reports the
// field path and the two values' types and lengths, nothing more. Nothing is written to disk
// and the database is not copied. Both readers open it read-only, as the installed app does.
//
// T3 may be writing while this runs, so each comparison is a sandwich: original, Rust,
// original again, at one pinned clock. A round only counts when the two original results
// are identical, which shows the allowed fields did not change in between.
//
// Sections: schema against the lab's reference, the reader's snapshot, the --once screen,
// the status line, and the incremental reader checked against the full reader on every poll.
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { spawn, spawnSync } from 'node:child_process'
import { DatabaseSync } from 'node:sqlite'
import { Source } from '../../oracle/src/source.ts'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const argv = process.argv.slice(2)
const opt = (name, fallback) => {
  const i = argv.indexOf(name)
  return i < 0 ? fallback : argv[i + 1]
}
const db = resolve((opt('--db') ?? '').replace(/^~(?=\/)/, homedir()))
if (!opt('--db')) {
  console.error('own-data-check needs --db PATH. It never picks a database by itself.')
  process.exit(2)
}
const rounds = Number(opt('--rounds', '5'))
// --only schema,snapshot,once,status,incremental runs just those sections.
const only = new Set((opt('--only', 'schema,snapshot,once,status,incremental')).split(','))
const polls = Number(opt('--polls', '120'))
const core = resolve(opt('--core', process.env.T3_NATIVE_BINARY ?? join(root, '..', 'target/release/t3-agents-core')))
const term = resolve(opt('--term', join(root, '..', 'target/release/t3-agents-term')))
const main = join(root, '..', 'oracle/src/main.ts')
const fixedClock = pathToFileURL(join(root, 'scripts/fixed-clock.mjs')).href
const NODE = ['--experimental-ffi', '--disable-warning=ExperimentalWarning', '--import', fixedClock]
const sha = (text) => createHash('sha256').update(text).digest('hex').slice(0, 16)
// Key order is not part of equality: the two sides serialize the same model differently.
const canonical = (v) => (Array.isArray(v) ? `[${v.map(canonical).join(',')}]` : v && typeof v === 'object' ? `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(',')}}` : JSON.stringify(v))
const report = { version: 1, core_sha256: sha(readFileSync(core)), term_sha256: sha(readFileSync(term)) }
let failures = 0

// ---- schema -------------------------------------------------------------------------------
// Names and definitions of tables, columns and indexes are T3's schema, not anyone's data.
// The reference is built by running the lab's schema file, so added columns are accounted for.
if (only.has('schema')) {
  const reference = new DatabaseSync(':memory:')
  reference.exec(readFileSync(join(root, 'fixtures/t3-schema.sql'), 'utf8'))
  const real = new DatabaseSync(db, { readOnly: true })
  real.exec('PRAGMA query_only = 1')
  const squash = (sql) => (sql ?? '').replace(/\s+/g, ' ').replace(/\s*([(),])\s*/g, '$1').replace(/"/g, '').replace(/if not exists /i, '').trim().toLowerCase()
  const tablesOf = (h) => h.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'").all().map((r) => r.name)
  const columnsOf = (h, t) => new Map(h.prepare(`PRAGMA table_info("${t}")`).all().map((c) => [c.name, `${String(c.type).toLowerCase()}${c.notnull ? ' not null' : ''}${c.pk ? ' pk' : ''}`]))
  const indexesOf = (h, t) => new Map(h.prepare("SELECT name, sql FROM sqlite_master WHERE type = 'index' AND tbl_name = ? AND sql IS NOT NULL").all(t).map((r) => [r.name, squash(r.sql)]))
  const realTables = new Set(tablesOf(real))
  const schema = { tables_in_reference: 0, tables_missing: [], columns_missing: [], columns_added: [], columns_typed_differently: [], indexes_in_reference: 0, indexes_identical: 0, indexes_missing: [], indexes_defined_differently: [], indexes_added: [], row_counts: {} }
  for (const table of tablesOf(reference)) {
    schema.tables_in_reference++
    if (!realTables.has(table)) {
      schema.tables_missing.push(table)
      continue
    }
    const want = columnsOf(reference, table)
    const have = columnsOf(real, table)
    for (const [name, kind] of want) {
      if (!have.has(name)) schema.columns_missing.push(`${table}.${name}`)
      else if (have.get(name) !== kind) schema.columns_typed_differently.push(`${table}.${name}`)
    }
    for (const name of have.keys()) if (!want.has(name)) schema.columns_added.push(`${table}.${name}`)
    const wantIndexes = indexesOf(reference, table)
    const haveIndexes = indexesOf(real, table)
    for (const [name, sql] of wantIndexes) {
      schema.indexes_in_reference++
      if (!haveIndexes.has(name)) schema.indexes_missing.push(name)
      else if (haveIndexes.get(name) !== sql) schema.indexes_defined_differently.push(name)
      else schema.indexes_identical++
    }
    for (const name of haveIndexes.keys()) if (!wantIndexes.has(name)) schema.indexes_added.push(name)
    schema.row_counts[table] = real.prepare(`SELECT COUNT(*) AS n FROM "${table}"`).get().n
  }
  schema.sqlite_version = real.prepare('SELECT sqlite_version() AS v').get().v
  schema.journal_mode = real.prepare('PRAGMA journal_mode').get().journal_mode
  schema.page_count = real.prepare('PRAGMA page_count').get().page_count
  report.schema = schema
  reference.close()
  real.close()
}

// ---- helpers ------------------------------------------------------------------------------
function normalized(value, top = false) {
  if (Array.isArray(value)) return value.map((v) => normalized(v))
  if (value && typeof value === 'object')
    return Object.fromEntries(Object.entries(value).filter(([k, v]) => v !== null && !(top && k === 'path')).map(([k, v]) => [k, normalized(v)]))
  return value
}
// A field name from the model is safe to print; a key that came from data is not.
const safeKey = (k) => (/^[A-Za-z_][A-Za-z0-9_]{0,31}$/.test(k) ? k : '<key>')
const shape = (v) => (v === undefined ? 'absent' : v === null ? 'null' : Array.isArray(v) ? `array(${v.length})` : typeof v === 'string' ? `string(${v.length})` : typeof v === 'object' ? `object(${Object.keys(v).length})` : typeof v)
function diffs(a, b, path, out) {
  if (Object.is(a, b)) return
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) out.push(`${path}: length ${a.length} != ${b.length}`)
    for (let i = 0; i < Math.min(a.length, b.length); i++) diffs(a[i], b[i], `${path}[]`, out)
    return
  }
  if (a && b && typeof a === 'object' && typeof b === 'object' && !Array.isArray(a) && !Array.isArray(b)) {
    for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) diffs(a[k], b[k], path ? `${path}.${safeKey(k)}` : safeKey(k), out)
    return
  }
  out.push(`${path}: ${shape(a)} != ${shape(b)}`)
}
const tally = (list) => Object.entries(list.reduce((m, x) => ((m[x] = (m[x] ?? 0) + 1), m), {})).sort((x, y) => y[1] - x[1]).slice(0, 25)
const env = (now) => ({ ...process.env, T3_FIXED_NOW: String(now), T3CODE_HOME: join(root, 'generated/no-such-home') })
const run = (file, args, now) => spawnSync(file, args, { env: env(now), encoding: 'buffer', timeout: 600000, maxBuffer: 2 ** 31 - 1 })

// A sandwich: `first()` twice around `second()`. Returns the valid rounds and how they compared.
function sandwich(name, sides) {
  if (!only.has(name)) return
  const result = { rounds: 0, valid: 0, equal: 0, changed_during_round: 0, errors: 0, where: [] }
  for (let i = 0; i < rounds * 4 && result.valid < rounds; i++) {
    result.rounds++
    const now = Date.now()
    let before, middle, after
    try {
      before = sides.original(now)
      middle = sides.rust(now)
      after = sides.original(now)
    } catch (error) {
      result.errors++
      result.last_error_kind = String(error?.code ?? error?.name ?? 'error')
      continue
    }
    if (before.hash !== after.hash) {
      result.changed_during_round++
      continue
    }
    result.valid++
    if (before.hash === middle.hash) result.equal++
    else if (sides.explain) result.where.push(...sides.explain(before, middle))
  }
  result.where = tally(result.where)
  if (result.valid === 0 || result.equal !== result.valid || result.errors) failures++
  report[name] = result
}

// ---- the reader's snapshot ------------------------------------------------------------------
sandwich('snapshot', {
  original(now) {
    const value = normalized(new Source(db).snapshot(now), true)
    report.agents = value.agents.length
    return { value, hash: sha(canonical(value)) }
  },
  rust(now) {
    const r = run(core, ['--db', db, '--now-ms', String(now), '--dump-snapshot'], now)
    if (r.status !== 0) throw Object.assign(new Error('rust reader failed'), { code: `exit-${r.status}` })
    const value = normalized(JSON.parse(r.stdout.toString('utf8')), true)
    return { value, hash: sha(canonical(value)) }
  },
  explain(a, b) {
    const out = []
    diffs(b.value, a.value, '', out)
    return out
  },
})

// ---- what the command line prints -----------------------------------------------------------
const printed = (r) => ({ hash: sha(Buffer.concat([r.stdout, Buffer.from(`\0${r.status}\0`), r.stderr])), bytes: r.stdout.length, status: r.status })
for (const [name, args, binary] of [['once', ['--once', '--db', db], term], ['status', ['--status', '--db', db], core]]) {
  sandwich(name, {
    original: (now) => printed(run(process.execPath, [...NODE, main, ...args], now)),
    rust: (now) => printed(run(binary, [...args, '--now-ms', String(now)], now)),
    explain: (a, b) => [`exit ${a.status} vs ${b.status}, ${a.bytes} vs ${b.bytes} bytes`],
  })
}

// ---- the incremental reader, checked against the full reader on every poll -------------------
if (only.has('incremental')) await new Promise((done) => {
  const child = spawn(core, ['--bench-reader-poll', '--db', db, '--self-check'], { env: env(Date.now()), stdio: ['pipe', 'pipe', 'pipe'] })
  const live = { polls: 0, served: {}, fallback_reasons: {}, mismatches: 0, poll_ms: [], threads_reread_max: 0, agents: 0 }
  let buffer = ''
  let sent = 0
  const next = () => {
    if (sent >= polls) return child.stdin.end()
    sent++
    child.stdin.write('\n')
  }
  child.stdout.on('data', (chunk) => {
    buffer += chunk
    for (let at; (at = buffer.indexOf('\n')) >= 0; buffer = buffer.slice(at + 1)) {
      const line = JSON.parse(buffer.slice(0, at))
      live.polls++
      live.served[line.served] = (live.served[line.served] ?? 0) + 1
      if (line.fallback_reason) live.fallback_reasons[line.fallback_reason] = (live.fallback_reasons[line.fallback_reason] ?? 0) + 1
      live.mismatches = line.mismatches
      live.agents = line.agents
      if (live.polls > 1) live.poll_ms.push(line.poll_ms)
      live.threads_reread_max = Math.max(live.threads_reread_max, line.threads_reread ?? 0)
      setTimeout(next, 1000)
    }
  })
  let stderrBytes = 0
  child.stderr.on('data', (chunk) => (stderrBytes += chunk.length))
  child.on('close', (code) => {
    const sorted = live.poll_ms.sort((a, b) => a - b)
    const at = (q) => (sorted.length ? Number(sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))].toFixed(3)) : null)
    report.incremental_reader = { ...live, poll_ms: { median: at(0.5), p95: at(0.95), max: at(1) }, exit: code, stderr_bytes: stderrBytes }
    if (code !== 0 || live.mismatches !== 0 || live.polls !== polls) failures++
    done()
  })
  next()
})

report.failures = failures
console.log(JSON.stringify(report, null, 1))
process.exit(failures ? 1 : 0)
