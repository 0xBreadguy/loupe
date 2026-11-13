// Readable contrast: text roles come from a contrast target, in a grayscale, a
// colorful and a light theme, and every letter or digit on screen meets it.
import assert from "node:assert/strict"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { Canvas } from "../src/canvas.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, type Snapshot } from "../src/source.ts"
import { MARK_CONTRAST, TEXT_CONTRAST, contrast, derivePalette, mix, ramp, type Palette } from "../src/theme.ts"
import { draw, initialState, type ViewState } from "../src/view.ts"
import { THEMES } from "./themes.ts"

const band = (p: Palette) => mix(p.bg, p.sel, 0.5) // the T3-linked rows

test("text roles reach 4.5:1 on the background and the tile panels", () => {
  for (const [name, vars] of Object.entries(THEMES)) {
    const p = derivePalette(vars)
    for (const role of ["fg", "text2", "dim", "accent", "hot", "warn"] as const) {
      for (const [surface, s] of [["bg", p.bg], ["panel", p.panel]] as const) {
        assert.ok(contrast(p[role], s) >= TEXT_CONTRAST, `${name}: ${role} on ${surface} is ${contrast(p[role], s).toFixed(2)}`)
      }
    }
    // Selected and linked rows step dim text up to text2 and text2 up to fg.
    for (const role of ["fg", "text2"] as const) {
      for (const [surface, s] of [["selection", p.sel], ["linked band", band(p)]] as const) {
        assert.ok(contrast(p[role], s) >= TEXT_CONTRAST, `${name}: ${role} on the ${surface} is ${contrast(p[role], s).toFixed(2)}`)
      }
    }
    // Still a hierarchy: each role dimmer than the one above it.
    assert.ok(contrast(p.fg, p.bg) > contrast(p.text2, p.bg) && contrast(p.text2, p.bg) > contrast(p.dim, p.bg), `${name}: fg > text2 > dim`)
  }
})

test("Solitude's text roles, measured", () => {
  const p = derivePalette(THEMES.solitude)
  const r = (a: number, b: number) => Math.round(contrast(a, b) * 100) / 100
  assert.equal(p.hot, 0xde6145, "its ember is unchanged")
  assert.deepEqual({ text2: r(p.text2, p.bg), dim: r(p.dim, p.bg), dimOnPanel: r(p.dim, p.panel), text2OnSel: r(p.text2, p.sel) },
    { text2: 7.6, dim: 4.81, dimOnPanel: 4.51, text2OnSel: 4.53 })
  assert.ok(r(p.faint, p.bg) < 2, "faint stays faint: it only draws marks")
})

test("context bars start at 3:1 against their track, on the selected row too", () => {
  for (const [name, vars] of Object.entries(THEMES)) {
    const p = derivePalette(vars)
    for (const track of [p.track, mix(p.track, p.sel, 0.4)]) {
      assert.ok(contrast(ramp(p, p.floor), track) >= MARK_CONTRAST, `${name}: ${contrast(ramp(p, p.floor), track).toFixed(2)}`)
    }
    assert.ok(p.floor < 0.84, `${name}: the ramp still has room to brighten`)
  }
})

const EPOCH = Date.parse("2026-11-12T12:00:00Z")

function demo(now: number): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-contrast-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), now, EPOCH)
    return new Source(join(dir, "statev2.sqlite")).snapshot(now)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

const TEXT = /[\p{L}\p{N}]/u

/** Letters and digits drawn below body-text contrast against their own cell's background. */
function unreadable(c: Canvas, p: Palette): string[] {
  const out = new Set<string>()
  for (let y = 0; y < c.h; y++) {
    for (let x = 0; x < c.w; x++) {
      const i = y * c.w + x
      const ch = String.fromCodePoint(c.ch[i])
      if (!TEXT.test(ch)) continue
      if (c.fg[i] === p.faint) out.add(`faint text "${ch}" at ${x},${y}`)
      const k = contrast(c.fg[i], c.bg[i])
      if (k < TEXT_CONTRAST) {
        const line = c.toText().split("\n")[y]
        out.add(`${k.toFixed(2)} at ${x},${y}: …${line.slice(Math.max(0, x - 12), x + 12)}…`)
      }
    }
  }
  return [...out]
}

test("every letter and digit on screen reads at 4.5:1, whatever is selected or linked", () => {
  for (const minutes of [2.5, 9]) {
    const now = EPOCH + minutes * 60_000
    const snap = demo(now)
    for (const [name, vars] of Object.entries(THEMES)) {
      const p = derivePalette(vars)
      for (const [w, h] of [[60, 20], [80, 24], [100, 30], [150, 50], [240, 70]]) {
        for (const pick of snap.agents) {
          const st: ViewState = initialState(2)
          st.bootAt = 0
          st.calm = true // values settle at once
          st.selected = pick.id
          // Link the thread two rows down, so a band sits beside the selection.
          const other = snap.agents[(snap.agents.indexOf(pick) + 2) % snap.agents.length]
          st.t3Title = `${other.project} / ${other.title}`
          st.follow = false
          const c = new Canvas(w, h)
          draw(c, st, snap, demoUsage(now, EPOCH), p, now)
          assert.deepEqual(unreadable(c, p), [], `${name} ${w}x${h} +${minutes}m, ${pick.title} selected`)
        }
      }
    }
  }
})
