// What --once and --status say when T3's database is missing.
import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { fileURLToPath } from "node:url"

const MAIN = fileURLToPath(new URL("../src/main.ts", import.meta.url))

function cli(env: Record<string, string | undefined>, ...args: string[]) {
  return spawnSync(process.execPath, ["--disable-warning=ExperimentalWarning", MAIN, ...args],
    { env: { ...process.env, T3CODE_HOME: undefined, ...env }, encoding: "utf8", timeout: 20_000 })
}

test("without a T3 database, --once names the path and the ways out", () => {
  const home = mkdtempSync(join(tmpdir(), "t3a-home-"))
  try {
    const run = cli({ HOME: home }, "--once")
    assert.equal(run.status, 1)
    assert.equal(run.stdout, "")
    assert.ok(run.stderr.includes(join(home, ".t3/userdata/statev2.sqlite")), run.stderr)
    assert.match(run.stderr, /T3 Code installed/)
    assert.match(run.stderr, /T3CODE_HOME/)
    assert.match(run.stderr, /--db PATH/)
    assert.match(run.stderr, /--demo/)

    const custom = join(home, "elsewhere")
    const moved = cli({ HOME: home, T3CODE_HOME: custom }, "--once")
    assert.ok(moved.stderr.includes(join(custom, "userdata/statev2.sqlite")), moved.stderr)
    assert.match(moved.stderr, /T3CODE_HOME is set to/)

    const given = cli({ HOME: home }, "--once", "--db", join(home, "nope.sqlite"))
    assert.match(given.stderr, /nope\.sqlite/)
    assert.match(given.stderr, /path given to --db/)
  } finally {
    rmSync(home, { recursive: true, force: true })
  }
})

test("without a T3 database, --status prints a short error the bar can show", () => {
  const home = mkdtempSync(join(tmpdir(), "t3a-home-"))
  try {
    const run = cli({ HOME: home }, "--status")
    assert.equal(run.status, 0, run.stderr)
    const json = JSON.parse(run.stdout)
    assert.equal(json.v, 1)
    assert.equal(json.running, 0)
    assert.equal(json.error, "No T3 database at ~/.t3/userdata/statev2.sqlite. Is T3 Code installed and opened once?")
    assert.ok(json.error.length <= 120, "fits the tooltip without truncation")
  } finally {
    rmSync(home, { recursive: true, force: true })
  }
})
