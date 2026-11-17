// Compare the composed screens of the original and the Rust build on a real T3 database,
// without revealing what is in it.
//
//   node --experimental-ffi --disable-warning=ExperimentalWarning \
//     checks/scripts/own-screen-check.mjs --db ~/.t3/userdata/statev2.sqlite [--shapes 6]
//
// The snapshot (only the fields the dashboard shows) is read once, read-only, and kept in a
// private file in memory (/dev/shm, mode 0600, removed when this exits). It is never written
// to disk. For every window size and key sequence below, both compose the
// screen and the cells are compared. What is printed: counts of cells that differ, and, with
// --shapes N, the shape of up to N differing rows, where every letter is shown as "a" and
// every digit as "9". No text from the database is printed.
import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const argv = process.argv.slice(2)
const opt = (name, fallback) => (argv.includes(name) ? argv[argv.indexOf(name) + 1] : fallback)
if (!opt('--db')) {
  console.error('own-screen-check needs --db PATH. It never picks a database by itself.')
  process.exit(2)
}
const db = resolve(opt('--db').replace(/^~(?=\/)/, homedir()))
const core = resolve(opt('--core', join(root, 'target/release/t3-agents-core')))
const shapes = Number(opt('--shapes', '0'))
// The last stage before the terminal. The original hands its renderer (OpenTUI) the canvas as
// runs of text, and the renderer regroups each run into grapheme clusters; the Rust build does
// the same in crates/core/src/cluster.rs. So equal canvases are not yet equal screens: this
// also puts every canvas through the original's own flush() into a real OpenTUI buffer and
// through the Rust layout, under both width rules, and compares the two buffers cell by cell.
const { OptimizedBuffer, ResourceContext, RGBA } = await import('../../oracle/node_modules/@opentui/core/index.node.js')
const { flush } = await import('../../oracle/src/app.ts')
const { Canvas } = await import('../../oracle/src/canvas.ts')
const DEFAULT_BG = 0x0100_0000
const owner = new ResourceContext({ objectCapacity: 64, renderCellsMax: 1 << 20 })
const transparent = RGBA.fromInts(0, 0, 0, 0)
const decoder = new TextDecoder()
const buffers = new Map()
// What the original's buffer holds for one canvas: per cell, its kind (a character, the start of
// a cluster, or a later cell of one), its text and width, colors and weight. As in cluster-fuzz.ts.
function originalBuffer(c, method) {
  const key = `${c.w}x${c.h}:${method}`
  let buf = buffers.get(key)
  if (!buf) buffers.set(key, (buf = OptimizedBuffer.create(c.w, c.h, method, { owner })))
  buf.clear(transparent)
  const canvas = new Canvas(c.w, c.h)
  canvas.ch.set(c.ch); canvas.fg.set(c.fg); canvas.bg.set(c.bg); canvas.at.set(c.at)
  flush(canvas, buf, 0, 0)
  return buf.withResolvedChars({ addLineBreaks: false, cellLengths: true }, (bytes, cells, lengths) => {
    const out = []
    let offset = 0
    for (let i = 0; i < c.w * c.h; i++) {
      const char = cells.char[i] >>> 0
      const text = decoder.decode(bytes.subarray(offset, offset + lengths[i]))
      offset += lengths[i]
      const type = (char & 0xc0000000) >>> 0
      const ext = (char >>> 26) & 0xf
      const rgb = (v, k) => (v[k * 4] << 16) | (v[k * 4 + 1] << 8) | v[k * 4 + 2]
      const bg = cells.bg[i * 4 + 3] === 0 ? DEFAULT_BG : rgb(cells.bg, i)
      const bold = (cells.attributes[i] & 1) !== 0
      if (type === 0x80000000) out.push([1, text, ext + 1, rgb(cells.fg, i), bg, bold])
      else if (type === 0xc0000000) out.push([2, '', ext, rgb(cells.fg, i), bg, bold])
      else out.push([0, String.fromCodePoint(char), 1, rgb(cells.fg, i), bg, bold])
    }
    return out
  })
}
const NODE = ['--experimental-ffi', '--disable-warning=ExperimentalWarning']
const env = { ...process.env, TZ: process.env.TZ ?? 'UTC', T3CODE_HOME: join(root, 'checks/generated/no-such-home') }
const run = (cmd, args, input) => {
  const p = spawnSync(cmd, args, { cwd: root, env, input, encoding: 'utf8', maxBuffer: 2 ** 30 })
  if (p.status !== 0) throw new Error(`${cmd.split('/').pop()} exited ${p.status}`)
  return p.stdout
}
const snapshot = run(core, ['--db', db, '--now-ms', String(Date.now()), '--dump-snapshot'])
const agents = JSON.parse(snapshot).agents.length
const held = mkdtempSync('/dev/shm/t3-screen-check-')
const file = join(held, 'snapshot.json')
const drop = () => rmSync(held, { recursive: true, force: true })
process.on('exit', drop)
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => { drop(); process.exit(130) })
writeFileSync(file, snapshot, { mode: 0o600 })
const grids = ['233x55', '160x44', '120x40', '95x35', '80x24']
// The "next waiting" key is left out: when nobody is waiting the real app shows a note, the Rust
// build shows the same note, and the reference harness used here does not show notes at all.
const actions = ['', 'down', 'down,down,down', 'end', 'pagedown', 'pagedown,pagedown', 'space', 'home,space', 'a', 'a,a', 't', 't,t', 'right', 'right,right', 'down,right', '?', 'end,up,up', 'a,down,down,down,down', 'end,up,up,up,up,up,up', 'a,a,down,down,pagedown']
const mask = (s) => s.replace(/\p{L}/gu, 'a').replace(/\p{N}/gu, '9')
const report = { agents, states: 0, states_that_differ: 0, text: 0, colors: 0, attributes: 0, by_state: [], shapes: [],
  laid_out: { buffers_compared: 0, cells: 0, clusters_of_several_code_points: 0, buffers_that_differ: 0, cells_that_differ: 0 } }
for (const grid of grids) for (const steps of actions) {
  const extra = steps ? ['--actions', steps] : []
  const a = JSON.parse(run('node', [...NODE, 'checks/scripts/grid-reference.ts', '--snapshot', file, '--grid', grid, ...extra, '--json']))
  const b = JSON.parse(run(core, ['--snapshot', file, '--grid', grid, ...extra, '--frame-json']))
  let text = 0, colors = 0, attributes = 0
  for (let i = 0; i < a.ch.length; i++) {
    text += a.ch[i] !== b.ch[i] ? 1 : 0
    colors += a.fg[i] !== b.fg[i] || a.bg[i] !== b.bg[i] ? 1 : 0
    attributes += a.at[i] !== b.at[i] ? 1 : 0
  }
  report.states++
  {
    const [w, h] = grid.split('x').map(Number)
    const canvas = { w, h, ch: a.ch, fg: a.fg, bg: a.bg, at: a.at }
    const methods = ['unicode', 'wcwidth']
    const rust = run(core, ['--layout-rows'], methods.map((m) => JSON.stringify({ m, ...canvas })).join('\n') + '\n').split('\n').filter(Boolean)
    methods.forEach((m, k) => {
      const want = originalBuffer(canvas, m), got = JSON.parse(rust[k])
      let differ = 0
      for (let i = 0; i < want.length; i++) {
        if (want[i][0] === 1 && [...want[i][1]].length > 1) report.laid_out.clusters_of_several_code_points++
        if (JSON.stringify(want[i]) !== JSON.stringify(got[i])) differ++
      }
      report.laid_out.buffers_compared++
      report.laid_out.cells += want.length
      report.laid_out.cells_that_differ += differ
      if (differ) report.laid_out.buffers_that_differ++
    })
  }
  report.text += text; report.colors += colors; report.attributes += attributes
  if (text || colors || attributes) {
    report.states_that_differ++
    if (report.by_state.length < 12) report.by_state.push({ grid, actions: steps, text, colors, attributes })
    if (text && report.shapes.length < shapes) {
      const x = a.text.split('\n'), y = b.text.split('\n')
      for (let r = 0; r < x.length && report.shapes.length < shapes; r++) {
        if (x[r] !== y[r]) {
          const p = mask(x[r]), q = mask(y[r] ?? '')
          let at = 0
          while (at < p.length && at < q.length && x[r][at] === y[r][at]) at++
          // The shape around the first difference only, so a row is never shown whole.
          report.shapes.push({ grid, actions: steps, row: r, column: at, original: p.slice(Math.max(0, at - 30), at + 50), rust: q.slice(Math.max(0, at - 30), at + 50) })
        }
      }
    }
  }
}
console.log(JSON.stringify(report, null, 1))
for (const buf of buffers.values()) buf.destroy()
process.exit(report.states_that_differ || report.laid_out.buffers_that_differ ? 1 : 0)
