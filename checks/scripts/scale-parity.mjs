// Scale-lab correctness cross-check per tier: the TypeScript Source snapshot
// against `t3-agents-core --db PATH --now-ms N --dump-snapshot`, normalized the
// way checks/scripts/demo-parity.mjs does it (recursive null stripping, root
// path ignored). The product suppresses private fields on secret and link cards.
// Apply that rule only to the reference so a disclosure in Rust fails the check.
//
//   node checks/scripts/scale-parity.mjs [--schema t3|demo] [tiers...]
//   node checks/scripts/scale-parity.mjs --db PATH [PATH...] [--now-ms N]
//
// --schema t3 (default) reads checks/generated/scale/t3/<tier>.sqlite;
// --schema demo reads the legacy checks/generated/scale/<tier>.sqlite.
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { dirname, resolve, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawnSync } from 'node:child_process'
import { Source } from '../../oracle/src/source.ts'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const binary = resolve(process.env.T3_NATIVE_BINARY ?? join(root, '..', 'target/release/t3-agents-core'))
const DEFAULT_NOW = 1794484800000
// The port to newest main is in: nothing is excluded any more.
const KNOWN_GAP = new Set([])
const argv = process.argv.slice(2)
const opt = (name, fallback) => {
  const i = argv.indexOf(name)
  return i < 0 ? fallback : argv[i + 1]
}
const schema = opt('--schema', 't3')
// A database that has been through the soak's write stream has its own clock; pass its now_ms.
const NOW = Number(opt('--now-ms', DEFAULT_NOW))
// Positional arguments: skip flag values.
const positional = argv.filter((a, i) => !a.startsWith('--') && argv[i - 1] !== '--schema' && argv[i - 1] !== '--db' && argv[i - 1] !== '--now-ms')
let dbs
if (argv.includes('--db')) {
  const i = argv.indexOf('--db')
  dbs = argv.slice(i + 1).filter((a, j, rest) => !a.startsWith('--') && rest[j - 1] !== '--now-ms')
  if (!dbs.length) throw new Error('--db needs at least one path')
} else {
  const dir = schema === 't3' ? join(root, 'generated/scale/t3') : join(root, 'generated/scale')
  dbs = (positional.length ? positional : ['s', 'm', 'l', 'xl', 'xxl']).map((t) => join(dir, `${t}.sqlite`))
}

function normalized(value, root = false) {
  if (Array.isArray(value)) return value.map((v) => normalized(v))
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value)
        .filter(([k, v]) => v !== null && !KNOWN_GAP.has(k) && !(root && k === 'path'))
        .map(([k, v]) => [k, normalized(v)]),
    )
  return value
}

function diffs(a, b, path, out, cap) {
  if (out.length >= cap) return
  if (Object.is(a, b)) return
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) out.push(`${path}: length ${a.length} != ${b.length}`)
    for (let i = 0; i < Math.min(a.length, b.length); i++) diffs(a[i], b[i], `${path}[${i}]`, out, cap)
    return
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) diffs(a[k], b[k], path ? `${path}.${k}` : k, out, cap)
    return
  }
  out.push(`${path}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`)
}

const report = { binary_sha256: createHash('sha256').update(readFileSync(binary)).digest('hex'), now: NOW, known_gap_fields: [...KNOWN_GAP], tiers: {} }
let failures = 0
for (const path of dbs) {
  const tier = dbs.length > 1 ? path.split('/').pop().replace('.sqlite', '') : path
  const expected = normalized(new Source(path).snapshot(NOW), true)
  // Keep the items, ownership, ordering and timings. A card contributes metadata only.
  for (const agent of expected.agents) {
    for (const card of [...agent.items, ...agent.recentFailures, agent.now]) {
      if (card?.type !== 'secret_request' && card?.type !== 'link_request') continue
      card.label = ''
      delete card.tool
      delete card.exit_code
    }
  }
  const result = spawnSync(binary, ['--db', path, '--now-ms', String(NOW), '--dump-snapshot'], { encoding: 'utf8', timeout: 300000, maxBuffer: 1024 * 1024 * 1024 })
  if (result.status !== 0) {
    report.tiers[tier] = { error: (result.stderr || result.error?.message || 'spawn failed').slice(0, 500) }
    failures++
    continue
  }
  const actual = normalized(JSON.parse(result.stdout), true)
  const found = []
  diffs(actual, expected, '', found, 40)
  report.tiers[tier] = {
    agents_rust: actual.agents.length,
    agents_ts: expected.agents.length,
    diffs_outside_known_gap: found.length,
    samples: found.slice(0, 10),
  }
  if (found.length) failures++
}
console.log(JSON.stringify(report, null, 1))
process.exit(failures ? 1 : 0)
