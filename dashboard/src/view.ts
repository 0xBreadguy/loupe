// Layout and drawing. Pure: (canvas, state, data, palette, time) -> pixels.
// Encoding rules: shape + brightness carry state (works in monochrome themes);
// the theme's hot color is reserved for "needs you", failures and limits.
import { BIG_ROWS, BOLD, Braille, Canvas, bigWidth, clean, drawBig, drawFont, fit, hbar, level, textWidth } from "./canvas.ts"
import { duration, shortModel, tokens, wrap } from "./format.ts"
import { t3Thread } from "./focus.ts"
import { LIVE, SLOW_TOOL_MS, SourceError, isLive, isRunning, ms, needsYou, request, waitingSince, pace, tree, category, type Agent, type Item, type Pace, type Snapshot,
  type SourceProblem, type TreeRow, tilde } from "./source.ts"
import { TEXT_CONTRAST, family, mix, ramp, reach, type Color, type Family, type Palette } from "./theme.ts"
import type { Provider } from "./usage.ts"

export const WINDOWS = [15, 30, 60, 180] // lane window, minutes

export interface ViewState {
  selected: string | null
  collapsed: Set<string>
  activeOnly: boolean
  paused: boolean
  help: boolean
  helpScroll: number
  fx: boolean
  detailScroll: number
  listScroll: number
  windowIndex: number
  error: string | null // the source's last error; the last snapshot, if any, stays on screen
  cause: Trouble | null // what that error was, for the advice in the agent list
  interval: number
  bootAt: number
  lastFrame: number
  tween: Map<string, number>
  firstSeen: Map<string, number> // tool item -> when a snapshot first brought it; 0: there from the start
  seeded: boolean // the first snapshot has been observed, so later items are new arrivals
  shimmer: Map<string, number> // agent -> when its newest tool call arrived, for a one-shot name shimmer
  calm: boolean // no decorative motion: still spinners, no tweens, wipe, shimmer or CRT, idle frame rate
  frozenAt: number | null // help is open: the moment the dashboard underneath stopped
  t3Title: string | null // title of the T3 Code window, null when unknown or closed
  t3Linked: string | null // the thread that title names, as last matched
  t3Link: boolean // Hyprland is reachable, so the link can work
  follow: boolean // select the thread T3 Code shows whenever it changes
  notice: { text: string; at: number; error: boolean } | null // a short footer message
  demo: boolean // made-up data: say so in the header
  picked: { id: string; at: number } | null // the user's last deliberate selection, and when
  waits: Map<string, number> // agent waiting on you -> when this dashboard first saw it waiting
  acked: Set<string> // waiting agents the user has seen since they began waiting: their ◆ holds still
}

export function initialState(interval = 2): ViewState {
  return { selected: null, collapsed: new Set(), activeOnly: false, paused: false, help: false, helpScroll: 0, fx: false,
    detailScroll: 0, listScroll: 0, windowIndex: 1, error: null, cause: null, interval, bootAt: Date.now(), lastFrame: 0,
    tween: new Map(), firstSeen: new Map(), seeded: false, shimmer: new Map(), calm: false, frozenAt: null,
    t3Title: null, t3Linked: null, t3Link: false, follow: true, notice: null, demo: false,
    picked: null, waits: new Map(), acked: new Set() }
}

/** Why T3's database could not be read: SourceError's classification, or "other". */
export interface Trouble { kind: SourceProblem | "other"; path: string; detail: string }

export function trouble(err: unknown, path: string): Trouble {
  if (err instanceof SourceError) return { kind: err.kind, path: err.path, detail: err.detail }
  return { kind: "other", path, detail: err instanceof Error ? err.message : String(err) }
}

export interface Rect { x: number; y: number; w: number; h: number }
export interface Hit { y: number; x0: number; x1: number; id: string; area: "list" | "detail" | "tile" }
export const NEEDS_YOU_TILE = "needs-you" // the id of the NEEDS YOU tile's hits
export interface Frame {
  rows: TreeRow[]
  hits: Hit[]
  list: Rect
  detail: Rect
  detailMax: number // maximum detail scroll
  helpMax: number // maximum help scroll: the overlay is taller than the window
  beat: boolean // spinners or a fading flash on screen: frames at the spinner beat
  smooth: boolean // a tween, name shimmer or boot wipe in flight: a higher frame rate, briefly
  linked: Set<string> // the thread open in T3 Code and everything under it
  paces: Map<string, Pace> // running agents: busy in a tool, working, or quiet
}

// ── Formatting ──────────────────────────────────────────────────────────────

export { duration, shortModel, tokens } from "./format.ts"

const since = (stamp: string | null | undefined, now: number): number | null => {
  const t = ms(stamp)
  return t === null ? null : (now - t) / 1000
}

const ratio = (a: Agent): number | null => {
  const { usedTokens: used, maxTokens: max } = a.context
  return typeof used === "number" && typeof max === "number" && max > 0 ? Math.max(0, Math.min(1, used / max)) : null
}

/** Provider family of an agent (Claude / OpenAI / Grok / other). */
export const familyOf = (a: Agent): Family => family(a.model, a.provider_id ?? a.run?.provider)
const usageFamily = (id: string): Family => family(id)
export const FAMILY_LABEL: Record<Family, string> = { claude: "claude", openai: "gpt · codex", grok: "grok", other: "other" }

const home = process.env.HOME ?? ""
const tidy = (s: string): string => clean(s.split("\n")[0]).replace(home ? new RegExp("^" + home.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")) : /^$/, "~").trim()

export function itemLabel(i: Item): string {
  if (i.type === "command_execution") return tidy(i.command) || "command"
  if (i.type === "file_change") return tidy(i.file) || "file change"
  if (i.type === "reasoning") return "thinking"
  if (i.type === "assistant_message") return "writing reply"
  return tidy(i.label).replace(home, "~") || i.type.replace(/_/g, " ")
}

// Every glyph the dashboard draws exists in JetBrainsMono Nerd Font, Omarchy's
// default terminal font (test/glyphs.test.ts keeps it so): no fallback-font
// symbols of another weight or width. Tool categories:
export const GLYPH: Record<string, string> = {
  shell: "❯", read: "◎", edit: "±", web: "⊕", agent: "⇉", ask: "◆", think: "∴",
  reply: "≡", you: "»", note: "•", tool: "◈", other: "·",
}
const TOOL = (cat: string) => cat !== "you" && cat !== "think" && cat !== "reply"
const SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
export const BEAT_MS = 100 // one spinner frame
const STILL = "◉" // a running agent in calm mode: still, and unlike the quiet ◌
const QUEUED = "◔" // queued or starting: not working yet, so it does not move
const RETRY = "↩" // the source is retrying; a usage window starts over
/** One shared beat: every spinner shows the same frame (the bar widget uses the same formula). */
export const spinner = (now: number) => SPIN[Math.floor(now / BEAT_MS) % SPIN.length]
const spin = (now: number, calm: boolean) => (calm ? STILL : spinner(now))
const blink = (now: number, period = 1000) => now % period < period * 0.55
/** Running and getting on with it: the only agents whose marks may move. Quiet, queued and waiting ones stay still. */
const working = (a: Agent, pc: Pace | undefined) => (a.status === "running" || a.status === "active") && !needsYou(a) && !pc?.quiet

/** `seen`: the user has acknowledged this wait, so its ◆ holds still (as it always does in calm mode). */
export function statusOf(a: Agent, now: number, p: Palette, pc?: Pace, calm = false, seen = false): { glyph: string; color: Color; label: string } {
  if (needsYou(a)) return { glyph: calm || seen || blink(now) ? "◆" : "◇", color: p.hot, label: "NEEDS YOU" }
  // A quiet agent's spinner stops: motion means work is happening.
  if (pc?.quiet) return { glyph: "◌", color: pc.stalled ? p.warn : p.text2, label: "RUNNING" }
  switch (a.status) {
    case "running": case "active": return { glyph: spin(now, calm), color: pc?.stalled ? p.warn : p.bright, label: "RUNNING" }
    case "starting": case "preparing": case "queued": case "pending":
      return { glyph: QUEUED, color: p.text2, label: a.status.toUpperCase() }
    case "completed": return { glyph: "✓", color: p.dim, label: "DONE" }
    case "interrupted": return { glyph: "⊘", color: p.warn, label: "INTERRUPTED" }
    case "failed": case "error": return { glyph: "✗", color: p.hot, label: "FAILED" }
    default: return { glyph: "·", color: p.dim, label: a.status.toUpperCase() }
  }
}

/** Context fill color by position on the bar: brighter rightward, warm past 75%, hot past 90%. */
function zoneColor(p: Palette, t: number): Color {
  if (t >= 0.9) return p.hot
  if (t >= 0.75) return p.warn
  // The dimmest cell still reads at 3:1 against the track (p.floor); brighter from there.
  return ramp(p, p.floor + (Math.max(p.floor, 0.84) - p.floor) * (t / 0.75))
}

// The bar moves only when its value does (the tween); the fill itself is still.
function contextBar(c: Canvas, x: number, y: number, w: number, frac: number, p: Palette, track = p.track) {
  hbar(c, x, y, w, frac, { track, color: (i, cells) => zoneColor(p, (i + 0.5) / cells) })
}

export const SHIMMER_MS = 450
export const FLASH_MS = 1600

/**
 * A name with a one-shot highlight that sweeps across it once, easing out,
 * starting at `at` (when a new tool call arrived). Plain text otherwise.
 */
function shimmer(c: Canvas, x: number, y: number, s: string, base: Color, p: Palette, now: number, at: number | undefined, attr = 0, bg?: Color): number {
  const t = at === undefined ? 1 : (now - at) / SHIMMER_MS
  if (t < 0 || t >= 1) return c.text(x, y, s, base, { attr, bg })
  const chars = [...clean(s)]
  const pos = -4 + (1 - (1 - t) ** 3) * (chars.length + 8) // enters and leaves off the ends, so it never pops
  let col = 0
  chars.forEach((ch, i) => {
    const k = Math.max(0, 1 - Math.abs(i - pos) / 4)
    col += c.text(x + col, y, ch, mix(base, p.bright, k), { attr, bg })
  })
  return col
}

function section(c: Canvas, x: number, y: number, w: number, label: string, p: Palette, right = "", rightColor = p.dim) {
  const n = c.text(x, y, label, p.text2, { attr: BOLD })
  const rw = right ? textWidth(right) + 1 : 0
  for (let i = x + n + 1; i < x + w - rw - 1; i++) c.set(i, y, "─", p.rule)
  if (right) c.textRight(x + w, y, right, rightColor)
}

// ── Lanes: activity per time slice, tinted while a run was in progress ─────

/**
 * Time slices on the wall clock, for every chart with a time axis: each slice
 * is whole seconds long and starts at a multiple of its length, so a column
 * changes only when data arrives or a new slice begins, never just because a
 * frame was drawn a moment later. `col` maps a time to its column; the newest
 * slice, the one holding `now`, is the last column.
 */
export function slices(cols: number, windowMs: number, now: number): { size: number; col: (t: number) => number } {
  const size = Math.max(1000, Math.round(windowMs / Math.max(1, cols) / 1000) * 1000)
  const last = Math.floor(now / size)
  return { size, col: (t) => cols - 1 - (last - Math.floor(t / size)) }
}

interface LaneData { counts: number[]; fails: boolean[]; you: boolean[]; busy: boolean[] }

function laneData(a: Agent, w: number, windowMs: number, now: number): LaneData {
  const { col } = slices(w, windowMs, now)
  const d: LaneData = { counts: Array(w).fill(0), fails: Array(w).fill(false), you: Array(w).fill(false), busy: Array(w).fill(false) }
  for (const e of a.events) {
    const i = Math.min(w - 1, col(e.t)) // a clock a few seconds ahead still lands in the newest slice
    if (i < 0 || e.t > now + 5000) continue
    if (e.cat === "you") d.you[i] = true
    else d.counts[i]++
    if (e.failed) d.fails[i] = true
  }
  for (const [s, e] of a.spans) {
    const i0 = Math.max(0, col(s))
    const i1 = Math.min(w - 1, col(e ?? now))
    for (let i = i0; i <= i1; i++) d.busy[i] = true
  }
  return d
}

function drawLane(c: Canvas, x: number, y: number, d: LaneData, max: number, p: Palette, rowBg: Color, tint: Color | null) {
  const w = d.counts.length
  const tick = Math.max(1, Math.round(w / 6))
  // A provider-tinted band while a run is in progress; activity blends toward the provider hue.
  const busy = tint === null ? mix(rowBg, p.busy, 1) : mix(rowBg, tint, 0.16)
  for (let i = 0; i < w; i++) {
    const bg = d.busy[i] ? busy : rowBg
    const k = Math.min(1, d.counts[i] / max)
    // A failure has its own shape, so it reads without the hot color too.
    if (d.fails[i]) c.set(x + i, y, "×", p.hot, bg, BOLD)
    else if (d.you[i]) c.set(x + i, y, "┃", p.accent, bg)
    else if (d.counts[i]) c.set(x + i, y, level(d.counts[i], max), tint === null ? ramp(p, 0.35 + 0.65 * k) : mix(tint, p.bright, 0.15 + 0.35 * k), bg)
    else c.set(x + i, y, (w - 1 - i) % tick === 0 ? "·" : " ", p.faint, bg)
  }
}

// ── Main draw ───────────────────────────────────────────────────────────────

export function draw(c: Canvas, st: ViewState, snap: Snapshot, usage: Provider[], p: Palette, now: number): Frame {
  // Help open: the dashboard underneath stops where it was, and nothing on it moves.
  if (st.help) now = st.frozenAt ??= now
  else st.frozenAt = null
  c.clear(p.bg, p.fg)
  const W = c.w
  const H = c.h
  const dt = st.lastFrame ? Math.min(0.25, Math.max(0, (now - st.lastFrame) / 1000)) : 0
  st.lastFrame = now
  const frame: Frame = { rows: [], hits: [], list: { x: 0, y: 0, w: 0, h: 0 }, detail: { x: 0, y: 0, w: 0, h: 0 },
    detailMax: 0, helpMax: 0, beat: false, smooth: false, linked: new Set<string>(),
    paces: new Map<string, Pace>() }
  if (W < 48 || H < 14) {
    c.text(1, 0, "T3 Agents", p.bright, { attr: BOLD })
    c.text(1, 2, "Enlarge the terminal to at least 48 × 14.", p.dim)
    c.text(1, 3, "q quit", p.accent)
    return frame
  }
  const agents = snap.agents
  frame.linked = link(st, agents)
  frame.paces = paces(agents, now)
  frame.rows = tree(agents, st.collapsed, st.activeOnly)
  const ids = frame.rows.map((r) => r.agent.id)
  if (!st.selected || !ids.includes(st.selected)) {
    st.selected = ids[0] ?? null
    st.detailScroll = 0
  }
  // Tween every bar toward its reported value (first frame sweeps up from zero); calm mode jumps there.
  for (const a of agents) {
    const target = ratio(a) ?? 0
    const cur = st.tween.get(a.id) ?? 0
    const next = st.calm ? target : cur + (target - cur) * (1 - Math.exp(-dt * 7))
    if (Math.abs(target - next) < 0.0015) st.tween.set(a.id, target)
    else {
      st.tween.set(a.id, next)
      frame.smooth = true
    }
  }
  const running = agents.filter(isRunning) // the same count as --once and --status
  const waiting = agents.filter(needsYou)
  // T3 not read yet, or failing: nothing on screen may claim all is calm.
  const blind = !snap.at || st.error !== null
  // Spinners turn only for agents at work; a name shimmer plays once per new tool call.
  if (!st.calm) {
    for (const a of agents) {
      if (!working(a, frame.paces.get(a.id))) continue
      frame.beat = true
      const at = st.shimmer.get(a.id)
      if (at !== undefined && now >= at && now - at < SHIMMER_MS) frame.smooth = true
    }
  }
  const unseen = acknowledge(st, waiting, frame.linked, now)
  if (unseen.length && !st.calm) frame.beat = true // the "needs you" blink, until each wait is acknowledged

  header(c, st, snap, p, now, W)
  let y = 1
  y += kpis(c, y, W, H >= KPI_BIG_MIN_H, st, agents, running, waiting, unseen, usage, p, now, frame, blind)
  const limited = usage.filter((u) => u.limits.length)
  // Short windows (beside T3) keep LIMITS to one line, so the agents get the rows.
  if (limited.length) y += H < LIMITS_FULL_MIN_H ? limitsLine(c, y, W, limited, p) : limits(c, y, W, limited, p, now)
  const mainTop = y
  const mainH = H - 1 - mainTop
  const wide = W >= 140
  if (wide) {
    const lw = Math.floor(W * 0.56)
    frame.list = { x: 0, y: mainTop, w: lw, h: mainH }
    frame.detail = { x: lw + 2, y: mainTop, w: W - lw - 3, h: mainH }
    for (let yy = mainTop; yy < H - 1; yy++) c.set(lw, yy, "│", p.rule)
  } else {
    // The agent list comes first: as many agents as fit, two rows each when all of
    // them fit that way, leaving the details at least a few rows (they scroll).
    const room = Math.max(5, mainH - Math.max(7, Math.floor(mainH * 0.3)))
    const cards = frame.rows.length * 2 + 2
    const lh = !snap.at ? mainH : Math.max(5, cards <= room ? cards : Math.min(room, frame.rows.length + 2))
    frame.list = { x: 0, y: mainTop, w: W, h: lh }
    frame.detail = { x: 1, y: mainTop + lh, w: W - 2, h: mainH - lh }
  }
  const listEnd = list(c, frame, st, snap, p, now)
  // Wide layouts: spend the spare list-column rows on cross-agent views.
  if (wide && snap.at && mainTop + mainH - listEnd >= 8) extras(c, { x: 1, y: listEnd + 1, w: frame.list.w - 2, h: mainTop + mainH - listEnd - 1 }, st, agents, usage, p, now)
  const sel = agents.find((a) => a.id === st.selected)
  if (sel && frame.detail.h > 3) {
    const height = detail(c, frame.detail, sel, st, p, now, frame)
    frame.detailMax = Math.max(0, height - frame.detail.h)
    if (st.detailScroll > frame.detailMax) st.detailScroll = frame.detailMax
    // The details scroll (J K, wheel): say so when there is more above or below.
    const { x: dx, y: dy, w: dw, h: dh } = frame.detail
    const below = frame.detailMax - st.detailScroll
    if (below > 0) c.textRight(dx + dw, dy + dh - 1, ` ▼ ${below} more`, p.text2, { bg: p.bg })
    if (st.detailScroll > 0) c.textRight(dx + dw, dy, ` ▲ ${st.detailScroll} more`, p.text2, { bg: p.bg })
  }
  footer(c, st, snap, p, now, W, H)
  if (st.help) frame.helpMax = help(c, st, p, W, H)
  // Boot: a scan line wipes the dashboard in from the top (not in calm mode).
  // Clamped: a clock that steps back hours would otherwise push the edge so far
  // up that `yy++` stops changing a float and the tint below never ends.
  const boot = Math.max(0, (now - st.bootAt) / 650)
  if (boot < 1 && !st.calm) {
    const edge = Math.floor((1 - (1 - boot) ** 3) * H)
    c.tint(0, edge + 1, W, H, () => [p.bg, p.bg])
    for (let x = 0; x < W; x++) c.set(x, Math.min(H - 1, edge), "▔", mix(p.accent, p.bright, 0.5), c.bgAt(x, edge))
    frame.smooth = true
  }
  if (st.help) frame.beat = frame.smooth = false // frozen underneath
  return frame
}

/** Identity of a tool item across snapshots: its start stays put while it runs and when it completes. */
export const itemKey = (i: Item): string =>
  [i.thread_id, i.run_id, i.node_id, i.type, i.tool, i.started_at ?? i.updated_at, i.command, i.file].join("\u0000")

const feedItem = (i: Item) => i.type !== "reasoning" && i.type !== "assistant_message"

/**
 * Note what a new snapshot brought, for every agent at once: tool items it did
 * not have before flash in the activity feed, and an agent with a new tool
 * call plays its name shimmer once. Only snapshots make anything new, so
 * changing the selection or following T3 to another thread never flashes. The
 * first snapshot only records what is already there.
 */
export function observe(st: ViewState, snap: Snapshot, now: number) {
  const seen = new Map<string, number>()
  for (const a of snap.agents) {
    let call = false
    for (const i of a.items) {
      if (!feedItem(i)) continue
      const key = itemKey(i)
      const before = st.firstSeen.get(key) ?? seen.get(key)
      if (before !== undefined) seen.set(key, before)
      else {
        seen.set(key, st.seeded ? now : 0)
        if (st.seeded && TOOL(category(i.type, i.tool))) call = true
      }
    }
    if (call) st.shimmer.set(a.id, now)
  }
  st.firstSeen = seen // only what the snapshot still holds
  st.seeded = true
  for (const [id, at] of st.shimmer) if (now - at >= SHIMMER_MS) st.shimmer.delete(id)
}

/** Pace for every running agent; a parent with running subagents is waiting on them. */
function paces(agents: Agent[], now: number): Map<string, Pace> {
  const parent = new Map(agents.map((a) => [a.id, a.parent]))
  const waiting = new Set<string>()
  for (const a of agents) {
    if (!isLive(a)) continue
    const seen = new Set<string>()
    for (let up = parent.get(a.id) ?? null; up && !seen.has(up); up = parent.get(up) ?? null) {
      seen.add(up)
      waiting.add(up)
    }
  }
  const out = new Map<string, Pace>()
  for (const a of agents) {
    const pc = pace(a, now, waiting.has(a.id))
    if (pc) out.set(a.id, pc)
  }
  return out
}

/**
 * What a running agent is doing, for one line: the tool in flight with its
 * time, or how long it has been quiet. In three parts so that when the line
 * is cut short, the label gives way and the time stays whole (see fitDoing).
 */
export interface Doing { lead: string; label: string; tail: string; color: Color }

export const SLOW = "△" // a tool call past SLOW_TOOL_MS: a shape, not only the warm color

export function doing(a: Agent, pc: Pace | undefined, p: Palette): Doing | null {
  if (pc?.quiet) {
    const last = a.items.find((i) => i.type !== "reasoning" && i.type !== "assistant_message")
    return { lead: `◌ ${pc.stalled ? "stalled" : "quiet"} ${duration(pc.quietMs / 1000)}`,
      label: last ? ` · last ${GLYPH[category(last.type, last.tool)] ?? "·"} ${itemLabel(last)}` : "", tail: "", color: pc.stalled ? p.warn : p.text2 }
  }
  if (!a.now) return null
  const cat = category(a.now.type, a.now.tool)
  const time = pc?.tool && pc.toolMs >= 30_000 ? ` · ${pc.stalled ? SLOW + " " : ""}${duration(pc.toolMs / 1000)}` : ""
  return { lead: `${GLYPH[cat] ?? "·"} `, label: itemLabel(a.now), tail: time, color: pc?.stalled ? p.warn : p.dim }
}

/** A Doing in `w` columns: the label is shortened first, so the running time and the stall stay whole. */
export function fitDoing(d: Doing, w: number): string {
  const whole = d.lead + d.label + d.tail
  if (textWidth(whole) <= w) return whole
  const room = w - textWidth(d.lead) - textWidth(d.tail)
  if (room >= 4) return d.lead + fit(d.label, room) + d.tail
  return fit(d.lead + d.tail, w)
}

// ── Needs you ───────────────────────────────────────────────────────────────

/** A deliberate selection by the user (keys, click, n): unlike the default selection, it acknowledges a wait. */
export function pick(st: ViewState, id: string, now: number) {
  if (id !== st.selected) st.detailScroll = 0
  st.selected = id
  st.picked = { id, at: now }
}

/**
 * Track which waits the user has seen. A wait is acknowledged once the user
 * picks that agent after it began waiting, or its thread is the one open in T3;
 * landing on it as the default selection does not count. A wait that ends is
 * forgotten, so the next one blinks again. Returns the waits not yet seen.
 */
function acknowledge(st: ViewState, waiting: Agent[], linked: Set<string>, now: number): Agent[] {
  const ids = new Set(waiting.map((a) => a.id))
  for (const id of st.waits.keys()) {
    if (ids.has(id)) continue
    st.waits.delete(id)
    st.acked.delete(id)
  }
  for (const a of waiting) {
    if (!st.waits.has(a.id)) st.waits.set(a.id, now)
    if (linked.has(a.id) || (st.picked?.id === a.id && st.picked.at >= st.waits.get(a.id)!)) st.acked.add(a.id)
  }
  return waiting.filter((a) => !st.acked.has(a.id))
}

/**
 * Select the next agent waiting on you after the selection, in list order,
 * wrapping around, and unfold its parents so its row shows. Null when nobody waits.
 */
export function nextWaiting(st: ViewState, agents: Agent[], now: number): string | null {
  const order = tree(agents, new Set(), st.activeOnly).map((r) => r.agent) // as listed, with everything unfolded
  const at = order.findIndex((a) => a.id === st.selected)
  const next = order.slice(at + 1).find(needsYou) ?? order.find(needsYou)
  if (!next) return null
  const parent = new Map(agents.map((a) => [a.id, a.parent]))
  const seen = new Set<string>()
  for (let up = parent.get(next.id) ?? null; up && !seen.has(up); up = parent.get(up) ?? null) {
    seen.add(up)
    st.collapsed.delete(up)
  }
  pick(st, next.id, now)
  return next.id
}

// ── T3 Code link ────────────────────────────────────────────────────────────

/**
 * Follow the thread T3 Code shows. When it changes, unfold it and select it,
 * unless the selection is already one of its subagents. Returns the thread with
 * everything under it.
 */
function link(st: ViewState, agents: Agent[]): Set<string> {
  const id = t3Thread(st.t3Title, agents)
  const family = new Set<string>()
  if (id) {
    const children = new Map<string, string[]>()
    for (const a of agents) if (a.parent) children.set(a.parent, [...(children.get(a.parent) ?? []), a.id])
    const stack = [id]
    while (stack.length) {
      const at = stack.pop()!
      if (family.has(at)) continue
      family.add(at)
      stack.push(...(children.get(at) ?? []))
    }
  }
  if (id !== st.t3Linked) {
    st.t3Linked = id
    if (id && st.follow) {
      const parent = new Map(agents.map((a) => [a.id, a.parent]))
      const seen = new Set<string>()
      for (let at: string | null = id; at && !seen.has(at); at = parent.get(at) ?? null) {
        seen.add(at)
        st.collapsed.delete(at)
      }
      if (!st.selected || !family.has(st.selected)) {
        st.selected = id
        st.detailScroll = 0
      }
    }
  }
  return family
}

function header(c: Canvas, st: ViewState, snap: Snapshot, p: Palette, now: number, W: number) {
  c.fill(0, 0, W, 1, p.panel)
  // The product name as plain text: a badge would read as T3's own branding.
  let x = 1 + c.text(1, 0, "T3 Agents", p.bright, { attr: BOLD }) + 2
  if (st.paused) x += c.text(x, 0, "‖ PAUSED", p.warn, { attr: BOLD })
  else if (st.error) x += c.text(x, 0, st.calm || blink(now) ? `${RETRY} RETRYING` : "  RETRYING", p.hot, { attr: BOLD })
  else {
    // The dot breathes on each refresh: bright right after a snapshot, fading out. Calm mode holds it.
    const age = st.calm ? 1 : Math.min(1, (now - snap.at) / (st.interval * 1000))
    x += c.text(x, 0, "●", mix(p.bright, p.accent, age))
    x += c.text(x + 1, 0, `LIVE ${st.interval}s`, p.text2) + 1
  }
  if (st.calm) x += 2 + c.text(x + 2, 0, "calm", p.text2)
  if (st.demo) x += 1 + c.text(x + 1, 0, " DEMO ", p.bg, { bg: p.text2, attr: BOLD })
  const linked = st.t3Linked ? snap.agents.find((a) => a.id === st.t3Linked) : null
  const room = W - x - 34
  if (linked && room >= 12) {
    x += 1 + c.text(x + 1, 0, "▎", p.fg)
    x += c.text(x, 0, "T3 ", p.dim)
    c.text(x, 0, fit(linked.title + (st.follow ? "" : " · follow off"), room - 4), p.text2)
  }
  const clock = new Date(now).toTimeString().slice(0, 8)
  const win = `lane ${WINDOWS[st.windowIndex] >= 60 ? WINDOWS[st.windowIndex] / 60 + "h" : WINDOWS[st.windowIndex] + "m"}`
  c.textRight(W - 1, 0, clock, p.fg, { attr: BOLD })
  if (W - x > 30) c.textRight(W - 11, 0, (st.activeOnly ? "active only · " : "") + win, p.dim)
}

// Below this height the tiles show their values as bold text to leave room for agents.
const KPI_BIG_MIN_H = 30

function kpis(c: Canvas, y: number, W: number, big: boolean, st: ViewState, agents: Agent[], running: Agent[], waiting: Agent[],
  unseen: Agent[], usage: Provider[], p: Palette, now: number, frame: Frame, blind: boolean): number {
  const paces = frame.paces
  const windowMs = WINDOWS[st.windowIndex] * 60_000
  const all = agents.flatMap((a) => a.events)
  const recent = all.filter((e) => e.t >= now - 5 * 60_000 && TOOL(e.cat))
  const rate = recent.length / 5
  const fails = all.filter((e) => e.failed && e.t >= now - windowMs)
  const lastFail = fails.length ? Math.max(...fails.map((e) => e.t)) : null
  const peakPool = running.length ? running : agents
  const peak = peakPool.reduce<Agent | null>((best, a) => ((ratio(a) ?? -1) > (best ? ratio(best) ?? -1 : -1) ? a : best), null)
  const peakRatio = peak ? ratio(peak) : null
  const today = usage.reduce((n, u) => n + u.today, 0)

  // A value is news (big digits), zero (small and dim: nothing to look at) or
  // missing (a small dash, with the reason underneath). Big bright numbers mean "look here".
  type Tile = { label: string; value: string; color: Color; quiet?: boolean; sub: (x: number, y: number, w: number) => void; alert?: boolean }
  const note = (text: string, color = p.dim) => (x: number, yy: number, w: number) => void c.text(x, yy, fit(text, w), color)
  const zero = (label: string, value: string, sub: Tile["sub"]): Tile => ({ label, value, color: p.dim, quiet: true, sub })
  const missing = (label: string, why: string, color = p.dim): Tile => ({ label, value: "—", color: p.dim, quiet: true, sub: note(why, color) })
  // Unread T3 must never pass for a calm one: no zeros, just a dash and why.
  const unread = (label: string) => missing(label, "can't read T3", p.text2)
  const tiles: Tile[] = [
    blind ? unread("ACTIVE") : !running.length ? zero("ACTIVE", "0", note("nothing running")) : { label: "ACTIVE", value: String(running.length), color: p.bright, sub: (x, yy, w) => {
      // One mark per agent, as in its row: a spinner while it works (all on one beat),
      // a still ◌ once it goes quiet, a still ◔ while it is queued. Then, in words,
      // what needs a look, most pressing first: what is warm says so, not only by color.
      const count = (test: (pc: Pace) => boolean) => running.filter((a) => { const pc = paces.get(a.id); return pc && test(pc) }).length
      const parts = [
        { text: `${count((pc) => pc.quiet && pc.stalled)} stalled`, color: p.warn, n: count((pc) => pc.quiet && pc.stalled) },
        { text: `${SLOW} ${count((pc) => !pc.quiet && pc.stalled)} slow`, color: p.warn, n: count((pc) => !pc.quiet && pc.stalled) },
        { text: `${count((pc) => pc.quiet && !pc.stalled)} quiet`, color: p.text2, n: count((pc) => pc.quiet && !pc.stalled) },
      ].filter((q) => q.n > 0)
      // The most pressing word always shows; the rest only while every mark still fits.
      const width = (n: number) => parts.slice(0, n).reduce((sum, q) => sum + textWidth(q.text) + 1, 0)
      let shown = parts.length ? 1 : 0
      while (shown < parts.length && running.length * 2 + width(shown + 1) <= w) shown++
      parts.length = shown
      const words = width(shown)
      let col = 0
      for (const a of running) {
        if (col + 2 > w - words) break
        const s = statusOf(a, now, p, paces.get(a.id), st.calm)
        c.set(x + col, yy, s.glyph, s.color)
        col += 2
      }
      for (const q of parts) col += c.text(x + col, yy, q.text, q.color) + 1
    } },
    blind ? unread("NEEDS YOU") : !waiting.length ? zero("NEEDS YOU", "0", note("nothing pending")) : { label: "NEEDS YOU", value: String(waiting.length), color: p.hot, alert: true,
      sub: (x, yy, w) => {
        // What is needed, by whom, and for how long: the longest wait not yet seen, else the longest wait.
        const first = [...waiting].sort((a, b) => Number(st.acked.has(a.id)) - Number(st.acked.has(b.id))
          || (waitingSince(a) ?? now) - (waitingSince(b) ?? now))[0]
        const ask = request(first)
        const since = waitingSince(first)
        const tail = (since === null ? "" : " " + duration(Math.max(0, now - since) / 1000).replace(/ \d+s$/, ""))
          + (waiting.length > 1 ? ` +${waiting.length - 1} more` : "")
        // The time and the count stay whole; the label gives way.
        const room = w - 2 - textWidth(tail)
        const what = room >= 4 ? fit(ask ? itemLabel(ask) : first.title, room) : ""
        const col = c.text(x, yy, "◆ " + what, p.hot, { max: w })
        c.text(x + col, yy, what ? tail : tail.trim(), p.text2, { max: w - col })
      } },
    blind ? unread("TOOLS / MIN") : { label: "TOOLS / MIN", value: rate >= 10 ? String(Math.round(rate)) : rate ? rate.toFixed(1) : "0", color: p.fg, quiet: !rate, sub: (x, yy, w) => {
      const counts = Array(w).fill(0)
      const { col } = slices(w, windowMs, now)
      for (const e of all) {
        const i = Math.min(w - 1, col(e.t))
        if (i >= 0 && TOOL(e.cat)) counts[i]++
      }
      const max = Math.max(1, ...counts)
      counts.forEach((n, i) => c.set(x + i, yy, n ? level(n, max) : "▁", n ? ramp(p, 0.3 + 0.7 * (n / max)) : p.track))
    } },
    blind ? unread("PEAK CONTEXT") : peakRatio === null ? missing("PEAK CONTEXT", agents.length ? "not reported" : "no agents")
      : { label: "PEAK CONTEXT", value: Math.round(peakRatio * 100) + "%", color: zoneColor(p, Math.max(0.5, peakRatio)),
        sub: note(peak!.title) },
    // Omarchy's records, not T3's: this one still counts when T3 can't be read.
    !usage.length ? missing("TOKENS TODAY", "no Omarchy usage data") : !today ? zero("TOKENS TODAY", "0", note("none yet today"))
      : { label: "TOKENS TODAY", value: tokens(today), color: p.fg, sub: (x, yy, w) => {
        let col = 0
        for (const u of usage.filter((u) => u.today)) {
          const t = `${u.id} ${tokens(u.today)}`
          if (col + textWidth(t) + 2 > w) break
          c.set(x + col, yy, "●", p.provider[usageFamily(u.id)])
          col += 2 + c.text(x + col + 2, yy, t, p.dim) + 2
        }
      } },
    blind ? unread("FAILED CALLS") : !fails.length ? zero("FAILED CALLS", "0", note(`none in ${WINDOWS[st.windowIndex]}m`))
      : { label: "FAILED CALLS", value: String(fails.length), color: lastFail && now - lastFail < 10 * 60_000 ? p.hot : p.warn,
        sub: note(`last ${duration((now - lastFail!) / 1000)} ago`) },
  ]
  const count = Math.max(2, Math.min(tiles.length, Math.floor((W + 1) / 19)))
  const tw = Math.floor((W - (count - 1)) / count)
  const valueRows = big ? BIG_ROWS : 1
  // Big digits leave half a row under the label; a blank row under them keeps
  // the spinners, sparkline and notes from touching the numbers.
  const gap = big ? 1 : 0
  const h = valueRows + gap + 2
  tiles.slice(0, count).forEach((t, n) => {
    const x = n * (tw + 1)
    const w = n === count - 1 ? W - x : tw
    c.fill(x, y, w, h, p.panel)
    // The alert edge blinks while a wait is unseen; once all are seen (or in calm mode) it holds steady.
    const steady = st.calm || !unseen.length
    if (t.alert) for (let yy = y; yy < y + h; yy++) c.set(x, yy, "▌", steady || blink(now) ? p.hot : mix(p.hot, p.panel, 0.6), p.panel)
    if (t.label === "NEEDS YOU") for (let yy = y; yy < y + h; yy++) frame.hits.push({ y: yy, x0: x, x1: x + w, id: NEEDS_YOU_TILE, area: "tile" })
    c.text(x + 2, y, t.label, t.alert ? p.hot : p.dim, { attr: BOLD, max: w - 3 })
    // Quiet values and values too wide for big digits are text on the big digits' baseline.
    if (t.quiet) c.text(x + 2, y + valueRows, t.value, p.dim, { max: w - 3 })
    else if (big && bigWidth(t.value) <= w - 3) drawBig(c, x + 2, y + 1, t.value, t.color)
    else c.text(x + 2, y + valueRows, t.value, t.color, { attr: BOLD, max: w - 3 })
    t.sub(x + 2, y + valueRows + gap + 1, w - 3)
  })
  return h
}

function limits(c: Canvas, y: number, W: number, providers: Provider[], p: Palette, now: number): number {
  const rows = providers.flatMap((u) => u.limits.map((l, i) => ({ u, l, first: i === 0 }))).slice(0, 6)
  const cols = W >= 180 && rows.length > 1 ? 2 : 1
  const perCol = Math.ceil(rows.length / cols)
  const colW = Math.floor((W - 2 - (cols - 1) * 3) / cols)
  section(c, 1, y, W - 2, "LIMITS", p, "│ marks time elapsed in each window", p.dim)
  rows.forEach(({ u, l, first }, n) => {
    const col = Math.floor(n / perCol)
    limitRow(c, 1 + col * (colW + 3), y + 1 + (n % perCol), colW, u, l, first || n % perCol === 0, p, now)
  })
  return perCol + 1
}

// Below this height LIMITS shrinks to one line.
export const LIMITS_FULL_MIN_H = 36

/** LIMITS in one line: each provider's fullest window, fullest first, as many as fit. */
function limitsLine(c: Canvas, y: number, W: number, providers: Provider[], p: Palette): number {
  let x = 1 + c.text(1, y, "LIMITS", p.text2, { attr: BOLD }) + 2
  const worst = providers.map((u) => ({ u, l: u.limits.reduce((a, b) => (b.percent > a.percent ? b : a)) }))
    .sort((a, b) => b.l.percent - a.l.percent)
  for (const [i, { u, l }] of worst.entries()) {
    const name = `${u.name.split(" ")[0]} ${(l.title ? l.title + " " : "") + l.label.replace(/\s*\(.*\)/, "")}`
    const hot = l.percent >= 0.9
    const ahead = l.elapsed !== null && l.percent > l.elapsed + 0.08 && l.percent >= 0.15
    const pct = `${Math.round(l.percent * 100)}%`
    const width = 2 + textWidth(name) + 1 + 8 + 1 + pct.length + (ahead ? 2 : 0)
    const more = worst.length - i - 1 ? `+${worst.length - i - 1}` : ""
    if (x + width + (more ? more.length + 3 : 0) > W - 1) {
      if (x + textWidth(`+${worst.length - i}`) <= W - 1) c.text(x, y, `+${worst.length - i}`, p.dim)
      break
    }
    c.set(x, y, "●", p.provider[usageFamily(u.id)])
    x += 2 + c.text(x + 2, y, name, p.text2) + 1
    hbar(c, x, y, 8, l.percent, {
      track: p.rule, line: true, color: (k, cells) => (hot ? p.hot : ramp(p, 0.3 + 0.7 * (k / cells))),
      marker: l.elapsed !== null ? { at: l.elapsed, color: p.bright } : undefined,
    })
    x += 9 + c.text(x + 9, y, pct, hot ? p.hot : p.fg, { attr: BOLD })
    if (ahead) x += 1 + c.text(x + 1, y, "▲", p.hot)
    x += 3
  }
  return 1
}

function limitRow(c: Canvas, X: number, y: number, W: number, u: Provider, l: Provider["limits"][number], named: boolean, p: Palette, now: number) {
  const narrow = W < 96
  if (named) c.set(X, y, "●", p.provider[usageFamily(u.id)])
  c.text(X + 2, y, named ? fit(u.name, 11) : "", p.text2, { attr: BOLD })
  let x = X + 14
  if (!narrow) {
    c.text(x, y, named ? fit(u.tier, 9) : "", p.dim)
    x += 10
  }
  const lw = narrow ? 11 : 14
  c.text(x, y, fit((l.title ? l.title + " " : "") + l.label.replace(/\s*\(.*\)/, ""), lw - 1), p.dim)
  x += lw
  const hot = l.percent >= 0.9
  const ahead = l.elapsed !== null && l.percent > l.elapsed + 0.08 && l.percent >= 0.15
  const reset = l.resetsAt ? `${RETRY} ` + duration(Math.max(0, (l.resetsAt - now) / 1000)).replace(/ \d+s$/, "") : ""
  const pace = narrow ? "" : hot ? "near limit" : ahead ? "▲ ahead of pace" : u.stale ? "stale" : "on pace"
  const bw = Math.max(6, Math.min(72, X + W - x - 5 - (reset ? 9 : 0) - (pace ? 16 : 0)))
  hbar(c, x, y, bw, l.percent, {
    track: p.rule, line: true,
    color: (i, cells) => (hot ? p.hot : ahead ? mix(ramp(p, 0.4 + 0.6 * (i / cells)), p.warn, 0.7) : ramp(p, 0.3 + 0.7 * (i / cells))),
    marker: l.elapsed !== null ? { at: l.elapsed, color: p.bright } : undefined,
  })
  x += bw + 1
  x += c.text(x, y, `${Math.round(l.percent * 100)}%`.padStart(4), hot ? p.hot : p.fg, { attr: BOLD }) + 1
  if (reset) c.text(x, y, reset, p.dim)
  if (pace) c.text(x + 9, y, pace, hot || ahead ? p.hot : p.dim)
}

function list(c: Canvas, frame: Frame, st: ViewState, snap: Snapshot, p: Palette, now: number): number {
  const { x: X, y: Y, w: W, h: H } = frame.list
  const rows = frame.rows
  const windowMs = WINDOWS[st.windowIndex] * 60_000
  const liveN = rows.filter((r) => isLive(r.agent)).length
  if (!snap.at) return unread(c, frame.list, st, snap, p)
  section(c, X + 1, Y, W - 2, "AGENTS", p, `${rows.length} shown · ${liveN} live`)
  const bodyY = Y + 1
  const bodyH = H - 1 // the time axis, then the agents
  // Two rows per agent when all fit that way, otherwise one; when even that
  // hides some, the last row says how many and what they hold.
  const card = rows.length * 2 <= bodyH - 1
  const per = card ? 2 : 1
  const visible = rows.length * per <= bodyH - 1 ? rows.length : Math.max(1, Math.floor((bodyH - 2) / per))
  // Scroll only when the selection leaves the visible window.
  const index = Math.max(0, rows.findIndex((r) => r.agent.id === st.selected))
  if (index < st.listScroll) st.listScroll = index
  if (index >= st.listScroll + visible) st.listScroll = index - visible + 1
  st.listScroll = Math.max(0, Math.min(st.listScroll, rows.length - visible))
  const first = st.listScroll
  // Folded parents hide their subagents; one waiting on you still shows, as a ◆ on the fold.
  const children = new Map<string, Agent[]>()
  for (const a of snap.agents) if (a.parent) children.set(a.parent, [...(children.get(a.parent) ?? []), a])
  const waitingUnder = (id: string, seen = new Set<string>()): boolean =>
    (children.get(id) ?? []).some((k) => !seen.has(k.id) && (seen.add(k.id), needsYou(k) || waitingUnder(k.id, seen)))
  // Hot and warm text, lifted just enough to read on the selected row's background.
  const hotUp = reach(p.hot, p.dark ? 0xffffff : 0, TEXT_CONTRAST, [p.sel])
  const warnUp = reach(p.warn, p.dark ? 0xffffff : 0, TEXT_CONTRAST, [p.sel])
  // Column geometry.
  const barW = card ? Math.max(10, Math.min(30, Math.floor(W * 0.26))) : Math.max(8, Math.min(18, Math.floor(W * 0.13)))
  const nameW = card ? 0 : Math.max(14, Math.min(40, Math.floor(W * 0.32)))
  const laneX = card ? X + 6 + barW + 6 : X + 4 + nameW + 2 + barW + 6
  // One-row lanes stop short of the provider dot and the age (up to 7 columns) at the right edge.
  const laneW = Math.max(0, X + W - laneX - (card ? 1 : 11))
  const laneMax = Math.max(3, ...rows.map((r) => Math.max(0, ...laneData(r.agent, Math.max(1, laneW), windowMs, now).counts)))
  // Time axis aligned with the lanes.
  const axisY = bodyY
  if (laneW >= 12) {
    const label = (m: number) => (m >= 60 ? `-${+(m / 60).toFixed(1)}h` : `-${Math.round(m)}m`)
    const mins = WINDOWS[st.windowIndex]
    c.text(laneX, axisY, label(mins), p.dim)
    c.textRight(laneX + laneW, axisY, "now", p.dim)
    if (laneW >= 30) c.text(laneX + Math.floor(laneW / 2) - 2, axisY, label(mins / 2), p.dim)
  }
  c.text(X + 2, axisY, card ? "CONTEXT" : "AGENT", p.dim)
  if (!card) c.text(X + 4 + nameW + 2, axisY, "CONTEXT", p.dim)
  if (!rows.length) {
    c.text(X + 2, bodyY + 2, st.activeOnly ? "No active agents. Press a to show all." : "No unarchived T3 threads found.", p.dim)
    return bodyY + 4
  }
  rows.slice(first, first + visible).forEach((row, n) => {
    const a = row.agent
    const y = bodyY + 1 + n * per
    const selected = a.id === st.selected
    // The thread open in T3 Code and its subagents share a band and a left rule.
    const linked = frame.linked.has(a.id)
    const rowBg = selected ? p.sel : linked ? mix(p.bg, p.sel, 0.5) : p.bg
    if (selected || linked) c.fill(X, y, W, per, rowBg)
    // Text on the lighter selected and linked rows steps up one role to stay readable.
    const ink = (color: Color) => (!selected && !linked ? color : color === p.dim ? p.text2 : color === p.text2 ? p.fg
      : color === p.hot ? hotUp : color === p.warn ? warnUp : color)
    frame.hits.push({ y, x0: X, x1: X + W, id: a.id, area: "list" })
    if (per === 2) frame.hits.push({ y: y + 1, x0: X, x1: X + W, id: a.id, area: "list" })
    const pc = frame.paces.get(a.id)
    const s = statusOf(a, now, p, pc, st.calm, st.acked.has(a.id))
    const attention = needsYou(a)
    // The selection's edge is a bright half block; the linked band's a thin dim rule.
    const edge = selected || attention ? "▌" : linked ? "▎" : " "
    const edgeColor = attention ? p.hot : selected ? p.bright : p.dim
    c.set(X, y, edge, edgeColor, rowBg)
    if (per === 2) c.set(X, y + 1, edge, edgeColor, rowBg)
    // Tree guides.
    let x = X + 2
    row.guides.forEach((g) => {
      c.set(x, y, g ? "│" : " ", p.rule, rowBg)
      if (per === 2) c.set(x, y + 1, g ? "│" : " ", p.rule, rowBg)
      x += 2
    })
    if (row.depth > 0) {
      c.text(x, y, row.last ? "└─" : "├─", p.rule, { bg: rowBg })
      if (per === 2 && !row.last) c.set(x, y + 1, "│", p.rule, rowBg)
      x += 2
    }
    c.set(x, y, s.glyph, s.color, rowBg, BOLD)
    const fold = row.hasChildren && st.collapsed.has(a.id) ? " ▸" : ""
    const folded = fold && waitingUnder(a.id) ? 2 : 0 // a ◆ after the fold mark: someone in there needs you
    const liveRow = isLive(a) && !attention
    // Motion means work: quiet or queued agents keep their text, but nothing on their rows moves.
    const moving = working(a, pc)
    const flare = moving && !st.calm ? st.shimmer.get(a.id) : undefined // a new tool call just arrived
    const nameColor = ink(attention || liveRow ? p.fg : p.text2)
    const nameAttr = liveRow || attention || selected ? BOLD : 0
    const ageText = liveRow || attention ? duration(since(a.started, now)) : a.ended ? duration(since(a.ended, now)) + " ago" : ""
    const frac = st.tween.get(a.id) ?? 0
    const r = ratio(a)
    const pctText = r === null ? "  --" : `${Math.round(r * 100)}%`.padStart(4)
    const pctColor = ink(r === null ? p.dim : r >= 0.9 ? p.hot : r >= 0.75 ? p.warn : p.text2)
    if (per === 2) {
      const right = X + W - 1
      const meta = `● ${shortModel(a.model)}  ${ageText}`
      const metaW = textWidth(meta)
      const nameMax = Math.max(6, right - metaW - 2 - (x + 2))
      const name = fit(a.title, nameMax - folded) + fold
      const used = shimmer(c, x + 2, y, name, nameColor, p, now, flare, nameAttr, rowBg) + folded
      if (folded) c.set(x + 1 + used, y, "◆", p.hot, rowBg, BOLD)
      const actionX = x + 2 + used + 2
      const actionW = right - metaW - 2 - actionX
      const act = liveRow || attention ? doing(a, pc, p) : null
      if (act && actionW > 8) c.text(actionX, y, fitDoing(act, actionW), ink(act.color), { bg: rowBg })
      c.textRight(right, y, meta, ink(p.dim), { bg: rowBg })
      c.set(right - metaW, y, "●", p.provider[familyOf(a)], rowBg)
      const shift = Math.max(0, row.depth - 2) * 2
      const bx = X + 6 + shift
      const bw = Math.max(4, barW - shift)
      if (row.hasChildren && !st.collapsed.has(a.id) && x < bx) c.set(x, y + 1, "│", p.rule, rowBg)
      if (r === null) c.text(bx, y + 1, fit(a.remote ? "remote · stats on other machine" : "context not reported", bw + 5), ink(p.dim), { bg: rowBg })
      else {
        contextBar(c, bx, y + 1, bw, frac, p, selected ? mix(p.track, p.sel, 0.4) : p.track)
        c.text(bx + bw + 1, y + 1, pctText, pctColor, { bg: rowBg, attr: BOLD })
      }
    } else {
      const name = fit(a.title, Math.max(4, X + 4 + nameW - (x + 2) - folded)) + fold
      const used = shimmer(c, x + 2, y, name, nameColor, p, now, flare, nameAttr, rowBg)
      if (folded) c.set(x + 3 + used, y, "◆", p.hot, rowBg, BOLD)
      const bx = X + 4 + nameW + 2
      if (r === null) c.text(bx, y, fit("--", barW), ink(p.dim), { bg: rowBg })
      else {
        contextBar(c, bx, y, barW, frac, p, selected ? mix(p.track, p.sel, 0.4) : p.track)
        c.text(bx + barW + 1, y, pctText, pctColor, { bg: rowBg, attr: BOLD })
      }
      const age = ageText.replace(" ago", "")
      c.textRight(X + W - 1, y, age, ink(p.dim), { bg: rowBg })
      c.set(X + W - 3 - textWidth(age), y, "●", p.provider[familyOf(a)], rowBg)
    }
    const fam = familyOf(a)
    if (laneW >= 8) drawLane(c, laneX, y + per - 1, laneData(a, laneW, windowMs, now), laneMax, p, rowBg,
      fam === "other" ? null : p.provider[fam])
  })
  const shown = Math.min(visible, rows.length - first)
  const hidden = [...rows.slice(0, first), ...rows.slice(first + shown)].map((r) => r.agent)
  if (hidden.length) overflow(c, X + 2, bodyY + 1 + shown * per, W - 3, first, rows.length - first - shown, hidden, frame, p)
  return bodyY + 1 + shown * per + (hidden.length ? 1 : 0)
}

/**
 * The list's overflow row: how many agents are scrolled out of view and what
 * they hold, e.g. "▼ 11 more · ◆1 · 1 stalled · 1 ✗". It turns hot while one
 * of them is waiting on you.
 */
function overflow(c: Canvas, x: number, y: number, w: number, above: number, below: number, hidden: Agent[], frame: Frame, p: Palette) {
  const count = (test: (a: Agent, pc: Pace | undefined) => boolean) => hidden.filter((a) => test(a, frame.paces.get(a.id))).length
  const waiting = count(needsYou)
  const stalled = count((_, pc) => !!pc?.quiet && pc.stalled)
  const slow = count((_, pc) => !!pc && !pc.quiet && pc.stalled)
  const failed = count((a) => a.status === "failed" || a.status === "error")
  const lead = above && below ? `▲ ${above} · ▼ ${below} more` : below ? `▼ ${below} more` : `▲ ${above} more`
  const parts: [string, Color][] = [[lead, waiting ? p.hot : p.text2]]
  if (waiting) parts.push([`◆${waiting}`, p.hot])
  if (stalled) parts.push([`${stalled} stalled`, p.warn])
  if (slow) parts.push([`${SLOW}${slow} slow`, p.warn])
  if (failed) parts.push([`${failed} ✗`, p.hot])
  let col = 0
  parts.forEach(([text, color], i) => {
    if (i) col += c.text(x + col, y, " · ", p.dim, { max: w - col })
    col += c.text(x + col, y, text, color, { attr: i === 0 && waiting ? BOLD : 0, max: w - col })
  })
}

/** No snapshot to show: one message in place of the agents, naming the database and what to try. */
function unread(c: Canvas, r: Rect, st: ViewState, snap: Snapshot, p: Palette): number {
  const { x: X, y: Y, w: W, h: H } = r
  const t = st.cause
  const path = tilde(t?.path || snap.path) || "T3's database"
  section(c, X + 1, Y, W - 2, "AGENTS", p, st.error ? "can't read T3" : "reading T3…", st.error ? p.hot : p.dim)
  const head = !st.error ? `Reading ${path}…`
    : t?.kind === "missing" ? `No T3 database at ${path}.`
    : t?.kind === "schema" ? `T3's database at ${path} has a layout this version doesn't know: ${t.detail}.`
    : `Can't read the T3 database at ${path}: ${t?.detail || st.error}.`
  const tries = !st.error ? [] : t?.kind === "schema"
    ? ["T3 Code may have changed its database: look for a newer T3 Agents."]
    : [t?.kind === "unreadable" ? "Check it is T3 Code's statev2.sqlite and that your user can read it."
        : "Is T3 Code installed, and has it been opened at least once?",
      "T3 data somewhere else? Start with --db PATH, or set T3CODE_HOME to its folder."]
  if (st.error) tries.push("Just looking? t3-agents --demo shows made-up agents.")
  let y = Y + 2
  const w = W - 4
  for (const line of wrap(head, w)) if (y < Y + H) c.text(X + 2, y++, line, p.fg, { attr: BOLD })
  y++
  for (const tip of tries) {
    wrap(tip, w - 2).forEach((line, i) => {
      if (y >= Y + H) return
      if (i === 0) c.text(X + 2, y, "·", p.dim)
      c.text(X + 4, y++, line, p.text2)
    })
  }
  return y
}

// ── Cross-agent panels for spare room in the list column ───────────────────

function extras(c: Canvas, r: Rect, st: ViewState, agents: Agent[], usage: Provider[], p: Palette, now: number) {
  const models = usage.flatMap((u) => u.models.map((m) => ({ ...m, fam: family(m.model, u.id) })))
    .sort((a, b) => b.tokens - a.tokens)
  const modelRows = Math.min(models.length, 6)
  const modelsH = modelRows ? modelRows + 2 : 0
  let y = r.y
  const pulseRoom = r.h - modelsH
  if (pulseRoom >= 6) y += pulse(c, r.x, y, r.w, Math.min(8, pulseRoom - 4), st, agents, p, now)
  if (modelRows && r.y + r.h - y >= 3) {
    section(c, r.x, y++, r.w, "MODELS TODAY", p, "Omarchy usage records")
    const max = models[0].tokens
    const labelW = Math.min(22, Math.max(...models.slice(0, modelRows).map((m) => textWidth(m.model))) + 1)
    for (const m of models.slice(0, Math.min(modelRows, r.y + r.h - y))) {
      const value = tokens(m.tokens)
      c.set(r.x, y, "●", p.provider[m.fam])
      c.text(r.x + 2, y, fit(m.model, labelW - 1), p.text2)
      const bw = Math.max(4, r.w - labelW - 4 - value.length)
      hbar(c, r.x + 2 + labelW, y, bw, m.tokens / max, {
        track: p.bg, color: (i, cells) => mix(p.provider[m.fam], p.bg, 0.5 - 0.3 * (i / cells)),
      })
      c.textRight(r.x + r.w, y++, value, p.fg)
    }
  }
}

function pulse(c: Canvas, X: number, Y: number, W: number, h: number, st: ViewState, agents: Agent[], p: Palette, now: number): number {
  const windowMs = WINDOWS[st.windowIndex] * 60_000
  const plot = new Braille(W, h)
  const slots = plot.w
  const { size, col: slot } = slices(slots, windowMs, now)
  const counts = new Array(slots).fill(0)
  const byFam = new Array(W).fill(0).map(() => ({ claude: 0, openai: 0, grok: 0, other: 0 } as Record<Family, number>))
  const fails = new Array(W).fill(false)
  for (const a of agents) {
    const fam = familyOf(a)
    for (const e of a.events) {
      const i = Math.min(slots - 1, slot(e.t))
      if (i < 0 || e.t > now + 5000 || !TOOL(e.cat)) continue
      counts[i]++
      byFam[i >> 1][fam]++
      if (e.failed) fails[i >> 1] = true
    }
  }
  const max = Math.max(1, ...counts)
  const height = (n: number) => Math.max(0.08, n / max)
  counts.forEach((n, i) => n && plot.column(i, height(n)))
  const perMin = max / (size / 60_000)
  const any = counts.some((n) => n > 0)
  section(c, X, Y, W, "PULSE", p, any ? `tool calls, all agents · peak ${perMin >= 10 ? Math.round(perMin) : perMin.toFixed(1)}/min` : "no tool calls in this window")
  plot.draw(c, X, Y + 1, (col, row) => {
    const f = byFam[col]
    const fam = (Object.keys(f) as Family[]).reduce((a, b) => (f[b] > f[a] ? b : a), "other")
    const lift = 0.1 + 0.3 * (1 - row / h)
    return fam === "other" ? ramp(p, 0.4 + 0.6 * (1 - row / h)) : mix(p.provider[fam], p.bright, lift)
  })
  // A slice with a failed call carries a × just above its column: a shape, not only the hot color.
  for (let col = 0; col < W; col++) {
    if (!fails[col]) continue
    const peak = Math.max(counts[col * 2] ?? 0, counts[col * 2 + 1] ?? 0)
    const top = Math.floor((plot.h - Math.round(height(peak) * plot.h)) / 4) // text row of the column's top dot
    c.set(X + col, Y + 1 + Math.max(0, top - 1), "×", p.hot, undefined, BOLD)
  }
  // A floor under empty slices only: the bottom row holds the shortest columns.
  for (let col = 0; col < W; col++) if (!counts[col * 2] && !counts[col * 2 + 1]) c.set(X + col, Y + h, "▁", p.track)
  const label = (m: number) => (m >= 60 ? `-${+(m / 60).toFixed(1)}h` : `-${Math.round(m)}m`)
  c.text(X, Y + h + 1, label(WINDOWS[st.windowIndex]), p.dim)
  c.textRight(X + W, Y + h + 1, "now", p.dim)
  // Legend: only the providers on screen.
  const present = (["claude", "openai", "grok", "other"] as Family[]).filter((f) => agents.some((a) => familyOf(a) === f))
  let lx = X + 8
  for (const f of present) {
    if (lx + FAMILY_LABEL[f].length + 4 > X + W - 5) break
    c.set(lx, Y + h + 1, "●", p.provider[f])
    lx += 2 + c.text(lx + 2, Y + h + 1, FAMILY_LABEL[f], p.dim) + 2
  }
  return h + 3
}

// ── Detail panel ────────────────────────────────────────────────────────────

type Block = (x: number, y: number, w: number, room: number) => number

function detail(c: Canvas, r: Rect, a: Agent, st: ViewState, p: Palette, now: number, frame: Frame): number {
  return c.clipped(r.x, r.y, r.w, r.h, st.detailScroll, () => {
    const W = r.w
    const top = r.y + titleBlock(c, r.x, r.y, W, a, p, now, frame.paces.get(a.id), st.calm, st.acked.has(a.id))
    const b = blocks(c, r, a, st, p, now, frame)
    if (W >= 100) {
      // Wide: measurements on the left, what the agent is doing on the right.
      const cw = Math.floor((W - 3) / 2)
      let ly = top
      for (const blk of [b.context, b.growth, b.tokens, b.cache]) ly += blk(r.x, ly, cw, r.y + r.h - ly)
      let ry = top
      for (const blk of [b.failures, b.plan, b.activity, b.tools]) ry += blk(r.x + cw + 3, ry, W - cw - 3, r.y + r.h - ry)
      for (let yy = top; yy < Math.max(ly, ry) - 1; yy++) c.set(r.x + cw + 1, yy, "┊", p.track)
      return Math.max(ly, ry) - r.y
    }
    let y = top
    for (const blk of [b.failures, b.context, b.plan, b.activity, b.tools, b.growth, b.tokens, b.cache]) y += blk(r.x, y, W, r.y + r.h - y)
    return y - r.y
  })
}

function titleBlock(c: Canvas, X: number, Y: number, W: number, a: Agent, p: Palette, now: number, pc?: Pace, calm = false, seen = false): number {
  let y = Y
  const s = statusOf(a, now, p, pc, calm, seen)
  const attention = needsYou(a)
  c.text(X, y++, fit(a.title, W), p.bright, { attr: BOLD })
  const opts = a.options.filter((o) => o && o.id && o.value !== undefined).map((o) => `${o.id} ${o.value}`)
  c.set(X, y, "●", p.provider[familyOf(a)])
  c.text(X + 2, y++, fit([a.model, a.project, a.kind, ...opts, a.branch ? "⌥ " + a.branch : ""].filter(Boolean).join(" · "), W - 2), p.dim)
  let x = X
  x += c.text(x, y, s.glyph + " " + s.label, s.color, { attr: BOLD })
  const elapsed = duration(since(a.started, a.ended ? ms(a.ended) ?? now : now))
  x += c.text(x, y, fit(`  ${elapsed}${a.ended ? " · finished " + duration(since(a.ended, now)) + " ago" : ""}`, X + W - x), p.text2)
  if (a.requests.length) c.text(x, y, fit("  · " + a.requests.join(", ").replace(/_/g, " "), X + W - x), p.hot)
  y++
  const act = isLive(a) || attention ? doing(a, pc, p) : null
  if (act) {
    const why = pc?.quiet ? (pc.stalled ? "  no activity for a while: check it in T3 (⏎)" : "") :
      pc?.stalled ? `  one call running over ${SLOW_TOOL_MS / 60_000}m` : ""
    c.text(X, y++, "  now  " + fitDoing(act, W - 7), act.color === p.dim ? p.text2 : act.color)
    if (why) c.text(X, y++, fit(why, W), p.dim)
  }
  if (a.remote) c.text(X, y++, fit(`  remote on ${a.remote.label ?? "another environment"}; context and activity live there`, W), p.accent)
  return y - Y + 1
}

function blocks(c: Canvas, r: Rect, a: Agent, st: ViewState, p: Palette, now: number, frame: Frame): Record<string, Block> {
  // Motion means work: only a working agent's running call spins in the activity feed.
  const moving = working(a, frame.paces.get(a.id))
  const call = a.turn.tokenUsage ?? null
  return {
    failures(X, Y, W) {
      const minutes = WINDOWS[st.windowIndex]
      const failed = a.recentFailures.filter((i) => (ms(i.updated_at) ?? 0) >= now - minutes * 60_000)
      if (!failed.length) return 0
      let y = Y
      section(c, X, y++, W, "RECENT FAILURES", p, `${failed.length} · ${minutes}m`, p.hot)
      for (const item of failed) {
        const age = duration(since(item.updated_at, now)).replace(/ \d+s$/, "")
        const exit = typeof item.exit_code === "number" ? `exit ${item.exit_code}` : "failed"
        const scope = item.run_id && item.run_id !== a.run.id ? " · earlier run" : ""
        const meta = `${age} ago · ${exit}${scope}`
        const lines = wrap(`${meta}  ${itemLabel(item)}`, W - 2)
        c.set(X, y, "✗", p.hot)
        for (const line of lines) c.text(X + 2, y++, line, p.text2)
        // Redundant shape and explicit exit code carry meaning without red text.
        c.text(X + 2, y - lines.length, fit(meta, W - 2), p.hot)
      }
      c.text(X, y++, fit("Failed calls; agent status is separate. Enter opens T3.", W), p.dim)
      return y + 1 - Y
    },
    context(X, Y, W) {
      let y = Y
      const rr = ratio(a)
      section(c, X, y++, W, "CONTEXT", p, a.context.updatedAt ? `reported ${duration(since(a.context.updatedAt, now))} ago` : "")
      if (rr === null) {
        c.text(X, y, "Context usage not reported for this agent.", p.dim)
        return y + 2 - Y
      }
      const tall = r.h >= 40 && W >= 56
      const color = rr >= 0.9 ? p.hot : rr >= 0.75 ? p.warn : p.bright
      const pct = Math.round(rr * 100) + "%"
      const fw = tall ? drawFont(c, X, y, pct, "block", color, mix(color, p.bg, 0.6)) : drawBig(c, X, y, pct, color)
      const fh = tall ? 6 : BIG_ROWS
      const tx = X + fw + 3
      c.text(tx, y, fit(`${tokens(a.context.usedTokens)} of ${tokens(a.context.maxTokens)} tokens`, X + W - tx), p.fg, { attr: BOLD })
      const prev = a.history.at(-2)
      const last = a.history.at(-1)
      if (prev && last && prev.provider === last.provider) {
        const delta = last.used - prev.used
        c.text(tx, y + 1, fit(`${delta >= 0 ? "+" : "−"}${tokens(Math.abs(delta))} last turn`, X + W - tx), p.text2)
      }
      if (fh > 2) c.text(tx, y + 2, fit(`${tokens((a.context.maxTokens ?? 0) - (a.context.usedTokens ?? 0))} free`, X + W - tx), p.dim)
      y += fh
      contextBar(c, X, y++, W, st.tween.get(a.id) ?? rr, p)
      for (const [t, label, tone] of [[0.5, "50", p.dim], [0.75, "75", p.warn], [0.9, "90", p.hot]] as const) {
        c.text(X + Math.round(t * W) - 1, y, "╵" + label, tone)
      }
      return y + 2 - Y
    },
    growth(X, Y, W, room) {
      if (a.history.length < 2) return 0
      const h = Math.max(3, Math.min(8, Math.floor(r.h / 7), room - 3))
      section(c, X, Y, W, "GROWTH", p, `${a.history.length} turns · % of window`)
      growth(c, X, Y + 1, W, h, a, p)
      return h + 3
    },
    tokens(X, Y, W) {
      const usage = a.turn.turnTokenUsage ?? null
      const src = usage ?? call
      if (!src) return 0
      let y = Y
      const cached = src.cachedInputTokens ?? 0
      const fresh = Math.max(0, (src.inputTokens ?? 0) - cached)
      const parts = [
        { label: "cached", n: cached, color: mix(p.accent, p.bg, 0.45) },
        { label: "fresh", n: fresh, color: p.accent },
        { label: "output", n: src.outputTokens ?? 0, color: p.bright },
        { label: "reasoning", n: src.reasoningTokens ?? src.reasoningOutputTokens ?? 0, color: p.warn },
      ].filter((q) => q.n > 0)
      section(c, X, y++, W, usage ? "LAST TURN" : "LATEST CALL", p, usage ? (usage.usageStatus ?? "") : "turn total pending")
      segmented(c, X, y++, W, parts, p)
      let lx = X
      for (const q of parts) {
        const t = `${q.label} ${tokens(q.n)}`
        if (lx + textWidth(t) + 2 > X + W) break
        c.set(lx, y, "■", q.color)
        lx += 2 + c.text(lx + 2, y, t, p.text2) + 2
      }
      y++
      if (usage?.hasSubagents) c.text(X, y++, fit("includes subagent usage; not added again elsewhere", W), p.dim)
      return y + 1 - Y
    },
    cache(X, Y, W) {
      // The prompt cache drains in real time; a cold cache makes the next turn costlier.
      const exp = ms(a.context.promptCacheExpiresAt ?? call?.promptCacheExpiresAt)
      const ttl = a.context.promptCacheTtlMs ?? call?.promptCacheTtlMs
      if (exp === null || !ttl) return 0
      const left = exp - now
      const frac = Math.max(0, Math.min(1, left / ttl))
      const label = left > 0 ? `warm · ${duration(left / 1000).replace(/ \d+s$/, "")} left` : `cold · ${duration(-left / 1000).replace(/ \d+s$/, "")} ago`
      const lw = textWidth(label) + 2
      c.text(X, Y, "PROMPT CACHE", p.text2, { attr: BOLD })
      hbar(c, X + 14, Y, Math.max(4, W - 14 - lw), frac, {
        track: p.track, color: () => (frac < 0.12 ? p.warn : mix(p.accent, p.bright, frac * 0.6)),
      })
      // Drains with the clock, an eighth of a cell at a time: the idle frame rate is enough.
      c.textRight(X + W, Y, label, left > 0 ? p.text2 : p.dim)
      return 2
    },
    plan(X, Y, W) {
      if (!a.plan?.length) return 0
      let y = Y
      const done = a.plan.filter((q) => q.status === "completed").length
      section(c, X, y++, W, "PLAN", p, `${done}/${a.plan.length} done`)
      const each = W / a.plan.length
      a.plan.forEach((q, i) => {
        // The step in progress is the bright one: a steady marker, not a blink.
        const tone = q.status === "completed" ? p.accent : q.status === "in_progress" ? p.bright : p.track
        const end = Math.round((i + 1) * each) - (each >= 3 ? 1 : 0)
        for (let k = Math.round(i * each); k < Math.max(end, Math.round(i * each) + 1); k++) c.set(X + k, y, "▀", tone)
      })
      y++
      for (const q of a.plan.slice(0, 8)) {
        const [g, tone] = q.status === "completed" ? ["✓", p.dim] : q.status === "in_progress" ? ["▸", p.bright] : ["○", p.text2]
        c.text(X, y, g, tone)
        c.text(X + 2, y++, fit(q.text, W - 2), tone)
      }
      if (a.plan.length > 8) c.text(X + 2, y++, `+${a.plan.length - 8} more`, p.dim)
      return y + 1 - Y
    },
    tools(X, Y, W) {
      const entries = Object.entries(a.mix).sort((m, n) => n[1] - m[1])
      if (!entries.length) return 0
      let y = Y
      const total = entries.reduce((n, [, v]) => n + v, 0)
      section(c, X, y++, W, "TOOLS", p, `${total} ${a.run.id ? "this run" : "in thread"}${a.failures ? ` · ${a.failures} failed` : ""}`, a.failures ? p.hot : p.dim)
      const max = entries[0][1]
      const nw = Math.max(...entries.map(([, n]) => String(n).length))
      for (const [cat, n] of entries.slice(0, 7)) {
        c.text(X, y, GLYPH[cat] ?? "·", p.text2)
        c.text(X + 2, y, cat, p.text2)
        hbar(c, X + 9, y, Math.max(4, W - 10 - nw), n / max, { track: p.bg, color: (i, cells) => ramp(p, 0.35 + 0.55 * (i / cells)) })
        c.textRight(X + W, y++, String(n), p.fg)
      }
      return y + 1 - Y
    },
    activity(X, Y, W, room) {
      // Newest first; brightness fades with age. Items a snapshot just brought flash in
      // (see observe()); calm mode holds the flash steady instead of fading it.
      const feed = a.items.filter(feedItem)
      let y = Y
      section(c, X, y++, W, "ACTIVITY", p, a.run.id ? "latest run" : "thread activity")
      if (!feed.length) {
        c.text(X, y++, a.run.id ? "No tool activity recorded for this run." : "No tool activity recorded for this thread.", p.dim)
        return y + 1 - Y
      }
      const limit = Math.max(6, Math.min(16, room - 2))
      for (const item of feed.slice(0, limit)) {
        const seen = st.firstSeen.get(itemKey(item)) ?? 0
        const fresh = Math.max(0, now - seen)
        const flash = seen && fresh < FLASH_MS ? (st.calm ? 1 : 1 - fresh / FLASH_MS) : 0
        if (flash > 0 && !st.calm) frame.beat = true
        const running = LIVE.has(item.status)
        const failed = item.status === "failed"
        const age = since(running ? item.started_at ?? item.updated_at : item.updated_at, now) ?? 0
        const cat = category(item.type, item.tool)
        // The fade stops at dim, the dimmest readable text.
        const fade = running ? p.bright : age < 15 ? p.fg : age < 90 ? p.text2 : p.dim
        const bg = flash > 0 ? mix(p.bg, p.accent, 0.35 * flash) : undefined
        if (bg !== undefined) c.fill(X, y, W, 1, bg)
        // A running call spins only while its agent works (not while it waits on you).
        // A request still waiting on you is the hot ◆, wherever it shows.
        const asking = running && cat === "ask"
        const mark = asking ? "◆" : running ? (moving ? spin(now, st.calm) : GLYPH[cat] ?? "·") : failed ? "✗" : GLYPH[cat] ?? "·"
        c.text(X, y, mark, asking || failed ? p.hot : running ? p.bright : mix(fade, p.accent, 0.3), { bg, attr: asking ? BOLD : 0 })
        c.text(X + 2, y, duration(age).replace(/ \d+s$/, "").padStart(6), running ? p.text2 : p.dim, { bg })
        c.text(X + 9, y++, fit(itemLabel(item), W - 9), failed ? mix(p.hot, fade, 0.3) : fade, { bg })
      }
      if (feed.length > limit) c.text(X, y++, fit(`▼ ${feed.length - limit} more calls · Enter opens T3`, W), p.dim)
      return y + 1 - Y
    },
  }
}

function growth(c: Canvas, X: number, y: number, W: number, h: number, a: Agent, p: Palette) {
  const axis = 5
  const cols = Math.max(4, W - axis)
  const plot = new Braille(cols, h)
  const pts = a.history.slice(-(plot.w))
  const n = pts.length
  const px = (i: number) => (n === 1 ? 0 : Math.round((i * (plot.w - 1)) / (n - 1)))
  for (let i = 0; i < n - 1; i++) {
    const [x0, x1] = [px(i), px(i + 1)]
    for (let x = x0; x <= x1; x++) {
      const t = x1 === x0 ? 0 : (x - x0) / (x1 - x0)
      plot.column(x, pts[i].pct + (pts[i + 1].pct - pts[i].pct) * t)
    }
  }
  const famAt = (col: number): Family => {
    const i = n === 1 ? 0 : Math.round(((col * 2) / Math.max(1, plot.w - 1)) * (n - 1))
    return family(pts[Math.min(n - 1, i)].model, pts[Math.min(n - 1, i)].provider)
  }
  plot.draw(c, X + axis, y, (col, row) => {
    const top = 1 - row / h
    if (top > 0.9) return p.hot
    if (top > 0.75) return p.warn
    const fam = famAt(col)
    return fam === "other" ? ramp(p, 0.3 + 0.7 * top) : mix(p.provider[fam], p.bright, 0.1 + 0.35 * top)
  })
  // Hand-off markers where the provider conversation changed.
  for (let i = 1; i < n; i++) {
    if (pts[i].provider === pts[i - 1].provider) continue
    const cx = X + axis + Math.floor(px(i) / 2)
    for (let yy = y; yy < y + h; yy++) if (c.bgAt(cx, yy) === p.bg) c.set(cx, yy, "┊", p.faint)
    const tag = fit("→ " + shortModel(pts[i].model || "handoff"), 16)
    c.text(cx + 1 + textWidth(tag) <= X + W ? cx + 1 : cx - textWidth(tag), y, tag, p.dim)
  }
  c.textRight(X + axis - 1, y, "100", p.dim)
  c.textRight(X + axis - 1, y + h - 1, "0", p.dim)
  if (h >= 4) c.textRight(X + axis - 1, y + Math.floor(h / 2), "50", p.dim)
  c.text(X + axis, y + h, "first turn", p.dim)
  c.textRight(X + W, y + h, "latest", p.dim)
}

function segmented(c: Canvas, x: number, y: number, w: number, parts: { n: number; color: Color }[], p: Palette) {
  const total = parts.reduce((s, q) => s + q.n, 0)
  if (!total) return void c.fill(x, y, w, 1, p.track)
  // Every non-zero part gets at least one cell so small slices stay visible.
  const cells = parts.map((q) => Math.max(1, Math.round((q.n / total) * w)))
  let over = cells.reduce((s, n) => s + n, 0) - w
  while (over > 0) {
    const i = cells.indexOf(Math.max(...cells))
    cells[i]--
    over--
  }
  let cx = x
  parts.forEach((q, i) => {
    for (let k = 0; k < cells[i]; k++) c.set(cx + k, y, "█", q.color)
    cx += cells[i]
  })
  for (; cx < x + w; cx++) c.set(cx, y, " ", p.track, p.track)
}

function footer(c: Canvas, st: ViewState, snap: Snapshot, p: Palette, now: number, W: number, H: number) {
  const y = H - 1
  c.fill(0, y, W, 1, p.panel)
  if (st.error) {
    c.text(1, y, fit(`SOURCE ERROR · ${snap.at ? "last snapshot kept · " : ""}retrying every ${st.interval}s · ${st.error}`, W - 2), p.hot)
    return
  }
  // n leads while someone waits on you: it is the key that matters then.
  const waiting = snap.agents.some(needsYou) ? [["n", "needs you"] as [string, string]] : []
  const keys: [string, string][] = [...waiting, ["↑↓", "select"], ["⏎", "open in T3"], ["␣", "fold"], ["a", "active"],
    ...(st.t3Link ? [["f", st.follow ? "follow T3" : "follow off"] as [string, string]] : []),
    ["t", "lane"], ["x", "fx"], ["?", "help"], ["q", "quit"]]
  // A message after an action (opening a thread) replaces the hints for a few seconds.
  if (st.notice && now - st.notice.at < 4000) {
    c.text(1, y, fit(st.notice.text, W - 2), st.notice.error ? p.hot : p.text2)
    return
  }
  let x = 1
  for (const [k, label] of keys) {
    if (x + k.length + label.length + 3 > W - 24) break
    x += c.text(x, y, k, p.accent, { attr: BOLD }) + 1
    x += c.text(x, y, label, p.dim) + 2
  }
  c.textRight(W - 1, y, st.demo ? "demo data · made up" : `read-only · ${duration(Math.max(0, (now - snap.at) / 1000))} ago`, p.dim)
}

/** The help overlay; returns how far it can scroll when the window is too short for it. */
function help(c: Canvas, st: ViewState, p: Palette, W: number, H: number): number {
  c.tint(0, 0, W, H, (f, b) => [mix(f, p.bg, 0.7), mix(b, p.bg, 0.5)])
  const keys: [string, string][] = [
    ["↑↓ j k", "select agent"], ["n", "next agent waiting on you"], ["⏎ o", "open the thread in T3 Code"], ["␣ ← →", "fold / expand"], ["a", "active agents only"], ["t", "lane 15m · 30m · 1h · 3h"],
    ["J K PgDn", "scroll details"], ["f", "follow the thread open in T3"], ["p", "pause updates"], ["x", "CRT effects"],
    ["r", "refresh now"], ["m", "calm mode: nothing moves"],
    ["mouse", "click selects, wheel scrolls"], ["q Esc", "quit (Esc closes help)"],
  ]
  const legend: [string, string][] = [
    ["⠹ ◆ ✓ ✗ ⊘", "running · needs you (blinks until seen) · done · failed · interrupted"],
    ["█████▌", "context used; warm past 75%, hot past 90%"],
    ["▂▄█", "lane: tool and model activity per time slice"],
    ["tint", "lane: a run was in progress"],
    ["┃  ×", "lane: a prompt you sent · a failed call"],
    ["│", "limit meter: time elapsed; fill beyond it is ahead of pace"],
    ["▎ band", "the thread open in T3 Code and its subagents"],
    ["◌", "running, but nothing new for 2m; \"stalled\" after 8m"],
    ["◔  ◉", "queued, not started yet · running, in calm mode"],
    ["❯ · △ 6m", "a tool call's run time; △ slow past 5m"],
  ]
  const two = W >= 84
  const bw = Math.min(W - 2, two ? 80 : 64)
  const keyRows = two ? Math.ceil(keys.length / 2) : keys.length
  const full = keyRows + legend.length + 10
  const bh = Math.min(H - 2, full)
  const max = full - bh
  st.helpScroll = Math.max(0, Math.min(st.helpScroll, max))
  const bx = Math.floor((W - bw) / 2)
  const by = Math.floor((H - bh) / 2)
  c.fill(bx, by, bw, bh, p.panel)
  for (let x = bx + 1; x < bx + bw - 1; x++) {
    c.set(x, by, "─", p.accent)
    c.set(x, by + bh - 1, "─", p.accent)
  }
  for (let y = by + 1; y < by + bh - 1; y++) {
    c.set(bx, y, "│", p.accent)
    c.set(bx + bw - 1, y, "│", p.accent)
  }
  c.set(bx, by, "╭", p.accent)
  c.set(bx + bw - 1, by, "╮", p.accent)
  c.set(bx, by + bh - 1, "╰", p.accent)
  c.set(bx + bw - 1, by + bh - 1, "╯", p.accent)
  c.text(bx + 2, by, " T3 Agents · help ", p.bright, { attr: BOLD })
  // Too short for all of it: the help scrolls (↓ ↑), and its border says what is out of view.
  if (st.helpScroll < max) c.textRight(bx + bw - 2, by + bh - 1, ` ▼ ${max - st.helpScroll} more · ↓ `, p.bright, { bg: p.panel })
  if (st.helpScroll > 0) c.textRight(bx + bw - 2, by, ` ▲ ${st.helpScroll} more · ↑ `, p.bright, { bg: p.panel })
  c.clipped(bx + 1, by + 1, bw - 2, bh - 2, st.helpScroll, () => {
    let y = by + 2
    const x0 = bx + 3
    const half = Math.floor((bw - 6) / 2)
    c.text(x0, y++, "KEYS", p.text2, { attr: BOLD })
    keys.forEach(([k, d], i) => {
      const col = two && i >= keyRows ? 1 : 0
      const yy = y + (two ? i % keyRows : i)
      const x = x0 + col * half
      c.text(x, yy, k, p.accent, { attr: BOLD })
      c.text(x + 11, yy, fit(d, (two ? half : bw - 6) - 12), p.fg)
    })
    y += keyRows + 1
    c.text(x0, y++, "LEGEND", p.text2, { attr: BOLD })
    for (const [g, d] of legend) {
      c.text(x0, y, g, p.bright)
      c.text(x0 + 11, y++, fit(d, bw - 17), p.fg)
    }
    let lx = x0 + 11
    c.text(x0, y, "● provider", p.bright)
    for (const f of ["claude", "openai", "grok", "other"] as Family[]) {
      c.set(lx, y, "●", p.provider[f])
      lx += 2 + c.text(lx + 2, y, FAMILY_LABEL[f], p.fg) + 2
    }
    y++
    c.text(x0, y + 1, fit("Read-only: local T3 SQLite and Omarchy usage records. No network.", bw - 6), p.dim)
    c.text(x0, y + 2, fit("Unofficial; not affiliated with T3 Code or Omarchy.", bw - 6), p.dim)
  })
  return max
}
