// A JavaScript cell grid the view draws into every frame. It is flushed to an
// OpenTUI buffer as coalesced runs, and can be dumped as text for tests.
import { fonts } from "@opentui/core"
import { cellWidth, clean, textWidth } from "./format.ts"
import { mix, type Color } from "./theme.ts"

export { cellWidth, clean, fit, textWidth } from "./format.ts"

export const CONTINUATION = 0
export const BOLD = 1 // TextAttributes.BOLD

export interface TextOpts {
  bg?: Color
  attr?: number
  max?: number
}

export class Canvas {
  w = 0
  h = 0
  ch = new Uint32Array(0)
  fg = new Uint32Array(0)
  bg = new Uint32Array(0)
  at = new Uint8Array(0)
  // Clip rectangle (inclusive-exclusive) and a vertical offset used for scrolling panels.
  cx0 = 0
  cy0 = 0
  cx1 = 0
  cy1 = 0
  oy = 0

  constructor(w = 1, h = 1) {
    this.resize(w, h)
  }

  resize(w: number, h: number) {
    if (w === this.w && h === this.h) return
    this.w = Math.max(1, w)
    this.h = Math.max(1, h)
    const n = this.w * this.h
    this.ch = new Uint32Array(n)
    this.fg = new Uint32Array(n)
    this.bg = new Uint32Array(n)
    this.at = new Uint8Array(n)
    this.unclip()
  }

  clear(bg: Color, fg: Color) {
    this.ch.fill(32)
    this.fg.fill(fg)
    this.bg.fill(bg)
    this.at.fill(0)
    this.unclip()
  }

  unclip() {
    this.cx0 = 0
    this.cy0 = 0
    this.cx1 = this.w
    this.cy1 = this.h
    this.oy = 0
  }

  /** Draw inside a rectangle only, optionally shifted up by `scroll` rows. */
  clipped<T>(x: number, y: number, w: number, h: number, scroll: number, fn: () => T): T {
    const saved = [this.cx0, this.cy0, this.cx1, this.cy1, this.oy]
    this.cx0 = Math.max(0, x)
    this.cy0 = Math.max(0, y)
    this.cx1 = Math.min(this.w, x + w)
    this.cy1 = Math.min(this.h, y + h)
    this.oy = scroll
    try {
      return fn()
    } finally {
      ;[this.cx0, this.cy0, this.cx1, this.cy1, this.oy] = saved
    }
  }

  private index(x: number, y: number): number {
    const yy = y - this.oy
    if (x < this.cx0 || x >= this.cx1 || yy < this.cy0 || yy >= this.cy1) return -1
    return yy * this.w + x
  }

  set(x: number, y: number, ch: string | number, fg: Color, bg?: Color, attr = 0) {
    const i = this.index(x, y)
    if (i < 0) return
    const cp = typeof ch === "number" ? ch : (ch.codePointAt(0) ?? 32)
    // Keep wide characters whole when something is drawn over half of one.
    if (cp !== CONTINUATION) {
      if (this.ch[i] === CONTINUATION && x > 0) this.ch[i - 1] = 32
      if (x + 1 < this.w && this.ch[i + 1] === CONTINUATION) this.ch[i + 1] = 32
    }
    this.ch[i] = cp
    this.fg[i] = fg
    if (bg !== undefined) this.bg[i] = bg
    this.at[i] = attr
  }

  bgAt(x: number, y: number): Color {
    const i = this.index(x, y)
    return i < 0 ? 0 : this.bg[i]
  }

  /** Draw a string; returns the number of columns used. */
  text(x: number, y: number, s: string, fg: Color, opts: TextOpts = {}): number {
    const max = opts.max ?? Infinity
    let col = 0
    for (const ch of clean(s)) {
      const cp = ch.codePointAt(0)!
      const cw = cellWidth(cp)
      if (cw === 0) continue
      if (col + cw > max) break
      this.set(x + col, y, cp, fg, opts.bg, opts.attr ?? 0)
      if (cw === 2) this.set(x + col + 1, y, CONTINUATION, fg, opts.bg, opts.attr ?? 0)
      col += cw
    }
    return col
  }

  /** Right-align text so it ends at column `right` (exclusive). */
  textRight(right: number, y: number, s: string, fg: Color, opts: TextOpts = {}): number {
    const w = Math.min(textWidth(clean(s)), opts.max ?? Infinity)
    return this.text(right - w, y, s, fg, opts)
  }

  fill(x: number, y: number, w: number, h: number, bg: Color, ch = " ", fg?: Color) {
    for (let yy = y; yy < y + h; yy++) for (let xx = x; xx < x + w; xx++) this.set(xx, yy, ch, fg ?? bg, bg)
  }

  /** Recolor a region in place (used for modal dimming and boot wipe). */
  tint(x: number, y: number, w: number, h: number, fn: (fg: Color, bg: Color) => [Color, Color]) {
    for (let yy = y; yy < y + h; yy++) {
      for (let xx = x; xx < x + w; xx++) {
        const i = this.index(xx, yy)
        if (i < 0) continue
        const [f, b] = fn(this.fg[i], this.bg[i])
        this.fg[i] = f
        this.bg[i] = b
      }
    }
  }

  toText(): string {
    const lines: string[] = []
    for (let y = 0; y < this.h; y++) {
      let line = ""
      for (let x = 0; x < this.w; x++) {
        const cp = this.ch[y * this.w + x]
        if (cp !== CONTINUATION) line += String.fromCodePoint(cp)
      }
      lines.push(line.trimEnd())
    }
    return lines.join("\n")
  }
}

// ── Glyph primitives ────────────────────────────────────────────────────────

const EIGHTHS = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"]
const LEVELS = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"]

export interface BarOpts {
  track: Color
  color: (cellIndex: number, cells: number) => Color
  marker?: { at: number; color: Color } // a tick on the track, e.g. elapsed time
  line?: boolean // draw the empty track as a thin rule instead of a filled background
}

/** Horizontal meter with 1/8-cell precision; the track is drawn as background. */
export function hbar(c: Canvas, x: number, y: number, w: number, frac: number, o: BarOpts) {
  if (w <= 0) return
  const units = Math.round(Math.max(0, Math.min(1, frac)) * w * 8)
  const full = Math.floor(units / 8)
  const rem = units % 8
  const bg = o.line ? undefined : o.track
  for (let i = 0; i < w; i++) {
    if (i < full) c.set(x + i, y, "█", o.color(i, w), bg)
    else if (i === full && rem) c.set(x + i, y, EIGHTHS[rem], o.color(i, w), bg)
    else c.set(x + i, y, o.line ? "─" : " ", o.track, bg)
  }
  if (o.marker) {
    const mx = Math.min(w - 1, Math.max(0, Math.round(o.marker.at * w)))
    const filled = mx < full
    c.set(x + mx, y, filled ? "▕" : "│", o.marker.color, filled ? o.color(mx, w) : bg)
  }
}

/** Sparkline: one column per value, 8 levels, empty for zero. */
export function spark(values: number[], max = Math.max(1, ...values)): string[] {
  return values.map((v) => (v <= 0 ? " " : LEVELS[Math.min(7, Math.max(0, Math.ceil((v / max) * 8) - 1))]))
}

export const level = (v: number, max: number): string =>
  v <= 0 ? " " : LEVELS[Math.min(7, Math.max(0, Math.ceil((v / Math.max(1, max)) * 8) - 1))]

/** 2x4 braille dot raster for high-resolution line and area charts. */
export class Braille {
  readonly w: number
  readonly h: number
  private bits: Uint8Array
  constructor(cols: number, rows: number) {
    this.w = cols * 2
    this.h = rows * 4
    this.bits = new Uint8Array(cols * rows)
  }
  private static MAP = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
  ]
  dot(px: number, py: number) {
    if (px < 0 || py < 0 || px >= this.w || py >= this.h) return
    const cols = this.w / 2
    this.bits[Math.floor(py / 4) * cols + Math.floor(px / 2)] |= Braille.MAP[py % 4][px % 2]
  }
  /** Fill a column from the bottom up to a 0..1 height. */
  column(px: number, frac: number) {
    const top = this.h - Math.round(Math.max(0, Math.min(1, frac)) * this.h)
    for (let py = this.h - 1; py >= top; py--) this.dot(px, py)
  }
  draw(c: Canvas, x: number, y: number, color: (col: number, row: number) => Color, bg?: Color) {
    const cols = this.w / 2
    for (let row = 0; row < this.h / 4; row++) {
      for (let col = 0; col < cols; col++) {
        const b = this.bits[row * cols + col]
        if (b) c.set(x + col, y + row, 0x2800 + b, color(col, row), bg)
      }
    }
  }
}

// ── ASCII-art fonts (OpenTUI's bundled cfonts data) ─────────────────────────

export type FontName = "block"
type Segment = { text: string; tone: number }
const parsed = new Map<string, Segment[][]>()

function glyph(font: FontName, ch: string): Segment[][] | null {
  const key = font + ch
  if (parsed.has(key)) return parsed.get(key)!
  const def = (fonts as unknown as Record<string, { chars: Record<string, string[]> }>)[font]
  const lines = def?.chars[ch]
  if (!lines) return null
  const rows = lines.map((line) => {
    const out: Segment[] = []
    const re = /<c(\d+)>(.*?)<\/c\d+>/g
    let last = 0
    let m: RegExpExecArray | null
    while ((m = re.exec(line))) {
      if (m.index > last) out.push({ text: line.slice(last, m.index), tone: 0 })
      out.push({ text: m[2], tone: Math.max(0, Number(m[1]) - 1) })
      last = m.index + m[0].length
    }
    if (last < line.length) out.push({ text: line.slice(last), tone: 0 })
    return out
  })
  parsed.set(key, rows)
  return rows
}

function glyphWidth(rows: Segment[][]): number {
  return Math.max(...rows.map((r) => r.reduce((n, s) => n + [...s.text].length, 0)))
}

export function fontWidth(text: string, font: FontName): number {
  let w = 0
  let n = 0
  for (const ch of text.toUpperCase()) {
    const g = glyph(font, ch)
    if (!g) continue
    w += glyphWidth(g)
    n++
  }
  return w + Math.max(0, n - 1)
}

/** Draw big text; tone 0 uses `color`, tone 1 (font shading) uses `shade`. */
export function drawFont(c: Canvas, x: number, y: number, text: string, font: FontName, color: Color, shade?: Color): number {
  let cx = x
  for (const ch of text.toUpperCase()) {
    const g = glyph(font, ch)
    if (!g) continue
    const gw = glyphWidth(g)
    g.forEach((row, ry) => {
      let col = 0
      for (const seg of row) {
        for (const chr of seg.text) {
          if (chr !== " ") c.set(cx + col, y + ry, chr, seg.tone ? (shade ?? mix(color, 0, 0.5)) : color)
          col++
        }
      }
    })
    cx += gw + 1
  }
  return cx - x - 1
}

// ── Big numbers: a 4×5 pixel font drawn with half blocks over three rows ─────
// Five pixel rows are the fewest that keep every digit distinct: at four (the
// bundled "tiny" font) 0, 6, 8 and 9 collapse into blobs. Rounded corners make
// 0 an oval and 8 two loops, so neither reads as a filled block. Pixel rows are
// listed top to bottom, "#" lit; each glyph sits on the last row, leaving the
// top half of the first row empty as a gap below a label.
const BIG_GLYPHS: Record<string, string> = {
  "0": ".##./#..#/#..#/#..#/.##.",
  "1": ".#./##./.#./.#./###",
  "2": "###./...#/.##./#.../####",
  "3": "###./...#/.##./...#/###.",
  "4": "#..#/#..#/####/...#/...#",
  "5": "####/#.../###./...#/###.",
  "6": ".##./#.../###./#..#/.##.",
  "7": "####/...#/..#./.#../.#..",
  "8": ".##./#..#/.##./#..#/.##.",
  "9": ".##./#..#/.###/...#/.##.",
  ".": "././././#",
  "-": ".../.../###/.../...",
}
export const BIG_ROWS = 3
const bigCells = new Map<string, string[]>()

/** A glyph as three rows of half-block characters ("▀", "▄", "█" or space). */
export function bigGlyph(ch: string): string[] | null {
  const cached = bigCells.get(ch)
  if (cached) return cached
  const def = BIG_GLYPHS[ch]
  if (!def) return null
  const rows = def.split("/")
  const lit = (row: number, col: number) => rows[row - 1]?.[col] === "#" // pixel row -1 is the top gap
  const out = [0, 1, 2].map((r) => Array.from(rows[0], (_, col) => {
    const top = lit(r * 2, col)
    const bottom = lit(r * 2 + 1, col)
    return top && bottom ? "█" : top ? "▀" : bottom ? "▄" : " "
  }).join(""))
  bigCells.set(ch, out)
  return out
}

/** Width of a big number; characters without a glyph (units: % K M B) are one text cell. */
export function bigWidth(text: string): number {
  let w = 0
  for (const ch of text) w += (bigGlyph(ch)?.[0].length ?? cellWidth(ch.codePointAt(0)!)) + 1
  return Math.max(0, w - 1)
}

/** Draw a big number three rows tall; units are drawn as text on the baseline row. */
export function drawBig(c: Canvas, x: number, y: number, text: string, color: Color): number {
  let cx = x
  for (const ch of text) {
    const g = bigGlyph(ch)
    if (!g) {
      cx += c.text(cx, y + BIG_ROWS - 1, ch, color, { attr: BOLD }) + 1
      continue
    }
    g.forEach((row, ry) => [...row].forEach((chr, col) => chr !== " " && c.set(cx + col, y + ry, chr, color)))
    cx += g[0].length + 1
  }
  return Math.max(0, cx - x - 1)
}
