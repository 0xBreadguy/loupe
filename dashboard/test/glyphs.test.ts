// Every character the dashboard draws exists in JetBrainsMono Nerd Font,
// Omarchy's default terminal font, so nothing falls back to another font with
// a different weight, width or baseline.
import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { Canvas } from "../src/canvas.ts"
import { buildDemo, demoUsage } from "../src/demo.ts"
import { Source, type Snapshot } from "../src/source.ts"
import { derivePalette } from "../src/theme.ts"
import { WINDOWS, draw, initialState, type ViewState } from "../src/view.ts"

// Checked one by one against the font's Regular and Bold faces
// (fc-list "JetBrainsMono Nerd Font:charset=<hex code point>"). The last test
// rechecks the list wherever the font is installed.
const SYMBOLS = "·×±»…‖•−←↑→↓↩⇉∴≡⊕⊘⌥⏎␣─│┃┊└├═║╔╗╚╝╭╮╯╰╵▀▁▂▃▄▅▆▇█▉▊▋▌▍▎▏▔▕■▲▸▼◆◇◈◉○◌◎●◔✓✗❯"
const range = (a: number, b: number) => Array.from({ length: b - a + 1 }, (_, i) => String.fromCodePoint(a + i))
const ALLOWED = new Set([
  ...range(0x20, 0x7e), // ASCII
  ...range(0x2800, 0x28ff), // braille: spinners, the pulse and growth charts
  ...SYMBOLS,
])

const EPOCH = Date.parse("2026-11-12T12:00:00Z")

function demo(now: number): Snapshot {
  const dir = mkdtempSync(join(tmpdir(), "t3a-glyphs-"))
  try {
    buildDemo(join(dir, "statev2.sqlite"), now, EPOCH)
    return new Source(join(dir, "statev2.sqlite")).snapshot(now)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test("a rendered demo frame draws only characters the font has", () => {
  const palette = derivePalette({})
  const outside = new Map<string, string>()
  const render = (snap: Snapshot, w: number, h: number, at: number, setup: (st: ViewState) => void, where: string) => {
    const st = initialState(2)
    st.bootAt = at - 300 // mid-wipe, so the scan line is drawn too
    st.demo = true
    setup(st)
    const c = new Canvas(w, h)
    draw(c, st, snap, demoUsage(at, EPOCH), palette, at)
    for (const ch of c.toText().replaceAll("\n", "")) if (!ALLOWED.has(ch) && !outside.has(ch)) outside.set(ch, where)
  }
  for (const minutes of [0, 9]) {
    const now = EPOCH + minutes * 60_000
    const snap = demo(now)
    const billing = snap.agents.find((a) => a.title === "Migrate billing to the v2 API")!.id
    for (const [w, h] of [[40, 10], [48, 14], [60, 20], [100, 40], [140, 45], [200, 80], [330, 70]]) {
      for (const help of [false, true]) {
        for (const calm of [false, true]) {
          // +600 ms: the "needs you" mark is in the off half of its blink.
          for (const at of [now, now + 600]) {
            render(snap, w, h, at, (st) => {
              Object.assign(st, { help, calm, selected: billing, windowIndex: (w + h) % WINDOWS.length })
            }, `${w}x${h} ${help ? "help" : ""} ${calm ? "calm" : ""} +${minutes}m`)
          }
        }
      }
    }
    // Every agent's details, the source failing, updates paused, and nothing to show.
    for (const a of snap.agents) render(snap, 200, 80, now, (st) => (st.selected = a.id), a.title)
    render(snap, 140, 45, now, (st) => (st.error = "database is locked"), "error")
    render(snap, 140, 45, now, (st) => (st.paused = true), "paused")
    render({ ...snap, agents: [] }, 140, 45, now, () => {}, "empty")
  }
  const list = [...outside].map(([ch, where]) => `${ch} U+${ch.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")} (${where})`)
  assert.deepEqual(list, [], "characters outside the verified list")
})

/** The code points a font file covers, from fontconfig. */
function charset(file: string): (cp: number) => boolean {
  const out = spawnSync("fc-query", ["--format=%{charset}", file], { encoding: "utf8" }).stdout ?? ""
  const ranges = out.trim().split(/\s+/).filter(Boolean).map((r) => r.split("-").map((x) => parseInt(x, 16)))
  return (cp) => ranges.some(([a, b = a]) => cp >= a && cp <= b)
}

test("the verified list matches JetBrainsMono Nerd Font where it is installed", (t) => {
  const faces = ["Regular", "Bold"].map((style) => {
    const r = spawnSync("fc-list", [`JetBrainsMono Nerd Font:style=${style}`, "file"], { encoding: "utf8" })
    return { style, file: r.status === 0 ? r.stdout.split("\n")[0].replace(/:\s*$/, "").trim() : "" }
  })
  if (faces.some((f) => !f.file)) return t.skip("JetBrainsMono Nerd Font (Regular and Bold) is not installed here")
  for (const { style, file } of faces) {
    const has = charset(file)
    const missing = [...ALLOWED].filter((ch) => !has(ch.codePointAt(0)!))
    assert.deepEqual(missing, [], `missing from ${style} (${file})`)
  }
})
