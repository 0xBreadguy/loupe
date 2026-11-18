// Differential gate: what a real terminal shows, and what it is left in.
//
// A private tmux server (never the default one) runs the original dashboard and
// t3-agents-term at the same size, on the same synthetic input, with the same
// keystrokes, SGR mouse bytes and resize. After each step both panes are captured
// with `tmux capture-pane -p -e -N` and compared cell by cell, including colour
// and bold. Exit checks cover q, C-c, SIGTERM and SIGHUP: alternate screen, cursor,
// mouse, keyboard modes and stty.
//
// Time is pinned with T3_FIXED_NOW (fixed-clock.mjs on the Node side, wall_now on
// the Rust side) and the presenter holds its demo elapsed clock at zero, so a
// calm screen can stop changing. Nothing here reads ~/.t3. HOME, T3CODE_HOME and
// XDG_RUNTIME_DIR are temporary directories, and xdg-open is a stub so Enter
// cannot raise a real window.
import { spawnSync } from "node:child_process"
import { chmodSync, cpSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { cellWidth } from "../../oracle/src/format.ts"
import { Demo } from "../../oracle/src/demo.ts"
import { enabled as recording, recordAsset, recordCase } from "./record.ts"
import { fnv1aText } from "./golden.ts"
import { CLUSTERS } from "./cluster-titles.ts"
import { DatabaseSync } from "node:sqlite"

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const REPO = resolve(ROOT, "..")
const termBin = resolve(process.env.T3_NATIVE_BINARY ?? join(ROOT, "..", "target/release/t3-agents-term"))
const mainTs = join(REPO, "oracle/src/main.ts")
const fixedClock = pathToFileURL(join(ROOT, "scripts/fixed-clock.mjs")).href
const NOW = 1_794_484_800_000 // 2026-11-12T12:00:00Z, the same instant cli-parity pins
const DAY = 86_400_000
const DB_AT = NOW - 400 * DAY // old enough that ages are in days, so they do not tick

const sock = `t3term-parity-${process.pid}`
const work = mkdtempSync(join(tmpdir(), "t3-terminal-parity-"))
const home = join(work, "home")
const t3home = join(work, "t3home")
const runtime = join(work, "run")
const stub = join(work, "bin")
const dbDir = join(work, "db")
mkdirSync(home, { recursive: true })
mkdirSync(t3home, { recursive: true })
mkdirSync(runtime, { recursive: true })
mkdirSync(stub, { recursive: true })
mkdirSync(dbDir, { recursive: true })
writeFileSync(join(stub, "xdg-open"), "#!/bin/sh\nexit 0\n")
chmodSync(join(stub, "xdg-open"), 0o755)

const demo = new Demo({ now: DB_AT, refreshMs: 0 })
copyFileSync(demo.path, join(dbDir, "statev2.sqlite"))
copyFileSync(join(demo.dir, "environment-id"), join(dbDir, "environment-id"))
demo.stop()
const dbPath = join(dbDir, "statev2.sqlite")
// Recording (T3_RECORD): the synthetic database the `db` cases read, byte for byte, so the Rust
// test can write the same file without Node. A no-op otherwise.
if (recording) {
  recordCase({
    kind: "database",
    sqlite: readFileSync(dbPath).toString("base64"),
    environmentId: readFileSync(join(dbDir, "environment-id"), "utf8"),
  })
}

type Cell = { ch: string; fg: string; bg: string; bold: boolean; extra: string }
type Grid = Cell[][]

let cases = 0
const differences: string[] = []
const errors: string[] = []
const notes: string[] = []
// Differences that are decided, each asserted to be exactly what is written here.
const deliberate: string[] = []

function quote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`
}

function sleep(ms: number) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms)
}

function tmux(args: string[], allowFail = false): Buffer {
  const result = spawnSync("tmux", ["-L", sock, "-f", "/dev/null", ...args], {
    timeout: 15_000,
  })
  if (result.error) throw result.error
  if ((result.status ?? 1) !== 0 && !allowFail) {
    throw new Error(`tmux ${args.join(" ")}: ${result.stderr.toString() || result.stdout.toString()}`)
  }
  return result.stdout ?? Buffer.alloc(0)
}

function serverUp() {
  // A tmux server with no session exits immediately, and a later new-session
  // starts a fresh server that has forgotten every option. Hold it with
  // exit-empty off before the boot session goes, so status, mouse and
  // default-terminal stay in force for the whole run.
  tmux(["new-session", "-d", "-s", "boot", "-x", "80", "-y", "24", "sleep", "30"])
  tmux(["set-option", "-s", "exit-empty", "off"])
  tmux(["set-option", "-g", "status", "off"])
  tmux(["set-option", "-g", "mouse", "off"])
  tmux(["set-option", "-g", "escape-time", "0"])
  tmux(["set-option", "-g", "focus-events", "off"])
  tmux(["set-option", "-g", "remain-on-exit", "on"])
  tmux(["set-option", "-g", "history-limit", "200"])
  tmux(["set-option", "-g", "default-terminal", "tmux-256color"])
  tmux(["kill-session", "-t", "boot"])
}

function writeScript(path: string, body: string) {
  writeFileSync(path, body, { mode: 0o755 })
}

function baseExports(term: string, colorterm: string | undefined, now: number): string[] {
  const lines = [
    `export HOME=${quote(home)}`,
    `export T3CODE_HOME=${quote(t3home)}`,
    `export XDG_RUNTIME_DIR=${quote(runtime)}`,
    `export TZ=UTC`,
    `export LANG=C.UTF-8`,
    `export PATH=${quote(`${stub}:${process.env.PATH ?? "/usr/bin"}`)}`,
    `export T3_FIXED_NOW=${quote(String(now))}`,
    // The original's theme behavior (the live Omarchy theme, else its gray fallback). The product's
    // themes are a deliberate difference, tested in crates/term/tests/pty.rs.
    `export T3_AGENTS_NO_THEMES=1`,
    `export TERM=${quote(term)}`,
    "unset HYPRLAND_INSTANCE_SIGNATURE NO_COLOR FORCE_COLOR COLORTERM",
  ]
  if (colorterm) lines.push(`export COLORTERM=${quote(colorterm)}`)
  return lines
}

function launch(session: string, cols: number, rows: number, term: string, colorterm: string | undefined, args: string[], side: "node" | "rust") {
  const script = join(work, `run-${session}.sh`)
  const argv = side === "node"
    ? [process.execPath, "--experimental-ffi", "--disable-warning=ExperimentalWarning", "--import", fixedClock, mainTs, ...args]
    : [termBin, ...args]
  const body = [
    "#!/bin/bash",
    "cd " + quote(REPO),
    ...baseExports(term, colorterm, NOW),
    "exec " + argv.map(quote).join(" "),
    "",
  ].join("\n")
  writeScript(script, body)
  tmux(["new-session", "-d", "-s", session, "-x", String(cols), "-y", String(rows), script])
  const size = tmux(["display-message", "-p", "-t", session, "#{pane_width}x#{pane_height}"]).toString().trim()
  if (size !== `${cols}x${rows}`) throw new Error(`${session} is ${size}, wanted ${cols}x${rows}`)
}

// Bracketed paste, as a terminal sends it. `paste:<hex>[*<n>]` is the text (repeated n times) given to
// tmux's `paste-buffer -p`, which wraps it in ESC[200~ and ESC[201~ only when the program in the pane
// has switched mode 2004 on, so it also checks that it did. `pieces:<hex>|<ms>|<hex>...` writes each
// piece raw with `send-keys -H` and waits the milliseconds between them: a paste in several reads, a
// marker cut in two.
function pasteText(spec: string): string {
  const [hex, times] = spec.split("*")
  return Buffer.from(hex!, "hex").toString("latin1").repeat(Number(times ?? 1))
}
const hexOf = (text: string) => Buffer.from(text, "latin1").toString("hex")

function sendKey(session: string, key: string) {
  if (key.startsWith("paste:")) {
    // tmux takes a command no longer than a few KB, so the text goes in through a file.
    const file = join(work, "paste.txt")
    writeFileSync(file, Buffer.from(pasteText(key.slice(6)), "latin1"))
    tmux(["load-buffer", "-b", "t3paste", file])
    tmux(["paste-buffer", "-p", "-d", "-b", "t3paste", "-t", session])
    return
  }
  if (key.startsWith("pieces:")) {
    for (const piece of key.slice(7).split("|")) {
      if (/^\d+ms$/.test(piece)) sleep(Number(piece.slice(0, -2)))
      else tmux(["send-keys", "-H", "-t", session, ...piece.match(/../g)!])
    }
    return
  }
  if (key.startsWith("hex:")) {
    tmux(["send-keys", "-H", "-t", session, ...key.slice(4).split(" ")])
    return
  }
  if (key.length === 1) tmux(["send-keys", "-l", "-t", session, key])
  else tmux(["send-keys", "-t", session, key])
}

function sgr(button: number, x: number, y: number): string {
  const bytes = Buffer.from(`\u001b[<${button};${x};${y}M`)
  return "hex:" + [...bytes].map((b) => b.toString(16).padStart(2, "0")).join(" ")
}

function capture(session: string): Buffer {
  return tmux(["capture-pane", "-p", "-e", "-N", "-t", session])
}

function parseGrid(raw: Buffer, cols: number, rows: number): { grid: Grid; problems: string[] } {
  const problems: string[] = []
  const grid: Grid = []
  let row: Cell[] = []
  let fg = "default"
  let bg = "default"
  let bold = false
  let extra = ""
  const style = () => ({ fg, bg, bold, extra })
  let i = 0
  const place = (ch: string) => {
    const width = Math.max(0, cellWidth(ch.codePointAt(0)!))
    if (width === 0) return
    row.push({ ch, ...style() })
    for (let k = 1; k < width; k++) row.push({ ch: "\u0000", ...style() })
  }
  while (i < raw.length) {
    const b = raw[i]
    if (b === 0x1b && raw[i + 1] === 0x5b) {
      let j = i + 2
      while (j < raw.length && raw[j]! < 0x40) j++
      if (j >= raw.length) {
        problems.push("truncated escape")
        break
      }
      const final = raw[j]!
      const body = raw.subarray(i + 2, j).toString("ascii")
      i = j + 1
      if (final === 0x6d) {
        const nums = (body.length ? body.split(";") : ["0"]).map((part) => (part === "" ? 0 : Number(part)))
        let k = 0
        const extras = new Set(extra ? extra.split(",") : [])
        while (k < nums.length) {
          const n = nums[k]!
          if (n === 0) {
            fg = "default"
            bg = "default"
            bold = false
            extras.clear()
          } else if (n === 1) bold = true
          else if (n === 22) bold = false
          else if (n === 39) fg = "default"
          else if (n === 49) bg = "default"
          else if (n >= 30 && n <= 37) fg = `ansi(${n - 30})`
          else if (n >= 40 && n <= 47) bg = `ansi(${n - 40})`
          else if (n >= 90 && n <= 97) fg = `ansi(${n - 90 + 8})`
          else if (n >= 100 && n <= 107) bg = `ansi(${n - 100 + 8})`
          else if (n === 2 || n === 3 || n === 4 || n === 5 || n === 7 || n === 9) extras.add(String(n))
          else if (n === 23 || n === 24 || n === 25 || n === 27 || n === 29) extras.delete(String(n - 20))
          else if ((n === 38 || n === 48) && nums[k + 1] === 5) {
            const value = `idx(${nums[k + 2]})`
            if (n === 38) fg = value
            else bg = value
            k += 2
          } else if ((n === 38 || n === 48) && nums[k + 1] === 2) {
            const value = `rgb(${nums[k + 2]},${nums[k + 3]},${nums[k + 4]})`
            if (n === 38) fg = value
            else bg = value
            k += 4
          }
          k++
        }
        extra = [...extras].sort().join(",")
      }
      continue
    }
    let ch: string
    if (b < 0x80) {
      ch = String.fromCharCode(b)
      i += 1
    } else if (b < 0xe0) {
      ch = raw.subarray(i, i + 2).toString("utf8")
      i += 2
    } else if (b < 0xf0) {
      ch = raw.subarray(i, i + 3).toString("utf8")
      i += 3
    } else {
      ch = raw.subarray(i, i + 4).toString("utf8")
      i += 4
    }
    if (ch === "\n") {
      grid.push(row)
      row = []
      continue
    }
    place(ch)
  }
  if (row.length) grid.push(row)
  if (grid.length !== rows) problems.push(`rows ${grid.length} != ${rows}`)
  grid.forEach((line, y) => {
    if (line.length !== cols) problems.push(`row ${y} width ${line.length} != ${cols}`)
  })
  return { grid, problems }
}

function signature(raw: Buffer, cols: number, rows: number): string {
  const grid = parseGrid(raw, cols, rows).grid
  const line = (y: number) => (grid[y] ?? []).map((cell) => (cell.ch === "\u0000" ? "" : cell.ch)).join("").trimEnd()
  return [0, 7, 8, 16, 18].map((y) => `${y}:${line(y)}`).join(" || ")
}

function visible(raw: Buffer): string {
  return raw.toString("utf8").replace(/\u001b\[[0-9;]*m/g, "")
}

function waitStable(session: string, cols: number, rows: number, timeoutMs: number): Buffer {
  const start = Date.now()
  let previous: Buffer | null = null
  let last = Buffer.alloc(0)
  while (Date.now() - start < timeoutMs) {
    last = capture(session)
    const text = visible(last)
    const ready = text.includes("T3 Agents") || text.includes("help") || text.includes("Calm")
    if (previous && previous.equals(last) && last.length > 0 && ready) return last
    previous = last
    sleep(80)
  }
  const dead = tmux(["display-message", "-p", "-t", session, "#{pane_dead}"], true).toString().trim()
  throw new Error(`${session} did not settle in ${timeoutMs}ms (dead=${dead}, ${cols}x${rows})\n${visible(last).slice(0, 500)}`)
}

// Calm mode paints at 2 fps, so two captures 80ms apart can both still be the
// frame from before the key. A shrink also flashes the lower halves of the
// old block characters (`╚═════`) for a few hundred milliseconds before the
// real paint; that leftover can sit still across two samples. A changed
// frame is accepted only after it has held for one idle frame, and a no-op
// only after that same idle frame has passed. Both panes are polled together.
function afterBoth(beforeNode: Buffer, beforeRust: Buffer, cols: number, rows: number): [Buffer, Buffer] {
  const start = Date.now()
  const holdMs = 600
  const deadline = start + 5_000
  let prevNode: Buffer | null = null
  let prevRust: Buffer | null = null
  let heldNode: number | null = null
  let heldRust: number | null = null
  let lastNode = beforeNode
  let lastRust = beforeRust
  let doneNode: Buffer | null = null
  let doneRust: Buffer | null = null
  const consider = (last: Buffer, before: Buffer, prev: Buffer | null, held: number | null): { done: Buffer | null; held: number | null } => {
    const fits = parseGrid(last, cols, rows).problems.length === 0
    const sameAsPrev = prev !== null && prev.equals(last) && last.length > 0
    if (!fits || !sameAsPrev) return { done: null, held: null }
    const since = held ?? Date.now()
    const waited = Date.now() - since
    const changed = !last.equals(before)
    if (changed && waited >= holdMs) return { done: last, held: since }
    if (!changed && Date.now() - start >= 800 && waited >= 160) return { done: last, held: since }
    return { done: null, held: since }
  }
  while (Date.now() < deadline && (doneNode === null || doneRust === null)) {
    if (doneNode === null) lastNode = capture("node")
    if (doneRust === null) lastRust = capture("rust")
    if (doneNode === null) {
      const next = consider(lastNode, beforeNode, prevNode, heldNode)
      doneNode = next.done
      heldNode = next.held
      prevNode = lastNode
    }
    if (doneRust === null) {
      const next = consider(lastRust, beforeRust, prevRust, heldRust)
      doneRust = next.done
      heldRust = next.held
      prevRust = lastRust
    }
    if (doneNode === null || doneRust === null) sleep(80)
  }
  return [doneNode ?? lastNode, doneRust ?? lastRust]
}

function same(a: Cell | undefined, b: Cell | undefined): boolean {
  if (!a || !b) return false
  return a.ch === b.ch && a.fg === b.fg && a.bg === b.bg && a.bold === b.bold && a.extra === b.extra
}

function describe(cell: Cell | undefined): string {
  if (!cell) return "missing"
  const glyph = cell.ch === "\u0000" ? "CONT" : JSON.stringify(cell.ch)
  return `${glyph} fg=${cell.fg} bg=${cell.bg} bold=${cell.bold}${cell.extra ? " attr=" + cell.extra : ""}`
}

function compareScreens(label: string, nodeRaw: Buffer, rustRaw: Buffer, cols: number, rows: number) {
  cases++
  const node = parseGrid(nodeRaw, cols, rows)
  const rust = parseGrid(rustRaw, cols, rows)
  const problems: string[] = []
  if (node.problems.length) problems.push(`node capture: ${node.problems.join("; ")}`)
  if (rust.problems.length) problems.push(`rust capture: ${rust.problems.join("; ")}`)
  let count = 0
  const samples: string[] = []
  const height = Math.max(node.grid.length, rust.grid.length)
  for (let y = 0; y < height; y++) {
    const width = Math.max(node.grid[y]?.length ?? 0, rust.grid[y]?.length ?? 0)
    for (let x = 0; x < width; x++) {
      if (!same(node.grid[y]?.[x], rust.grid[y]?.[x])) {
        count++
        if (samples.length < 6) samples.push(`(${x},${y}) node ${describe(node.grid[y]?.[x])} rust ${describe(rust.grid[y]?.[x])}`)
      }
    }
  }
  if (count) {
    const rows: string[] = []
    for (let y = 0; y < height && rows.length < 8; y++) {
      const width = Math.max(node.grid[y]?.length ?? 0, rust.grid[y]?.length ?? 0)
      let rowDiff = false
      for (let x = 0; x < width; x++) if (!same(node.grid[y]?.[x], rust.grid[y]?.[x])) rowDiff = true
      if (!rowDiff) continue
      const text = (grid: Grid) => (grid[y] ?? []).map((cell) => (cell.ch === "\u0000" ? "" : cell.ch)).join("")
      rows.push(`row ${y} node ${JSON.stringify(text(node.grid))} rust ${JSON.stringify(text(rust.grid))}`)
    }
    problems.push(`${count} cells differ: ${samples.join(" | ")}; ${rows.join("; ")}`)
  }
  if (problems.length) differences.push(`${label}: ${problems.join("; ")}`)
}

/** The digest of a parsed grid, so a replay can prove its parser reads a capture as this one did. */
function gridDigest(grid: Grid): string {
  const code = (cell: Cell) => `${cell.ch === "\u0000" ? 0 : cell.ch.codePointAt(0)}:${cell.fg}:${cell.bg}:${cell.bold ? 1 : 0}:${cell.extra}`
  return fnv1aText(grid.map((row) => row.map(code).join(" ")).join("\n"))
}

/**
 * Recording (T3_RECORD): what the original showed after one step of a screen case. The capture
 * is kept verbatim (content-addressed, so identical screens cost once) together with the key
 * that was sent, so the replay needs no port of the step list.
 */
function recordScreen(scenario: Record<string, unknown>, step: string, key: string | null, resize: [number, number] | null, raw: Buffer, cols: number, rows: number) {
  if (!recording) return
  const file = join(work, "recorded-capture.txt")
  writeFileSync(file, raw)
  recordCase({
    kind: "screen",
    ...scenario,
    step,
    key,
    resize,
    cols,
    rows,
    capture: recordAsset(file, "text"),
    cells: gridDigest(parseGrid(raw, cols, rows).grid),
  })
}

function killSessions() {
  tmux(["kill-session", "-t", "node"], true)
  tmux(["kill-session", "-t", "rust"], true)
}

const stepsFor = (cols: number, rows: number): { name: string; key: string; resize?: [number, number] }[] => {
  const clickY = Math.min(10, rows - 2)
  const detailY = Math.max(2, rows - 3)
  const next: [number, number] = cols < 96 ? [110, 30] : cols >= 200 ? [180, 48] : [cols + 16, rows + 6]
  return [
    { name: "j", key: "j" },
    { name: "k", key: "k" },
    { name: "down", key: "Down" },
    { name: "up", key: "Up" },
    { name: "left", key: "Left" },
    { name: "right", key: "Right" },
    { name: "home", key: "Home" },
    { name: "end", key: "End" },
    { name: "g", key: "g" },
    { name: "G", key: "G" },
    { name: "pageup", key: "PageUp" },
    { name: "pagedown", key: "PageDown" },
    { name: "J", key: "J" },
    { name: "K", key: "K" },
    { name: "space", key: " " },
    { name: "n", key: "n" },
    { name: "a", key: "a" },
    { name: "t", key: "t" },
    { name: "f", key: "f" },
    { name: "p", key: "p" },
    { name: "r", key: "r" },
    { name: "x", key: "x" },
    { name: "help-h", key: "h" },
    { name: "help-j", key: "j" },
    { name: "help-wheel", key: sgr(65, 8, clickY) },
    { name: "escape", key: "Escape" },
    { name: "help-?", key: "?" },
    { name: "escape-2", key: "Escape" },
    { name: "click", key: sgr(0, 8, clickY) },
    { name: "wheel-down", key: sgr(65, 8, clickY) },
    { name: "wheel-up", key: sgr(64, 8, clickY) },
    { name: "wheel-detail", key: sgr(65, 8, detailY) },
    { name: "enter", key: "Enter" },
    { name: "o", key: "o" },
    { name: "m-off", key: "m" },
    { name: "m-on", key: "m" },
    { name: "resize", key: "", resize: next },
  ]
}

// ── Bracketed paste ──────────────────────────────────────────────────────────
// Both programs switch mode 2004 on, so tmux (which wraps a paste in markers only for a program that
// asked) sends them `ESC[200~ text ESC[201~`, and nothing the app shows or does reacts to it. Each paste
// is followed by a `j`, so that state the paste may have changed without showing it comes out on the
// next screen. The plan is replayed from the recording by golden_terminal.rs.
const PASTE_START = "\u001b[200~"
const PASTE_END = "\u001b[201~"
const PASTE_PLAN: { name: string; key: string; changes: boolean }[] = [
  // A paste of keys the app uses: `q` quits it, `?` opens help, `j` moves the selection.
  { name: "paste-q", key: "paste:" + hexOf("qqqq"), changes: false },
  { name: "paste-keys", key: "paste:" + hexOf("jjjkk?xxnapt fhG\u0020"), changes: false },
  // Newlines: tmux sends them as Enter (CR), which opens the selected agent's details when typed.
  { name: "paste-multiline", key: "paste:" + hexOf("q\nq\nj\nj\n\n"), changes: false },
  { name: "j-after-pastes", key: "j", changes: true },
  // 100 KB, in one command and then in however many reads the terminal makes of it.
  { name: "paste-long", key: "paste:" + hexOf("jq? \n") + "*20000", changes: false },
  { name: "j-after-long", key: "j", changes: true },
  // Escape sequences and a Ctrl-C inside a paste, written raw as one piece.
  { name: "paste-escapes", key: "pieces:" + hexOf(PASTE_START + "\u001b[A\u001b[B\u001b[<0;12;4M\u001b[<65;3;8M\u001b[?62;4c\u001b[1;2R\u001b[I\u001bq\u0003" + PASTE_END), changes: false },
  { name: "j-after-escapes", key: "j", changes: true },
  // A paste in three reads, the end marker cut in two, 300 ms apart.
  { name: "paste-pieces", key: "pieces:" + [hexOf(PASTE_START + "qq"), "300ms", hexOf("qq" + PASTE_END.slice(0, 3)), "300ms", hexOf(PASTE_END.slice(3))].join("|"), changes: false },
  { name: "j-after-pieces", key: "j", changes: true },
  // Keys typed right after a paste are keys.
  { name: "paste-then-key", key: "pieces:" + hexOf(PASTE_START + "qq" + PASTE_END + "j"), changes: true },
]

function screenCase(source: "demo" | "db", cols: number, rows: number, term: string, colorterm: string | undefined, plan?: { prefix: string; steps: { name: string; key: string; changes: boolean }[] }) {
  const color = colorterm ?? "no-colorterm"
  const label = `${plan?.prefix ?? ""}${source} ${cols}x${rows} ${term} ${color}`
  const args = ["--calm", "--interval", "30", ...(source === "demo" ? ["--demo"] : ["--db", dbPath])]
  console.error(`running ${label}`)
  try {
    // Key bytes come from tmux's terminfo, so the pane terminal has to be the
    // same TERM the application is told it is running in.
    tmux(["set-option", "-g", "default-terminal", term])
    launch("node", cols, rows, term, colorterm, args, "node")
    launch("rust", cols, rows, term, colorterm, args, "rust")
    let nodeRaw = waitStable("node", cols, rows, 15_000)
    let rustRaw = waitStable("rust", cols, rows, 15_000)
    // One more pair, taken back to back, once both have stopped on their own.
    nodeRaw = waitStable("node", cols, rows, 4_000)
    rustRaw = waitStable("rust", cols, rows, 4_000)
    compareScreens(`${label} initial`, nodeRaw, rustRaw, cols, rows)
    const scenario = { scenario: label, source, cols: cols, rows: rows, term, colorterm: colorterm ?? null }
    recordScreen(scenario, "initial", null, null, nodeRaw, cols, rows)
    if (process.env.T3_PARITY_TRACE) console.error(`trace initial\n  node ${signature(nodeRaw, cols, rows)}\n  rust ${signature(rustRaw, cols, rows)}`)
    let curCols = cols
    let curRows = rows
    const stepLimit = process.env.T3_PARITY_STEPS ? Number(process.env.T3_PARITY_STEPS) : Number.POSITIVE_INFINITY
    let stepCount = 0
    for (const step of (plan?.steps ?? stepsFor(cols, rows)) as { name: string; key: string; resize?: [number, number] }[]) {
      if (Number.isFinite(stepLimit) && stepCount >= stepLimit) break
      stepCount++
      const nodeBefore = nodeRaw
      const rustBefore = rustRaw
      if (step.resize) {
        curCols = step.resize[0]
        curRows = step.resize[1]
        tmux(["resize-window", "-t", "node", "-x", String(curCols), "-y", String(curRows)])
        tmux(["resize-window", "-t", "rust", "-x", String(curCols), "-y", String(curRows)])
      } else {
        sendKey("node", step.key)
        sendKey("rust", step.key)
      }
      ;[nodeRaw, rustRaw] = afterBoth(nodeBefore, rustBefore, curCols, curRows)
      compareScreens(`${label} ${step.name}`, nodeRaw, rustRaw, curCols, curRows)
      // The paste plan must be able to fail: the original must leave the screen alone for a paste and
      // move it for the key that follows, or there is nothing here for Rust to match.
      const changes = (step as { changes?: boolean }).changes
      if (changes !== undefined && nodeRaw.equals(nodeBefore) === changes) {
        differences.push(`${label} ${step.name}: the original's screen ${changes ? "did not change" : "changed"}; the check has nothing to compare`)
      }
      recordScreen(scenario, step.name, step.resize ? null : step.key, step.resize ?? null, nodeRaw, curCols, curRows)
      if (process.env.T3_PARITY_TRACE) console.error(`trace ${step.name}\n  node ${signature(nodeRaw, curCols, curRows)}\n  rust ${signature(rustRaw, curCols, curRows)}`)
    }
  } catch (err) {
    errors.push(`${label}: ${err instanceof Error ? err.message : err}`)
  } finally {
    killSessions()
  }
}

// Modes the two programs are expected to put back. Defaults are a cooked tmux pane.
function finalModes(raw: Buffer): Record<string, string> {
  const state: Record<string, boolean> = {
    "1": false,
    "7": true,
    "25": true,
    "1000": false,
    "1002": false,
    "1003": false,
    "1004": false,
    "1006": false,
    "1049": false,
    "2004": false,
    "2026": false,
    "2027": false,
    "2031": false,
  }
  let modify = "0"
  let kitty = "off"
  let i = 0
  while (i < raw.length) {
    if (raw[i] !== 0x1b) {
      i++
      continue
    }
    if (raw[i + 1] !== 0x5b) {
      i += 2
      continue
    }
    let j = i + 2
    const marker = raw[j]
    const priv = marker === 0x3f
    const gt = marker === 0x3e
    if (priv || gt) j++
    const start = j
    while (j < raw.length && raw[j]! >= 0x20 && raw[j]! <= 0x3f) j++
    if (j >= raw.length) break
    const final = raw[j]!
    const body = raw.subarray(start, j).toString("ascii")
    if (priv && (final === 0x68 || final === 0x6c)) {
      const on = final === 0x68
      for (const part of body.split(";")) if (/^\d+$/.test(part)) state[part] = on
    } else if (gt && final === 0x6d && body.startsWith("4;")) {
      modify = body.slice(2)
    } else if (gt && final === 0x75) {
      kitty = body || "0"
    }
    i = j + 1
  }
  const out: Record<string, string> = {}
  for (const key of Object.keys(state).sort()) out[key] = state[key] ? "on" : "off"
  out.kitty = kitty
  out.modifyOtherKeys = modify
  return out
}

function exitCase(how: "q" | "SIGTERM" | "SIGHUP" | "C-c" | "paste+q" | "paste+SIGTERM") {
  const label = `exit ${how}`
  const nodeDir = join(work, `exit-node-${how}`)
  const rustDir = join(work, `exit-rust-${how}`)
  mkdirSync(nodeDir, { recursive: true })
  mkdirSync(rustDir, { recursive: true })
  const scriptFor = (dir: string, side: "node" | "rust") => {
    const argv = side === "node"
      ? [process.execPath, "--experimental-ffi", "--disable-warning=ExperimentalWarning", "--import", fixedClock, mainTs, "--calm", "--interval", "30", "--demo"]
      : [termBin, "--calm", "--interval", "30", "--demo"]
    const path = join(dir, "run.sh")
    writeScript(path, [
      "#!/bin/bash",
      "cd " + quote(REPO),
      ...baseExports("tmux-256color", "truecolor", NOW),
      `stty -g > ${quote(join(dir, "stty-before"))}`,
      `stty -a > ${quote(join(dir, "stty-a-before"))}`,
      "(",
      `  echo $BASHPID > ${quote(join(dir, "pid"))}`,
      // The app waits here so pipe-pane is attached before the first query.
      `  while [ ! -f ${quote(join(dir, "go"))} ]; do sleep 0.02; done`,
      "  exec " + argv.map(quote).join(" "),
      ")",
      "status=$?",
      `echo $status > ${quote(join(dir, "exit"))}`,
      `stty -g > ${quote(join(dir, "stty-after"))}`,
      `stty -a > ${quote(join(dir, "stty-a-after"))}`,
      "echo DONE:$status",
      "sleep 30",
      "",
    ].join("\n"))
    return path
  }
  try {
    const nodeScript = scriptFor(nodeDir, "node")
    const rustScript = scriptFor(rustDir, "rust")
    tmux(["set-option", "-g", "default-terminal", "tmux-256color"])
    tmux(["new-session", "-d", "-s", "node", "-x", "80", "-y", "24", nodeScript])
    tmux(["new-session", "-d", "-s", "rust", "-x", "80", "-y", "24", rustScript])
    waitFile(join(nodeDir, "pid"), 5_000)
    waitFile(join(rustDir, "pid"), 5_000)
    tmux(["pipe-pane", "-t", "node", "-o", `cat > ${join(nodeDir, "raw.bin")}`])
    tmux(["pipe-pane", "-t", "rust", "-o", `cat > ${join(rustDir, "raw.bin")}`])
    writeFileSync(join(nodeDir, "go"), "1")
    writeFileSync(join(rustDir, "go"), "1")
    waitStable("node", 80, 24, 15_000)
    waitStable("rust", 80, 24, 15_000)
    // `paste+...`: a bracketed paste of quit keys first. Both programs must still be running after it,
    // and leave the same things switched on when the way out that follows is taken.
    const action = how.startsWith("paste+") ? how.slice(6) : how
    let survivedPaste: boolean | null = null
    if (action !== how) {
      sendKey("node", "paste:" + hexOf("qqqq q?jk"))
      sendKey("rust", "paste:" + hexOf("qqqq q?jk"))
      sleep(1500)
      survivedPaste = !visible(capture("node")).includes("DONE:") && !visible(capture("rust")).includes("DONE:")
    }
    if (action === "q") {
      sendKey("node", "q")
      sendKey("rust", "q")
    } else if (action === "C-c") {
      sendKey("node", "C-c")
      sendKey("rust", "C-c")
    } else {
      const signal = action === "SIGTERM" ? "SIGTERM" : "SIGHUP"
      const nodePid = pidOf(join(nodeDir, "pid"))
      const rustPid = pidOf(join(rustDir, "pid"))
      process.kill(nodePid, signal)
      process.kill(rustPid, signal)
    }
    const nodeExit = waitFile(join(nodeDir, "exit"), 8_000)
    const rustExit = waitFile(join(rustDir, "exit"), 8_000)
    cases++
    const problems: string[] = []
    if (survivedPaste === false) problems.push("a program quit on a bracketed paste of quit keys")
    if (survivedPaste !== null) {
      // Each must have asked for bracketed paste (tmux sends the markers only then) and given it back.
      for (const [side, buf] of [["node", readBuf(join(nodeDir, "raw.bin"))], ["rust", readBuf(join(rustDir, "raw.bin"))]] as const) {
        const text = buf.toString("latin1")
        if (!text.includes("\u001b[?2004h")) problems.push(`${side} never switched mode 2004 on`)
        else if (text.lastIndexOf("\u001b[?2004l") < text.lastIndexOf("\u001b[?2004h")) problems.push(`${side} did not switch mode 2004 off after the last time it switched it on`)
      }
    }
    if (nodeExit !== rustExit) problems.push(`exit code ${nodeExit} vs ${rustExit}`)
    const nodeStty = readText(join(nodeDir, "stty-after"))
    const rustStty = readText(join(rustDir, "stty-after"))
    const nodeBefore = readText(join(nodeDir, "stty-before"))
    const rustBefore = readText(join(rustDir, "stty-before"))
    if (nodeStty !== nodeBefore) problems.push("node stty was not restored")
    if (rustStty !== rustBefore) problems.push("rust stty was not restored")
    if (nodeStty !== rustStty) problems.push("stty -g differs after exit")
    const nodeA = readText(join(nodeDir, "stty-a-after"))
    const rustA = readText(join(rustDir, "stty-a-after"))
    if (nodeA !== rustA) problems.push("stty -a differs after exit")
    sleep(200)
    const nodeRawOut = readBuf(join(nodeDir, "raw.bin"))
    const rustRawOut = readBuf(join(rustDir, "raw.bin"))
    if (nodeRawOut.length < 16) problems.push(`node raw output is ${nodeRawOut.length} bytes`)
    if (rustRawOut.length < 16) problems.push(`rust raw output is ${rustRawOut.length} bytes`)
    const nodeModes = finalModes(nodeRawOut)
    const rustModes = finalModes(rustRawOut)
    const modeKeys = [...new Set([...Object.keys(nodeModes), ...Object.keys(rustModes)])].sort()
    // OpenTUI turns on mode 2027 (grapheme clustering) and leaves it on when it exits,
    // so the terminal is not handed back as it was found. The Rust presenter switches it
    // on only when the terminal says it can be, and puts it back as it found it; tmux
    // reports nothing for it, so here it is never touched (terminal-caps-parity.ts covers
    // a terminal that does). That one difference is deliberate and is counted, not hidden:
    // it must be exactly "original on, Rust off", and anything else still fails.
    const modeDiffs = modeKeys.filter((key) => {
      if (nodeModes[key] === rustModes[key]) return false
      if (key === "2027" && nodeModes[key] === "on" && rustModes[key] === "off") {
        deliberate.push(`${label}: the original leaves mode 2027 on; the Rust presenter leaves it as it found it`)
        return false
      }
      return true
    })
    if (modeDiffs.length) {
      problems.push("terminal modes differ: " + modeDiffs.map((key) => `${key} node=${nodeModes[key]} rust=${rustModes[key]}`).join(", "))
    }
    const wantOff = ["1000", "1002", "1003", "1004", "1006", "1049", "2004", "2026"]
    for (const key of wantOff) {
      if (nodeModes[key] === "on") problems.push(`node left mode ${key} on`)
      if (rustModes[key] === "on") problems.push(`rust left mode ${key} on`)
    }
    if (nodeModes["25"] !== "on") problems.push("node left the cursor hidden")
    if (rustModes["25"] !== "on") problems.push("rust left the cursor hidden")
    if (nodeModes.kitty !== "off" || rustModes.kitty !== "off") problems.push(`kitty keyboard node=${nodeModes.kitty} rust=${rustModes.kitty}`)
    if (nodeModes.modifyOtherKeys !== "0" || rustModes.modifyOtherKeys !== "0") {
      problems.push(`modifyOtherKeys node=${nodeModes.modifyOtherKeys} rust=${rustModes.modifyOtherKeys}`)
    }
    const nodePane = visible(capture("node"))
    const rustPane = visible(capture("rust"))
    if (recording) {
      // What the original leaves behind, as facts the Rust program is held to.
      recordCase({
        kind: "exit",
        how,
        exitCode: nodeExit,
        modes: nodeModes,
        sttyRestored: nodeStty === nodeBefore && nodeA === readText(join(nodeDir, "stty-a-before")),
        mainScreen: nodePane.includes("DONE:"),
        survivedPaste,
      })
    }
    if (!nodePane.includes("DONE:")) problems.push("node pane is not back on the main screen")
    if (!rustPane.includes("DONE:")) problems.push("rust pane is not back on the main screen")
    if (problems.length) differences.push(`${label}: ${problems.join("; ")}`)
    if (how === "q") {
      notes.push("startup queries are not a screen difference. node begins " + queryPreview(nodeRawOut) + "; rust begins " + queryPreview(rustRawOut))
    }
  } catch (err) {
    errors.push(`${label}: ${err instanceof Error ? err.message : err}`)
  } finally {
    killSessions()
  }
}

function pidOf(path: string): number {
  const text = readText(path).trim()
  const pid = Number(text)
  if (!Number.isInteger(pid) || pid <= 1) throw new Error(`no process id in ${path} (${JSON.stringify(text)})`)
  return pid
}

function readText(path: string): string {
  return readBuf(path).toString("utf8")
}

function readBuf(path: string): Buffer {
  return spawnSync("cat", [path]).stdout ?? Buffer.alloc(0)
}

function waitFile(path: string, timeoutMs: number): string {
  const start = Date.now()
  while (Date.now() - start < timeoutMs) {
    const buf = spawnSync("cat", [path]).stdout
    if (buf && buf.length) return buf.toString("utf8").trim()
    sleep(50)
  }
  throw new Error(`timed out waiting for ${path}`)
}

function queryPreview(raw: Buffer): string {
  const end = raw.indexOf(Buffer.from("\u001b[?1049h"))
  const head = (end > 0 ? raw.subarray(0, end) : raw.subarray(0, 180)).toString("utf8")
  const named = head.replace(/\u001b/g, "ESC").replace(/\u0007/g, "BEL").replace(/[^\x20-\x7e]/g, ".")
  return JSON.stringify(named.slice(0, 240))
}

function survival(label: string, cols: number, rows: number, term: string, inner = false) {
  const args = ["--calm", "--demo"]
  const observe = (side: "node" | "rust") => {
    const session = side
    const innerSock = `${sock}-${side}`
    try {
      if (!inner) {
        launch(session, cols, rows, term, "truecolor", args, side)
      } else {
        const argv = side === "node"
          ? [process.execPath, "--experimental-ffi", "--disable-warning=ExperimentalWarning", "--import", fixedClock, mainTs, ...args]
          : [termBin, ...args]
        const innerScript = join(work, `inner-${side}.sh`)
        writeScript(innerScript, ["#!/bin/bash", "cd " + quote(REPO), ...baseExports("tmux-256color", "truecolor", NOW), "exec " + argv.map(quote).join(" "), ""].join("\n"))
        const outer = join(work, `outer-${side}.sh`)
        writeScript(outer, ["#!/bin/bash", `exec tmux -L ${quote(innerSock)} -f /dev/null new-session -x ${cols - 4} -y ${rows - 2} ${quote(innerScript)}`, ""].join("\n"))
        tmux(["new-session", "-d", "-s", session, "-x", String(cols), "-y", String(rows), outer])
      }
      sleep(2500)
      const dead = tmux(["display-message", "-p", "-t", session, "#{pane_dead}"], true).toString().trim()
      if (dead === "1") {
        return `dead during startup: ${visible(capture(session)).slice(0, 240)}`
      }
      if (!inner) sendKey(session, "q")
      else {
        // q typed at the outer pane can stop at the inner client. Send it to
        // the application pane on the inner server.
        spawnSync("tmux", ["-L", innerSock, "-f", "/dev/null", "send-keys", "q"], { timeout: 5_000 })
      }
      sleep(1500)
      const after = tmux(["display-message", "-p", "-t", session, "#{pane_dead}"], true).toString().trim()
      const text = visible(tmux(["capture-pane", "-p", "-t", session], true)).slice(0, 240)
      if (inner) {
        const innerCmd = spawnSync("tmux", ["-L", innerSock, "-f", "/dev/null", "display-message", "-p", "#{pane_current_command}"], { timeout: 5_000 })
        const cmd = (innerCmd.stdout ?? Buffer.alloc(0)).toString().trim()
        if (innerCmd.status === 0 && (cmd === "node" || cmd.includes("t3-agents"))) {
          return `still running after q (inner ${cmd}): ${text}`
        }
      } else if (after !== "1" && !text.includes("DONE") && !text.toLowerCase().includes("exit")) {
        // The app may have exited back to a shell that is still alive. A second q
        // would hit the shell. Look for the process instead.
        const cmd = tmux(["display-message", "-p", "-t", session, "#{pane_current_command}"], true).toString().trim()
        if (cmd === "node" || cmd.includes("t3-agents")) return `still running after q (${cmd}): ${text}`
      }
      return `alive then quit (${text.replace(/\s+/g, " ").trim()})`
    } catch (err) {
      return `error ${err instanceof Error ? err.message : err}`
    } finally {
      tmux(["kill-session", "-t", session], true)
      if (inner) tmuxKill(`${sock}-${side}`)
    }
  }
  const node = observe("node")
  const rust = observe("rust")
  notes.push(`${label}: node ${node}; rust ${rust}`)
  const nodeBad = /dead|still running|error/.test(node)
  // The outcome text ends in the pane's own words and timestamp, so only its class is recorded.
  if (recording) recordCase({ kind: "survival", label, cols, rows, term, inner, bad: nodeBad, outcome: node.split(/[:(]/)[0]!.trim() })
  const rustBad = /dead|still running|error/.test(rust)
  if (nodeBad !== rustBad) differences.push(`${label}: node ${node}; rust ${rust}`)
}

function tmuxKill(name: string) {
  spawnSync("tmux", ["-L", name, "-f", "/dev/null", "kill-server"], { timeout: 5_000 })
}

function nonTTY() {
  const env = {
    ...process.env,
    HOME: home,
    T3CODE_HOME: t3home,
    XDG_RUNTIME_DIR: runtime,
    TZ: "UTC",
    T3_FIXED_NOW: String(NOW),
    T3_AGENTS_NO_THEMES: "1",
    PATH: `${stub}:${process.env.PATH ?? ""}`,
  }
  delete env.HYPRLAND_INSTANCE_SIGNATURE
  delete env.NO_COLOR
  delete env.FORCE_COLOR
  const run = (cmd: string, args: string[]) => spawnSync(cmd, args, {
    env, timeout: 20_000, cwd: REPO, encoding: "utf8",
  })
  const node = run(process.execPath, ["--experimental-ffi", "--disable-warning=ExperimentalWarning", "--import", fixedClock, mainTs, "--demo"])
  const rust = run(termBin, ["--demo"])
  cases++
  const problems: string[] = []
  if (node.status !== rust.status) problems.push(`exit ${node.status} vs ${rust.status}`)
  if ((node.stderr ?? "") !== (rust.stderr ?? "")) problems.push(`stderr ${JSON.stringify(node.stderr)} vs ${JSON.stringify(rust.stderr)}`)
  if ((node.stdout ?? "") !== (rust.stdout ?? "")) problems.push(`stdout ${JSON.stringify(node.stdout)} vs ${JSON.stringify(rust.stdout)}`)
  if (recording) recordCase({ kind: "not-a-terminal", status: node.status, stderr: node.stderr ?? "", stdout: node.stdout ?? "" })
  if (problems.length) differences.push(`not a terminal: ${problems.join("; ")}`)
  else notes.push(`not a terminal: both exit ${node.status} with ${JSON.stringify(node.stderr)}`)
}

// ── Clusters ──────────────────────────────────────────────────────────────────
// Thread titles made of grapheme clusters (cluster-titles.ts) in a real tmux pane. tmux 3.6 and
// later cluster graphemes; the original measures with its unicode widths there and moves the
// cursor after every cluster, and the Rust presenter must leave the same cells. Not recorded for
// the Rust-only replay: the recorded cases are the ones above.
// The captures are compared as tmux wrote them, line for line with every SGR: the cell parser
// above measures with the canvas's widths, which a cluster does not have in tmux.
const clusterDb = join(work, "clusters", "statev2.sqlite")
function compareRaw(label: string, nodeRaw: Buffer, rustRaw: Buffer) {
  cases++
  if (nodeRaw.equals(rustRaw)) return
  const a = nodeRaw.toString("utf8").split("\n")
  const b = rustRaw.toString("utf8").split("\n")
  const row = a.findIndex((line, i) => line !== b[i])
  // One kind of difference is not the programs' and cannot be seen. Where tmux measures a cluster
  // differently from the original's renderer (a lone Hangul vowel, an emoji modifier after a letter,
  // a doubled modifier), the blank cells to the right of it are not written again, so they keep
  // the attributes an earlier frame gave them, and the original's earliest frames vary from run to
  // run (seen in about one run in four). Both programs write the same bytes for the row. What
  // differs is the foreground color of cells that hold a space, which shows nothing. Accepted only
  // then: every differing row has the same text, and cell for cell the same background and
  // attributes, and the same foreground wherever the cell is not a space.
  const plain = (line: string | undefined) => (line ?? "").replace(/\u001b\[[0-9;:]*m/g, "")
  const rows = a.map((line, i) => i).filter((i) => a[i] !== b[i])
  const outside = rows.filter((i) => !onlyBlankForegrounds(a[i] ?? "", b[i] ?? ""))
  if (a.length === b.length && outside.length === 0) {
    notes.push(`open: ${label}: rows ${rows.join(", ")} differ only in the foreground color of blank cells, beside a cluster tmux measures differently from the renderer (see compareRaw)`)
    return
  }
  const why = outside.map((i) => `${i}: ${plain(a[i]) !== plain(b[i]) ? "text differs" : "a visible color or attribute differs"}`).join("; ")
  const shown = (outside.length ? outside : [row]).slice(0, 3).map((i) => `row ${i}: node ${JSON.stringify(a[i])} rust ${JSON.stringify(b[i])}`).join(" | ")
  differences.push(`${label}: ${a.length} and ${b.length} lines; rows ${rows.join(", ")} differ (${why || "line counts"}); ${shown}`)
}
/** One captured line as its code points, each with the colors and attributes in force. */
function styled(line: string): { ch: string; fg: string; bg: string; attrs: string }[] {
  const out: { ch: string; fg: string; bg: string; attrs: string }[] = []
  let fg = "", bg = ""
  const attrs = new Set<string>()
  const token = /\u001b\[([0-9;:]*)m|([\s\S])/gu
  for (const m of line.matchAll(token)) {
    if (m[2] !== undefined) {
      out.push({ ch: m[2], fg, bg, attrs: [...attrs].sort().join(",") })
      continue
    }
    const codes = (m[1] || "0").split(";")
    for (let k = 0; k < codes.length; k++) {
      const code = codes[k]!
      if (code === "0" || code === "") { fg = ""; bg = ""; attrs.clear() }
      else if (code === "38" || code === "48") {
        const take = codes[k + 1] === "2" ? 5 : codes[k + 1] === "5" ? 3 : 1
        const value = codes.slice(k, k + take).join(";")
        if (code === "38") fg = value
        else bg = value
        k += take - 1
      } else if (code === "39") fg = ""
      else if (code === "49") bg = ""
      else if (/^(3[0-7]|9[0-7])$/.test(code)) fg = code
      else if (/^(4[0-7]|10[0-7])$/.test(code)) bg = code
      else if (/^2[1-9]$/.test(code)) attrs.delete(String(Number(code) - 20 === 2 ? 1 : Number(code) - 20))
      else attrs.add(code)
    }
  }
  return out
}
/** Same text, same backgrounds and attributes, and the same foreground on every cell that shows one. */
function onlyBlankForegrounds(a: string, b: string): boolean {
  const x = styled(a), y = styled(b)
  if (x.length !== y.length) return false
  return x.every((cell, i) => {
    const other = y[i]!
    if (cell.ch !== other.ch || cell.bg !== other.bg) return false
    // Bold changes nothing on a space; underline, reverse and the rest would show.
    const visible = (attrs: string) => attrs.split(",").filter((c) => c && !(cell.ch === " " && c === "1")).join(",")
    if (visible(cell.attrs) !== visible(other.attrs)) return false
    return cell.fg === other.fg || cell.ch === " "
  })
}
function clusterCase(cols: number, rows: number) {
  const label = `clusters ${cols}x${rows} tmux-256color truecolor`
  mkdirSync(dirname(clusterDb), { recursive: true })
  copyFileSync(dbPath, clusterDb)
  copyFileSync(join(dbDir, "environment-id"), join(dirname(clusterDb), "environment-id"))
  const db = new DatabaseSync(clusterDb)
  const threads = db.prepare("select thread_id, payload_json from orchestration_v2_projection_threads order by thread_id").all() as { thread_id: string; payload_json: string }[]
  const update = db.prepare("update orchestration_v2_projection_threads set payload_json = ? where thread_id = ?")
  threads.slice(0, CLUSTERS.length).forEach((row, i) => update.run(JSON.stringify({ ...JSON.parse(row.payload_json), title: CLUSTERS[i] }), row.thread_id))
  db.close()
  const args = ["--calm", "--interval", "30", "--db", clusterDb]
  console.error(`running ${label}`)
  try {
    tmux(["set-option", "-g", "default-terminal", "tmux-256color"])
    launch("node", cols, rows, "tmux-256color", "truecolor", args, "node")
    launch("rust", cols, rows, "tmux-256color", "truecolor", args, "rust")
    let nodeRaw = waitStable("node", cols, rows, 15_000)
    let rustRaw = waitStable("rust", cols, rows, 15_000)
    nodeRaw = waitStable("node", cols, rows, 4_000)
    rustRaw = waitStable("rust", cols, rows, 4_000)
    compareRaw(`${label} initial`, nodeRaw, rustRaw)
    for (const key of ["j", "j", "j", "j", "j", "j", "j", "?", "?", "j", "j", "j", "j"]) {
      const [nodeBefore, rustBefore] = [nodeRaw, rustRaw]
      sendKey("node", key)
      sendKey("rust", key)
      ;[nodeRaw, rustRaw] = afterBoth(nodeBefore, rustBefore, cols, rows)
      compareRaw(`${label} ${key}`, nodeRaw, rustRaw)
    }
  } catch (err) {
    errors.push(`${label}: ${err instanceof Error ? err.message : err}`)
  } finally {
    killSessions()
  }
}

const only = process.env.T3_PARITY_FILTER ?? ""
function wanted(label: string): boolean {
  return only === "" || label.includes(only)
}

try {
  serverUp()
  const full: [string, string | undefined][] = [["tmux-256color", "truecolor"]]
  for (const [term, color] of full) {
    for (const source of ["demo", "db"] as const) {
      for (const [cols, rows] of [[80, 24], [120, 40], [233, 55], [60, 18]] as const) {
        const colorName = color ?? "no-colorterm"
        if (wanted(`${source} ${cols}x${rows} ${term} ${colorName}`)) screenCase(source, cols, rows, term, color)
      }
    }
  }
  // The other TERM values and the missing COLORTERM, at the size where the
  // full sequence already ran, so colour downgrades cannot hide in one layout.
  for (const [term, color] of [
    ["tmux-256color", undefined],
    ["xterm-256color", "truecolor"],
    ["xterm-256color", undefined],
    ["foot", "truecolor"],
    ["foot", undefined],
  ] as const) {
    const colorName = color ?? "no-colorterm"
    if (wanted(`demo 80x24 ${term} ${colorName}`)) screenCase("demo", 80, 24, term, color)
  }
  if (wanted("db 80x24 tmux-256color no-colorterm")) screenCase("db", 80, 24, "tmux-256color", undefined)
  for (const [cols, rows] of [[120, 40], [80, 24]] as const) if (wanted(`clusters ${cols}x${rows}`)) clusterCase(cols, rows)
  for (const how of ["q", "SIGTERM", "SIGHUP", "C-c", "paste+q", "paste+SIGTERM"] as const) {
    if (wanted(`exit ${how}`)) exitCase(how)
  }
  if (wanted("paste demo 80x24 tmux-256color truecolor")) screenCase("demo", 80, 24, "tmux-256color", "truecolor", { prefix: "paste ", steps: PASTE_PLAN })
  if (wanted("not a terminal")) nonTTY()
  if (wanted("TERM=dumb")) survival("TERM=dumb", 80, 24, "dumb")
  if (wanted("size 10x3")) survival("size 10x3", 10, 3, "tmux-256color")
  if (wanted("size 1x1")) survival("size 1x1", 1, 1, "tmux-256color")
  if (wanted("nested tmux")) survival("nested tmux", 80, 24, "tmux-256color", true)
} finally {
  tmux(["kill-server"], true)
  if (differences.length || errors.length) {
    // One folder per run. Several agents share a machine, and one fixed path let a failing run
    // replace the files another run had kept.
    const keep = `${work}-kept`
    cpSync(work, keep, { recursive: true })
    console.error(`kept ${keep}`)
  }
  rmSync(work, { recursive: true, force: true })
}

const report = { version: 1, cases, differences: differences.length, errors: errors.length, deliberate: deliberate.length }
console.log(JSON.stringify(report))
for (const item of deliberate) console.error("deliberate: " + item)
for (const note of notes) console.error("note: " + note)
for (const difference of differences) console.error("difference: " + difference)
for (const error of errors) console.error("error: " + error)
if (differences.length || errors.length) process.exitCode = 1
