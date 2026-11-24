// End to end through OpenTUI's native renderer (headless test renderer).
import assert from "node:assert/strict"
import { test } from "node:test"
import { Renderable, type OptimizedBuffer } from "@opentui/core"
import { createTestRenderer } from "@opentui/core/testing"
import { crt, flush } from "../src/app.ts"
import { Canvas, hbar } from "../src/canvas.ts"

test("the canvas flushes to OpenTUI cell for cell", async () => {
  const canvas = new Canvas(24, 3)
  canvas.clear(0x101315, 0xcacccc)
  canvas.text(0, 0, "LOUPE ⠹ ◆ ✓", 0xffffff)
  hbar(canvas, 0, 1, 12, 0.6, { track: 0x202020, color: (i) => (i > 5 ? 0xde6145 : 0x798186) })
  canvas.text(0, 2, "漢字 wide ok", 0xcacccc)
  class Probe extends Renderable {
    protected renderSelf(buffer: OptimizedBuffer) {
      flush(canvas, buffer, this.screenX, this.screenY)
    }
  }
  const { renderer, renderOnce, captureCharFrame } = await createTestRenderer({ width: 24, height: 3 })
  try {
    renderer.root.add(new Probe(renderer, { width: "100%", height: "100%" }))
    await renderOnce()
    const lines = captureCharFrame().split("\n").map((l) => l.trimEnd())
    assert.deepEqual(lines.slice(0, 3), canvas.toText().split("\n"))
  } finally {
    renderer.destroy()
    await renderer.closed
  }
})

test("the CRT post-process keeps characters and only changes colors", async () => {
  const canvas = new Canvas(30, 4)
  canvas.clear(0x101315, 0xcacccc)
  canvas.text(0, 1, "LOUPE ⠹ ◆ ✓ █▌", 0xffffff)
  class Probe extends Renderable {
    protected renderSelf(buffer: OptimizedBuffer) {
      flush(canvas, buffer, this.screenX, this.screenY)
    }
  }
  const { renderer, renderOnce, captureCharFrame } = await createTestRenderer({ width: 30, height: 4 })
  try {
    renderer.root.add(new Probe(renderer, { width: "100%", height: "100%" }))
    renderer.addPostProcessFn(crt())
    await renderOnce()
    await renderOnce()
    assert.equal(captureCharFrame().split("\n")[1].trimEnd(), "LOUPE ⠹ ◆ ✓ █▌")
  } finally {
    renderer.destroy()
    await renderer.closed
  }
})
