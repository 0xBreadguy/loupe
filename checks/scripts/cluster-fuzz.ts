// Differential fuzz: the original's flush() into OpenTUI 0.8.3's own buffer, against the Rust
// layout (crates/core/src/cluster.rs), on generated canvases full of grapheme clusters.
//
// The terminal cells the original writes are its OpenTUI buffer: the renderer writes every cell of
// it that changed, a cluster as one piece of text with its width. So the buffer is compared cell
// by cell: what each cell holds (a byte of ASCII, the start of a cluster with its text and width,
// or the n-th cell of a cluster), its colors (a cleared cell has the terminal's own background)
// and weight. All three width methods the original picks between are run on every canvas.
//
// Canvases are built two ways. Text rows are drawn with the original's own Canvas.text and fit,
// as the dashboard draws: several pieces per row at increasing columns, in a few styles (so runs
// join and split), cut by a column limit or with an ellipsis, some inside a clip rectangle that
// cuts a wide character in half, some drawn over half of a wide character. Raw rows put any code
// point in any cell, continuation markers anywhere, with styles changing mid-cluster: text the
// canvas cleans (joiners, tags, variation selectors) still reaches drawText this way.
//
//   node --experimental-ffi checks/scripts/cluster-fuzz.ts [--strings N] [--seed S] [--show N]
//   T3_NATIVE_BINARY=<loupe-core> to test another build.
//
//   ... --exhaustive   every code point (surrogates aside) in twenty contexts that tell its grapheme
//                      class and width apart (alone, after a letter, before a combining mark, after
//                      ZWJ, after an emoji modifier base, between Indic letters, beside Hangul jamo,
//                      after a regional indicator, before VS16, after a prepend, ...), each context
//                      drawn with drawText in all three width methods and compared cell by cell. Minutes.
// Same seed, same canvases. Exit 1 on any difference, with the first few and the input.
import { spawnSync } from "node:child_process"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { OptimizedBuffer, ResourceContext, RGBA } from "../../oracle/node_modules/@opentui/core/index.node.js"
import { flush } from "../../oracle/src/app.ts"
import { CONTINUATION, Canvas, fit } from "../../oracle/src/canvas.ts"

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
const coreBin = resolve(process.env.T3_NATIVE_BINARY ?? join(REPO, "target/release/loupe-core"))
const args = process.argv.slice(2)
const opt = (name: string, d: number) => (args.includes(name) ? Number(args[args.indexOf(name) + 1]) : d)
const STRINGS = opt("--strings", 20000)
const SEED = opt("--seed", 1)
const SHOW = opt("--show", 5)
const DEFAULT_BG = 0x0100_0000

// ── Seeded generation ─────────────────────────────────────────────────────────

let rng = SEED >>> 0 || 1
function rand(): number {
  // mulberry32
  rng = (rng + 0x6d2b79f5) >>> 0
  let t = rng
  t = Math.imul(t ^ (t >>> 15), t | 1)
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296
}
const int = (n: number) => Math.floor(rand() * n)
const pick = <T,>(xs: readonly T[]): T => xs[int(xs.length)]!
const between = (a: number, b: number) => a + int(b - a + 1)
const s = (...cps: number[]) => String.fromCodePoint(...cps)

const MOD = () => between(0x1f3fb, 0x1f3ff)
const RI = () => between(0x1f1e6, 0x1f1ff)
const BASES = [0x1f44d, 0x1f44b, 0x270b, 0x1f466, 0x261d, 0x1f3c3, 0x1f9d1, 0x1f46e]
const PEOPLE = [0x1f468, 0x1f469, 0x1f467, 0x1f466, 0x1f9d1]
// Each kind makes one piece of text. The list in the brief, and the neighbours that make them hard.
const KINDS: Record<string, () => string> = {
  ascii: () => Array.from({ length: between(1, 6) }, () => String.fromCharCode(between(0x21, 0x7e))).join("") + (rand() < 0.5 ? " " : ""),
  wide: () => s(...Array.from({ length: between(1, 3) }, () => pick([between(0x4e00, 0x9fff), between(0xac00, 0xd7a3), between(0xff01, 0xff5e), between(0x3041, 0x3096), between(0x20000, 0x2a6df)]))),
  skin: () => pick([() => s(pick(BASES), MOD()), () => s(pick(BASES), MOD(), MOD()), () => s(MOD()), () => s(0x61, MOD()), () => s(pick(BASES), 0xfe0f, MOD())])(),
  zwj: () => pick([
    () => s(0x1f468, 0x200d, 0x1f469, 0x200d, 0x1f467, 0x200d, 0x1f466),
    () => s(0x1f3f3, 0xfe0f, 0x200d, 0x1f308),
    () => s(0x1f3f3, 0x200d, 0x1f308),
    () => s(0x1f9d1, 0x200d, 0x1f4bb),
    () => s(0x1f469, MOD(), 0x200d, 0x1f52c),
    () => s(0x1f3f4, 0x200d, 0x2620, 0xfe0f),
    () => s(0x1f468, 0x200d, 0x2764, 0xfe0f, 0x200d, 0x1f468),
    () => s(pick(PEOPLE), 0x200d),
    () => s(pick(PEOPLE), 0x200d, 0x61),
    () => s(0x200d, pick(PEOPLE)),
    () => s(...Array.from({ length: between(5, 10) }, (_, i) => (i % 2 ? 0x200d : pick(PEOPLE)))),
  ])(),
  flags: () => s(...Array.from({ length: between(1, 5) }, RI)),
  keycap: () => s(pick([0x30 + int(10), 0x23, 0x2a]), ...(rand() < 0.6 ? [0xfe0f] : []), 0x20e3),
  vs: () => s(pick([0x2764, 0x263a, 0x2614, 0x61, 0xa9, 0x1f600, 0x1f3f3, 0x2615, 0x231a, 0x4e00, 0x30]), pick([0xfe0e, 0xfe0f])),
  combining: () => s(pick([0x61, 0x65, 0x6f, 0x928, 0x5d0, 0x1f600, 0x4e00, 0x3b1]), ...Array.from({ length: between(1, 4) }, () => pick([between(0x300, 0x36f), between(0x483, 0x489), 0x951, between(0x1ab0, 0x1abd), between(0x20d0, 0x20f0), 0x903, 0x93e, 0x94d]))),
  tags: () => pick([
    () => s(0x1f3f4, 0xe0067, 0xe0062, 0xe0065, 0xe006e, 0xe0067, 0xe007f),
    () => s(0x1f3f4, 0xe0067, 0xe0062, 0xe0073, 0xe0063, 0xe0074, 0xe007f),
    () => s(0x1f3f4, 0xe0067, 0xe0062),
    () => s(0x61, 0xe0067, 0xe007f),
  ])(),
  jamo: () => pick([
    () => s(between(0x1100, 0x1112), between(0x1161, 0x1175), ...(rand() < 0.5 ? [between(0x11a8, 0x11c2)] : [])),
    () => s(0x115f, 0x1160),
    () => s(0x115f),
    () => s(0x1160, between(0x1161, 0x1175)),
    () => s(between(0xac00, 0xd7a3), between(0x11a8, 0x11c2)),
    () => s(between(0x1161, 0x1175)),
    () => s(between(0xa960, 0xa97c), between(0xd7b0, 0xd7c6)),
  ])(),
  indic: () => pick([
    () => s(0x915, 0x94d, 0x937),
    () => s(0x915, 0x94d, 0x200d, 0x937, 0x93f),
    () => s(0x928, 0x94d, 0x930),
    () => s(0x930, 0x94d, 0x915),
    () => s(0xe01, 0xe33),
    () => s(0x995, 0x9cd, 0x9b7),
    () => s(0x915, 0x93c, 0x94d, 0x924),
  ])(),
  odd: () => s(pick([0xfffd, 0x1, 0x7f, 0x85, 0x9, 0xad, 0x200b, 0x200c, 0x200e, 0x2060, 0xfeff, 0xe000, 0x378, 0xffff, 0x30000, 0x2615, 0x1f321, 0x2190, 0x2588, 0xb7, 0x2026, 0x1f35e, 0x2e3a, 0x3000, 0x115f, 0xd, 0xa, 0x600, 0x110bd])),
  long: () => s(0x61, ...Array.from({ length: between(40, 80) }, () => 0x301)),
  random: () => s(rand() < 0.7 ? between(0xa0, 0xffff) : between(0x10000, 0x3ffff)),
}
const KIND_NAMES = Object.keys(KINDS)
function piece(): string {
  let p = KINDS[pick(KIND_NAMES)]!()
  if (/[\ud800-\udfff]/.test(p.replace(/[\ud800-\udbff][\udc00-\udfff]/g, ""))) p = "?"
  return p
}

const PALETTE = [0xcacccc, 0x7a7d7e, 0x101315, 0x2a2d2f, 0xe5e6e6, 0x7aa2f7, 0xff0000]
type Style = { fg: number; bg: number; at: number }
const styles: Style[] = [
  { fg: PALETTE[0]!, bg: PALETTE[2]!, at: 0 },
  { fg: PALETTE[0]!, bg: PALETTE[2]!, at: 1 },
  { fg: PALETTE[4]!, bg: PALETTE[3]!, at: 0 },
  { fg: PALETTE[5]!, bg: PALETTE[2]!, at: 0 },
  { fg: PALETTE[1]!, bg: PALETTE[3]!, at: 1 },
]

type Case = { kind: "text" | "raw"; w: number; h: number; ch: number[]; fg: number[]; bg: number[]; at: number[]; note: string[] }

function textCase(): Case {
  const w = between(1, 48)
  const h = between(1, 2)
  const canvas = new Canvas(w, h)
  canvas.clear(PALETTE[2]!, PALETTE[0]!)
  const note: string[] = []
  for (let y = 0; y < h; y++) {
    let x = int(3)
    while (x < w) {
      const st = rand() < 0.5 ? styles[0]! : pick(styles)
      let text = Array.from({ length: between(1, 4) }, piece).join(rand() < 0.3 ? " " : "")
      const draw = () => {
        if (rand() < 0.2) text = fit(text, between(1, 12))
        const max = rand() < 0.3 ? between(0, 10) : undefined
        note.push(`${y}:${x}${max === undefined ? "" : `/${max}`} ${JSON.stringify(text)}`)
        return canvas.text(x, y, text, st.fg, { bg: st.bg, attr: st.at, max })
      }
      let used: number
      if (rand() < 0.15) {
        // A panel's clip rectangle that starts inside what is drawn: the left half of a wide
        // character outside, its continuation inside.
        const cx = Math.min(w, x + between(0, 2))
        used = canvas.clipped(cx, 0, w - cx, h, 0, draw)
      } else used = draw()
      x += Math.max(1, used) + (rand() < 0.4 ? 0 : int(3))
      // Draw over what is there, sometimes half of a wide character.
      if (rand() < 0.1 && x > 1) {
        const at = x - between(1, 2)
        const o = pick(styles)
        const t = piece()
        note.push(`${y}:${at} over ${JSON.stringify(t)}`)
        canvas.text(at, y, t, o.fg, { bg: o.bg, attr: o.at })
      }
    }
  }
  return { kind: "text", w, h, ch: [...canvas.ch], fg: [...canvas.fg], bg: [...canvas.bg], at: [...canvas.at], note }
}

function rawCase(): Case {
  const w = between(1, 40)
  const h = 1
  const ch: number[] = []
  const fg: number[] = []
  const bg: number[] = []
  const at: number[] = []
  let st = pick(styles)
  while (ch.length < w) {
    const cps = [...piece()].map((c) => c.codePointAt(0)!)
    for (const cp of cps) {
      if (ch.length >= w) break
      if (rand() < 0.15) st = pick(styles) // a style change, sometimes inside a cluster
      const cont = rand() < 0.12
      ch.push(cont ? CONTINUATION : cp)
      fg.push(st.fg)
      bg.push(st.bg)
      at.push(st.at)
      if (!cont && rand() < 0.2 && ch.length < w) {
        ch.push(CONTINUATION)
        fg.push(st.fg)
        bg.push(st.bg)
        at.push(st.at)
      }
    }
  }
  return { kind: "raw", w, h, ch, fg, bg, at, note: [JSON.stringify(String.fromCodePoint(...ch.map((c) => (c === 0 ? 0x2400 : c))))] }
}

// ── The original ──────────────────────────────────────────────────────────────

type Out = [number, string, number, number, number, boolean]
const owner = new ResourceContext({ objectCapacity: 64, renderCellsMax: 1 << 20 })
const cleared = RGBA.fromInts(0, 0, 0, 0)
const decoder = new TextDecoder()
const buffers = new Map<string, OptimizedBuffer>()

function original(c: Case, method: "unicode" | "unicode-wide" | "wcwidth"): Out[] {
  const key = `${c.w}x${c.h}:${method}`
  let buf = buffers.get(key)
  if (!buf) buffers.set(key, (buf = OptimizedBuffer.create(c.w, c.h, method, { owner })))
  // The renderer clears its next buffer to a transparent background before the frame is drawn.
  buf.clear(cleared)
  const canvas = new Canvas(c.w, c.h)
  canvas.ch.set(c.ch)
  canvas.fg.set(c.fg)
  canvas.bg.set(c.bg)
  canvas.at.set(c.at)
  flush(canvas, buf, 0, 0)
  return (buf as any).withResolvedChars({ addLineBreaks: false, cellLengths: true }, (bytes: Uint8Array, cells: any, lengths: Uint8Array) => {
    const out: Out[] = []
    let offset = 0
    for (let i = 0; i < c.w * c.h; i++) {
      const char = cells.char[i] >>> 0
      const len = lengths[i]!
      const text = decoder.decode(bytes.subarray(offset, offset + len))
      offset += len
      const type = char & 0xc0000000
      const ext = ((char >>> 26) & 0xf)
      const rgb = (a: any, k: number) => (a[k * 4] << 16) | (a[k * 4 + 1] << 8) | a[k * 4 + 2]
      const bgv = cells.bg[i * 4 + 3] === 0 ? DEFAULT_BG : rgb(cells.bg, i)
      const bold = (cells.attributes[i] & 1) !== 0
      if (type >>> 0 === 0x80000000) out.push([1, text, ext + 1, rgb(cells.fg, i), bgv, bold])
      else if (type >>> 0 === 0xc0000000) out.push([2, "", ext, rgb(cells.fg, i), bgv, bold])
      else out.push([0, String.fromCodePoint(char), 1, rgb(cells.fg, i), bgv, bold])
    }
    return out
  })
}

// ── Exhaustive ────────────────────────────────────────────────────────────────

const CONTEXTS: ((c: string) => string)[] = [
  (c) => c, (c) => "a" + c, (c) => c + "a", (c) => c + c, (c) => c + "\u0301",
  (c) => "\u{1f468}\u200d" + c, (c) => "\u{1f44d}" + c, (c) => c + "\u{1f3fb}", (c) => "\u0915\u094d" + c,
  (c) => c + "\u094d\u0915", (c) => "\u1100" + c, (c) => c + "\u1161", (c) => c + "\u11a8", (c) => "\u{1f1e6}" + c,
  (c) => c + "\ufe0f", (c) => "\u0600" + c, (c) => c + "\u200d\u{1f466}", (c) => "\u0915" + c, (c) => "\uac00" + c,
  (c) => c + "\u0903",
]
const PROBE_W = 160

function exhaustive() {
  const fg = RGBA.fromInts(200, 200, 200, 255)
  const bg = RGBA.fromInts(1, 2, 3, 255)
  const CHUNK = 4096
  let rows = 0
  let failures = 0
  const shown: string[] = []
  const trim = (sig: string) => sig.replace(/( P20)+$/, "")
  for (const method of ["unicode", "unicode-wide", "wcwidth"] as const) {
    const buf = OptimizedBuffer.create(PROBE_W, CHUNK, method, { owner })
    for (let base = 0; base < 0x110000; base += CHUNK) {
      const cps: number[] = []
      for (let cp = base; cp < Math.min(base + CHUNK, 0x110000); cp++) if (cp < 0xd800 || cp > 0xdfff) cps.push(cp)
      const texts = cps.map((cp) => CONTEXTS.map((f) => f(String.fromCodePoint(cp))).join("\n"))
      buf.clear(cleared)
      texts.forEach((t, y) => buf.drawText(t, 0, y, fg, bg, 0))
      const want: string[] = (buf as any).withResolvedChars({ addLineBreaks: false, cellLengths: true }, (bytes: Uint8Array, cells: any, lengths: Uint8Array) => {
        const out: string[] = []
        let offset = 0
        for (let y = 0; y < texts.length; y++) {
          const sig: string[] = []
          for (let x = 0; x < PROBE_W; x++) {
            const i = y * PROBE_W + x
            const char = cells.char[i] >>> 0
            const len = lengths[i]!
            const text = decoder.decode(bytes.subarray(offset, offset + len))
            offset += len
            const type = (char & 0xc0000000) >>> 0
            const ext = (char >>> 26) & 0xf
            sig.push(type === 0x80000000 ? `S${ext + 1}.${text.length}` : type === 0xc0000000 ? `C${ext}` : `P${char.toString(16)}`)
          }
          out.push(trim(sig.join(" ")))
        }
        // The rest of the buffer's rows hold nothing of ours.
        return out
      })
      const input = texts.map((t) => `${method === "wcwidth" ? "w" : method === "unicode-wide" ? "x" : "u"} ${PROBE_W} ${[...t].map((c) => c.codePointAt(0)!.toString(16)).join(",")}`).join("\n") + "\n"
      const run = spawnSync(coreBin, ["--layout-strings"], { input, maxBuffer: 1 << 30, encoding: "utf8" })
      if (run.status !== 0) throw new Error(`${coreBin} --layout-strings exited ${run.status}: ${run.stderr}`)
      const got = run.stdout.split("\n").filter(Boolean).map(trim)
      if (got.length !== texts.length) throw new Error(`expected ${texts.length} rows, got ${got.length}`)
      if (base % (CHUNK * 32) === 0) console.error(`progress: ${method} U+${base.toString(16).toUpperCase()}, ${failures} differing so far`)
      for (let y = 0; y < texts.length; y++) {
        rows++
        if (want[y] !== got[y]) {
          failures++
          if (shown.length < SHOW) shown.push(`U+${cps[y]!.toString(16).toUpperCase().padStart(4, "0")} (${method}):\n    original ${want[y]}\n    rust     ${got[y]}`)
        }
      }
    }
    buf.destroy()
  }
  console.log(JSON.stringify({ exhaustive: true, code_points: rows / 3, contexts: CONTEXTS.length, rows_compared: rows, differing_code_points: failures }))
  for (const b of shown) console.error("DIFFERENCE: " + b)
  if (failures) process.exit(1)
  process.exit(0)
}
if (args.includes("--exhaustive")) exhaustive()

// ── Run ───────────────────────────────────────────────────────────────────────

const cases: Case[] = []
for (let i = 0; i < STRINGS; i++) cases.push(rand() < 0.65 ? textCase() : rawCase())
const METHODS = ["unicode", "unicode-wide", "wcwidth"] as const
const input = cases.flatMap((c) => METHODS.map((m) => JSON.stringify({ m, w: c.w, h: c.h, ch: c.ch, fg: c.fg, bg: c.bg, at: c.at }))).join("\n") + "\n"
const run = spawnSync(coreBin, ["--layout-rows"], { input, maxBuffer: 1 << 30, encoding: "utf8" })
if (run.status !== 0) {
  console.error(run.stderr)
  throw new Error(`${coreBin} --layout-rows exited ${run.status}`)
}
const rustLines = run.stdout.split("\n").filter(Boolean)
if (rustLines.length !== cases.length * METHODS.length) throw new Error(`expected ${cases.length * METHODS.length} rows from Rust, got ${rustLines.length}`)

let compared = 0
let cells = 0
let multi = 0
let clearedCells = 0
const bad: string[] = []
let failures = 0
cases.forEach((c, n) => {
  METHODS.forEach((m, k) => {
    compared++
    const want = original(c, m)
    const got = JSON.parse(rustLines[n * METHODS.length + k]!) as Out[]
    cells += want.length
    for (const cell of want) {
      if (cell[0] === 1 && [...cell[1]].length > 1) multi++
      if (cell[4] === DEFAULT_BG) clearedCells++
    }
    const diffs: string[] = []
    for (let i = 0; i < want.length; i++) {
      if (JSON.stringify(want[i]) !== JSON.stringify(got[i])) diffs.push(`(${i % c.w},${Math.floor(i / c.w)}) original ${JSON.stringify(want[i])} rust ${JSON.stringify(got[i])}`)
    }
    if (diffs.length) {
      failures++
      if (bad.length < SHOW) bad.push(`case ${n} (${c.kind}, ${c.w}x${c.h}, ${m}): ${diffs.length} cells differ\n    ${diffs.slice(0, 4).join("\n    ")}\n    input: ${c.note.join(" | ")}`)
    }
  })
})
for (const b of buf()) b.destroy()
function buf() {
  return [...buffers.values()]
}
console.log(JSON.stringify({ seed: SEED, strings: STRINGS, canvases_compared: compared, cells_compared: cells, multi_code_point_clusters: multi, cleared_cells: clearedCells, differing_canvases: failures }))
for (const b of bad) console.error("DIFFERENCE: " + b)
if (failures) process.exit(1)
