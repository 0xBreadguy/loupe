// Live palette derived from the active Omarchy theme (colors.toml).
// Roles, not hues: the dashboard encodes state with shape and brightness first,
// and reserves the single most chromatic theme color for "needs attention".
import { readFileSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

export type Color = number // 0xRRGGBB
export type Family = "claude" | "openai" | "grok" | "gemini" | "other"

export interface Palette {
  dark: boolean
  bg: Color
  panel: Color // raised surface for tiles
  fg: Color // primary ink
  text2: Color // secondary ink; also readable on the selected row, so dim text steps up to it there
  dim: Color // tertiary ink, captions: the dimmest text, still readable on bg and panel
  faint: Color // non-text marks only: grid ticks, tree guides, rules. Never text.
  track: Color // meter tracks
  floor: number // lowest ramp() position that reads at 3:1 against a context bar's track
  rule: Color // dividers
  accent: Color
  bright: Color // emphasis above fg
  hot: Color // critical / needs you
  warn: Color // approaching a limit
  sel: Color // selected row
  busy: Color // lane tint while a run is in progress
  provider: Record<Family, Color> // categorical identity of the model provider
}

/**
 * Model provider family from a model name, falling back to a provider id hint. A model name says
 * Gemini only by containing "gemini"; a hint also by naming Google or T3's Antigravity provider,
 * which serves Gemini models.
 */
export function family(model?: string | null, hint?: string | null): Family {
  const test = (raw: string | null | undefined, provider = false): Family | null => {
    const s = (raw ?? "").toLowerCase()
    if (/claude|opus|sonnet|haiku|fable|anthropic/.test(s)) return "claude"
    if (/gpt|codex|openai|\bo[1-9]\b/.test(s)) return "openai"
    if (/grok|xai/.test(s)) return "grok"
    if (/gemini/.test(s) || (provider && /google|antigravity/.test(s))) return "gemini"
    return null
  }
  return test(model) ?? test(hint, true) ?? "other"
}

// Fallback provider hues, validated with the dataviz palette checks as one
// categorical set together with the default ember (#de6145): all pairs pass CVD
// separation (worst ΔE 10.2) and the normal-vision floor (17.5) on a dark surface.
// Claude takes plum, not its brand clay, because any warm hue collapses into
// the "needs you" color under deuteranopia.
const PROVIDER_FALLBACK: Record<Exclude<Family, "gemini" | "other">, Color> = { claude: 0x8b5896, openai: 0x1f9b82, grok: 0x4c94ec }

/**
 * The blues Gemini takes when a theme's own don't work, in order: Google's blue, then deeper,
 * lighter and deeper still, all near OKLCH hue 260. A theme whose Grok already holds one of them
 * still has another.
 */
export const GEMINI_STOCK: Color[] = [0x4285f4, 0x2867e4, 0x83c4ff, 0x1e4eac, 0x113585]

export const THEME_PATH = join(homedir(), ".local/state/omarchy/current/theme/colors.toml")

export const rgb = (c: Color): [number, number, number] => [(c >> 16) & 255, (c >> 8) & 255, c & 255]

export function hex(value: string | undefined): Color | null {
  const m = /^#?([0-9a-f]{6})$/i.exec((value ?? "").trim())
  return m ? parseInt(m[1], 16) : null
}

export function mix(a: Color, b: Color, t: number): Color {
  const k = Math.max(0, Math.min(1, t))
  const [ar, ag, ab] = rgb(a)
  const [br, bg, bb] = rgb(b)
  return (Math.round(ar + (br - ar) * k) << 16) | (Math.round(ag + (bg - ag) * k) << 8) | Math.round(ab + (bb - ab) * k)
}

export function luminance(c: Color): number {
  const lin = (v: number) => {
    const s = v / 255
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }
  const [r, g, b] = rgb(c)
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

export function contrast(a: Color, b: Color): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi + 0.05) / (lo + 0.05)
}

export const chroma = (c: Color): number => {
  const [r, g, b] = rgb(c)
  return Math.max(r, g, b) - Math.min(r, g, b)
}

export function parseColorsToml(text: string): Record<string, string> {
  const vars: Record<string, string> = {}
  for (const line of text.split("\n")) {
    const m = /^\s*([A-Za-z0-9_]+)\s*=\s*"([^"]*)"/.exec(line)
    if (m) vars[m[1]] = m[2]
  }
  return vars
}

// Telling colors apart: OKLab, with protanopia and deuteranopia simulated at full severity after
// Machado, Oliveira and Fernandes (2009). The same arithmetic as crates/core/src/integration.rs,
// operation for operation, so both derive the same Gemini color.
const linear = (c: Color): number[] => rgb(c).map((v) => {
  const s = v / 255
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
})

function oklab([r, g, b]: number[]): number[] {
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
  return [
    0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
  ]
}

type Vision = number[][] | null
const PROTAN: Vision = [[0.152286, 1.052583, -0.204868], [0.114503, 0.786281, 0.099216], [-0.003882, -0.048116, 1.051998]]
const DEUTAN: Vision = [[0.367322, 0.860646, -0.227968], [0.280085, 0.672501, 0.047413], [-0.011820, 0.042940, 0.968881]]

function simulate(c: Color, m: Vision): number[] {
  const [r, g, b] = linear(c)
  return m ? m.map((row) => Math.min(1, Math.max(0, row[0] * r + row[1] * g + row[2] * b))) : [r, g, b]
}

/** The distance between two colors as seen with `vision` (normal when null): OKLab ΔE, times 100. */
export function deltaE(a: Color, b: Color, vision: Vision = null): number {
  const [p, q] = [oklab(simulate(a, vision)), oklab(simulate(b, vision))]
  const [dl, da, db] = [p[0] - q[0], p[1] - q[1], p[2] - q[2]]
  return 100 * Math.sqrt(dl * dl + da * da + db * db)
}

/** The smaller of the protanopia and deuteranopia distances. */
export const colorblindDelta = (a: Color, b: Color): number => Math.min(deltaE(a, b, PROTAN), deltaE(a, b, DEUTAN))

/** OKLCH hue 230° (past cyan) to 275° (short of violet), and chroma of at least 0.08. */
export function readsBlue(c: Color): boolean {
  const [, a, b] = oklab(linear(c))
  const deg = Math.atan2(b, a) * (180 / Math.PI)
  const h = deg < 0 ? deg + 360 : deg
  return h >= 230 && h <= 275 && Math.sqrt(a * a + b * b) >= 0.08
}

/**
 * Gemini's color. The candidates are the theme's `blue`, its `bright_blue`, then GEMINI_STOCK, each
 * darkened in a light theme as the derived providers are. The first that reads as blue, reaches 3:1
 * on the background and stands at least 12 apart (OKLab ΔE × 100) for normal vision, and 6 for
 * protanopia and deuteranopia, from every other color that means something (`signals`: Claude,
 * OpenAI, Grok, needs you and the warning, as drawn) is Gemini's. When none qualifies, the stock
 * blue that reaches 3:1 and stands farthest from its nearest signal; when none reaches 3:1, the one
 * with the most contrast.
 */
function deriveGemini(theme: (Color | null)[], shade: (c: Color) => Color, bg: Color, signals: Color[]): Color {
  const fits = (c: Color) =>
    readsBlue(c) && contrast(c, bg) >= 3 && signals.every((s) => deltaE(c, s) >= 12 && colorblindDelta(c, s) >= 6)
  const stock = GEMINI_STOCK.map(shade)
  const found = [...theme.filter((c): c is Color => c !== null).map(shade), ...stock].find(fits)
  if (found !== undefined) return found
  const apart = (c: Color) => Math.min(...signals.map((s) => Math.min(deltaE(c, s) / 12, colorblindDelta(c, s) / 6)))
  const best = (score: (c: Color) => number, among: Color[]) =>
    among.reduce<Color | null>((b, c) => (b === null || score(c) > score(b) ? c : b), null)
  return best(apart, stock.filter((c) => contrast(c, bg) >= 3)) ?? best((c) => contrast(c, bg), stock)!
}

const mostChromatic = (colors: (Color | null)[]): Color | null =>
  colors.filter((c): c is Color => c !== null).sort((a, b) => chroma(b) - chroma(a))[0] ?? null

/** Body text: WCAG AA for normal-size text, which is all a terminal has. */
export const TEXT_CONTRAST = 4.5
/** Non-text marks that carry meaning, such as a meter's fill against its track (WCAG 1.4.11). */
export const MARK_CONTRAST = 3

/**
 * `from` moved toward `to` as little as possible, but far enough to reach
 * `target` contrast against every surface. When even `to` falls short, the
 * mix that comes closest.
 */
export function reach(from: Color, to: Color, target: number, surfaces: Color[]): Color {
  const worst = (c: Color) => Math.min(...surfaces.map((s) => contrast(c, s)))
  if (worst(from) >= target) return from
  let best = from
  for (let i = 1; i <= 100; i++) {
    const c = mix(from, to, i / 100)
    if (worst(c) >= target) return c
    if (worst(c) > worst(best)) best = c
  }
  return best
}

/**
 * The dimmest ink between `fg` (t = 0) and `bg` (t = 1) that still reaches
 * `target` against every surface: text roles are derived from a contrast
 * target, not a fixed fraction, so they stay readable in any theme.
 */
function dimmest(fg: Color, bg: Color, target: number, surfaces: Color[]): Color {
  let lo = 0
  let hi = 1
  for (let i = 0; i < 16; i++) {
    const t = (lo + hi) / 2
    if (Math.min(...surfaces.map((s) => contrast(mix(fg, bg, t), s))) >= target) lo = t
    else hi = t
  }
  return mix(fg, bg, lo)
}

export function derivePalette(vars: Record<string, string>): Palette {
  const bg = hex(vars.background) ?? 0x101315
  const dark = vars.mode ? vars.mode !== "light" : luminance(bg) < 0.4
  const ink = dark ? 0xffffff : 0x000000 // the far end from the background
  const panel = mix(bg, hex(vars.foreground) ?? 0xcacccc, 0.04)
  const page = [bg, panel] // every surface text sits on, apart from the selection
  // Colors that carry text reach body-text contrast on both surfaces; a theme
  // that already does (all the bundled ones) keeps its exact colors.
  const fg = reach(hex(vars.foreground) ?? 0xcacccc, ink, TEXT_CONTRAST, page)
  let accent = hex(vars.accent) ?? hex(vars.blue) ?? fg
  if (contrast(accent, bg) < 2.4) accent = mix(accent, fg, 0.5)
  accent = reach(accent, fg, TEXT_CONTRAST, page) // key hints are drawn in it
  // The hot color must read as different from every gray: prefer the theme's red,
  // then any strongly chromatic theme color, then a fixed ember.
  let hot = mostChromatic([hex(vars.bright_red), hex(vars.red)])
  if (hot === null || chroma(hot) < 70) {
    const any = mostChromatic(Object.values(vars).map(hex))
    hot = any !== null && chroma(any) >= 70 ? any : 0xde6145
  }
  hot = reach(hot, ink, TEXT_CONTRAST, page)
  let warn = mostChromatic([hex(vars.yellow), hex(vars.bright_yellow)])
  if (warn === null || chroma(warn) < 60) warn = mix(hot, fg, 0.5)
  warn = reach(warn, ink, TEXT_CONTRAST, page) // "◌ stalled 9m" is text
  let sel = hex(vars.selection) ?? mix(bg, fg, 0.14)
  if (contrast(sel, bg) < 1.25) sel = mix(bg, accent, 0.3)
  // Selected and T3-linked rows step their text up one role, so text2 must
  // also read on the selection; the linked band lies between bg and sel.
  const text2 = dimmest(fg, bg, TEXT_CONTRAST, [...page, sel])
  const dim = dimmest(fg, bg, TEXT_CONTRAST, page)
  const track = mix(fg, bg, 0.86)
  // A chromatic theme supplies its own distinct hues; a monochrome one gets the validated set.
  const themed = { claude: hex(vars.magenta), openai: hex(vars.green), grok: hex(vars.blue) }
  const usable = Object.values(themed).every((c) => c !== null && chroma(c) >= 60 && contrast(c, bg) >= 2.6)
  const base = usable ? (themed as Record<Exclude<Family, "gemini" | "other">, Color>) : PROVIDER_FALLBACK
  const shade = (c: Color) => (dark ? c : mix(c, 0x000000, 0.12))
  const [claude, openai, grok] = [shade(base.claude), shade(base.openai), shade(base.grok)]
  // Gemini reads as blue in every theme, apart from Grok, which usually holds the theme's own blue.
  const gemini = deriveGemini([hex(vars.blue), hex(vars.bright_blue)], shade, bg, [claude, openai, grok, hot, warn])
  const provider = { claude, openai, grok, gemini, other: mix(fg, bg, 0.52) }
  const p: Palette = {
    provider,
    dark, bg, fg, accent, hot, warn, sel, panel, text2, dim, track,
    faint: mix(fg, bg, 0.74),
    rule: mix(fg, bg, 0.8),
    bright: mix(fg, ink, 0.5),
    busy: mix(bg, accent, 0.16),
    floor: 0,
  }
  // A context bar's first cells must stand out from its track, on the selected row too.
  const tracks = [track, mix(track, sel, 0.4)]
  const worst = (k: number) => Math.min(...tracks.map((t) => contrast(ramp(p, k), t)))
  let floor = 0
  while (floor < 1 && worst(floor) < MARK_CONTRAST) floor = Math.round((floor + 0.01) * 100) / 100
  p.floor = floor < 1 ? floor : [...Array(101).keys()].map((i) => i / 100).reduce((a, b) => (worst(b) > worst(a) ? b : a))
  return p
}

/** Sequential ramp: magnitude reads as brightness (one hue, half-accent -> accent -> bright). */
export function ramp(p: Palette, t: number): Color {
  const k = Math.max(0, Math.min(1, t))
  return k < 0.55 ? mix(mix(p.accent, p.bg, 0.5), p.accent, k / 0.55) : mix(p.accent, p.bright, (k - 0.55) / 0.45)
}

export class ThemeWatcher {
  private path: string
  private mtime = -1
  private checked = 0
  palette: Palette

  constructor(path = THEME_PATH) {
    this.path = path
    this.palette = derivePalette({})
    this.poll(true)
  }

  /** Re-read colors.toml when it changes (theme switches replace the file). */
  poll(force = false): boolean {
    const now = Date.now()
    if (!force && now - this.checked < 1000) return false
    this.checked = now
    try {
      const mtime = statSync(this.path).mtimeMs
      if (mtime === this.mtime) return false
      this.mtime = mtime
      this.palette = derivePalette(parseColorsToml(readFileSync(this.path, "utf8")))
      return true
    } catch {
      if (this.mtime !== -2) {
        this.mtime = -2
        this.palette = derivePalette({})
        return true
      }
      return false
    }
  }
}
