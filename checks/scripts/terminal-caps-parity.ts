// Differential gate: how each program draws in a terminal that supports explicit-width text.
//
// terminal-parity.ts runs both programs in tmux, which supports neither OSC 66 (kitty's text-sizing
// protocol, which foot speaks for its `w` key) nor grapheme-cluster mode, so it cannot see how
// either program behaves in a terminal that does. This gate has a small terminal of its own, on
// a pseudo-terminal (util-linux `script` provides the pty). It answers the queries the way foot
// 1.28 does where that is known (see PROFILES), keeps a grid of cells, and records for every cell
// whether it arrived with an explicit width. Both programs run in it on the same synthetic input
// with the same keys; the grids, the per-cell flags, the modes left on and the state at exit are
// compared. A second profile answers "not supported" the way tmux does; a third answers nothing.
// There the Rust build must draw exactly what it drew before it probed at all.
//
// Nothing here opens a window or reads ~/.t3: HOME and T3CODE_HOME are temporary directories, and
// both programs run on the built-in demo with the clock pinned.
//
//   node --experimental-ffi checks/scripts/terminal-caps-parity.ts
//   ... --startup N     measure launch to first complete frame in a terminal that answers nothing
//                       (N runs, alternating the build and T3_CAPS_BASELINE_BINARY)
//
// Scratch files (the demo database and its copies, a few MB) go under TMPDIR, as in the other gates.
// Other knobs, for looking by hand: T3_CAPS_KEYS (a JSON array of input to send instead of the
// default keys), T3_CAPS_KEEP=<dir> (keep the bytes each program wrote), T3_CAPS_SHOW=<n> (list n
// differing cells instead of 6).
//
// Grapheme clusters (see the section "Clusters" below): thread titles full of them, drawn in every
// profile that both programs run in, compared like everything else. Any differing cell is a
// difference, and open_cluster_cells in the report counts them.
//
// T3_CAPS_BASELINE_BINARY: an earlier build. When set, the Rust build's output in the unsupported and
// silent profiles must equal it byte for byte once the probe is removed from both. The demo has no
// cluster, so that output must not have changed. T3_CAPS_ONLY=clusters runs only the cluster section.
import { spawn, spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { DatabaseSync } from "node:sqlite"
import { copyFileSync } from "node:fs"
import { Demo } from "../../oracle/src/demo.ts"
import { cellWidth } from "../../oracle/src/format.ts"
import { CLUSTERS } from "./cluster-titles.ts"

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const REPO = resolve(ROOT, "..")
const termBin = resolve(process.env.T3_NATIVE_BINARY ?? join(REPO, "target/release/loupe-term"))
const baselineBin = process.env.T3_CAPS_BASELINE_BINARY ? resolve(process.env.T3_CAPS_BASELINE_BINARY) : null
const mainTs = join(REPO, "oracle/src/main.ts")
const fixedClock = pathToFileURL(join(ROOT, "scripts/fixed-clock.mjs")).href
const NOW = 1_794_484_800_000 // 2026-11-12T12:00:00Z, pinned like the other gates
const COLS = 120
const ROWS = 34

// ── The fake terminal ─────────────────────────────────────────────────────────

type Cell = { ch: string; fg: string; bg: string; bold: boolean; explicit: number }
const blank = (fg = "default", bg = "default", bold = false): Cell => ({ ch: " ", fg, bg, bold, explicit: 0 })

// What a terminal answers. `foot` is foot 1.28.0 where that can be established: its own manual
// (foot-ctlseqs(7)) lists the modes, the kitty keyboard protocol, XTVERSION and "OSC 66, only w", and
// its changelog says `s` is not implemented, so the scale probe moves the cursor one column.
// Not established, and answered in the most likely way: what DA1 lists, the exact XTGETTCAP
// reply, whether OSC 99 queries are answered (here: not), and mode 2027's state at start (foot
// reports it from its grapheme-shaping setting, which is on by default in a build with
// +graphemes, so both states are run).
type Profile = {
  name: string
  answers: boolean // false: the terminal never writes back
  osc66: boolean // implements the `w` key
  known: Set<number> // private modes it recognises; others answer 0
  initial: Record<number, boolean> // private modes' starting state
  da1: string
  // Answers that are slow, or arrive in two pieces: how long after the query, where the answer is
  // cut (bytes in the first piece), and how long between the pieces.
  latencyMs?: number
  splitAt?: number
  splitGapMs?: number
  // Answers starting with this are not cut. `?` is the Help key, so two cut answers that each
  // carry a `?` would open the overlay and close it again and hide the leak: leave one whole.
  keepWhole?: string
  // The XTVERSION answer, for a profile that gives one other than foot's.
  xtversion?: string
  // Environment both programs get in this terminal.
  env?: Record<string, string>
  // Draws a regional indicator in two cells, alone or paired with the next (Ghostty and WezTerm
  // do; OpenTUI measures a lone one as one cell), and draws other bare text code point by code
  // point, as every profile here does.
  palette?: string[]
  truecolorReply?: string
  riWide?: boolean
}
const FOOT_MODES = [1, 7, 12, 25, 45, 47, 66, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1015, 1016, 1035, 1036, 1042, 1047, 1048, 1049, 1070, 2004, 2026, 2027, 2031, 2033]
const BASE_INITIAL: Record<number, boolean> = { 7: true, 25: true, 12: true }
const COLOR_PROFILES = ["color-256-silent", "color-rgb-silent", "color-neither", "color-palette", "color-tc", "color-late-tc", "color-screen"]
const PROFILES: Record<string, Profile> = {
  ...Object.fromEntries(COLOR_PROFILES.map((name) => [name, {
    name, answers: !name.endsWith("silent"), osc66: false, known: new Set<number>(), initial: {...BASE_INITIAL}, da1: "\x1b[?1;2c",
    env: {COLORTERM: name === "color-rgb-silent" ? "truecolor" : "", TERM: name === "color-neither" ? "xterm" : name === "color-screen" ? "screen-256color" : "xterm-256color"},
    ...(name === "color-palette" ? {palette: ["#181825", "#1e1e2e", "#cdd6f4", "#f38ba8", "#a6e3a1", "#89b4fa", "#fab387", "#f9e2af", "#cba6f7", "#94e2d5", "#f5c2e7", "#74c7ec", "#b4befe", "#bac2de", "#a6adc8", "#6c7086"]} : {}),
    ...(["color-tc", "color-late-tc", "color-screen"].includes(name) ? {truecolorReply: "\x1bP1+r5463=31\x1b\\"} : {}),
    ...(name === "color-late-tc" ? {latencyMs: 400, splitAt: 4, splitGapMs: 5} : {}),
  }])),
  "foot-2027-reset": { name: "foot-2027-reset", answers: true, osc66: true, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL, 2027: false }, da1: "\x1b[?62;4c" },
  "foot-2027-set": { name: "foot-2027-set", answers: true, osc66: true, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL, 2027: true }, da1: "\x1b[?62;4c" },
  // foot, but over a slow link: each answer starts 20 ms late and breaks right after its ESC, the rest
  // 40 ms later (more than the 25 ms the parser gives a lone Escape). The three answers are done by
  // 140 ms, well inside the presenter's 250 ms wait even on a loaded machine, so they still count,
  // and no piece may be a key.
  "foot-slow": { name: "foot-slow", answers: true, osc66: true, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL, 2027: false }, da1: "\x1b[?62;4c", latencyMs: 20, splitAt: 1, splitGapMs: 40 },
  // Too slow: 400 ms, broken after `ESC [` with 80 ms between the pieces. Past the wait, so the
  // terminal counts as one that did not answer, and the answers still must not be a key.
  "foot-late": { name: "foot-late", answers: true, osc66: true, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL, 2027: false }, da1: "\x1b[?62;4c", latencyMs: 400, splitAt: 2, splitGapMs: 80, keepWhole: "\x1b[?62" },
  // tmux's side of it: answers the cursor and attribute queries, knows no 2027, ignores OSC 66.
  unsupported: { name: "unsupported", answers: true, osc66: false, known: new Set([1, 7, 12, 25, 1000, 1002, 1003, 1004, 1006, 1049, 2004, 2026]), initial: { ...BASE_INITIAL }, da1: "\x1b[?1;2c" },
  // tmux 3.8 as seen from inside a pane: the environment tmux sets, and the answers it gave on this
  // machine to the original's queries (2027 not recognized, XTVERSION "tmux 3.8", the cursor not
  // moved by OSC 66). Both programs then trust no width and move the cursor after every cluster.
  "tmux-env": { name: "tmux-env", answers: true, osc66: false, known: new Set([1, 7, 12, 25, 1000, 1002, 1003, 1004, 1006, 1049, 2004, 2026]), initial: { ...BASE_INITIAL }, da1: "\x1b[?1;2;4c", xtversion: "\x1bP>|tmux 3.8\x1b\\", env: { TMUX: "/nonexistent/t3-caps,1,0", TERM: "tmux-256color", TERM_PROGRAM: "tmux", TERM_PROGRAM_VERSION: "3.8" } },
  // Ghostty 1.4.0 as it answered in an isolated desktop on this machine (checks/scripts/window-parity.sh
  // keeps the answers as ghostty-*.in): mode 2027 already on, an XTVERSION answer with Omarchy's
  // build suffix, which OpenTUI's version check does not accept as 1.3 or later, so it chooses
  // `unicode`; OSC 66 ignored; a lone flag letter drawn in two cells.
  ghostty: { name: "ghostty", answers: true, osc66: false, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL, 2027: true }, da1: "\x1b[?62;22;52c", xtversion: "\x1bP>|ghostty 1.4.0+omarchy.1\x1b\\", riWide: true, env: { TERM: "xterm-ghostty", TERM_PROGRAM: "ghostty", TERM_PROGRAM_VERSION: "1.4.0+omarchy.1" } },
  // The same terminal with a plain version, which OpenTUI takes for one that draws wide graphemes
  // (1.3 and later): it chooses `unicode-wide`.
  "ghostty-stock": { name: "ghostty-stock", answers: true, osc66: false, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL, 2027: true }, da1: "\x1b[?62;22;52c", xtversion: "\x1bP>|ghostty 1.4.0\x1b\\", riWide: true, env: { TERM: "xterm-ghostty", TERM_PROGRAM: "ghostty", TERM_PROGRAM_VERSION: "1.4.0" } },
  // kitty 0.49.2 reached with TERM=xterm-256color and nothing else in the environment, as over
  // SSH (checks/scripts/window-parity.sh --ssh-like): its answers as recorded there. It does not
  // know mode 2027 (reports 0), says what it is only in the XTVERSION answer, and draws `w=` text.
  // OpenTUI takes it for a terminal that clusters graphemes without 2027, so `unicode`.
  "kitty-ssh": { name: "kitty-ssh", answers: true, osc66: true, known: new Set(FOOT_MODES.filter((m) => m !== 2027)), initial: { ...BASE_INITIAL }, da1: "\x1b[?62;52;c", xtversion: "\x1bP>|kitty(0.49.2)\x1b\\" },
  // A plain pty with nobody on the other end.
  silent: { name: "silent", answers: false, osc66: false, known: new Set(FOOT_MODES), initial: { ...BASE_INITIAL }, da1: "" },
}

class Screen {
  cells: Cell[]
  constructor(cols: number, rows: number) {
    this.cells = Array.from({ length: cols * rows }, () => blank())
  }
}

export class FakeTerminal {
  readonly profile: Profile
  readonly cols: number
  readonly rows: number
  main: Screen
  alt: Screen
  onAlt = false
  x = 0
  y = 0
  pending = false // the cursor is past the last column, waiting to wrap
  saved = { x: 0, y: 0 }
  altSaved = { x: 0, y: 0 }
  fg = "default"
  bg = "default"
  bold = false
  modes = new Map<number, boolean>()
  kitty: number[] = []
  modifyOtherKeys = 0
  esc: number[] = []
  state: "ground" | "esc" | "csi" | "str" = "ground"
  strKind = ""
  utf: number[] = []
  replies: Buffer[] = []
  bytes = 0
  frames = 0 // synchronized updates closed
  modeLog: { mode: number; on: boolean }[] = []
  unwrapped: string[] = [] // sequences nobody here understands, deduplicated
  sizeLog: number[] = [] // widths given to OSC 66
  riAt = -1 // the cell holding a lone regional indicator just printed, which the next one may join
  constructor(profile: Profile, cols: number, rows: number) {
    this.profile = profile
    this.cols = cols
    this.rows = rows
    this.main = new Screen(cols, rows)
    this.alt = new Screen(cols, rows)
    for (const [k, v] of Object.entries(profile.initial)) this.modes.set(Number(k), v)
  }

  get screen() {
    return this.onAlt ? this.alt : this.main
  }
  private mode(n: number) {
    return this.modes.get(n) ?? false
  }

  feed(chunk: Buffer): Buffer[] {
    this.replies = []
    this.bytes += chunk.length
    for (const b of chunk) this.byte(b)
    return this.profile.answers ? this.replies : []
  }

  private byte(b: number) {
    if (this.state === "ground") {
      if (this.utf.length) {
        this.utf.push(b)
        const need = this.utf[0]! >= 0xf0 ? 4 : this.utf[0]! >= 0xe0 ? 3 : 2
        if (this.utf.length === need) {
          const text = Buffer.from(this.utf).toString("utf8")
          this.utf = []
          this.print(text, 0)
        }
        return
      }
      if (b === 0x1b) {
        this.state = "esc"
        this.esc = []
        return
      }
      if (b >= 0xc0) {
        this.utf = [b]
        return
      }
      if (b >= 0x20 && b < 0x7f) return this.print(String.fromCharCode(b), 0)
      return this.control(b)
    }
    if (this.state === "esc") {
      if (b === 0x5b) {
        this.state = "csi"
        return
      }
      if (b === 0x5d || b === 0x50 || b === 0x5f || b === 0x5e || b === 0x58) {
        this.state = "str"
        this.strKind = String.fromCharCode(b)
        this.esc = []
        return
      }
      this.state = "ground"
      if (b === 0x37) this.saved = { x: this.x, y: this.y }
      else if (b === 0x38) [this.x, this.y, this.pending] = [this.saved.x, this.saved.y, false]
      else if (b === 0x63) this.reset()
      return
    }
    if (this.state === "csi") {
      this.esc.push(b)
      if (b >= 0x40 && b <= 0x7e) {
        const seq = Buffer.from(this.esc).toString("latin1")
        this.esc = []
        this.state = "ground"
        this.csi(seq)
      } else if (b === 0x1b) {
        this.state = "esc"
        this.esc = []
      }
      return
    }
    // A string: OSC, DCS, APC, PM or SOS ends at ST, and an OSC also at BEL.
    if (b === 0x07 && this.strKind === "]") return this.endString(this.esc)
    if (b === 0x5c && this.esc[this.esc.length - 1] === 0x1b) return this.endString(this.esc.slice(0, -1))
    this.esc.push(b)
    if (this.esc.length > 70_000) this.state = "ground"
  }

  private reset() {
    this.main = new Screen(this.cols, this.rows)
    this.alt = new Screen(this.cols, this.rows)
    this.onAlt = false
    this.x = this.y = 0
  }

  private control(b: number) {
    this.riAt = -1
    if (b === 0x0d) {
      this.x = 0
      this.pending = false
    } else if (b === 0x0a || b === 0x0b || b === 0x0c) {
      this.pending = false
      this.lineFeed()
    } else if (b === 0x08) {
      this.x = Math.max(0, this.x - 1)
      this.pending = false
    } else if (b === 0x09) {
      this.x = Math.min(this.cols - 1, (this.x + 8) & ~7)
    }
  }

  private lineFeed() {
    if (this.y < this.rows - 1) this.y++
    else {
      const s = this.screen
      s.cells.splice(0, this.cols)
      for (let i = 0; i < this.cols; i++) s.cells.push(blank(this.fg, this.bg, false))
    }
  }

  private endString(body: number[]) {
    this.state = "ground"
    const text = Buffer.from(body).toString("utf8")
    if (this.strKind === "]") return this.osc(text)
    if (this.strKind === "P") return this.dcs(text)
    // APC (kitty graphics queries) and the rest: foot answers none of them.
    this.note(`${this.strKind}:${text.slice(0, 24)}`)
  }

  private note(s: string) {
    if (!this.unwrapped.includes(s) && this.unwrapped.length < 40) this.unwrapped.push(s)
  }

  private osc(text: string) {
    const semi = text.indexOf(";")
    const code = semi < 0 ? text : text.slice(0, semi)
    const rest = semi < 0 ? "" : text.slice(semi + 1)
    if (code === "4" && this.profile.palette) {
      const [index, query] = rest.split(";")
      const color = this.profile.palette[Number(index)]
      if (query === "?" && color) this.reply(`\x1b]4;${index};${color}\x07`)
      return
    }
    if (code === "66") {
      if (!this.profile.osc66) return this.note("OSC 66 ignored")
      const at = rest.indexOf(";")
      const params = new Map(rest.slice(0, at).split(":").filter(Boolean).map((kv) => kv.split("=") as [string, string]))
      const w = Number(params.get("w") ?? 0)
      // Only w is implemented. Text with w=0 takes the width the terminal's own tables give it.
      const body = rest.slice(at + 1)
      if (body.trim() !== "") this.sizeLog.push(w) // a blank is a probe, not a drawn cell
      const width = w > 0 ? w : [...body].reduce((n, c) => n + Math.max(0, cellWidth(c.codePointAt(0)!)), 0)
      this.printGroup(body, width, w > 0 ? w : 0)
      return
    }
    if (code === "10" || code === "11") {
      if (rest === "?") this.reply(`\x1b]${code};rgb:${code === "10" ? "c0c0/caca/f5f5" : "1a1a/1b1b/2626"}\x07`)
      return
    }
    this.note(`OSC ${code}`)
  }

  private dcs(text: string) {
    if (text.startsWith("+q")) {
      // XTGETTCAP. Foot has a built-in terminfo: Ms is the clipboard escape. Hex of the value.
      const name = text.slice(2)
      const value = Buffer.from("\x1b]52;%p1%s;%p2%s\x1b\\").toString("hex")
      if (["524742", "5463"].includes(name) && this.profile.truecolorReply) { this.reply(this.profile.truecolorReply); return }
      this.reply(name === "4d73" ? `\x1bP1+r${name}=${value}\x1b\\` : `\x1bP0+r\x1b\\`)
      return
    }
    this.note(`DCS ${text.slice(0, 12)}`)
  }

  private reply(text: string) {
    this.replies.push(Buffer.from(text))
  }

  private print(text: string, explicit: number) {
    for (const ch of text) {
      const cp = ch.codePointAt(0)!
      if (this.profile.riWide && cp >= 0x1f1e6 && cp <= 0x1f1ff) {
        // The second letter of a pair joins the first one's two cells; a lone one takes two.
        const prev = this.riAt >= 0 ? this.screen.cells[this.riAt] : undefined
        if (prev && [...prev.ch].length === 1 && this.riAt === this.y * this.cols + this.x - 2) prev.ch += ch
        else {
          const at = this.y * this.cols + this.x
          this.printGroup(ch, 2, explicit)
          this.riAt = at
          continue
        }
        this.riAt = -1
        continue
      }
      this.riAt = -1
      const w = cellWidth(cp)
      if (w === 0) {
        // A combining mark joins the cell before it.
        const prev = this.screen.cells[this.y * this.cols + Math.max(0, this.x - 1)]
        if (prev) prev.ch += ch
        continue
      }
      this.printGroup(ch, w, explicit)
    }
  }

  // Put text that occupies `width` columns at the cursor. `explicit` is the width the sender gave.
  private printGroup(text: string, width: number, explicit: number) {
    if (width <= 0) return
    const s = this.screen
    if (this.pending && this.mode(7)) {
      this.x = 0
      this.pending = false
      this.lineFeed()
    }
    if (this.x + width > this.cols) {
      if (this.mode(7)) {
        this.x = 0
        this.lineFeed()
      } else this.x = this.cols - width
    }
    const i = this.y * this.cols + this.x
    // Writing over half of a wide character blanks the other half.
    if (s.cells[i]!.ch === "" && this.x > 0) s.cells[i - 1] = blank(this.fg, this.bg)
    const end = this.x + width - 1
    if (end + 1 < this.cols && s.cells[i + width]?.ch === "") s.cells[i + width] = blank(this.fg, this.bg)
    s.cells[i] = { ch: text, fg: this.fg, bg: this.bg, bold: this.bold, explicit }
    for (let k = 1; k < width; k++) s.cells[i + k] = { ch: "", fg: this.fg, bg: this.bg, bold: this.bold, explicit }
    if (this.x + width >= this.cols) {
      this.x = this.cols - 1
      this.pending = true
    } else this.x += width
  }

  private csi(seq: string) {
    this.riAt = -1
    const final = seq[seq.length - 1]!
    let body = seq.slice(0, -1)
    const marker = /^[<=>?]/.test(body) ? body[0]! : ""
    if (marker) body = body.slice(1)
    const inter = body.match(/[ -/]+$/)?.[0] ?? ""
    if (inter) body = body.slice(0, -inter.length)
    const nums = body === "" ? [] : body.split(/[;:]/).map((p) => (p === "" ? 0 : Number(p)))
    const n = (i: number, d: number) => (nums[i] === undefined || nums[i] === 0 ? d : nums[i]!)
    const s = this.screen
    this.pending = final === "m" || final === "h" || final === "l" || final === "n" || final === "c" || final === "p" || final === "q" || final === "u" || final === "s" || final === "t" ? this.pending : false
    switch (marker + inter + final) {
      case "H": case "f":
        this.y = Math.min(this.rows - 1, n(0, 1) - 1)
        this.x = Math.min(this.cols - 1, n(1, 1) - 1)
        return
      case "A": this.y = Math.max(0, this.y - n(0, 1)); return
      case "B": this.y = Math.min(this.rows - 1, this.y + n(0, 1)); return
      case "C": this.x = Math.min(this.cols - 1, this.x + n(0, 1)); return
      case "D": this.x = Math.max(0, this.x - n(0, 1)); return
      case "G": this.x = Math.min(this.cols - 1, n(0, 1) - 1); return
      case "d": this.y = Math.min(this.rows - 1, n(0, 1) - 1); return
      case "J": {
        const mode = nums[0] ?? 0
        const from = mode === 0 ? this.y * this.cols + this.x : 0
        const to = mode === 1 ? this.y * this.cols + this.x + 1 : s.cells.length
        for (let i = from; i < to; i++) s.cells[i] = blank(this.fg, this.bg)
        return
      }
      case "K": {
        const mode = nums[0] ?? 0
        const row = this.y * this.cols
        const from = mode === 0 ? row + this.x : row
        const to = mode === 1 ? row + this.x + 1 : row + this.cols
        for (let i = from; i < to; i++) s.cells[i] = blank(this.fg, this.bg)
        return
      }
      case "m": return this.sgr(nums)
      case "s": this.saved = { x: this.x, y: this.y }; return
      case "u": [this.x, this.y, this.pending] = [this.saved.x, this.saved.y, false]; return
      case "?h": case "?l": {
        for (const mode of nums) this.setMode(mode, final === "h")
        return
      }
      case "?$p": {
        const mode = nums[0] ?? 0
        const value = !this.profile.known.has(mode) ? 0 : this.mode(mode) ? 1 : 2
        this.reply(`\x1b[?${mode};${value}$y`)
        return
      }
      case "n": {
        if (nums[0] === 6) this.reply(`\x1b[${this.y + 1};${this.x + 1}R`)
        else if (nums[0] === 5) this.reply("\x1b[0n")
        return
      }
      case "c": this.reply(this.profile.da1); return
      case ">q": if ((nums[0] ?? 0) === 0 && (this.profile.xtversion || this.profile.name.startsWith("foot"))) this.reply(this.profile.xtversion ?? "\x1bP>|foot(1.28.0)\x1b\\"); return
      case "t": if (nums[0] === 14) this.reply(`\x1b[4;${this.rows * 16};${this.cols * 8}t`); return
      case ">m": if (nums[0] === 4) this.modifyOtherKeys = nums[1] ?? 0; return
      case "?u": if (this.profile.name.startsWith("foot")) this.reply(`\x1b[?${this.kitty[this.kitty.length - 1] ?? 0}u`); return
      case ">u": if (this.profile.name.startsWith("foot")) this.kitty.push(nums[0] ?? 0); return
      case "<u": if (this.profile.name.startsWith("foot")) for (let k = 0; k < n(0, 1); k++) this.kitty.pop(); return
      case "=u": if (this.profile.name.startsWith("foot")) this.kitty[Math.max(0, this.kitty.length - 1)] = nums[0] ?? 0; return
      default:
        this.note(`CSI ${marker}${inter}${final}`)
    }
  }

  private setMode(mode: number, on: boolean) {
    this.modeLog.push({ mode, on })
    this.modes.set(mode, on)
    if (mode === 1049) {
      if (on && !this.onAlt) {
        this.altSaved = { x: this.x, y: this.y }
        this.onAlt = true
        this.alt = new Screen(this.cols, this.rows)
      } else if (!on && this.onAlt) {
        this.onAlt = false
        ;[this.x, this.y, this.pending] = [this.altSaved.x, this.altSaved.y, false]
      }
    }
    if (mode === 2026 && !on) this.frames++
  }

  private sgr(nums: number[]) {
    if (!nums.length) nums = [0]
    for (let k = 0; k < nums.length; k++) {
      const v = nums[k]!
      if (v === 0) [this.fg, this.bg, this.bold] = ["default", "default", false]
      else if (v === 1) this.bold = true
      else if (v === 22) this.bold = false
      else if (v === 39) this.fg = "default"
      else if (v === 49) this.bg = "default"
      else if ((v === 38 || v === 48) && nums[k + 1] === 2) {
        const c = `rgb(${nums[k + 2]},${nums[k + 3]},${nums[k + 4]})`
        if (v === 38) this.fg = c
        else this.bg = c
        k += 4
      } else if ((v === 38 || v === 48) && nums[k + 1] === 5) {
        const c = `idx(${nums[k + 2]})`
        if (v === 38) this.fg = c
        else this.bg = c
        k += 2
      }
    }
  }

  // The visible grid of the screen the program is on, as comparable rows.
  grid(): Cell[] {
    return this.screen.cells.map((c) => ({ ...c }))
  }

  // What is switched on. A cooked terminal has none of these, apart from autowrap and the cursor.
  stateAtRest() {
    const on = [...this.modes.entries()].filter(([, v]) => v).map(([k]) => k).sort((a, b) => a - b)
    return { modesOn: on, altScreen: this.onAlt, kitty: [...this.kitty], modifyOtherKeys: this.modifyOtherKeys }
  }
}

// ── Running a program in it ───────────────────────────────────────────────────

const work = mkdtempSync(join(tmpdir(), "t3-caps-parity-"))
const home = join(work, "home")
const t3home = join(work, "t3home")
const runtime = join(work, "run")
const stubs = join(work, "bin")
for (const dir of [home, t3home, runtime, stubs]) mkdirSync(dir, { recursive: true })

const quote = (v: string) => `'${v.replace(/'/g, `'\\''`)}'`
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

type Side = "node" | "rust" | "baseline"
function argvFor(side: Side, input: string[]): string[] {
  const args = ["--calm", "--interval", "30", ...input]
  if (side === "node") return [process.execPath, "--experimental-ffi", "--disable-warning=ExperimentalWarning", "--import", fixedClock, mainTs, ...args]
  return [side === "baseline" ? baselineBin! : termBin, ...args]
}

type Snapshot = { step: string; grid: Cell[]; modes: ReturnType<FakeTerminal["stateAtRest"]>; frames: number }
type Run = {
  side: Side
  profile: string
  snapshots: Snapshot[]
  exit: { code: number | null; state: ReturnType<FakeTerminal["stateAtRest"]> }
  modeLog: { mode: number; on: boolean }[]
  raw: Buffer
  firstGrid?: Cell[]
  firstFrameMs: number | null // launch to the first complete synchronized update
  notes: string[]
  oscWidths: Record<string, number>
  error?: string
}

// Runs kept under T3_CAPS_KEEP so far, per program and profile: later runs get -2, -3, ...
const keptRuns = new Map<string, number>()
async function runProgram(side: Side, profileName: string, keys: string[], input: string[] = ["--demo"], extraEnv: Record<string, string> = {}): Promise<Run> {
  const profile = PROFILES[profileName]!
  const term = new FakeTerminal(profile, COLS, ROWS)
  const env: Record<string, string> = {
    PATH: `${stubs}:${process.env.PATH ?? "/usr/bin"}`,
    HOME: home,
    T3CODE_HOME: t3home,
    XDG_RUNTIME_DIR: runtime,
    TZ: "UTC",
    LANG: "C.UTF-8",
    T3_FIXED_NOW: String(NOW),
    // The original's theme behavior; the product's themes are tested in crates/term/tests/pty.rs.
    LOUPE_NO_THEMES: "1",
    TERM: "xterm-256color",
    COLORTERM: "truecolor",
    COLUMNS: String(COLS),
    LINES: String(ROWS),
    ...profile.env,
    ...extraEnv,
  }
  // Empty overrides in a color profile mean the SSH-like absence of this variable.
  if (env.COLORTERM === "") delete env.COLORTERM
  // -echo: replies written to the pty must not be echoed back into the output before the program
  // has put the line discipline in raw mode.
  const command = `stty cols ${COLS} rows ${ROWS} -echo; cd ${quote(REPO)}; exec ${argvFor(side, input).map(quote).join(" ")}`
  const child = spawn("script", ["-qefc", command, "/dev/null"], { env, stdio: ["pipe", "pipe", "pipe"] })
  const chunks: Buffer[] = []
  const launched = performance.now()
  let firstGrid: Cell[] | undefined
  let firstFrameMs: number | null = null
  let lastData = performance.now()
  let exited = false
  let exitCode: number | null = null
  // A terminal's answers leave it in order, whole or in pieces: one reply is finished before the
  // next one starts, so a slow link delays the stream and never interleaves two answers.
  let streamFree = performance.now()
  const sendReply = (reply: Buffer) => {
    const write = (b: Buffer) => {
      if (!exited) child.stdin.write(b)
    }
    const cut = profile.splitAt && !(profile.keepWhole && reply.toString("latin1").startsWith(profile.keepWhole))
    const pieces = cut ? [reply.subarray(0, profile.splitAt), reply.subarray(profile.splitAt)] : [reply]
    const wait = (profile.latencyMs ?? 0) > 0 || profile.splitAt
    let at = Math.max(performance.now() + (profile.latencyMs ?? 0), streamFree)
    pieces.forEach((piece, i) => {
      if (i > 0) at += profile.splitGapMs ?? 0
      if (wait) setTimeout(() => write(piece), Math.max(0, at - performance.now()))
      else write(piece)
    })
    streamFree = at
  }
  child.stdout.on("data", (chunk: Buffer) => {
    chunks.push(chunk)
    lastData = performance.now()
    const before = term.frames
    const replies = term.feed(chunk)
    if (firstFrameMs === null && term.frames > before) {
      firstFrameMs = performance.now() - launched
      firstGrid = term.grid()
    }
    for (const reply of replies) sendReply(reply)
  })
  child.stderr.on("data", () => {})
  child.stdin.on("error", () => {})
  child.on("close", (code) => {
    exited = true
    exitCode = code
  })
  const snapshots: Snapshot[] = []
  let error: string | undefined
  // Settled: at least one frame, and nothing written for a while. Calm mode stops drawing.
  const settle = async (label: string, quiet = 700, limit = 20_000) => {
    const start = performance.now()
    while (performance.now() - start < limit) {
      await sleep(40)
      if (exited) return false
      if (term.frames > 0 && performance.now() - lastData >= quiet) return true
    }
    error = `${label}: no settled frame in ${limit} ms`
    return false
  }
  try {
    if (await settle("start")) {
      snapshots.push({ step: "start", grid: term.grid(), modes: term.stateAtRest(), frames: term.frames })
      for (const key of keys) {
        const framesBefore = term.frames
        child.stdin.write(key)
        // Calm mode draws at 2 fps: wait for the frame the key causes, then for quiet.
        const sent = performance.now()
        while (term.frames === framesBefore && performance.now() - sent < 4000 && !exited) await sleep(40)
        if (!(await settle(`key ${JSON.stringify(key)}`))) break
        snapshots.push({ step: `key ${JSON.stringify(key)}`, grid: term.grid(), modes: term.stateAtRest(), frames: term.frames })
      }
    }
    if (!exited) {
      child.stdin.write("q")
      const start = performance.now()
      while (!exited && performance.now() - start < 8000) await sleep(20)
      if (!exited) error = (error ? error + "; " : "") + "did not exit on q"
    }
  } finally {
    if (!exited) child.kill("SIGKILL")
  }
  await sleep(50)
  // T3_CAPS_KEEP=<dir>: keep what each program wrote, for looking at by hand.
  if (process.env.T3_CAPS_KEEP) {
    mkdirSync(process.env.T3_CAPS_KEEP, { recursive: true })
    const name = `${side}-${profileName}`
    keptRuns.set(name, (keptRuns.get(name) ?? 0) + 1)
    writeFileSync(join(process.env.T3_CAPS_KEEP, `${name}${keptRuns.get(name)! > 1 ? `-${keptRuns.get(name)}` : ""}.bin`), Buffer.concat(chunks))
  }
  const widths: Record<string, number> = {}
  for (const w of term.sizeLog) widths[`w=${w}`] = (widths[`w=${w}`] ?? 0) + 1
  return {
    side,
    profile: profileName,
    snapshots,
    exit: { code: exitCode, state: term.stateAtRest() },
    modeLog: term.modeLog,
    raw: Buffer.concat(chunks),
    firstFrameMs,
    firstGrid,
    notes: term.unwrapped,
    oscWidths: widths,
    error,
  }
}

// ── Comparing ─────────────────────────────────────────────────────────────────

const differences: string[] = []
const deliberate: string[] = []
const notes: string[] = []
const errors: string[] = []
let cases = 0
let cellsCompared = 0
let explicitCells = 0
let clusterCells = 0
let clusterCellsSeen = 0
// While the cluster scenarios run, every differing cell is counted here too.
let clusterTally: { cells: number } | null = null

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b)

function describe(c: Cell) {
  return `${JSON.stringify(c.ch)} fg=${c.fg} bg=${c.bg}${c.bold ? " bold" : ""}${c.explicit ? ` w=${c.explicit}` : ""}`
}

// One kind of difference is not the programs' and cannot be seen, and terminal-parity.ts accepts the
// same thing in tmux (see compareRaw there). In a terminal without explicit widths, where the
// terminal measures a cluster differently from the original's renderer, the blank cells to the right
// of it are not written again, so they keep the foreground an earlier frame gave them, and the
// original's earliest frames vary from run to run. The slow start below is one way that happens and
// is caught by its first frame; on GitHub's two-core runner the original also did it once with a
// first frame that looked complete (2026-11-17: three cells, original white, Rust default, in the
// unsupported profile only, with code that had passed there four times). A cell that holds a space
// in both programs, with the same background and weight, shows nothing whatever its foreground.
// With `blankForeground` those cells are counted and noted, to the right of a non-ASCII cell on
// their row, and nowhere else.
let blankForegroundCells = 0
function compareGrids(label: string, a: Cell[], b: Cell[], nameA = "original", nameB = "rust", flags = true, blankForeground = false) {
  cases++
  const bad: string[] = []
  let flagDiffs = 0
  let total = 0
  let open = 0
  for (let row = 0; row * COLS < a.length; row++) {
    let afterWide = false // a non-ASCII cell earlier on this row
    for (let i = row * COLS; i < (row + 1) * COLS && i < a.length; i++) {
      cellsCompared++
      if (a[i]!.explicit) explicitCells++
      const x = a[i]!
      const y = b[i]!
      const body = x.ch === y.ch && x.fg === y.fg && x.bg === y.bg && x.bold === y.bold
      const flag = !flags || x.explicit === y.explicit
      const invisible = blankForeground && afterWide && flag && x.ch === " " && y.ch === " " && x.bg === y.bg && x.bold === y.bold
      if ([...x.ch].some((c) => c.codePointAt(0)! > 0x7f) || [...y.ch].some((c) => c.codePointAt(0)! > 0x7f)) afterWide = true
      if (body && flag) continue
      if (invisible) {
        open++
        continue
      }
      total++
      if (!flag) flagDiffs++
      if (bad.length < Number(process.env.T3_CAPS_SHOW ?? 6)) bad.push(`(${i % COLS},${Math.floor(i / COLS)}) ${nameA} ${describe(x)} | ${nameB} ${describe(y)}`)
    }
  }
  if (open) {
    blankForegroundCells += open
    notes.push(`open: ${label}: ${open} blank cells differ only in foreground color, beside a cluster this terminal measures differently from the renderer (see compareGrids)`)
  }
  if (clusterTally) clusterTally.cells += total
  if (total) differences.push(`${label}: ${total} cells differ (${flagDiffs} only in explicit width); ${bad.join("; ")}`)
}

const bare = (r: Run) => r.raw.toString("latin1")

// What the probe adds to the stream, so the rest can be compared with a build that never probed.
// What a build from before the probe did not write: the probe, and, since bracketed paste, one
// switch-on and one switch-off of mode 2004. One of each is removed and no more, so a second
// copy of any of them, or any other byte that differs, still fails the comparison.
function withoutProbe(raw: Buffer): Buffer {
  let text = raw.toString("latin1")
  for (const added of ["\x1b[>0q\x1b[?2027$p\x1b[H\x1b]66;w=1; \x1b\\\x1b[6n\x1b[c", "\x1b[?2027$p\x1b[H\x1b]66;w=1; \x1b\\\x1b[6n\x1b[c", "\x1b[?2004h", "\x1b[?2004l"]) {
    const at = text.indexOf(added)
    if (at >= 0) text = text.slice(0, at) + text.slice(at + added.length)
  }
  return Buffer.from(text, "latin1")
}

// `either`: the original's own setting of 2027 is a race with the terminal's answer (see scenario()).
function checkExit(label: string, r: Run, initial: Record<number, boolean>, expect2027: "left on" | "either" | "untouched") {
  const problems: string[] = []
  if (r.exit.code !== 0) problems.push(`exit code ${r.exit.code}`)
  const left = new Set(r.exit.state.modesOn)
  // What a cooked terminal has on: whatever was on at the start.
  const startOn = new Set(Object.entries(initial).filter(([, v]) => v).map(([k]) => Number(k)))
  for (const m of left) {
    if (m === 2027 && expect2027 !== "untouched") continue
    if (!startOn.has(m)) problems.push(`mode ${m} left on`)
  }
  for (const m of startOn) if (!left.has(m)) problems.push(`mode ${m} was on at the start and is off at exit`)
  if (r.exit.state.altScreen) problems.push("still on the alternate screen")
  if (r.exit.state.kitty.length) problems.push(`kitty keyboard flags left pushed: ${r.exit.state.kitty}`)
  if (r.exit.state.modifyOtherKeys !== 0) problems.push(`modifyOtherKeys ${r.exit.state.modifyOtherKeys}`)
  if (expect2027 === "left on" && !left.has(2027)) problems.push("2027 expected left on")
  cases++
  if (problems.length) differences.push(`${label}: ${problems.join("; ")}`)
}

// Modes the program switched on at some point, for the notes.
function everOn(r: Run): number[] {
  return [...new Set(r.modeLog.filter((m) => m.on).map((m) => m.mode))].sort((a, b) => a - b)
}

// The original has two ways to start. Usually its first frame holds the dashboard. On a slow
// machine it draws an empty frame before its data is ready and then the dashboard as an update, and
// an update moves the cursor after every wide cluster where a first frame does not. In a terminal
// without explicit widths the two leave different cells beside a cluster the terminal measures
// differently: the original differs from itself. The Rust build always draws the dashboard in its
// first frame, so a slow start of the original says nothing about it. Such a run is repeated, and
// if the original starts slowly every time the cells are not compared and the profile is counted.
const SLOW_START_ATTEMPTS = 4
const slowStarts: string[] = []
function startedSlowly(raw: Buffer): boolean {
  const from = raw.indexOf("\x1b[?2026h")
  if (from < 0) return false
  const to = raw.indexOf("\x1b[?2026l", from)
  return !raw.subarray(from, to < 0 ? raw.length : to).includes("Loupe")
}

async function scenario(profileName: string, keys: string[], input: string[] = ["--demo"], tag = "") {
  const label = `${profileName}${tag}`
  const profile = PROFILES[profileName]!
  let node!: Awaited<ReturnType<typeof runProgram>>
  let rust!: Awaited<ReturnType<typeof runProgram>>
  let slow = false
  for (let attempt = 1; attempt <= SLOW_START_ATTEMPTS; attempt++) {
    ;[node, rust] = await Promise.all([runProgram("node", profileName, keys, input), runProgram("rust", profileName, keys, input)])
    slow = !profile.osc66 && !node.error && startedSlowly(node.raw)
    if (!slow) break
  }
  for (const r of [node, rust]) if (r.error) errors.push(`${label} ${r.side}: ${r.error}`)
  if (node.error || rust.error) return { node, rust }
  if (!node.error && startedSlowly(rust.raw)) differences.push(`${label}: Rust drew an empty first frame`)
  if (slow) {
    slowStarts.push(label)
    notes.push(`${label}: the original drew an empty frame before the dashboard in all ${SLOW_START_ATTEMPTS} attempts (a slow machine); its cells were not compared`)
    return { node, rust }
  }
  if (node.snapshots.length !== rust.snapshots.length) {
    differences.push(`${label}: ${node.snapshots.length} settled screens from the original, ${rust.snapshots.length} from Rust`)
    return { node, rust }
  }
  // Blank cells' foregrounds are let through only where they can differ without anything to see:
  // cluster titles, in a terminal without explicit widths.
  node.snapshots.forEach((s, i) => compareGrids(`${label} ${s.step}`, s.grid, rust.snapshots[i]!.grid, "original", "rust", true, clusterTally !== null && !profile.osc66))
  if (profileName === "color-late-tc") {
    if (!rust.firstGrid?.some((c) => c.fg.startsWith("idx("))) differences.push(`${label}: Rust did not draw indexed colors before the late reply`)
    if (!rust.snapshots.at(-1)?.grid.some((c) => c.fg.startsWith("rgb("))) differences.push(`${label}: late Tc reply did not repaint in RGB`)
  }
  const supported = profile.osc66
  const explicitNode = node.snapshots.reduce((n, s) => n + s.grid.filter((c) => c.explicit).length, 0)
  const explicitRust = rust.snapshots.reduce((n, s) => n + s.grid.filter((c) => c.explicit).length, 0)
  if (supported && explicitNode === 0) differences.push(`${label}: the original wrote no explicit-width cell; the gate has nothing to compare`)
  if (!supported && (explicitNode || explicitRust)) differences.push(`${label}: explicit-width cells in a terminal that does not support them (original ${explicitNode}, rust ${explicitRust})`)
  // The width given to each explicit-width write, over the whole run: the same set of widths.
  if (supported && !same(Object.keys(node.oscWidths).sort(), Object.keys(rust.oscWidths).sort())) {
    differences.push(`${label}: widths used with OSC 66 differ: original ${JSON.stringify(node.oscWidths)} rust ${JSON.stringify(rust.oscWidths)}`)
  }
  // Mode 2027 at exit. The original leaves it on whatever it found; Rust puts it back as found.
  const found = profile.initial[2027] ?? false
  // The original never switches 2027 off. Whether it switches it on depends on whether the terminal's
  // answer reaches it before its own setup runs: in this terminal it did in most runs and not in
  // one in fifteen, with the same answers (and in the capture from foot it did). So its end state is
  // only held to "still on" when the terminal started with it on. Rust puts it back as it found
  // it: on only if it was already on, whatever the original did.
  checkExit(`${label} exit original`, node, profile.initial, found ? "left on" : "either")
  checkExit(`${label} exit rust`, rust, profile.initial, found ? "left on" : "untouched")
  // What Rust does with 2027 follows the answer, not a race: on if reported off and settable, left
  // alone if already on or not reported.
  const rustSet = rust.modeLog.some((m) => m.mode === 2027 && m.on)
  const rustTouched = rust.modeLog.some((m) => m.mode === 2027)
  if (profile.known.has(2027) && !found && !rustSet) differences.push(`${label}: Rust did not switch on mode 2027, which the terminal reported off`)
  if (profile.known.has(2027) && found && rustTouched) differences.push(`${label}: Rust touched mode 2027, which was already on`)
  if (!profile.known.has(2027) && rustTouched) differences.push(`${label}: Rust touched mode 2027, which the terminal does not report`)
  const nodeEnd = node.exit.state.modesOn.includes(2027)
  const rustEnd = rust.exit.state.modesOn.includes(2027)
  if (nodeEnd !== rustEnd) {
    if (nodeEnd && !rustEnd && !found) deliberate.push(`${label}: the original leaves mode 2027 on; the Rust presenter leaves it as it found it (off)`)
    else differences.push(`${label}: mode 2027 at exit: original ${nodeEnd ? "on" : "off"}, rust ${rustEnd ? "on" : "off"}, found ${found ? "on" : "off"}`)
  }
  notes.push(`${label}: modes switched on at some point: original ${JSON.stringify(everOn(node))}, rust ${JSON.stringify(everOn(rust))}`)
  return { node, rust }
}

async function startup(trials: number) {
  // Launch to the first complete frame, in a terminal that answers nothing. Alternating trials.
  const rows: Record<string, number[]> = { after: [], before: [] }
  const order: Side[] = []
  for (let i = 0; i < trials; i++) order.push(i % 2 === 0 ? "rust" : "baseline")
  for (const side of order) {
    const r = await runProgram(side, "silent", [])
    if (r.error || r.firstFrameMs === null) {
      errors.push(`startup ${side}: ${r.error ?? "no frame"}`)
      continue
    }
    rows[side === "rust" ? "after" : "before"]!.push(r.firstFrameMs)
  }
  const stat = (v: number[]) => {
    const s = [...v].sort((a, b) => a - b)
    return { trials: s.length, median_ms: +s[Math.floor(s.length / 2)]!.toFixed(1), min_ms: +s[0]!.toFixed(1), max_ms: +s[s.length - 1]!.toFixed(1), all_ms: v.map((x) => +x.toFixed(1)) }
  }
  console.log(JSON.stringify({ startup: { terminal: "answers nothing", before: stat(rows.before!), after: stat(rows.after!) }, errors: errors.length }))
}

// T3_CAPS_KEYS: a JSON array of other input to send, one string per step, for trying things by hand.
// The demo's database with some thread titles replaced, built here from the oracle's own demo.
// `agree` titles are wide text, a family split by clean() at its joiners, bread, and Latin-1.
// `clusters` titles are in the section "Clusters" below.
function titledDb(name: string, titles: string[]): string {
  const dir = join(work, name)
  mkdirSync(dir, { recursive: true })
  const demo = new Demo({ now: NOW - 400 * 86_400_000, refreshMs: 0 })
  copyFileSync(demo.path, join(dir, "statev2.sqlite"))
  copyFileSync(join(demo.dir, "environment-id"), join(dir, "environment-id"))
  demo.stop()
  const db = new DatabaseSync(join(dir, "statev2.sqlite"))
  const rows = db.prepare("select thread_id, payload_json from orchestration_v2_projection_threads order by thread_id").all() as { thread_id: string; payload_json: string }[]
  const update = db.prepare("update orchestration_v2_projection_threads set payload_json = ? where thread_id = ?")
  rows.slice(0, titles.length).forEach((row, i) => update.run(JSON.stringify({ ...JSON.parse(row.payload_json), title: titles[i] }), row.thread_id))
  db.close()
  return join(dir, "statev2.sqlite")
}
const AGREE = [
  "wide \u65e5\u672c\u8a9e and \ud55c\uad6d\uc5b4",
  "family \u{1f468}\u200d\u{1f469}\u200d\u{1f467}\u200d\u{1f466} done",
  "bread \u{1f35e}\u{1f35e}\u{1f35e} loaves",
  "technologist \u{1f9d1}\u{1f4bb} \u00b7 caf\u00e9 na\u00efve \u00d7 \u2713",
]
// ── Clusters ──────────────────────────────────────────────────────────────────
// The titles are in cluster-titles.ts, shared with terminal-parity.ts.
// Keys that move the selection across the cluster titles (each row changes style twice), open and
// close the help overlay over them, and leave the last screen with a cluster title selected.
const CLUSTER_KEYS = ["j", "j", "j", "j", "j", "j", "j", "?", "?", "j", "j", "j", "j"]
// Every profile both programs run in. tmux-env is a plain terminal in which the original moves the
// cursor after every cluster; unsupported one in which it trusts the terminal's advance.
const CLUSTER_PROFILES = ["foot-2027-reset", "foot-2027-set", "unsupported", "tmux-env", "ghostty", "ghostty-stock", "kitty-ssh"]
async function clusterScenarios() {
  const db = titledDb("clusters", CLUSTERS)
  clusterTally = { cells: 0 }
  for (const name of CLUSTER_PROFILES) {
    const { node } = await scenario(name, CLUSTER_KEYS, ["--db", db], " clusters")
    // Not a vacuous pass: the original wrote clusters whole (a skin-tone sequence, a flag pair).
    // In foot they land in one cell; a terminal without explicit widths puts each code point in a
    // cell of its own, so there the bytes are what show it.
    const seen = node.snapshots.reduce((n, snap) => n + snap.grid.filter((c) => [...c.ch].length > 1).length, 0)
    clusterCellsSeen += seen
    const whole = ["\u{1f44d}\u{1f3fd}", "\u{1f1ef}\u{1f1f5}"].every((t) => node.raw.includes(Buffer.from(t)))
    if (!node.error && (!whole || (PROFILES[name]!.osc66 && seen === 0))) differences.push(`${name} clusters: the original wrote no cluster whole; the titles test nothing`)
  }
  clusterCells = clusterTally.cells
  clusterTally = null
}

const KEYS: string[] = process.env.T3_CAPS_KEYS ? JSON.parse(process.env.T3_CAPS_KEYS) : ["?", "?", "j", "x", "x"]

// ── Bracketed paste ───────────────────────────────────────────────────────────
// Both programs switch mode 2004 on when they start (even in a terminal that does not know it, or
// answers nothing) and off on every way out, and neither does anything with a paste: the bytes between
// ESC[200~ and ESC[201~ are dropped, whatever they hold and however many reads they come in. This
// terminal can cut a write wherever it likes, which tmux cannot, so it carries the cases tmux cannot
// time: a start marker cut between reads, and a paste whose end marker never comes.
//
// Each case: start, then steps (writes with pauses), a snapshot of the screen after each, then the end
// marker (a stray one is nothing to a program that is not pasting) and `q`. The step after each paste
// is a `j`, so state a paste changed without showing it comes out on the next screen.
const PASTE_S = "\x1b[200~"
const PASTE_E = "\x1b[201~"
// How long the Rust key parser waits with no byte before it gives up on a paste (keys.rs, PASTE_QUIET).
const PASTE_QUIET_MS = 2000

// A write is a string; a number is a pause in milliseconds.
type PasteStep = { name: string; write: (string | number)[]; frame?: boolean; signal?: NodeJS.Signals }
type PasteCase = { name: string; profile: string; steps: PasteStep[]; endWith?: "q" | "signal" }

async function runPaste(side: "node" | "rust", c: PasteCase): Promise<Run & { alive: boolean[]; pid: number | null; leaked: boolean }> {
  const profile = PROFILES[c.profile]!
  const term = new FakeTerminal(profile, COLS, ROWS)
  const env: Record<string, string> = {
    PATH: `${stubs}:${process.env.PATH ?? "/usr/bin"}`,
    HOME: home,
    T3CODE_HOME: t3home,
    XDG_RUNTIME_DIR: runtime,
    TZ: "UTC",
    LANG: "C.UTF-8",
    T3_FIXED_NOW: String(NOW),
    // The original's theme behavior; the product's themes are tested in crates/term/tests/pty.rs.
    LOUPE_NO_THEMES: "1",
    TERM: "xterm-256color",
    COLORTERM: "truecolor",
    COLUMNS: String(COLS),
    LINES: String(ROWS),
  }
  // The program's own process id goes through the pty, so a step can signal it.
  const command = `stty cols ${COLS} rows ${ROWS} -echo; cd ${quote(REPO)}; echo PID:$$; exec ${argvFor(side, ["--demo"]).map(quote).join(" ")}`
  const child = spawn("script", ["-qefc", command, "/dev/null"], { env, stdio: ["pipe", "pipe", "pipe"] })
  const chunks: Buffer[] = []
  let lastData = performance.now()
  let exited = false
  let exitCode: number | null = null
  let pid: number | null = null
  child.stdout.on("data", (chunk: Buffer) => {
    if (pid === null) pid = Number(/PID:(\d+)/.exec(chunk.toString("latin1"))?.[1] ?? NaN) || null
    chunks.push(chunk)
    lastData = performance.now()
    for (const reply of term.feed(chunk)) if (!exited) child.stdin.write(reply)
  })
  child.stderr.on("data", () => {})
  child.stdin.on("error", () => {})
  child.on("close", (code) => {
    exited = true
    exitCode = code
  })
  const settle = async (quiet: number, limit = 20_000) => {
    const start = performance.now()
    while (performance.now() - start < limit) {
      await sleep(40)
      if (exited) return false
      if (term.frames > 0 && performance.now() - lastData >= quiet) return true
    }
    return false
  }
  const snapshots: Snapshot[] = []
  const alive: boolean[] = []
  let error: string | undefined
  let leaked = false
  const snap = (step: string) => {
    snapshots.push({ step, grid: term.grid(), modes: term.stateAtRest(), frames: term.frames })
    alive.push(!exited)
  }
  try {
    if (!(await settle(700))) error = "start: no settled frame"
    else {
      snap("start")
      for (const st of c.steps) {
        if (exited) break
        const framesBefore = term.frames
        if (st.signal && pid) process.kill(pid, st.signal)
        for (const piece of st.write) {
          if (typeof piece === "number") await sleep(piece)
          // A terminal sends the markers only to a program that has switched mode 2004 on.
          else child.stdin.write(term.modes.get(2004) ? piece : piece.split(PASTE_S).join("").split(PASTE_E).join(""))
        }
        if (st.signal) {
          const start = performance.now()
          while (!exited && performance.now() - start < 6000) await sleep(20)
          break
        }
        // A key makes a frame (calm mode draws at 2 fps, so it can take a while). A paste makes none:
        // wait long enough for one to show if the program wrongly acted on it.
        if (st.frame) {
          const sent = performance.now()
          while (term.frames === framesBefore && performance.now() - sent < 4000 && !exited) await sleep(40)
          await settle(700)
        } else {
          await sleep(1500)
          await settle(400)
        }
        snap(st.name)
      }
      leaked = exited
    }
    if (!exited && c.endWith !== "signal") {
      child.stdin.write((term.modes.get(2004) ? PASTE_E : "") + "q")
      const start = performance.now()
      while (!exited && performance.now() - start < 8000) await sleep(20)
      if (!exited) error = (error ? error + "; " : "") + "did not exit on the end marker and q"
    }
  } finally {
    if (!exited) child.kill("SIGKILL")
  }
  await sleep(50)
  return {
    side,
    profile: c.profile,
    snapshots,
    exit: { code: exitCode, state: term.stateAtRest() },
    modeLog: term.modeLog,
    raw: Buffer.concat(chunks),
    firstFrameMs: null,
    notes: term.unwrapped,
    oscWidths: {},
    error,
    alive,
    pid,
    leaked,
  }
}

// Mode 2004 in the order the terminal saw it: on after the alternate screen, off before it is left.
function check2004(label: string, r: Run) {
  cases++
  const log = r.modeLog.map((m, i) => ({ ...m, i }))
  const on = log.filter((m) => m.mode === 2004 && m.on)
  const off = log.filter((m) => m.mode === 2004 && !m.on)
  const enter = log.find((m) => m.mode === 1049 && m.on)
  const leave = [...log].reverse().find((m) => m.mode === 1049 && !m.on)
  const problems: string[] = []
  if (!on.length) problems.push("never switched mode 2004 on")
  if (!off.length) problems.push("never switched mode 2004 off")
  if (on.length && enter && on[0]!.i < enter.i) problems.push("switched 2004 on before the alternate screen")
  if (on.length && off.length && off[off.length - 1]!.i < on[on.length - 1]!.i) problems.push("2004 is on at the end of the log")
  if (off.length && leave && off[off.length - 1]!.i > leave.i) problems.push("2004 switched off after the alternate screen was left")
  if (problems.length) differences.push(`${label}: ${problems.join("; ")}`)
}

const same2 = (a: Cell[], b: Cell[]) => a.every((c, i) => c.ch === b[i]!.ch && c.fg === b[i]!.fg && c.bg === b[i]!.bg && c.bold === b[i]!.bold && c.explicit === b[i]!.explicit)

const PASTE_ANSWERS = "\x1b[1;2R\x1b[?2027;2$y\x1b[?62;4c"
const ESCAPES = "\x1b[A\x1b[B\x1bOA\x1b[5~" + PASTE_ANSWERS + "\x1b[<0;12;4M\x1b[<65;3;8M\x1b[<64;3;8m\x1b[I\x1b[O\x1b]52;c;AAAA\x07\x1b[200~qq\x1bq\x03"
const J: PasteStep = { name: "j", write: ["j"], frame: true }
const paste = (name: string, body: string): PasteStep[] => [{ name, write: [PASTE_S + body + PASTE_E] }, { ...J, name: `${name} then j` }]
const CAPS_PASTE_CASES: PasteCase[] = [
  { name: "letters", profile: "foot-2027-reset", steps: paste("paste qqqq", "qqqq jjkk? xnapt fhG") },
  { name: "multi-line", profile: "foot-2027-reset", steps: paste("paste lines", "q\nq\r\nj\n\r\nj\n\n") },
  { name: "escapes and the probe's answers", profile: "foot-2027-reset", steps: paste("paste escapes", ESCAPES) },
  { name: "very long", profile: "foot-2027-reset", steps: paste("paste 1 MiB", "jq? \n".repeat(209_716)) },
  { name: "keys right after", profile: "foot-2027-reset", steps: [{ name: "paste then j", write: [PASTE_S + "qq" + PASTE_E + "j"], frame: true }, { ...J, name: "j again" }] },
  { name: "two pastes in one read", profile: "foot-2027-reset", steps: paste("two pastes", "qq" + PASTE_E + PASTE_S + "jj") },
  // In pieces: the body in reads 300 ms apart, and the end marker cut after every byte of it.
  ...[1, 2, 3, 4, 5].map((cut) => ({
    name: `end marker cut after ${cut} bytes, 300 ms between reads`,
    profile: "foot-2027-reset",
    steps: [{ name: "paste in pieces", write: [PASTE_S + "qq", 300, "qq" + PASTE_E.slice(0, cut), 300, PASTE_E.slice(cut)] }, { ...J, name: "j" }],
  })),
  // The start marker whole, then the body and the end marker a byte per read.
  { name: "one byte per read after the start marker, 3 ms apart", profile: "foot-2027-reset", steps: [{ name: "paste byte by byte", write: [PASTE_S, ...[..."qq\x1bq\x03" + PASTE_E].flatMap((x) => [3, x])] }, { ...J, name: "j" }] },
  // The same pastes in terminals that do not answer or do not know mode 2004: the original still
  // switches it on, and so must the Rust build.
  { name: "letters, unsupported terminal", profile: "unsupported", steps: paste("paste qqqq", "qqqq jjkk? xnapt fhG") },
  { name: "letters, silent terminal", profile: "silent", steps: paste("paste qqqq", "qqqq jjkk? xnapt fhG") },
]
// A start marker cut between two reads a few milliseconds apart is still a start marker, in both
// (the original waits 20 ms for the rest of a sequence). The wait is not the same in the two and the
// machine is not quiet, so the gap is short and a run in which even the original gave up is run again.
const START_CUTS: PasteCase[] = [1, 2, 3, 4, 5].map((cut) => ({
  name: `start marker cut after ${cut} bytes, 5 ms between reads`,
  profile: "foot-2027-reset",
  steps: [{ name: "paste in pieces", write: [PASTE_S.slice(0, cut), 5, PASTE_S.slice(cut) + "qqqq jjkk?" + PASTE_E] }, { ...J, name: "j" }],
}))

async function pasteCase(c: PasteCase, attempts = 1) {
  const label = `paste ${c.profile} ${c.name}`
  let node!: Awaited<ReturnType<typeof runPaste>>
  let rust!: Awaited<ReturnType<typeof runPaste>>
  for (let attempt = 1; attempt <= attempts; attempt++) {
    ;[node, rust] = await Promise.all([runPaste("node", c), runPaste("rust", c)])
    // The original gave up on the cut marker (a slow machine): the run says nothing about either.
    if (!node.leaked || attempt === attempts) break
  }
  for (const r of [node, rust]) if (r.error) errors.push(`${label} ${r.side}: ${r.error}`)
  if (node.error || rust.error) return { node, rust }
  if (node.leaked) {
    errors.push(`${label}: the original quit during the case${attempts > 1 ? `, ${attempts} times (a start marker cut by a few milliseconds that the machine was too slow to deliver)` : ""}`)
    return { node, rust }
  }
  if (rust.leaked) {
    differences.push(`${label}: Rust quit during the case; the original did not`)
    return { node, rust }
  }
  if (node.snapshots.length !== rust.snapshots.length) {
    differences.push(`${label}: ${node.snapshots.length} settled screens from the original, ${rust.snapshots.length} from Rust`)
    return { node, rust }
  }
  node.snapshots.forEach((snapshot, i) => compareGrids(`${label} ${snapshot.step}`, snapshot.grid, rust.snapshots[i]!.grid))
  // The check has to be able to fail: the original must have ignored the paste, and acted on the `j`.
  const start = node.snapshots[0]!.grid
  c.steps.forEach((step, i) => {
    const grid = node.snapshots[i + 1]!.grid
    const wantSame = !step.frame
    cases++
    if (same2(grid, start) !== wantSame) differences.push(`${label}: the original's screen after "${step.name}" ${wantSame ? "changed" : "did not change"}; the check has nothing to compare`)
  })
  for (const r of [node, rust]) {
    check2004(`${label} ${r.side}`, r)
    checkExit(`${label} exit ${r.side}`, r, PROFILES[c.profile]!.initial, r.side === "node" ? "either" : "untouched")
  }
  const sameExit = node.exit.code === rust.exit.code
  cases++
  if (!sameExit) differences.push(`${label}: exit code ${node.exit.code} from the original, ${rust.exit.code} from Rust`)
  return { node, rust }
}

// A paste that never ends. The original discards every byte until the end marker, for as long as it
// takes, and a lost one leaves it deaf to the keyboard (Ctrl-C included; only a signal ends it). The
// Rust build gives up after PASTE_QUIET_MS without a byte. That one difference is deliberate: it is
// asserted here to be exactly that, and anything else fails.
async function lostEndMarker() {
  const profile = "foot-2027-reset"
  const label = `paste ${profile} lost end marker`
  const lost: PasteCase = {
    name: "lost end marker",
    profile,
    steps: [
      { name: "start marker, no end", write: [PASTE_S + "qqq"] },
      // Inside the bound: swallowed by both.
      { name: "j 400 ms later", write: [400, "j"], frame: false },
      // Past it, counted from that last byte.
      { name: "j after the bound", write: [PASTE_QUIET_MS + 600, "j"], frame: true },
      { name: "Ctrl-C, escape, a click", write: ["\x03", 100, "\x1b", 200, "\x1b[<0;12;4M"], frame: false },
    ],
  }
  const control: PasteCase = { name: "control", profile, steps: [{ name: "j", write: ["j"], frame: true }] }
  const [node, rust, ref] = await Promise.all([runPaste("node", lost), runPaste("rust", lost), runPaste("node", control)])
  for (const r of [node, rust, ref]) if (r.error) errors.push(`${label} ${r.side}: ${r.error}`)
  if (node.error || rust.error || ref.error) return
  const start = node.snapshots[0]!.grid
  const afterJ = ref.snapshots[1]!.grid
  const nodeAt = (name: string) => node.snapshots.find((s) => s.step === name)?.grid
  const rustAt = (name: string) => rust.snapshots.find((s) => s.step === name)?.grid
  cases++
  // Inside the bound both swallow what is typed.
  for (const [side, run] of [["original", nodeAt], ["rust", rustAt]] as const) {
    const g = run("j 400 ms later")
    if (!g || !same2(g, start)) differences.push(`${label}: ${side} acted on a key typed 400 ms into a paste`)
  }
  // The original is still deaf after the bound; Rust has its keyboard back.
  const a = nodeAt("j after the bound")
  const b = rustAt("j after the bound")
  cases++
  if (!a || !same2(a, start)) differences.push(`${label}: the original acted on a key typed ${PASTE_QUIET_MS + 600} ms after the paste went quiet; it was expected to wait for the end marker`)
  else if (!b || !same2(b, afterJ)) differences.push(`${label}: Rust did not act on a key typed ${PASTE_QUIET_MS + 600} ms after the paste went quiet`)
  else deliberate.push(`${label}: the original ignores the keyboard from a start marker until its end marker, however long that takes (a key typed ${PASTE_QUIET_MS + 600} ms after the last byte did nothing); the Rust build gives up on a paste after ${PASTE_QUIET_MS} ms without a byte and acts on that key`)
  // Both are alive until the end marker and `q`, and both exit cleanly with nothing left on.
  if (node.alive.length < 3 || !node.alive[node.alive.length - 1]) differences.push(`${label}: the original quit before the end marker; it should be swallowing everything`)
  for (const r of [node, rust]) {
    check2004(`${label} ${r.side}`, r)
    checkExit(`${label} exit ${r.side}`, r, PROFILES[profile].initial, r.side === "node" ? "either" : "untouched")
  }

  // A signal still ends both, in the middle of a paste, with the modes put back.
  for (const signal of ["SIGTERM", "SIGHUP"] as const) {
    const sig: PasteCase = { name: `lost end marker, then ${signal}`, profile, endWith: "signal", steps: [{ name: "start marker, no end", write: [PASTE_S + "qqq", 300] }, { name: signal, write: [], signal }] }
    const [n, r] = await Promise.all([runPaste("node", sig), runPaste("rust", sig)])
    const l = `paste ${profile} ${sig.name}`
    for (const x of [n, r]) if (x.error) errors.push(`${l} ${x.side}: ${x.error}`)
    if (n.error || r.error) continue
    cases++
    if (n.exit.code !== r.exit.code) differences.push(`${l}: exit code ${n.exit.code} from the original, ${r.exit.code} from Rust`)
    for (const x of [n, r]) {
      if (x.exit.code === null) differences.push(`${l}: ${x.side} did not exit`)
      check2004(`${l} ${x.side}`, x)
      checkExit(`${l} exit ${x.side}`, x, PROFILES[profile].initial, x.side === "node" ? "either" : "untouched")
    }
  }
}

async function pasteSection() {
  for (const c of CAPS_PASTE_CASES) await pasteCase(c)
  for (const c of START_CUTS) await pasteCase(c, 3)
  await lostEndMarker()
}

async function main() {
  const probe = spawnSync("script", ["--version"], { encoding: "utf8" })
  if (probe.error) throw new Error("util-linux `script` is needed for the pseudo-terminal")
  if (process.argv.includes("--startup")) {
    if (!baselineBin) throw new Error("--startup needs T3_CAPS_BASELINE_BINARY")
    await startup(Number(process.argv[process.argv.indexOf("--startup") + 1] ?? 7))
    rmSync(work, { recursive: true, force: true })
    return
  }
  // T3_CAPS_ONLY=clusters: only the cluster scenarios, for looking by hand.
  if (process.env.T3_CAPS_ONLY === "clusters") {
    await clusterScenarios()
    return finish()
  }
  for (const name of COLOR_PROFILES) await scenario(name, KEYS)
  if (process.env.T3_CAPS_ONLY === "colors") return finish()
  // 1. Terminals that support it, with mode 2027 found off and found on.
  const ran: Record<string, { node: Run; rust: Run }> = {}
  for (const name of ["foot-2027-reset", "foot-2027-set"]) ran[name] = await scenario(name, KEYS)
  // 1b. Wide text and Latin-1 in thread titles, from a database of our own.
  await scenario("foot-2027-reset", ["j"], ["--db", titledDb("agree", AGREE)], " titles")
  // 1c. Clusters: see the section above. Gated like every other screen.
  await clusterScenarios()
  // 2. A terminal that says "not supported": the Rust build writes what it wrote before it probed.
  ran.unsupported = await scenario("unsupported", KEYS)
  // 3. A terminal that answers nothing: only the Rust build, which must not wait for the original's backstop.
  const silent = await runProgram("rust", "silent", KEYS)
  if (silent.error) errors.push(`silent rust: ${silent.error}`)
  else {
    const plain = ran.unsupported!.rust
    if (!plain.error && silent.snapshots.length === plain.snapshots.length) {
      silent.snapshots.forEach((s, i) => compareGrids(`silent vs unsupported ${s.step}`, s.grid, plain.snapshots[i]!.grid, "silent", "unsupported"))
    }
    checkExit("silent exit rust", silent, PROFILES.silent!.initial, "untouched")
    if (silent.modeLog.some((m) => m.mode === 2027)) differences.push("silent: mode 2027 touched in a terminal that never said it knows it")
    if (silent.snapshots.some((s) => s.grid.some((c) => c.explicit))) differences.push("silent: explicit-width cells in a terminal that answered nothing")
    if (bare(silent).includes("\x1b]66;w=2") || bare(silent).replace("\x1b]66;w=1; \x1b\\", "").includes("\x1b]66;")) differences.push("silent: OSC 66 written to a terminal that never said it supports it")
  }
  // 3b. Answers that are slow and arrive in pieces. Inside the presenter's wait they still count
  // (the screen ends up as the original draws it in foot); past it the terminal counts as one that
  // did not answer (the screen ends up as in the unsupported profile). Either way no piece of an
  // answer is a key: the help overlay does not open, the program does not quit, and every screen
  // after a key is the one the keys alone give.
  for (const [name, counts, reference] of [
    ["foot-slow", true, ran["foot-2027-reset"]!.node],
    ["foot-late", false, ran.unsupported!.rust],
  ] as const) {
    const r = await runProgram("rust", name, KEYS)
    if (r.error) {
      errors.push(`${name} rust: ${r.error}`)
      continue
    }
    if (reference.error || r.snapshots.length !== reference.snapshots.length) {
      differences.push(`${name}: ${r.snapshots.length} settled screens, ${reference.snapshots.length} in the run it should match`)
      continue
    }
    r.snapshots.forEach((snap, i) => compareGrids(`${name} ${snap.step}`, reference.snapshots[i]!.grid, snap.grid, counts ? "original in foot" : "unsupported", `rust ${name}`))
    const drawn = Object.keys(r.oscWidths).length > 0
    if (drawn !== counts) differences.push(`${name}: explicit widths ${drawn ? "used" : "not used"}, expected ${counts ? "used" : "not used"}`)
    const switched = r.modeLog.some((m) => m.mode === 2027 && m.on)
    if (switched !== counts) differences.push(`${name}: mode 2027 ${switched ? "switched on" : "left alone"}, expected ${counts ? "switched on" : "left alone"}`)
    checkExit(`${name} exit rust`, r, PROFILES[name]!.initial, "untouched")
  }
  // 4. Byte-for-byte against a build from before the probe, when there is one.
  if (baselineBin) {
    for (const name of ["unsupported", "silent"]) {
      const before = await runProgram("baseline", name, KEYS)
      const after = name === "silent" ? silent : ran.unsupported!.rust
      cases++
      if (before.error || after.error) errors.push(`baseline ${name}: ${before.error ?? after.error}`)
      // The probe is taken out of both: the build before may be one from before the probe existed,
      // or one that already sends it.
      else if (!withoutProbe(after.raw).equals(withoutProbe(before.raw))) {
        const a = withoutProbe(after.raw)
        before.raw = withoutProbe(before.raw)
        let at = 0
        while (at < a.length && a[at] === before.raw[at]) at++
        differences.push(`${name}: the Rust output without the probe and the paste switches differs from the build before it at byte ${at} (${a.length} vs ${before.raw.length} bytes): ${JSON.stringify(a.subarray(Math.max(0, at - 20), at + 30).toString("latin1"))} vs ${JSON.stringify(before.raw.subarray(Math.max(0, at - 20), at + 30).toString("latin1"))}`)
      }
    }
  } else notes.push("T3_CAPS_BASELINE_BINARY not set: the unsupported and silent profiles were not compared byte for byte with a build from before the probe")

  // 5. Bracketed paste: dropped unread in both, and mode 2004 on and off in both.
  await pasteSection()

  return finish()
}

function finish() {
  const report = {
    version: 1,
    cases,
    cells_compared: cellsCompared,
    explicit_width_cells_seen: explicitCells,
    differences: differences.length,
    errors: errors.length,
    deliberate: deliberate.length,
    open_cluster_cells: clusterCells,
    blank_foreground_cells_let_through: blankForegroundCells,
    cluster_cells_seen: clusterCellsSeen,
    profiles_skipped_for_a_slow_start: slowStarts.length,
  }
  // A machine too slow for the original to start the usual way in any attempt has compared
  // nothing in those terminals: that is not a pass.
  if (slowStarts.length > 2) errors.push(`the original started slowly in every attempt of ${slowStarts.length} profiles (${slowStarts.join(", ")}): this machine is too slow to compare terminals without explicit widths`)
  for (const item of notes) console.error("note: " + item)
  for (const item of [...new Set(deliberate)]) console.error("deliberate: " + item)
  for (const item of differences) console.error("DIFFERENCE: " + item)
  for (const item of errors) console.error("ERROR: " + item)
  console.log(JSON.stringify(report))
  rmSync(work, { recursive: true, force: true })
  if (differences.length || errors.length) process.exit(1)
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.stack : err)
    rmSync(work, { recursive: true, force: true })
    process.exit(1)
  })
}
