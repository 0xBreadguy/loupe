// Live palette derived from the active Omarchy theme (colors.toml).
// Roles, not hues: the dashboard encodes state with shape and brightness first,
// and reserves the single most chromatic theme color for "needs attention".
import { readFileSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

export type Color = number // 0xRRGGBB
export type Family = "claude" | "openai" | "grok" | "other"

export interface Palette {
  dark: boolean
  bg: Color
  panel: Color // raised surface for tiles
  fg: Color // primary ink
  text2: Color // secondary ink
  dim: Color // tertiary ink, captions
  faint: Color // grid ticks, empty states
  track: Color // meter tracks
  rule: Color // dividers
  accent: Color
  bright: Color // emphasis above fg
  hot: Color // critical / needs you
  warn: Color // approaching a limit
  sel: Color // selected row
  busy: Color // lane tint while a run is in progress
  provider: Record<Family, Color> // categorical identity of the model provider
}

/** Model provider family from a model name, falling back to a provider id hint. */
export function family(model?: string | null, hint?: string | null): Family {
  const test = (raw: string | null | undefined): Family | null => {
    const s = (raw ?? "").toLowerCase()
    if (/claude|opus|sonnet|haiku|fable|anthropic/.test(s)) return "claude"
    if (/gpt|codex|openai|\bo[1-9]\b/.test(s)) return "openai"
    if (/grok|xai/.test(s)) return "grok"
    return null
  }
  return test(model) ?? test(hint) ?? "other"
}

// Fallback provider hues, validated with the dataviz palette checks as one
// categorical set together with the default ember (#de6145): all pairs pass CVD
// separation (worst ΔE 10.2) and the normal-vision floor (17.5) on a dark surface.
// Claude takes plum, not its brand clay, because any warm hue collapses into
// the "needs you" color under deuteranopia.
const PROVIDER_FALLBACK: Record<Exclude<Family, "other">, Color> = { claude: 0x8b5896, openai: 0x1f9b82, grok: 0x4c94ec }

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

const mostChromatic = (colors: (Color | null)[]): Color | null =>
  colors.filter((c): c is Color => c !== null).sort((a, b) => chroma(b) - chroma(a))[0] ?? null

export function derivePalette(vars: Record<string, string>): Palette {
  const bg = hex(vars.background) ?? 0x101315
  const fg = hex(vars.foreground) ?? 0xcacccc
  const dark = vars.mode ? vars.mode !== "light" : luminance(bg) < 0.4
  const ink = dark ? 0xffffff : 0x000000
  let accent = hex(vars.accent) ?? hex(vars.blue) ?? fg
  if (contrast(accent, bg) < 2.4) accent = mix(accent, fg, 0.5)
  // The hot color must read as different from every gray: prefer the theme's red,
  // then any strongly chromatic theme color, then a fixed ember.
  let hot = mostChromatic([hex(vars.bright_red), hex(vars.red)])
  if (hot === null || chroma(hot) < 70) {
    const any = mostChromatic(Object.values(vars).map(hex))
    hot = any !== null && chroma(any) >= 70 ? any : 0xde6145
  }
  if (contrast(hot, bg) < 2.6) hot = mix(hot, ink, 0.3)
  let warn = mostChromatic([hex(vars.yellow), hex(vars.bright_yellow)])
  if (warn === null || chroma(warn) < 60) warn = mix(hot, fg, 0.5)
  let sel = hex(vars.selection) ?? mix(bg, fg, 0.14)
  if (contrast(sel, bg) < 1.25) sel = mix(bg, accent, 0.3)
  // A chromatic theme supplies its own distinct hues; a monochrome one gets the validated set.
  const themed = { claude: hex(vars.magenta), openai: hex(vars.green), grok: hex(vars.blue) }
  const usable = Object.values(themed).every((c) => c !== null && chroma(c) >= 60 && contrast(c, bg) >= 2.6)
  const base = usable ? (themed as Record<Exclude<Family, "other">, Color>) : PROVIDER_FALLBACK
  const shade = (c: Color) => (dark ? c : mix(c, 0x000000, 0.12))
  const provider = { claude: shade(base.claude), openai: shade(base.openai), grok: shade(base.grok), other: mix(fg, bg, 0.52) }
  return {
    provider,
    dark, bg, fg, accent, hot, warn, sel,
    panel: mix(bg, fg, 0.04),
    text2: mix(fg, bg, 0.3),
    dim: mix(fg, bg, 0.52),
    faint: mix(fg, bg, 0.74),
    track: mix(fg, bg, 0.86),
    rule: mix(fg, bg, 0.8),
    bright: mix(fg, ink, 0.5),
    busy: mix(bg, accent, 0.16),
  }
}

/** Sequential ramp: magnitude reads as brightness (one hue, faint -> accent -> bright). */
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
