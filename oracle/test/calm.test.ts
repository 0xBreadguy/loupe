// Calm mode follows Omarchy's animations toggle.
import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { animationsOff } from "../src/calm.ts"

test("Omarchy's no-animations flag turns calm mode on, in either state directory", () => {
  const home = mkdtempSync(join(tmpdir(), "t3a-"))
  try {
    assert.equal(animationsOff({ HOME: home }), false)
    const flag = (root: string) => {
      mkdirSync(join(root, "omarchy", "toggles", "hypr"), { recursive: true })
      writeFileSync(join(root, "omarchy", "toggles", "hypr", "no-animations.lua"), "")
    }
    flag(join(home, "xdg-state"))
    assert.equal(animationsOff({ HOME: home }), false, "XDG_STATE_HOME unset: only ~/.local/state counts")
    assert.equal(animationsOff({ HOME: home, XDG_STATE_HOME: join(home, "xdg-state") }), true)
    flag(join(home, ".local", "state"))
    assert.equal(animationsOff({ HOME: home }), true)
  } finally {
    rmSync(home, { recursive: true, force: true })
  }
})
