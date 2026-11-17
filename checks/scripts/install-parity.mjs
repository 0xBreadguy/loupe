#!/usr/bin/env node
// Differential gate: the original installer and launcher (`oracle/install.sh`, the oracle)
// against the Rust replacement (`install.sh` at the repository root).
//
// Two temporary homes, a PATH of stub programs that only record how they were called, and no
// network. The original's `node` is a stub too, so its launcher records the program argv
// instead of running the dashboard; the native install gets a recording `--binary` stub. No
// window opens, no terminal starts and nothing under ~/.t3 is read or written.
//
// Deliberate differences from the original, all exercised below rather than papered over:
//
//   1. No Node.js or npm check and no node_modules. The new installer needs cargo unless it is
//      given --binary, and it installs one binary. (The original's install output is otherwise
//      byte-identical for the default name.)
//   2. The new installer takes --name, --title and --binary. The original is fixed to
//      t3-agents, so only the new one can sit two builds side by side. --uninstall also exists
//      only here: the original is removed by hand (README "Update and remove").
//   3. The launcher runs the installed `t3-agents-term` instead of
//      `node --experimental-ffi .../src/main.ts`. Everything after the interpreter prefix is
//      compared, which is the argv the program receives.
//   4. When the program is missing the message names t3-agents-term rather than Node.js, and
//      the --status JSON carries that wording. The JSON shape and exit 127 are compared.
//   5. The marker comment differs ("t3-agents-rust"), so uninstall can tell the two installs
//      apart. The rest of the launcher entry is compared line for line.
//   6. The launcher resolves the program directory from XDG_DATA_HOME at run time, exactly as
//      the original resolves its app directory; only the file inside it differs.
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import {
  chmodSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync,
  rmSync, symlinkSync, writeFileSync,
} from "node:fs"
import { tmpdir, userInfo } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const ORIGINAL_INSTALLER = join(ROOT, "..", "oracle", "install.sh")
const NATIVE_INSTALLER = join(ROOT, "..", "install.sh")
const REAL_NODE = process.execPath
const REAL_JQ = "/usr/bin/jq"
const REAL_BASH = "/usr/bin/bash"
const REAL_HEAD = "/usr/bin/head"
const TMP = mkdtempSync(join(tmpdir(), "t3-install-parity-"))
const STUBS = join(TMP, "stubs")
const PROGRAM_STUB = join(STUBS, "t3-agents-term")
const ORIGINAL_MARKER = "Managed by t3-agents (github.com/0xBreadguy/t3-agents)"
const NATIVE_MARKER = "Managed by t3-agents-rust (github.com/0xBreadguy/t3-agents)"

const DELIBERATE = [
  "no Node.js/npm check, no node_modules: the native installer needs cargo or --binary",
  "native-only flags: --name, --title, --binary, --uninstall",
  "the launcher runs t3-agents-term, not `node .../src/main.ts`",
  "the missing-program message names t3-agents-term, not Node.js (JSON shape and exit 127 match)",
  "the marker comment differs so uninstall can tell the two installs apart",
  "after installing or removing, the native installer ends that install's running status watcher so the bar restarts it on the new program; the stubs here have no pkill, so the gate does not exercise it",
  "the installer builds into ${XDG_CACHE_HOME:-$HOME/.cache}/t3-agents/target, outside the checkout, unless CARGO_TARGET_DIR is set; the original has no build step",
  "the native installer refuses to run when HOME is not the account's home folder while XDG_DATA_HOME or XDG_CACHE_HOME still points into it; the original would install there",
]

// The real install must be exactly as it was when this started. Every case below runs in a
// temporary home with a curated environment, so a change to any of these means a case got out
// of it. Only names, sizes and times are looked at. (The account's home comes from its passwd
// entry, so this works whatever HOME says.)
const ACCOUNT_HOME = userInfo().homedir
const REAL_DATA = process.env.XDG_DATA_HOME || join(ACCOUNT_HOME, ".local", "share")
const WATCHED = ["t3-agents", "t3-agents-next"].flatMap((name) => [
  [`bin/${name}`, join(ACCOUNT_HOME, ".local", "bin", name)],
  [`share/${name}`, join(REAL_DATA, name)],
  [`share/${name}/t3-agents-term`, join(REAL_DATA, name, "t3-agents-term")],
  [`share/${name}/src`, join(REAL_DATA, name, "src")],
  [`applications/${name}.desktop`, join(REAL_DATA, "applications", `${name}.desktop`)],
])
function identity(path) {
  try {
    const s = lstatSync(path, { bigint: true })
    return `${s.ino} ${s.size} ${s.mtimeNs} ${s.ctimeNs}`
  } catch {
    return "absent"
  }
}
const WATCHED_BEFORE = WATCHED.map(([, path]) => identity(path))

let cases = 0
const differences = []
const errors = []

function eq(label, actual, expected) {
  cases++
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    differences.push({
      case: label,
      differences: [`got ${JSON.stringify(actual)}, wanted ${JSON.stringify(expected)}`],
    })
  }
}

function ok(label, condition, detail) {
  cases++
  if (!condition) differences.push({ case: label, differences: [detail] })
}

function compare(label, a, b, options = {}) {
  cases++
  const diffs = []
  if (a.exit !== b.exit) diffs.push(`exit ${a.exit} vs ${b.exit}`)
  if (!options.skipStdout && a.stdout !== b.stdout) {
    diffs.push(`stdout ${JSON.stringify(a.stdout)} vs ${JSON.stringify(b.stdout)}`)
  }
  if (!options.skipStderr && a.stderr !== b.stderr) {
    diffs.push(`stderr ${JSON.stringify(a.stderr)} vs ${JSON.stringify(b.stderr)}`)
  }
  for (const key of ["program", "foot", "xdg", "terminal", "hyprctl", "jq", "notify"]) {
    if ((options.skip ?? []).includes(key)) continue
    if (JSON.stringify(a[key]) !== JSON.stringify(b[key])) {
      diffs.push(`${key} ${JSON.stringify(a[key])} vs ${JSON.stringify(b[key])}`)
    }
  }
  if (diffs.length) differences.push({ case: label, differences: diffs })
}

// ---------------------------------------------------------------- stub programs

function recorder() {
  return [
    "record() {",
    "  local file=$1",
    "  shift",
    "  {",
    "    for a in \"$@\"; do printf '%s' \"$a\"; printf '\\x1f'; done",
    "    printf '\\x1e'",
    "  } >>\"${RECORD:?}/$file.calls\"",
    "}",
  ].join("\n")
}

function stub(name, body) {
  const path = join(STUBS, name)
  writeFileSync(path, `#!/bin/bash\n# recording stub; never the real program\n${recorder()}\n${body}\n`)
  chmodSync(path, 0o755)
}

function buildStubs() {
  mkdirSync(STUBS, { recursive: true })
  for (const name of ["foot", "xdg-terminal-exec", "notify-send", "myterm"]) {
    stub(name, `record ${name} "$@"\nexit 0`)
  }
  stub("hyprctl", [
    'record hyprctl "$@"',
    'if [[ ${1:-} == clients ]]; then printf \'%s\\n\' "${HYPR_JSON:-[]}"; exit 0; fi',
    'if [[ ${1:-} == dispatch ]]; then',
    '  case ${HYPR_FOCUS:-modern} in',
    "    modern) exit 0 ;;",
    '    legacy) [[ ${2:-} == focuswindow ]] && exit 0 || exit 1 ;;',
    "    *) exit 1 ;;",
    "  esac",
    "fi",
    "exit 0",
  ].join("\n"))
  // The launcher's jq filter is real jq; the call is recorded and then delegated.
  stub("jq", 'record jq "$@"\nexec "${REAL_JQ:?}" "$@"')
  // Node is only a launcher interpreter in this test. The installer's `-e` version check is
  // answered, and its `node -` desktop-entry writer delegates to the real Node with its stdin.
  stub("node", [
    'if [[ ${1:-} == - ]]; then exec "${REAL_NODE:?}" "$@" ; fi',
    'if [[ ${1:-} == -e ]]; then exit 0 ; fi',
    'record node "$@"',
    "exit 0",
  ].join("\n"))
  stub("npm", 'record npm "$@"\nmkdir -p node_modules\nexit 0')
  // The build path is exercised by the real end-to-end run; here cargo only proves the command.
  stub("cargo", 'record cargo "$@"\nexit 1')
  stub("t3-agents-term", 'record program "$@"\nexit 0')
}

// A cargo stub for the build-directory cases: it records the target directory the installer
// chose and writes a stand-in binary there. Nothing is compiled.
function buildCargoPath() {
  const dir = join(TMP, "path-build")
  mkdirSync(dir, { recursive: true })
  const cargo = join(dir, "cargo")
  writeFileSync(cargo, [
    "#!/bin/bash",
    recorder(),
    'record cargo "$@"',
    'mkdir -p -- "${CARGO_TARGET_DIR:?}/release"',
    'printf \'#!/bin/sh\\nexit 0\\n\' > "${CARGO_TARGET_DIR:?}/release/t3-agents-term"',
    'chmod 755 -- "${CARGO_TARGET_DIR:?}/release/t3-agents-term"',
    'printf \'%s\\n\' "${CARGO_TARGET_DIR:?}" >> "${RECORD:?}/cargo-target-dir.txt"',
    "exit 0",
  ].join("\n") + "\n")
  chmodSync(cargo, 0o755)
  for (const name of readdirSync(STUBS)) {
    if (name !== "cargo") symlinkSync(join(STUBS, name), join(dir, name))
  }
  return `${dir}:${INSTALL_PATH}`
}

// A PATH for one launcher run: the stubs (minus exclusions) plus only the utilities the
// launcher needs. /usr/bin is deliberately not on it, so a missing stub can never fall
// through to the real foot, hyprctl or xdg-terminal-exec on this machine.
const pathCache = new Map()
function launcherPath(exclude = []) {
  const key = [...exclude].sort().join("|") || "full"
  if (pathCache.has(key)) return pathCache.get(key)
  const dir = join(TMP, "path", key.replaceAll("|", "-"))
  mkdirSync(dir, { recursive: true })
  for (const file of readdirSync(STUBS)) {
    if (exclude.includes(file)) continue
    symlinkSync(join(STUBS, file), join(dir, file))
  }
  symlinkSync(REAL_BASH, join(dir, "bash"))
  symlinkSync(REAL_HEAD, join(dir, "head"))
  pathCache.set(key, dir)
  return dir
}

// An install-time PATH with real coreutils but no real node/npm/cargo ahead of the stubs.
const INSTALL_PATH = `${STUBS}:/usr/bin:/bin`
// A PATH without cargo, for the prerequisite check: coreutils only.
const NOCARGO_PATH = join(TMP, "path-no-cargo")
function noCargoPath() {
  mkdirSync(NOCARGO_PATH, { recursive: true })
  for (const name of [
    "bash", "cat", "chmod", "cp", "dirname", "grep", "head", "install", "ls", "mkdir",
    "mktemp", "mv", "rm",
  ]) {
    const target = join("/usr/bin", name)
    if (!existsSync(join(NOCARGO_PATH, name)) && existsSync(target)) {
      symlinkSync(target, join(NOCARGO_PATH, name))
    }
  }
  return NOCARGO_PATH
}

// ---------------------------------------------------------------- temporary homes

function makeHome(label) {
  const base = join(TMP, label)
  const home = join(base, "home")
  const record = join(base, "record")
  mkdirSync(home, { recursive: true })
  mkdirSync(record, { recursive: true })
  return { label, base, home, record, data: join(home, ".local", "share"), bin: join(home, ".local", "bin") }
}

function clearRecord(h) {
  rmSync(h.record, { recursive: true, force: true })
  mkdirSync(h.record, { recursive: true })
}

function baseEnv(h) {
  // A curated environment: HOME is the temporary home, and nothing (T3CODE_HOME, XDG_*,
  // HYPRLAND_INSTANCE_SIGNATURE, TERMINAL) leaks in from the machine running the test.
  return { HOME: h.home, TMPDIR: TMP, LC_ALL: "C", TZ: "UTC" }
}

function installEnv(h, path = INSTALL_PATH) {
  return { ...baseEnv(h), PATH: path, RECORD: h.record, REAL_NODE, REAL_JQ }
}

function runEnv(h, path, extra) {
  const env = { ...baseEnv(h), PATH: path, RECORD: h.record, REAL_NODE, REAL_JQ }
  for (const [key, value] of Object.entries(extra ?? {})) {
    if (value !== undefined) env[key] = value
  }
  return env
}

function spawn(file, args, env) {
  const r = spawnSync(file, args, { env, encoding: "utf8", timeout: 120_000 })
  if (r.error) throw r.error
  return { exit: r.status, stdout: r.stdout, stderr: r.stderr }
}

function installHomes(side, h, args) {
  clearRecord(h)
  const installer = side === "original" ? ORIGINAL_INSTALLER : NATIVE_INSTALLER
  return spawn(installer, args, installEnv(h))
}

function normalizeHome(text, h) {
  return text.split(h.home).join("<HOME>")
}

// ---------------------------------------------------------------- recorded calls

function calls(h, name) {
  const path = join(h.record, `${name}.calls`)
  if (!existsSync(path)) return []
  return readFileSync(path, "utf8")
    .split("\x1e")
    .filter((call) => call !== "")
    .map((call) => {
      const argv = call.split("\x1f")
      if (argv[argv.length - 1] === "") argv.pop()
      return argv
    })
}

// Replace the interpreter+script (original) or the installed binary (native) with one marker,
// so the two sides' argv can be compared for the arguments that follow.
function canonical(argv, side, appDir) {
  const out = [...argv]
  for (let i = 0; i < out.length; i++) {
    if (side === "native" && out[i] === join(appDir, "t3-agents-term")) {
      out.splice(i, 1, "<PROGRAM>")
      return out
    }
    if (
      side === "original" && /\/src\/main\.ts$/.test(out[i]) &&
      out[i - 1] === "--disable-warning=ExperimentalWarning" &&
      out[i - 2] === "--experimental-ffi" && out[i - 3] === join(STUBS, "node")
    ) {
      out.splice(i - 3, 4, "<PROGRAM>")
      return out
    }
  }
  return out
}

function observe(h, side, args, { env = {}, exclude = [] } = {}) {
  clearRecord(h)
  const appDir = join(h.data, "t3-agents")
  const result = spawn(join(h.bin, "t3-agents"), args, runEnv(h, launcherPath(exclude), env))
  const node = calls(h, "node")
  const program = side === "original" ? (node[0] ?? []).slice(3) : (calls(h, "program")[0] ?? [])
  const terminal = (name) => calls(h, name).map((argv) => canonical(argv, side, appDir))
  return {
    ...result,
    program,
    foot: terminal("foot"),
    xdg: terminal("xdg-terminal-exec"),
    terminal: terminal("myterm"),
    hyprctl: calls(h, "hyprctl"),
    jq: calls(h, "jq"),
    notify: calls(h, "notify-send"),
  }
}

function launcherCase(label, args, options = {}) {
  const a = observe(ORIGINAL, "original", args, options)
  const b = observe(NATIVE, "native", args, options)
  compare(label, a, b)
  return { original: a, native: b }
}

// ---------------------------------------------------------------- files

function read(path) {
  return existsSync(path) ? readFileSync(path, "utf8") : null
}

function hash(path) {
  return existsSync(path) ? createHash("sha256").update(readFileSync(path)).digest("hex") : null
}

function desktop(h) {
  return read(join(h.data, "applications", "t3-agents.desktop"))
}

function entries(dir) {
  return existsSync(dir) ? readdirSync(dir).sort() : []
}

function filesUnder(dir) {
  const out = []
  const walk = (at) => {
    for (const entry of readdirSync(at, { withFileTypes: true })) {
      const path = join(at, entry.name)
      if (entry.isDirectory()) walk(path)
      else out.push(path)
    }
  }
  if (existsSync(dir)) walk(dir)
  return out.sort()
}

// ---------------------------------------------------------------- run

buildStubs()

const ORIGINAL = makeHome("original")
const NATIVE = makeHome("native")
const NATIVE_ARGS = ["--binary", PROGRAM_STUB]

function phase(label, fn) {
  try {
    fn()
  } catch (error) {
    errors.push(`${label}: ${error?.stack ?? error}`)
  }
}

// 1. The install itself, into separate homes, with the same observable output.
phase("install", () => {
  const a = installHomes("original", ORIGINAL, [])
  const b = installHomes("native", NATIVE, NATIVE_ARGS)
  compare("first install output", { ...a, stdout: normalizeHome(a.stdout, ORIGINAL), stderr: normalizeHome(a.stderr, ORIGINAL) },
    { ...b, stdout: normalizeHome(b.stdout, NATIVE), stderr: normalizeHome(b.stderr, NATIVE) })
  eq("original install stdout", normalizeHome(a.stdout, ORIGINAL),
    "Installed T3 Agents. Run t3-agents or t3-agents --window.\n" +
    "Note: <HOME>/.local/bin is not on your PATH, so run <HOME>/.local/bin/t3-agents or add that folder to PATH.\n")

  // The original needed Node and npm; the new one used neither (deliberate difference 1).
  eq("original npm ci calls", calls(ORIGINAL, "npm").length, 1)
  eq("native node calls", calls(NATIVE, "node").length, 0)
  eq("native npm calls", calls(NATIVE, "npm").length, 0)

  // Same launcher entry, apart from the marker line (deliberate difference 5).
  const od = desktop(ORIGINAL).split("\n")
  const nd = desktop(NATIVE).split("\n")
  eq("original entry marker", od[0], `# ${ORIGINAL_MARKER}`)
  eq("native entry marker", nd[0], `# ${NATIVE_MARKER}`)
  eq("launcher entry after the marker line",
    nd.slice(1).map((line) => normalizeHome(line, NATIVE)),
    od.slice(1).map((line) => normalizeHome(line, ORIGINAL)))

  eq("original program directory", entries(join(ORIGINAL.data, "t3-agents")),
    [".installed-lock", "node_modules", "package-lock.json", "package.json", "src"])
  eq("native program directory", entries(join(NATIVE.data, "t3-agents")),
    [".t3-agents-install", "t3-agents-term"])
  ok("native command exists", existsSync(join(NATIVE.bin, "t3-agents")), "command not written")
  // Nothing else: no autostart entry, keybinding, window rule or service.
  eq("native install wrote only its own files", filesUnder(NATIVE.home), [
    join(NATIVE.home, ".local/bin/t3-agents"),
    join(NATIVE.home, ".local/share/applications/t3-agents.desktop"),
    join(NATIVE.home, ".local/share/t3-agents/.t3-agents-install"),
    join(NATIVE.home, ".local/share/t3-agents/t3-agents-term"),
  ])
})

// 2. Launcher branches. Every case compares exit code, stdout, stderr and every recorded call.
phase("launcher", () => {
  const plain = launcherCase("plain arguments", [])
  eq("plain arguments reach the program", plain.native.program, [])

  const once = launcherCase("pass-through --once --demo", ["--once", "--demo"])
  eq("pass-through args", once.native.program, ["--once", "--demo"])
  eq("native launcher runs no node", calls(NATIVE, "node"), [])

  const status = launcherCase("--status", ["--status", "--demo"])
  eq("--status args", status.native.program, ["--status", "--demo"])

  const watch = launcherCase("--status --watch", ["--status", "--watch", "--demo"])
  eq("--status --watch args", watch.native.program, ["--status", "--watch", "--demo"])

  const foot = launcherCase("--window with foot", ["--window", "--demo"])
  eq("foot argv", foot.native.foot,
    [["--app-id=TUI.t3-agents", "--title=T3 Agents", "<PROGRAM>", "--demo"]])
  ok("foot case exits 0", foot.native.exit === 0, `exit ${foot.native.exit}`)

  const xdg = launcherCase("--window with xdg-terminal-exec", ["--window", "--demo"], { exclude: ["foot"] })
  eq("xdg-terminal-exec argv", xdg.native.xdg,
    [["--app-id=TUI.t3-agents", "--title=T3 Agents", "--", "<PROGRAM>", "--demo"]])

  const terminal = launcherCase("--window with TERMINAL", ["--window", "--demo"], {
    exclude: ["foot", "xdg-terminal-exec"], env: { TERMINAL: "myterm --class demo" },
  })
  eq("TERMINAL argv", terminal.native.terminal,
    [["--class", "demo", "-e", "<PROGRAM>", "--demo"]])

  const none = launcherCase("--window with no terminal", ["--window"], {
    exclude: ["foot", "xdg-terminal-exec"],
  })
  const message = "t3-agents --window needs foot, xdg-terminal-exec or a TERMINAL variable. Or run t3-agents inside any terminal."
  eq("no-terminal stderr", none.native.stderr, `${message}\n`)
  eq("no-terminal exit", none.native.exit, 1)
  eq("no-terminal notification", none.native.notify, [["T3 Agents", message]])
  eq("no-terminal starts nothing", [none.native.foot, none.native.xdg, none.native.terminal], [[], [], []])

  const hypr = { HYPRLAND_INSTANCE_SIGNATURE: "test" }
  const open = JSON.stringify([{ class: "TUI.t3-agents", address: "0x55a" }])
  const other = JSON.stringify([{ class: "foot", address: "0x1" }])

  const modern = launcherCase("Hyprland, window open, current dispatch", ["--window", "--demo"], {
    env: { ...hypr, HYPR_JSON: open, HYPR_FOCUS: "modern" },
  })
  eq("Hyprland modern dispatch", modern.native.hyprctl,
    [["clients", "-j"], ["dispatch", 'hl.dsp.focus({ window = "address:0x55a" })']])
  eq("Hyprland focus opens no terminal", [modern.native.foot, modern.native.xdg, modern.native.terminal], [[], [], []])
  eq("Hyprland focus exit", modern.native.exit, 0)

  const legacy = launcherCase("Hyprland, window open, legacy dispatch", ["--window", "--demo"], {
    env: { ...hypr, HYPR_JSON: open, HYPR_FOCUS: "legacy" },
  })
  eq("Hyprland legacy dispatch", legacy.native.hyprctl, [
    ["clients", "-j"],
    ["dispatch", 'hl.dsp.focus({ window = "address:0x55a" })'],
    ["dispatch", "focuswindow", "address:0x55a"],
  ])

  const failed = launcherCase("Hyprland, focus fails, falls back to a terminal", ["--window"], {
    env: { ...hypr, HYPR_JSON: open, HYPR_FOCUS: "fail" },
  })
  eq("failed focus still opens foot", failed.native.foot,
    [["--app-id=TUI.t3-agents", "--title=T3 Agents", "<PROGRAM>"]])

  const shut = launcherCase("Hyprland, window not open", ["--window"], {
    env: { ...hypr, HYPR_JSON: other },
  })
  eq("closed window opens foot", shut.native.foot,
    [["--app-id=TUI.t3-agents", "--title=T3 Agents", "<PROGRAM>"]])

  const nojq = launcherCase("Hyprland without jq", ["--window", "--demo"], {
    env: { ...hypr, HYPR_JSON: open }, exclude: ["jq"],
  })
  eq("no jq opens foot", nojq.native.foot,
    [["--app-id=TUI.t3-agents", "--title=T3 Agents", "<PROGRAM>", "--demo"]])
  eq("no jq runs no hyprctl", nojq.native.hyprctl, [])
})

// 3. The missing-program path: the original's Node is gone, the new binary is gone. The message
// text differs by design (difference 4); the exit code and the --status JSON shape must not.
phase("missing program", () => {
  const node = join(STUBS, "node")
  const program = join(NATIVE.data, "t3-agents", "t3-agents-term")
  renameSync(node, `${node}.off`)
  renameSync(program, `${program}.off`)
  try {
    const withStatus = observe(ORIGINAL, "original", ["--status"], {})
    const withStatusNative = observe(NATIVE, "native", ["--status"], {})
    eq("missing program --status exit", [withStatus.exit, withStatusNative.exit], [127, 127])
    const parse = (text) => {
      try { return JSON.parse(text.trim()) } catch { return null }
    }
    const o = parse(withStatus.stdout)
    const n = parse(withStatusNative.stdout)
    eq("missing program --status JSON shape", [o && Object.keys(o).sort(), n && Object.keys(n).sort()],
      [[ "agents", "error", "failed", "running", "v", "waiting" ], [ "agents", "error", "failed", "running", "v", "waiting" ]])
    eq("missing program --status counts", [o?.v, o?.running, o?.waiting, o?.failed, o?.agents, n?.v, n?.running, n?.waiting, n?.failed, n?.agents],
      [1, 0, 0, 0, [], 1, 0, 0, 0, []])
    ok("missing program --status error text", Boolean(o?.error) && Boolean(n?.error) && o.error !== n.error,
      `error fields ${JSON.stringify([o?.error, n?.error])}`)

    const plainO = observe(ORIGINAL, "original", ["--demo"], {})
    const plainN = observe(NATIVE, "native", ["--demo"], {})
    eq("missing program exit", [plainO.exit, plainN.exit], [127, 127])
    eq("missing program stdout", [plainO.stdout, plainN.stdout], ["", ""])
    ok("missing program stderr", plainO.stderr.length > 0 && plainN.stderr.length > 0,
      "one side was silent")
    ok("missing program stderr names the program", /t3-agents-term/.test(plainN.stderr), plainN.stderr)
  } finally {
    renameSync(`${node}.off`, node)
    renameSync(`${program}.off`, program)
  }
})

// 4. Rerun is the update path: same output, same files, and the original skips npm the second
// time because node_modules and the lock are already there.
phase("rerun", () => {
  const before = { desktop: hash(join(ORIGINAL.data, "applications", "t3-agents.desktop")), command: hash(join(ORIGINAL.bin, "t3-agents")) }
  const a = installHomes("original", ORIGINAL, [])
  eq("original rerun output", normalizeHome(a.stdout, ORIGINAL),
    "Installed T3 Agents. Run t3-agents or t3-agents --window.\n" +
    "Note: <HOME>/.local/bin is not on your PATH, so run <HOME>/.local/bin/t3-agents or add that folder to PATH.\n")
  eq("original rerun skips npm", calls(ORIGINAL, "npm"), [])
  eq("original rerun leaves its files alone", [hash(join(ORIGINAL.data, "applications", "t3-agents.desktop")), hash(join(ORIGINAL.bin, "t3-agents"))],
    [before.desktop, before.command])

  const b = installHomes("native", NATIVE, NATIVE_ARGS)
  eq("native rerun output", normalizeHome(b.stdout, NATIVE),
    "Installed T3 Agents. Run t3-agents or t3-agents --window.\n" +
    "Note: <HOME>/.local/bin is not on your PATH, so run <HOME>/.local/bin/t3-agents or add that folder to PATH.\n")
  eq("native rerun program directory", entries(join(NATIVE.data, "t3-agents")),
    [".t3-agents-install", "t3-agents-term"])
  const rerun = observe(NATIVE, "native", ["--once", "--demo"], {})
  eq("rerun command still works", rerun.program, ["--once", "--demo"])
})

// 5. A launcher the installer did not create is never overwritten.
phase("foreign launcher", () => {
  const foreign = "#!/bin/sh\nprintf 'someone else\\n'\n"
  const ho = makeHome("foreign-original")
  const hn = makeHome("foreign-native")
  for (const h of [ho, hn]) {
    mkdirSync(h.bin, { recursive: true })
    writeFileSync(join(h.bin, "t3-agents"), foreign)
    chmodSync(join(h.bin, "t3-agents"), 0o755)
  }
  const a = installHomes("original", ho, [])
  const b = installHomes("native", hn, NATIVE_ARGS)
  compare("foreign launcher refusal",
    { ...a, stderr: normalizeHome(a.stderr, ho) },
    { ...b, stderr: normalizeHome(b.stderr, hn) })
  eq("foreign launcher exit", [a.exit, b.exit], [1, 1])
  eq("foreign launcher stderr", normalizeHome(b.stderr, hn), `Refusing to overwrite existing file: <HOME>/.local/bin/t3-agents\n`)
  eq("foreign launcher untouched", [read(join(ho.bin, "t3-agents")), read(join(hn.bin, "t3-agents"))], [foreign, foreign])
  eq("foreign launcher writes nothing", [existsSync(join(ho.data, "t3-agents")), existsSync(join(hn.data, "t3-agents"))], [false, false])
})

phase("foreign desktop entry", () => {
  const foreign = "[Desktop Entry]\nName=Something Else\n"
  const ho = makeHome("foreign-desktop-original")
  const hn = makeHome("foreign-desktop-native")
  for (const h of [ho, hn]) {
    mkdirSync(join(h.data, "applications"), { recursive: true })
    writeFileSync(join(h.data, "applications", "t3-agents.desktop"), foreign)
  }
  const a = installHomes("original", ho, [])
  const b = installHomes("native", hn, NATIVE_ARGS)
  eq("foreign entry exit", [a.exit, b.exit], [1, 1])
  eq("foreign entry untouched",
    [read(join(ho.data, "applications", "t3-agents.desktop")), read(join(hn.data, "applications", "t3-agents.desktop"))],
    [foreign, foreign])
  eq("foreign entry writes nothing", [existsSync(join(ho.bin, "t3-agents")), existsSync(join(hn.bin, "t3-agents"))], [false, false])
})

// The program directory is replaced only when a marker of ours or of the Node installer says
// who made it. An unmarked directory is refused, even with the launcher and entry clear.
phase("foreign program directory", () => {
  const h = makeHome("foreign-app")
  const keep = join(h.data, "t3-agents", "keep.txt")
  mkdirSync(dirname(keep), { recursive: true })
  writeFileSync(keep, "not ours\n")
  const r = spawn(NATIVE_INSTALLER, NATIVE_ARGS, installEnv(h))
  eq("foreign app exit", r.exit, 1)
  eq("foreign app stderr", normalizeHome(r.stderr, h),
    "Refusing to overwrite existing directory: <HOME>/.local/share/t3-agents\n" +
    "It has no marker from this installer or the Node.js installer; remove it yourself if it is a T3 Agents install.\n")
  eq("foreign app untouched", read(keep), "not ours\n")
  eq("foreign app writes no command", existsSync(join(h.bin, "t3-agents")), false)
})

// 6. Two names side by side: the new one must not touch the original install.
phase("two names side by side", () => {
  const shared = makeHome("side-by-side")
  const first = installHomes("original", shared, [])
  eq("side-by-side: original installs", first.exit, 0)
  const originalLauncher = join(shared.bin, "t3-agents")
  const originalDesktop = join(shared.data, "applications", "t3-agents.desktop")
  const before = { launcher: hash(originalLauncher), desktop: hash(originalDesktop), app: entries(join(shared.data, "t3-agents")) }

  const second = installHomes("native", shared, ["--name", "t3-agents-next", "--binary", PROGRAM_STUB])
  eq("side-by-side: native installs", second.exit, 0)
  eq("side-by-side: original untouched", {
    launcher: hash(originalLauncher), desktop: hash(originalDesktop), app: entries(join(shared.data, "t3-agents")),
  }, before)
  eq("side-by-side: new command", existsSync(join(shared.bin, "t3-agents-next")), true)
  eq("side-by-side: new program directory", entries(join(shared.data, "t3-agents-next")),
    [".t3-agents-install", "t3-agents-term"])
  eq("side-by-side: first entry", read(originalDesktop).split("\n").find((line) => line.startsWith("StartupWMClass=")),
    "StartupWMClass=TUI.t3-agents")
  const nextDesktop = read(join(shared.data, "applications", "t3-agents-next.desktop"))
  eq("side-by-side: new entry", [
    nextDesktop.split("\n").find((line) => line.startsWith("Name=")),
    nextDesktop.split("\n").find((line) => line.startsWith("StartupWMClass=")),
  ], ["Name=T3 Agents (t3-agents-next)", "StartupWMClass=TUI.t3-agents-next"])

  // The old command still hands its arguments to the Node app, and the new one to the binary.
  clearRecord(shared)
  spawn(originalLauncher, ["--demo"], runEnv(shared, launcherPath(), {}))
  eq("side-by-side: original still runs", calls(shared, "node")[0]?.slice(3), ["--demo"])
  clearRecord(shared)
  spawn(join(shared.bin, "t3-agents-next"), ["--demo"], runEnv(shared, launcherPath(), {}))
  eq("side-by-side: new command runs", calls(shared, "program"), [["--demo"]])
})

// 7. Upgrade an existing Node install in place, with no node_modules left behind.
phase("upgrade from the Node install", () => {
  const h = makeHome("upgrade")
  const first = installHomes("original", h, [])
  eq("upgrade: Node install in place", first.exit, 0)
  ok("upgrade: node_modules before", existsSync(join(h.data, "t3-agents", "node_modules")), "no node_modules")
  eq("upgrade: Node marker on the launcher", read(join(h.bin, "t3-agents")).includes(ORIGINAL_MARKER), true)

  const upgraded = installHomes("native", h, ["--binary", PROGRAM_STUB])
  eq("upgrade: exit", upgraded.exit, 0)
  eq("upgrade: output", normalizeHome(upgraded.stdout, h),
    "Installed T3 Agents. Run t3-agents or t3-agents --window.\n" +
    "Note: <HOME>/.local/bin is not on your PATH, so run <HOME>/.local/bin/t3-agents or add that folder to PATH.\n")
  eq("upgrade: no stale Node files", entries(join(h.data, "t3-agents")),
    [".t3-agents-install", "t3-agents-term"])
  eq("upgrade: marker is ours", read(join(h.data, "applications", "t3-agents.desktop")).split("\n")[0], `# ${NATIVE_MARKER}`)
  eq("upgrade: launcher is ours", read(join(h.bin, "t3-agents")).includes(NATIVE_MARKER), true)
  clearRecord(h)
  spawn(join(h.bin, "t3-agents"), ["--status", "--demo"], runEnv(h, launcherPath(), {}))
  eq("upgrade: command runs the binary", calls(h, "program"), [["--status", "--demo"]])
})

// 8. Uninstall removes exactly what this name's install wrote.
phase("uninstall", () => {
  const h = makeHome("uninstall")
  installHomes("native", h, NATIVE_ARGS)
  // Uninstall needs no cargo: a PATH of coreutils only.
  const path = noCargoPath()
  const run = (args) => {
    clearRecord(h)
    return spawn(NATIVE_INSTALLER, args, installEnv(h, path))
  }
  const first = run(["--uninstall"])
  eq("uninstall: exit", first.exit, 0)
  eq("uninstall: removed files", [
    existsSync(join(h.bin, "t3-agents")),
    existsSync(join(h.data, "applications", "t3-agents.desktop")),
    existsSync(join(h.data, "t3-agents")),
  ], [false, false, false])
  eq("uninstall: leftover files", filesUnder(h.home), [])
  const again = run(["--uninstall"])
  eq("uninstall: rerun is quiet", [again.exit, normalizeHome(again.stdout, h)], [0, "Nothing to remove for t3-agents.\n"])

  // A foreign file is left in place, with a warning, and the exit code stays 0.
  mkdirSync(h.bin, { recursive: true })
  writeFileSync(join(h.bin, "t3-agents"), "#!/bin/sh\n")
  const foreign = run(["--uninstall"])
  eq("uninstall: foreign exit", foreign.exit, 0)
  eq("uninstall: foreign file kept", existsSync(join(h.bin, "t3-agents")), true)
  ok("uninstall: foreign warning", /Leaving .*not created by this installer/.test(foreign.stderr), foreign.stderr)

  // The native uninstaller must not remove an untouched Node install.
  const nodeHome = makeHome("uninstall-node")
  installHomes("original", nodeHome, [])
  const nodeRun = spawn(NATIVE_INSTALLER, ["--uninstall"], installEnv(nodeHome, path))
  eq("uninstall: Node install exit", nodeRun.exit, 0)
  eq("uninstall: Node install kept", [
    existsSync(join(nodeHome.bin, "t3-agents")),
    existsSync(join(nodeHome.data, "applications", "t3-agents.desktop")),
    existsSync(join(nodeHome.data, "t3-agents", "node_modules")),
  ], [true, true, true])
})

// 9. The PATH warning is present only when the bin directory is not on PATH.
phase("PATH warning", () => {
  const ho = makeHome("path-original")
  const hn = makeHome("path-native")
  const a = spawn(ORIGINAL_INSTALLER, [], installEnv(ho, `${ho.bin}:${INSTALL_PATH}`))
  const b = spawn(NATIVE_INSTALLER, NATIVE_ARGS, installEnv(hn, `${hn.bin}:${INSTALL_PATH}`))
  compare("bin directory on PATH", { ...a, stdout: normalizeHome(a.stdout, ho), stderr: normalizeHome(a.stderr, ho) },
    { ...b, stdout: normalizeHome(b.stdout, hn), stderr: normalizeHome(b.stderr, hn) })
  eq("no PATH note", [a.stdout.includes("not on your PATH"), b.stdout.includes("not on your PATH")], [false, false])
})

// 10. A different name everywhere, including the title and the --title override.
phase("named install", () => {
  const h = makeHome("named")
  const named = spawn(NATIVE_INSTALLER, ["--name", "t3-agents-next", "--binary", PROGRAM_STUB], installEnv(h))
  eq("named: install", named.exit, 0)
  const entry = read(join(h.data, "applications", "t3-agents-next.desktop"))
  eq("named: title and app id", [
    entry.split("\n").find((line) => line.startsWith("Name=")),
    entry.split("\n").find((line) => line.startsWith("StartupWMClass=")),
  ], ["Name=T3 Agents (t3-agents-next)", "StartupWMClass=TUI.t3-agents-next"])
  clearRecord(h)
  spawn(join(h.bin, "t3-agents-next"), ["--window", "--demo"], runEnv(h, launcherPath(), {}))
  eq("named: window uses the new app id and title", calls(h, "foot"),
    [["--app-id=TUI.t3-agents-next", "--title=T3 Agents (t3-agents-next)", join(h.data, "t3-agents-next", "t3-agents-term"), "--demo"]])

  const titled = spawn(NATIVE_INSTALLER, ["--name", "custom", "--title", "My Agents", "--binary", PROGRAM_STUB], installEnv(h))
  eq("titled: install", titled.exit, 0)
  const own = read(join(h.data, "applications", "custom.desktop"))
  eq("titled: name", own.split("\n").find((line) => line.startsWith("Name=")), "Name=My Agents")
  clearRecord(h)
  spawn(join(h.bin, "custom"), ["--window"], runEnv(h, launcherPath(), {}))
  eq("titled: window title", calls(h, "foot")[0]?.[1], "--title=My Agents")
})

// 11. Native-only prerequisite failures: nothing is copied when the installer cannot build.
phase("native prerequisites", () => {
  const h = makeHome("no-cargo")
  const r = spawn(NATIVE_INSTALLER, [], installEnv(h, noCargoPath()))
  eq("no cargo: exit", r.exit, 1)
  ok("no cargo: message", /cargo is required/.test(r.stderr) && /--binary PATH/.test(r.stderr), r.stderr)
  eq("no cargo: writes nothing", [existsSync(h.bin), existsSync(join(h.data, "t3-agents"))], [false, false])

  const f = makeHome("failing-cargo")
  const build = spawn(NATIVE_INSTALLER, [], installEnv(f))
  eq("failing cargo: exit", build.exit, 1)
  eq("failing cargo: command", calls(f, "cargo"), [["build", "--release", "--locked", "-p", "t3-agents-term"]])
  eq("failing cargo: writes nothing", [existsSync(f.bin), existsSync(join(f.data, "t3-agents"))], [false, false])

  const b = makeHome("bad-binary")
  const missing = spawn(NATIVE_INSTALLER, ["--binary", join(TMP, "no-such-binary")], installEnv(b))
  eq("missing --binary: exit", missing.exit, 1)
  ok("missing --binary: message", /not an executable/.test(missing.stderr), missing.stderr)
  eq("missing --binary: writes nothing", [existsSync(b.bin), existsSync(join(b.data, "t3-agents"))], [false, false])

  // A prebuilt binary installs without cargo on PATH at all.
  const prebuilt = makeHome("prebuilt-no-cargo")
  const prebuiltRun = spawn(NATIVE_INSTALLER, ["--binary", PROGRAM_STUB], installEnv(prebuilt, noCargoPath()))
  eq("prebuilt without cargo: exit", prebuiltRun.exit, 0)
  eq("prebuilt without cargo: files", entries(join(prebuilt.data, "t3-agents")),
    [".t3-agents-install", "t3-agents-term"])
  eq("prebuilt without cargo: no build", calls(prebuilt, "cargo"), [])

  const n = makeHome("bad-name")
  const badName = spawn(NATIVE_INSTALLER, ["--name", "../escape", "--binary", PROGRAM_STUB], installEnv(n))
  eq("bad --name: exit", badName.exit, 1)
  eq("bad --name: writes nothing", existsSync(join(n.data, "t3-agents")), false)
})

// 12. The build directory stays out of the checkout, because this repository is also an
// installed Omarchy plugin and its validator rejects parts of a Cargo build directory.
phase("build directory", () => {
  const path = buildCargoPath()
  const repo = resolve(ROOT, "..")
  const outside = (target) => ok("build dir: outside the checkout", !target.startsWith(repo + "/"), target)

  const home = makeHome("build-cache")
  const built = spawn(NATIVE_INSTALLER, [], installEnv(home, path))
  eq("build dir: exit", built.exit, 0)
  const fallback = read(join(home.record, "cargo-target-dir.txt"))?.trim() ?? ""
  eq("build dir: default target", fallback, join(home.home, ".cache", "t3-agents", "target"))
  outside(fallback)
  eq("build dir: cargo argv", calls(home, "cargo"), [["build", "--release", "--locked", "-p", "t3-agents-term"]])
  eq("build dir: installed the built binary", read(join(home.data, "t3-agents", "t3-agents-term")), "#!/bin/sh\nexit 0\n")

  const cacheHome = makeHome("build-xdg-cache")
  const cache = join(cacheHome.base, "cache")
  const xdg = spawn(NATIVE_INSTALLER, [], { ...installEnv(cacheHome, path), XDG_CACHE_HOME: cache })
  eq("XDG cache: exit", xdg.exit, 0)
  eq("XDG cache: target", read(join(cacheHome.record, "cargo-target-dir.txt"))?.trim() ?? "", join(cache, "t3-agents", "target"))

  const presetHome = makeHome("build-preset")
  const preset = join(presetHome.base, "preset-target")
  const presetRun = spawn(NATIVE_INSTALLER, [], { ...installEnv(presetHome, path), CARGO_TARGET_DIR: preset })
  eq("preset target dir: exit", presetRun.exit, 0)
  eq("preset target dir: used as given", read(join(presetHome.record, "cargo-target-dir.txt"))?.trim() ?? "", preset)
})

// 13. A stand-in HOME does not move XDG_DATA_HOME or XDG_CACHE_HOME, and desktop sessions export
// both. An install run by hand with only HOME replaced once wrote into a real install, so the
// installer now refuses that combination. These cases stay harmless even if the refusal were
// broken: the name is a probe that no real install uses, and the XDG folders are fresh, unique
// folders (inside the account's home, as the rule requires) that this removes afterwards.
phase("stand-in HOME", () => {
  const probe = join(ACCOUNT_HOME, ".cache", `t3-agents-gate-probe-${process.pid}`)
  const name = `t3-agents-gate-probe-${process.pid}`
  if (existsSync(probe)) throw new Error("the probe folder already exists; not touching it")
  try {
    const refused = [
      ["data folder", ["--name", name, "--binary", PROGRAM_STUB], { XDG_DATA_HOME: join(probe, "share") }, "XDG_DATA_HOME"],
      ["cache folder", ["--name", name, "--binary", PROGRAM_STUB], { XDG_CACHE_HOME: join(probe, "cache") }, "XDG_CACHE_HOME"],
      ["uninstall", ["--name", name, "--uninstall"], { XDG_DATA_HOME: join(probe, "share") }, "XDG_DATA_HOME"],
    ]
    for (const [label, args, extra, variable] of refused) {
      const h = makeHome(`stand-in-${label.replace(/ /g, "-")}`)
      const r = spawn(NATIVE_INSTALLER, args, { ...installEnv(h), ...extra })
      eq(`stand-in HOME, ${label} in the account's home: exit`, r.exit, 1)
      ok(`stand-in HOME, ${label}: names the variable`,
        r.stderr.includes(variable) && r.stderr.includes("Nothing was changed"), r.stderr)
      eq(`stand-in HOME, ${label}: nothing written in the account's home`, existsSync(probe), false)
      eq(`stand-in HOME, ${label}: nothing written in the stand-in home`, entries(h.home), [])
    }

    // Allowed: a folder under the stand-in HOME, and one nowhere near the account's home.
    const under = makeHome("stand-in-under")
    const underRun = spawn(NATIVE_INSTALLER, ["--name", name, "--binary", PROGRAM_STUB],
      { ...installEnv(under), XDG_DATA_HOME: join(under.home, "data") })
    eq("stand-in HOME, data folder under it: exit", underRun.exit, 0)
    eq("stand-in HOME, data folder under it: installed there",
      existsSync(join(under.home, "data", name, "t3-agents-term")), true)

    // When the temporary folder itself is inside the account's home, "elsewhere" is too.
    const tmpInAccount = TMP === ACCOUNT_HOME || TMP.startsWith(ACCOUNT_HOME + "/")
    const apart = makeHome("stand-in-apart")
    const apartRun = spawn(NATIVE_INSTALLER, ["--name", name, "--binary", PROGRAM_STUB],
      { ...installEnv(apart), XDG_DATA_HOME: join(apart.base, "elsewhere") })
    eq("stand-in HOME, data folder outside the account's home: exit", apartRun.exit, tmpInAccount ? 1 : 0)
    eq("stand-in HOME, data folder outside the account's home: installed there",
      existsSync(join(apart.base, "elsewhere", name, "t3-agents-term")), !tmpInAccount)
  } finally {
    rmSync(probe, { recursive: true, force: true })
  }
})

// The real install, checked last. If this fires and nobody installed or updated T3 Agents on
// this machine while the checks ran, a case wrote outside its temporary home.
WATCHED.forEach(([label, path], i) => {
  ok(`the real install is as it was: ${label}`, identity(path) === WATCHED_BEFORE[i],
    `${label} changed while the checks ran`)
})

// ---------------------------------------------------------------- summary

for (const item of differences) {
  process.stderr.write(`DIFF ${item.case}\n`)
  for (const line of item.differences) process.stderr.write(`  ${line}\n`)
}
for (const error of errors) process.stderr.write(`ERROR ${error}\n`)

const summary = {
  cases,
  differences: differences.length,
  errors: errors.length,
  deliberateDifferences: DELIBERATE,
}
process.stdout.write(`${JSON.stringify(summary)}\n`)

if (differences.length === 0 && errors.length === 0) {
  rmSync(TMP, { recursive: true, force: true })
  process.exit(0)
}
process.stderr.write(`temp trees kept at ${TMP}\n`)
process.stderr.write("To rerun a case by hand, start from an empty environment (env -i HOME=... PATH=...). Replacing only HOME is not enough: XDG_DATA_HOME and XDG_CACHE_HOME are exported on most desktops and would aim the installer at your real install.\n")
process.exit(1)
