// Pure text helpers with no dependencies, shared by the dashboard, --once and --status.

/** Strip terminal control and bidi/format characters before they reach the screen. */
export function clean(value: unknown): string {
  return String(value ?? "").replace(/\p{C}/gu, " ")
}

const WIDE: [number, number][] = [
  [0x1100, 0x115f], [0x2e80, 0x303e], [0x3041, 0x33ff], [0x3400, 0x4dbf], [0x4e00, 0x9fff],
  [0xa000, 0xa4cf], [0xac00, 0xd7a3], [0xf900, 0xfaff], [0xfe30, 0xfe4f], [0xff00, 0xff60],
  [0xffe0, 0xffe6], [0x1f300, 0x1f64f], [0x1f900, 0x1f9ff], [0x1fa70, 0x1faff], [0x20000, 0x3fffd],
]

export function cellWidth(cp: number): number {
  if (cp < 0x300) return 1
  if ((cp >= 0x300 && cp <= 0x36f) || (cp >= 0x200b && cp <= 0x200f) || (cp >= 0xfe00 && cp <= 0xfe0f)) return 0
  for (const [a, b] of WIDE) if (cp >= a && cp <= b) return 2
  return 1
}

export function textWidth(s: string): number {
  let w = 0
  for (const ch of s) w += cellWidth(ch.codePointAt(0)!)
  return w
}

/** Truncate to a display width, ending with an ellipsis when shortened. */
export function fit(s: string, max: number): string {
  if (max <= 0) return ""
  if (textWidth(s) <= max) return s
  let out = ""
  let w = 0
  for (const ch of s) {
    const cw = cellWidth(ch.codePointAt(0)!)
    if (w + cw > max - 1) break
    out += ch
    w += cw
  }
  return out + "…"
}

export function tokens(n: number | undefined | null): string {
  if (typeof n !== "number" || !Number.isFinite(n)) return "--"
  if (n >= 1e9) return (n / 1e9).toFixed(1) + "B"
  if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e8 ? 0 : 1) + "M"
  if (n >= 1e3) return (n / 1e3).toFixed(n >= 1e5 ? 0 : 1) + "K"
  return String(Math.round(n))
}

export function duration(sec: number | null): string {
  if (sec === null || !Number.isFinite(sec)) return "--"
  const s = Math.max(0, Math.floor(sec))
  if (s >= 86400) return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`
  if (s >= 3600) return `${Math.floor(s / 3600)}h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`
  if (s >= 60) return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`
  return `${s}s`
}

export const shortModel = (m: string): string =>
  m.replace(/^claude-/, "").replace(/-(\d+)-(\d+)$/, " $1.$2").replace(/^gpt-/, "gpt ")
