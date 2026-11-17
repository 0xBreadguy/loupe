// Differential gate: the product command line, original vs native.
//
// Runs oracle/src/main.ts with a preloaded fixed clock and the native binary with the same
// arguments (plus --now-ms so the Rust side sees the pinned instant), then compares stdout
// bytes, the exit code and stderr. Synthetic databases only: every path is under a temporary
// directory and HOME/T3CODE_HOME always point there, never at the real T3 data.
import { spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir, userInfo } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { DatabaseSync } from "node:sqlite"
import { Demo } from "../../oracle/src/demo.ts"
import { PREFIX } from "../../oracle/src/source.ts"
// Opt-in: with T3_RECORD set, export-cli.ts turns this run into the recorded fixture.
import { enabled as recording, recordAsset, recordCase, recordTree } from "./record.ts"

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const binary = resolve(process.argv[2] ?? process.env.T3_NATIVE_BINARY ?? join(ROOT, "..", "target/release/t3-agents-core"))
const main = join(ROOT, "..", "oracle/src/main.ts")
const fixedClock = pathToFileURL(join(ROOT, "scripts/fixed-clock.mjs")).href
const NODE = ["--experimental-ffi", "--disable-warning=ExperimentalWarning", "--import", fixedClock]
const EPOCH = 1_794_484_800_000 // 2026-11-12T12:00:00Z
const HOUR = 3_600_000
const DAY = 86_400_000
const ZONES = ["UTC", "Asia/Kolkata", "America/Los_Angeles"]
const dir = mkdtempSync(join(tmpdir(), "t3-cli-parity-"))
const baseHome = join(dir, "home")
mkdirSync(baseHome)
const demos: Demo[] = []
let cases = 0
const differences: string[] = []

type Env = Record<string, string | undefined>
interface Options {
  env?: Env
  now?: number
  nativeArgs?: string[]
  /** Usage errors: Node's wording on line one need not match; everything after it must. */
  ignoreFirstStderrLine?: boolean
}
interface Run {
  status: number | null
  stdout: Buffer
  stderr: Buffer
}

function environment(options: Options): Env {
  const env: Env = { ...process.env, HOME: baseHome, TZ: "UTC", T3_FIXED_NOW: String(options.now ?? EPOCH) }
  delete env.T3CODE_HOME
  delete env.XDG_STATE_HOME
  for (const [key, value] of Object.entries(options.env ?? {})) {
    if (value === undefined) delete env[key]
    else env[key] = value
  }
  return env
}

function original(args: string[], env: Env): Run {
  return spawnSync(process.execPath, [...NODE, main, ...args], {
    env, encoding: "buffer" as never, timeout: 120_000, maxBuffer: 128 * 1024 * 1024,
  }) as unknown as Run
}

function native(args: string[], env: Env): Run {
  return spawnSync(binary, args, {
    env, encoding: "buffer" as never, timeout: 120_000, maxBuffer: 128 * 1024 * 1024,
  }) as unknown as Run
}

function afterFirstLine(buffer: Buffer): Buffer {
  const newline = buffer.indexOf(0x0a)
  return newline < 0 ? Buffer.alloc(0) : buffer.subarray(newline + 1)
}

function preview(buffer: Buffer): string {
  const text = buffer.toString("utf8")
  return text.length > 240 ? text.slice(0, 240) + "…" : text
}

// ── Recording helpers ──────────────────────────────────────────────────────
// Paths under the gate's temporary root become `{tmp}`; inputs the gate built outside it are
// registered as assets and become `@token`. The Rust test rebuilds the inputs and puts paths back.
const replacements = new Map<string, () => string>()
function remember(path: string, make: () => string) {
  if (recording && !replacements.has(path)) replacements.set(path, make)
}
function placeholder(text: string): string {
  // Registered inputs first (they live under `dir`, so their longer paths must win), then the
  // temporary root itself.
  let out = text
  for (const key of [...replacements.keys()].sort((a, b) => b.length - a.length)) out = out.split(key).join(replacements.get(key)!())
  return out.split(dir).join("{tmp}")
}

function compare(label: string, args: string[], options: Options = {}) {
  cases++
  const env = environment(options)
  const expected = original(args, env)
  const actual = native([...args, ...(options.nativeArgs ?? [])], env)
  const problems: string[] = []
  if (expected.status !== actual.status) problems.push(`exit ${expected.status} vs ${actual.status}`)
  if (!expected.stdout.equals(actual.stdout)) {
    problems.push(`stdout\n  ts:   ${JSON.stringify(preview(expected.stdout))}\n  rust: ${JSON.stringify(preview(actual.stdout))}`)
  }
  const expectedErr = options.ignoreFirstStderrLine ? afterFirstLine(expected.stderr) : expected.stderr
  const actualErr = options.ignoreFirstStderrLine ? afterFirstLine(actual.stderr) : actual.stderr
  if (!expectedErr.equals(actualErr)) {
    problems.push(`stderr\n  ts:   ${JSON.stringify(preview(expectedErr))}\n  rust: ${JSON.stringify(preview(actualErr))}`)
  }
  if (problems.length) differences.push(`${label}: ${problems.join("; ")}`)
  if (recording) recordCase({
    kind: "cli",
    label,
    args: args.map(placeholder),
    nativeArgs: options.nativeArgs ?? [],
    env: {
      HOME: placeholder(env.HOME ?? ""),
      TZ: env.TZ ?? null,
      ...(env.T3CODE_HOME === undefined ? {} : { T3CODE_HOME: placeholder(env.T3CODE_HOME) }),
    },
    ignoreFirstStderrLine: Boolean(options.ignoreFirstStderrLine),
    // Byte-exact hex, with temporary paths replaced before encoding (latin1 keeps every byte).
    expected: {
      status: expected.status,
      stdout: Buffer.from(placeholder(expected.stdout.toString("latin1")), "latin1").toString("hex"),
      stderr: Buffer.from(placeholder(expectedErr.toString("latin1")), "latin1").toString("hex"),
    },
  })
}

// ── Synthetic databases ─────────────────────────────────────────────────────

function demoAt(phase: number): string {
  const demo = new Demo({ now: EPOCH, refreshMs: 0 })
  demos.push(demo)
  demo.rebuild(phase)
  return demo.path
}

function withoutRows(path: string, fill: (db: DatabaseSync) => void): string {
  const db = new DatabaseSync(path)
  try {
    const tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all() as { name: string }[]
    for (const { name } of tables) db.exec(`DELETE FROM "${name}"`)
    fill(db)
  } finally {
    db.close()
  }
  return path
}

/** A demo database retitled for the 44-cell name column: ASCII edges, CJK and emoji. */
function titlesDatabase(): string {
  const path = demoAt(EPOCH)
  return withoutRows(path, (db) => {
    db.exec("DELETE FROM orchestration_v2_projection_threads")
    const titles = [
      "A".repeat(43),
      "B".repeat(44),
      "C".repeat(45),
      "日本語".repeat(11),
      "模型".repeat(15),
      "🍞".repeat(20),
      "🍞".repeat(23),
      "e\u0301".repeat(30),
      "Long title · 日本語 · e\u0301 · 🍞 · " + "wide ".repeat(12),
    ]
    // Model names around the column's 16 cells: a real database has provider-prefixed
    // names longer than that, which the one-shot output clips with an ellipsis.
    const models = [
      "gpt-5", "claude-opus-4-5", "moonshotai/kimi-k3", "deepseek/deepseek-flash",
      "m".repeat(15), "m".repeat(16), "m".repeat(17), "模型/" + "模".repeat(9), "provider/🍞-model-with-a-long-name",
    ]
    const insert = db.prepare(`INSERT INTO ${PREFIX}threads(thread_id,payload_json) VALUES(?,?)`)
    titles.forEach((title, i) => {
      const id = `title-${i}`
      insert.run(id, JSON.stringify({
        id, projectId: "p", title, branch: null, activeProviderThreadId: null,
        modelSelection: { model: models[i % models.length], options: [] },
        updatedAt: new Date(EPOCH - i * 1000).toISOString(),
      }))
    })
  })
}

/** The cross-machine fixture from cross-machine-parity.ts, without any prompts or results. */
function crossDatabase(): string {
  const path = demoAt(EPOCH)
  writeFileSync(join(dirname(path), "environment-id"), "machine-a\n")
  return withoutRows(path, (db) => {
    const thread = (id: string, extra: object = {}) =>
      db.prepare(`INSERT INTO ${PREFIX}threads(thread_id,payload_json) VALUES(?,?)`)
        .run(id, JSON.stringify({ id, title: id, ...extra }))
    const ref = (environmentId: string, threadId: string) => ({ environmentId, threadId })
    thread("root"); thread("local", { delegatedFrom: ref("machine-a", "root") })
    thread("leaf", { delegatedFrom: ref("machine-b", "parent") })
    thread("unknown", { delegatedFrom: { ...ref("machine-c", "absent"), title: "Missing remote" } })
    thread("unknown-2", { delegatedFrom: ref("machine-c", "absent") })
    thread("bad", { delegatedFrom: ref("../bad", "parent") })
    thread("incomplete", { delegatedFrom: { environmentId: "machine-b" } })
    thread("a", { delegatedFrom: ref("machine-a", "b") })
    thread("b", { delegatedFrom: ref("machine-a", "a") })
    for (let i = 0; i < 3; i++) {
      const id = `retry-${i}`
      db.prepare(`INSERT INTO ${PREFIX}subagents(subagent_id,thread_id,payload_json) VALUES(?,?,?)`).run(id, "root",
        JSON.stringify({ id, threadId: "root", runId: "rr", providerThreadId: `provider-${i}`,
          status: i === 2 ? "completed" : "running", title: `Retry ${i}`,
          updatedAt: new Date(EPOCH - 3000 + i * 1000).toISOString(),
          remoteChild: { ...ref("machine-b", "parent"), label: "B" } }))
      db.prepare(`INSERT INTO ${PREFIX}turn_items(thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,ordinal,payload_json) VALUES(?,?,?,?,?,?,?,?,?)`)
        .run("root", "rr", id, `provider-${i}`, "command_execution", "completed", new Date(EPOCH - 1000).toISOString(), i,
          JSON.stringify({ input: "synthetic command", title: "Synthetic" }))
      db.prepare(`INSERT INTO ${PREFIX}provider_turns(thread_id,provider_thread_id,ordinal,started_at,payload_json) VALUES(?,?,?,?,?)`)
        .run("root", `provider-${i}`, i, new Date(EPOCH - 2000).toISOString(),
          JSON.stringify({ providerThreadId: `provider-${i}`, tokenUsage: { usedTokens: 10 + i, maxTokens: 100 } }))
    }
  })
}

function emptyDatabase(): string {
  return withoutRows(demoAt(EPOCH), () => {})
}

/** A database path under a data folder, for the three origins. */
function seeded(base: string, kind: "missing" | "unreadable" | "schema"): string {
  const path = join(base, "userdata", "statev2.sqlite")
  mkdirSync(join(base, "userdata"), { recursive: true })
  if (kind === "unreadable") writeFileSync(path, "this is not a SQLite database\n")
  if (kind === "schema") {
    const db = new DatabaseSync(path)
    db.exec("CREATE TABLE untouched(value TEXT)")
    db.close()
  }
  return path
}

try {
  const phases = [
    { path: demoAt(EPOCH), now: EPOCH },
    { path: demoAt(EPOCH + 6 * 60_000), now: EPOCH + 6 * 60_000 },
    { path: demoAt(EPOCH + 15 * 60_000), now: EPOCH + 15 * 60_000 },
  ]
  const cross = crossDatabase()
  const empty = emptyDatabase()
  const titles = titlesDatabase()
  if (recording) {
    remember(baseHome, () => recordTree(baseHome))
    for (const phase of phases) remember(phase.path, () => recordAsset(phase.path, "sql"))
    for (const path of [empty, titles]) remember(path, () => recordAsset(path, "sql"))
    // The cross-machine database names the machine it is on, so its directory travels with it.
    remember(dirname(cross), () => recordTree(dirname(cross)))
  }

  // ── --help and -h ─────────────────────────────────────────────────────────
  compare("help:long", ["--help"])
  compare("help:short", ["-h"])

  // ── --once at demo phases, clocks and timezones ───────────────────────────
  for (const [i, phase] of phases.entries()) {
    for (const offset of [0, HOUR, 26 * HOUR]) {
      for (const tz of ZONES) {
        const now = phase.now + offset
        compare(`once:phase-${i}:${offset}:${tz}`, ["--once", "--db", phase.path], {
          now, env: { TZ: tz }, nativeArgs: ["--now-ms", String(now)],
        })
      }
    }
  }
  for (const tz of ZONES) {
    compare(`once:cross:${tz}`, ["--once", "--db", cross], {
      now: EPOCH, env: { TZ: tz }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }
  compare("once:cross:next-day", ["--once", "--db", cross], {
    now: EPOCH + DAY, nativeArgs: ["--now-ms", String(EPOCH + DAY)],
  })
  for (const tz of ZONES) {
    compare(`once:empty:${tz}`, ["--once", "--db", empty], {
      now: EPOCH, env: { TZ: tz }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }
  for (const tz of ZONES) {
    compare(`once:titles:${tz}`, ["--once", "--db", titles], {
      now: EPOCH, env: { TZ: tz }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }
  const titledLater = EPOCH + 26 * HOUR
  compare("once:titles:next-day", ["--once", "--db", titles], {
    now: titledLater, nativeArgs: ["--now-ms", String(titledLater)],
  })

  // ── --once --demo ─────────────────────────────────────────────────────────
  // The fixed clock makes the demo's start instant equal on both sides, so the whole
  // snapshot is comparable; nothing about the demo's own start time is excluded.
  for (const [i, now] of [EPOCH, EPOCH + 7 * 60_000, EPOCH + 2 * DAY].entries()) {
    compare(`once:demo:${i}`, ["--once", "--demo"], {
      now, nativeArgs: ["--now-ms", String(now)],
    })
  }

  // ── The default database path: T3CODE_HOME and HOME ──────────────────────
  {
    const t3home = join(dir, "t3code-home")
    const db = demoAt(EPOCH)
    mkdirSync(join(t3home, "userdata"), { recursive: true })
    const seededPath = join(t3home, "userdata", "statev2.sqlite")
    new DatabaseSync(seededPath).close()
    // Copy the demo rows into the default location.
    const source = new DatabaseSync(db)
    const target = new DatabaseSync(seededPath)
    const tables = source.prepare("SELECT name FROM sqlite_master WHERE type='table'").all() as { name: string }[]
    for (const { name } of tables) {
      const create = source.prepare("SELECT sql FROM sqlite_master WHERE name=?").get(name) as { sql: string }
      target.exec(create.sql)
      for (const row of source.prepare(`SELECT * FROM "${name}"`).all() as Record<string, unknown>[]) {
        const keys = Object.keys(row)
        target.prepare(`INSERT INTO "${name}"(${keys.map((k) => `"${k}"`).join(",")}) VALUES(${keys.map(() => "?").join(",")})`)
          .run(...keys.map((k) => row[k] as never))
      }
    }
    source.close()
    target.close()
    if (recording) remember(t3home, () => recordTree(t3home))
    compare("once:T3CODE_HOME", ["--once"], {
      now: EPOCH, env: { T3CODE_HOME: t3home }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
    const home = join(dir, "default-home")
    mkdirSync(join(home, ".t3", "userdata"), { recursive: true })
    const homeDb = join(home, ".t3", "userdata", "statev2.sqlite")
    const copy = new DatabaseSync(db)
    const into = new DatabaseSync(homeDb)
    for (const { name } of copy.prepare("SELECT name FROM sqlite_master WHERE type='table'").all() as { name: string }[]) {
      const { sql } = copy.prepare("SELECT sql FROM sqlite_master WHERE name=?").get(name) as { sql: string }
      into.exec(sql)
      for (const row of copy.prepare(`SELECT * FROM "${name}"`).all() as Record<string, unknown>[]) {
        const keys = Object.keys(row)
        into.prepare(`INSERT INTO "${name}"(${keys.map((k) => `"${k}"`).join(",")}) VALUES(${keys.map(() => "?").join(",")})`)
          .run(...keys.map((k) => row[k] as never))
      }
    }
    copy.close()
    into.close()
    if (recording) remember(home, () => recordTree(home))
    compare("once:default-HOME", ["--once"], {
      now: EPOCH, env: { HOME: home }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }

  // ── Database errors at each origin ────────────────────────────────────────
  for (const kind of ["missing", "unreadable", "schema"] as const) {
    const explicit = join(dir, `explicit-${kind}`)
    mkdirSync(join(explicit, "userdata"), { recursive: true })
    const dbPath = seeded(explicit, kind)
    if (recording) remember(explicit, () => recordTree(explicit))
    compare(`error:${kind}:db`, ["--once", "--db", dbPath], {
      nativeArgs: ["--now-ms", String(EPOCH)],
    })
    const fromEnv = join(dir, `env-${kind}`)
    seeded(fromEnv, kind)
    if (recording) remember(fromEnv, () => recordTree(fromEnv))
    compare(`error:${kind}:T3CODE_HOME`, ["--once"], {
      env: { T3CODE_HOME: fromEnv }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
    const fromDefault = join(dir, `default-${kind}`)
    seeded(join(fromDefault, ".t3"), kind)
    if (recording) remember(fromDefault, () => recordTree(fromDefault))
    compare(`error:${kind}:default`, ["--once"], {
      env: { HOME: fromDefault }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }

  // ── Usage errors and interval parsing ─────────────────────────────────────
  compare("usage:demo-and-db", ["--demo", "--db", join(dir, "unused.sqlite")], { ignoreFirstStderrLine: false })
  compare("usage:unknown-long", ["--bogus"], { ignoreFirstStderrLine: true })
  compare("usage:unknown-short", ["-x"], { ignoreFirstStderrLine: true })
  compare("usage:db-missing-value", ["--db"], { ignoreFirstStderrLine: true })
  for (const value of ["0", "0.1", "abc", "5"]) {
    compare(`interval:${value}`, ["--once", "--db", phases[0].path, "--interval", value], {
      now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }

  // ── --status still matches ────────────────────────────────────────────────
  for (const tz of ZONES) {
    compare(`status:demo-db:${tz}`, ["--status", "--db", phases[0].path], {
      now: EPOCH, env: { TZ: tz }, nativeArgs: ["--now-ms", String(EPOCH)],
    })
  }
  compare("status:cross", ["--status", "--db", cross], {
    now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
  })
  compare("status:empty", ["--status", "--db", empty], {
    now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
  })
  compare("status:missing", ["--status", "--db", join(dir, "no-status.sqlite")], {
    now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
  })
  // ── No HOME at all: the account's home folder stands in ──────────────────
  // Node's homedir() falls back to the passwd entry when HOME is unset, so the original still
  // shortens a path under it to ~. Live only: the expected text depends on whose account runs
  // this, so it is not recorded. The path does not exist and nothing under the real home
  // folder is opened; T3CODE_HOME points at an empty folder in case --db were ever ignored.
  if (!recording) {
    const unused = join(dir, "unused-t3-home")
    mkdirSync(unused, { recursive: true })
    const absent = join(userInfo().homedir, ".cache", `t3-agents-gate-absent-${process.pid}`, "statev2.sqlite")
    for (const mode of ["--status", "--once"]) {
      compare(`${mode.slice(2)}:missing:no-HOME`, [mode, "--db", absent], {
        now: EPOCH, env: { HOME: undefined, T3CODE_HOME: unused }, nativeArgs: ["--now-ms", String(EPOCH)],
      })
    }
  }
  compare("status:demo", ["--status", "--demo"], {
    now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
  })
  compare("status:wins-over-once", ["--status", "--once", "--db", phases[0].path], {
    now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
  })
  compare("status:calm", ["--status", "--calm", "--db", phases[0].path], {
    now: EPOCH, nativeArgs: ["--now-ms", String(EPOCH)],
  })

  // ── Interactive without a terminal ───────────────────────────────────────
  compare("terminal:demo", ["--demo"], { now: EPOCH })
  compare("terminal:calm", ["--calm", "--db", phases[0].path], { now: EPOCH })
  compare("terminal:interval", ["--interval", "0.1"], { now: EPOCH })
  compare("terminal:watch-without-status", ["--watch", "--db", phases[0].path], { now: EPOCH })
} finally {
  for (const demo of demos) demo.stop()
  rmSync(dir, { recursive: true, force: true })
}

const report = { version: 1, binary, cases, differences: differences.length, errors: differences.length }
mkdirSync(join(ROOT, "generated"), { recursive: true })
writeFileSync(join(ROOT, "generated", "cli-parity.json"), JSON.stringify({ ...report, mismatches: differences }, null, 2) + "\n")
console.log(JSON.stringify(report))
for (const difference of differences) console.error(difference)
if (differences.length) process.exitCode = 1
