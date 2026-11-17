// Recording hooks for `text-parity.ts`, used only when `T3_RECORD` is set (see record.ts).
//
// The text and number gate feeds about 5.1 million generated inputs through the helpers. Storing
// them or their outputs would be far too large, so a recording keeps a digest per group of
// inputs instead (a "group" is one `compare(...)` call):
//
//   inputs  digest of the exact bytes piped to the probe, which proves that the Rust port of the
//           generators (`crates/core/tests/golden_text/generators.rs`) built the same inputs
//   head    the oracle's first 64 outputs verbatim, so a failure there names the exact input
//   blocks  one digest per 64 outputs, so a failure anywhere else names the 64 inputs
//
// An output's digest is `canonicalDigest(value)` (key-order independent, numbers by value), and a
// block's digest is FNV-1a over the 16-hex digests joined with commas, the same convention as
// `numbersDigest`. The probe's lines are parsed and digested the same way on the Rust side.
//
// `export-text.ts` runs the gate in three timezones and keeps one digest list per group, plus a
// list per timezone only for the groups where the zones disagree (clock and when).
import { canonicalDigest } from './golden.ts'
import { enabled, recordCase } from './record.ts'

export { enabled }
export const BLOCK = 64
const HEAD = 64

/** FNV-1a over bytes, 64-bit, with two 32-bit halves so it stays fast on hundreds of megabytes. */
export function fnv64(bytes: Uint8Array): string {
  let hi = 0xcbf29ce4
  let lo = 0x84222325
  for (let i = 0; i < bytes.length; i++) {
    lo = (lo ^ bytes[i]) >>> 0
    // (hi:lo) * 0x100000001b3 mod 2^64, where 0x100000001b3 = 2^40 + 0x1b3.
    const product = lo * 0x1b3
    const carry = Math.floor(product / 4294967296)
    const nextLo = product % 4294967296
    const nextHi = (hi * 0x1b3 + carry + ((lo << 8) >>> 0)) % 4294967296
    hi = nextHi
    lo = nextLo
  }
  return hi.toString(16).padStart(8, '0') + lo.toString(16).padStart(8, '0')
}

const encoder = new TextEncoder()
export const fnvText = (text: string): string => fnv64(encoder.encode(text))

let sequence = 0
const seen = new Map<string, number>()

/** One `compare(name, inputs, ...)` call: its stdin payload and the oracle's canonical outputs. */
export function recordGroup(name: string, payload: string, outputs: unknown[]): void {
  if (!enabled) return
  const occurrence = seen.get(name) ?? 0
  seen.set(name, occurrence + 1)
  const digests = outputs.map((value) => canonicalDigest(value))
  const blocks: string[] = []
  for (let start = HEAD; start < digests.length; start += BLOCK) {
    blocks.push(fnvText(digests.slice(start, start + BLOCK).join(',')))
  }
  recordCase({
    kind: 'group',
    seq: sequence++,
    name,
    occurrence,
    n: outputs.length,
    inputs: fnvText(payload),
    // Verbatim, as JSON text, so the Rust side can name the exact input and what differs.
    head: outputs.slice(0, HEAD).map((value) => JSON.stringify(value === undefined ? null : value)),
    blocks,
  })
}

/**
 * The oracle's own decision about which generated inputs it can run (the wrap helper never ends
 * on some inputs, so the gate skips them). The decision depends on the oracle's character widths,
 * so it is recorded and the Rust port applies it instead of re-deciding. `null` means all kept.
 */
export function recordKept(label: string, flags: boolean[]): void {
  if (!enabled) return
  recordCase({
    kind: 'kept',
    label,
    of: flags.length,
    flags: flags.every(Boolean) ? null : flags.map((flag) => (flag ? '1' : '0')).join(''),
  })
}

/** The `clean` group enumerates every code point; the oracle's answer is a short list of ranges. */
export function recordHidden(payload: string, count: number, hidden: (cp: number) => boolean): void {
  if (!enabled) return
  const ranges: [number, number][] = []
  for (let cp = 0; cp < count; cp++) {
    if (!hidden(cp)) continue
    const last = ranges[ranges.length - 1]
    if (last && last[1] === cp - 1) last[1] = cp
    else ranges.push([cp, cp])
  }
  recordCase({ kind: 'hidden', count, inputs: fnvText(payload), ranges })
}
