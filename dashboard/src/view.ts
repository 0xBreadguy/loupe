// Layout and drawing. Pure: (canvas, state, data, palette, time) -> pixels.
// Encoding rules: shape + brightness carry state (works in monochrome themes);
// the theme's hot color is reserved for "needs you", failures and limits.
import { BIG_ROWS, BOLD, Braille, Canvas, bigWidth, clean, drawBig, drawFont, fit, hbar, level, textWidth } from "./canvas.ts"
import { duration, shortModel, tokens } from "./format.ts"
import { t3Thread } from "./focus.ts"
import { BAD, LIVE, SLOW_TOOL_MS, isLive, ms, needsYou, pace, tree, category, type Agent, type Item, type Pace, type Snapshot,
  type TreeRow } from "./source.ts"
import { family, mix, ramp, type Color, type Family, type Palette } from "./theme.ts"
import type { Provider } from "./usage.ts"

export const WINDOWS = [15, 30, 60, 180] // lane window, minutes

export interface ViewState {
  selected: string | null
  collapsed: Set<string>
  activeOnly: boolean
  paused: boolean
  help: boolean
  fx: boolean
  detailScroll: number
  listScroll: number
  windowIndex: number
  error: string | null
  interval: number
  bootAt: number
  lastFrame: number
  tween: Map<string, number>
  firstSeen: Map<string, number>
  t3Title: string | null // title of the T3 Code window, null when unknown or closed
  t3Linked: string | null // the thread that title names, as last matched
  t3Link: boolean // Hyprland is reachable, so the link can work
  follow: boolean // select the thread T3 Code shows whenever it changes
  notice: { text: string; at: number; error: boolean } | null // a short footer message
  demo: boolean // made-up data: say so in the header
}

export function initialState(interval = 2): ViewState {
  return { selected: null, collapsed: new Set(), activeOnly: false, paused: false, help: false, fx: false,
    detailScroll: 0, listScroll: 0, windowIndex: 1, error: null, interval, bootAt: Date.now(), lastFrame: 0,
    tween: new Map(), firstSeen: new Map(), t3Title: null, t3Linked: null, t3Link: false, follow: true,
    notice: null, demo: false }
}

export interface Rect { x: number; y: number; w: number; h: number }
export interface Hit { y: number; x0: number; x1: number; id: string; area: "list" | "detail" }
export interface Frame {
  rows: TreeRow[]
  hits: Hit[]
  list: Rect
  detail: Rect
  detailMax: number // maximum detail scroll
  animating: boolean
  fast: boolean // spinners / shimmer need a high frame rate
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

export const GLYPH: Record<string, string> = {
  shell: "❯", read: "◎", edit: "✎", web: "⌕", agent: "⑂", ask: "◆", think: "∴",
  reply: "≡", you: "»", note: "•", tool: "◈", other: "·",
}
const TOOL = (cat: string) => cat !== "you" && cat !== "think" && cat !== "reply"
const SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
const QUEUE = "◴◷◶◵"
const spinner = (now: number, offset = 0) => SPIN[Math.floor(now / 80 + offset) % SPIN.length]
const blink = (now: number, period = 1000) => now % period < period * 0.55

export function statusOf(a: Agent, now: number, p: Palette, pc?: Pace): { glyph: string; color: Color; label: string } {
  if (needsYou(a)) return { glyph: blink(now) ? "◆" : "◇", color: p.hot, label: "NEEDS YOU" }
  // A quiet agent's spinner stops: motion means work is happening.
  if (pc?.quiet) return { glyph: "◌", color: pc.stalled ? p.warn : p.text2, label: "RUNNING" }
  switch (a.status) {
    case "running": case "active": return { glyph: spinner(now, a.id.length), color: pc?.stalled ? p.warn : p.bright, label: "RUNNING" }
    case "starting": case "preparing": case "queued": case "pending":
      return { glyph: QUEUE[Math.floor(now / 250) % 4], color: p.text2, label: a.status.toUpperCase() }
    case "completed": return { glyph: "✓", color: p.dim, label: "DONE" }
    case "interrupted": return { glyph: "⊘", color: p.warn, label: "INTERRUPTED" }
    case "failed": case "error": return { glyph: "✗", color: p.hot, label: "FAILED" }
    default: return { glyph: "·", color: p.faint, label: a.status.toUpperCase() }
  }
}

/** Context fill color by position on the bar: brighter rightward, warm past 75%, hot past 90%. */
function zoneColor(p: Palette, t: number): Color {
  if (t >= 0.9) return p.hot
  if (t >= 0.75) return p.warn
  return ramp(p, 0.2 + t * 0.85)
}

function contextBar(c: Canvas, x: number, y: number, w: number, frac: number, live: boolean, now: number, p: Palette, track = p.track) {
  const pos = ((now % 1800) / 1800) * (w * frac + 8) - 4
  hbar(c, x, y, w, frac, {
    track,
    color: (i, cells) => {
      const base = zoneColor(p, (i + 0.5) / cells)
      if (!live) return base
      const k = Math.max(0, 1 - Math.abs(i - pos) / 3)
      return mix(base, p.bright, k * 0.55)
    },
  })
}

function shimmer(c: Canvas, x: number, y: number, s: string, base: Color, p: Palette, now: number, on: boolean, attr = 0, bg?: Color): number {
  if (!on) return c.text(x, y, s, base, { attr, bg })
  const chars = [...clean(s)]
  const span = chars.length + 14
  const pos = ((now % 2600) / 2600) * span - 7
  let col = 0
  chars.forEach((ch, i) => {
    const k = Math.max(0, 1 - Math.abs(i - pos) / 4.5)
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

interface LaneData { counts: number[]; fails: boolean[]; you: boolean[]; busy: boolean[] }

function laneData(a: Agent, w: number, windowMs: number, now: number): LaneData {
  const start = now - windowMs
  const bucket = windowMs / w
  const d: LaneData = { counts: Array(w).fill(0), fails: Array(w).fill(false), you: Array(w).fill(false), busy: Array(w).fill(false) }
  for (const e of a.events) {
    if (e.t < start || e.t > now + 5000) continue
    const i = Math.min(w - 1, Math.floor((e.t - start) / bucket))
    if (e.cat === "you") d.you[i] = true
    else d.counts[i]++
    if (e.failed) d.fails[i] = true
  }
  for (const [s, e] of a.spans) {
    const end = e ?? now
    if (end < start) continue
    const i0 = Math.max(0, Math.floor((s - start) / bucket))
    const i1 = Math.min(w - 1, Math.floor((end - start) / bucket))
    for (let i = i0; i <= i1; i++) d.busy[i] = true
  }
  return d
}

function drawLane(c: Canvas, x: number, y: number, d: LaneData, max: number, live: boolean, now: number, p: Palette, rowBg: Color, tint: Color | null) {
  const w = d.counts.length
  const tick = Math.max(1, Math.round(w / 6))
  // A provider-tinted band while a run is in progress; activity blends toward the provider hue.
  const busy = tint === null ? mix(rowBg, p.busy, 1) : mix(rowBg, tint, 0.16)
  for (let i = 0; i < w; i++) {
    const bg = d.busy[i] ? busy : rowBg
    const k = Math.min(1, d.counts[i] / max)
    if (d.fails[i]) c.set(x + i, y, level(Math.max(1, d.counts[i]), max), p.hot, bg)
    else if (d.you[i]) c.set(x + i, y, "┃", p.accent, bg)
    else if (d.counts[i]) c.set(x + i, y, level(d.counts[i], max), tint === null ? ramp(p, 0.35 + 0.65 * k) : mix(tint, p.bright, 0.15 + 0.35 * k), bg)
    else c.set(x + i, y, (w - 1 - i) % tick === 0 ? "·" : " ", p.faint, bg)
  }
  if (live) c.set(x + w - 1, y, blink(now, 700) ? "▌" : " ", p.bright, busy)
}

// ── Main draw ───────────────────────────────────────────────────────────────

export function draw(c: Canvas, st: ViewState, snap: Snapshot, usage: Provider[], p: Palette, now: number): Frame {
  c.clear(p.bg, p.fg)
  const W = c.w
  const H = c.h
  const dt = st.lastFrame ? Math.min(0.25, Math.max(0, (now - st.lastFrame) / 1000)) : 0
  st.lastFrame = now
  const frame: Frame = { rows: [], hits: [], list: { x: 0, y: 0, w: 0, h: 0 }, detail: { x: 0, y: 0, w: 0, h: 0 },
    detailMax: 0, animating: false, fast: false, linked: new Set<string>(),
    paces: new Map<string, Pace>() }
  if (W < 48 || H < 14) {
    c.text(1, 0, "T3 AGENTS", p.bright, { attr: BOLD })
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
  // Tween every bar toward its reported value (first frame sweeps up from zero).
  for (const a of agents) {
    const target = ratio(a) ?? 0
    const cur = st.tween.get(a.id) ?? 0
    const next = cur + (target - cur) * (1 - Math.exp(-dt * 7))
    if (Math.abs(target - next) < 0.0015) st.tween.set(a.id, target)
    else {
      st.tween.set(a.id, next)
      frame.animating = true
    }
  }
  if (st.firstSeen.size > 1500) {
    for (const [k, t] of st.firstSeen) if (t < now - 600_000) st.firstSeen.delete(k)
  }
  const live = agents.filter((a) => isLive(a) && !needsYou(a))
  const waiting = agents.filter(needsYou)
  if (live.length) frame.fast = true
  if (waiting.length) frame.animating = true

  header(c, st, snap, p, now, W)
  let y = 1
  y += kpis(c, y, W, H >= KPI_BIG_MIN_H, st, agents, live, waiting, usage, p, now, frame.paces)
  const limited = usage.filter((u) => u.limits.length)
  if (limited.length) y += limits(c, y, W, limited, p, now)
  const mainTop = y
  const mainH = H - 1 - mainTop
  const wide = W >= 140
  if (wide) {
    const lw = Math.floor(W * 0.56)
    frame.list = { x: 0, y: mainTop, w: lw, h: mainH }
    frame.detail = { x: lw + 2, y: mainTop, w: W - lw - 3, h: mainH }
    for (let yy = mainTop; yy < H - 1; yy++) c.set(lw, yy, "│", p.rule)
  } else {
    const need = frame.rows.length * 2 + 2
    const lh = Math.max(5, Math.min(need, Math.floor(mainH * 0.46)))
    frame.list = { x: 0, y: mainTop, w: W, h: lh }
    frame.detail = { x: 1, y: mainTop + lh, w: W - 2, h: mainH - lh }
  }
  const listEnd = list(c, frame, st, p, now)
  // Wide layouts: spend the spare list-column rows on cross-agent views.
  if (wide && mainTop + mainH - listEnd >= 8) extras(c, { x: 1, y: listEnd + 1, w: frame.list.w - 2, h: mainTop + mainH - listEnd - 1 }, st, agents, usage, p, now)
  const sel = agents.find((a) => a.id === st.selected)
  if (sel && frame.detail.h > 3) {
    const height = detail(c, frame.detail, sel, st, p, now, frame)
    frame.detailMax = Math.max(0, height - frame.detail.h)
    if (st.detailScroll > frame.detailMax) st.detailScroll = frame.detailMax
  } else if (!agents.length) {
    c.text(frame.detail.x + 1, frame.detail.y + 1, "No unarchived T3 threads found.", p.dim)
  }
  footer(c, st, snap, p, now, W, H)
  if (st.help) help(c, p, W, H)
  // Boot: a scan line wipes the dashboard in from the top.
  const boot = (now - st.bootAt) / 650
  if (boot < 1) {
    const edge = Math.floor((1 - (1 - boot) ** 3) * H)
    c.tint(0, edge + 1, W, H, () => [p.bg, p.bg])
    for (let x = 0; x < W; x++) c.set(x, Math.min(H - 1, edge), "▔", mix(p.accent, p.bright, 0.5), c.bgAt(x, edge))
    frame.animating = true
  }
  return frame
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

/** What a running agent is doing, for one line: the tool in flight with its time, or how long it has been quiet. */
function doing(a: Agent, pc: Pace | undefined, p: Palette): { text: string; color: Color } | null {
  if (pc?.quiet) {
    const last = a.items.find((i) => i.type !== "reasoning" && i.type !== "assistant_message")
    const after = last ? ` · last ${GLYPH[category(last.type, last.tool)] ?? "·"} ${itemLabel(last)}` : ""
    return { text: `◌ quiet ${duration(pc.quietMs / 1000)}${after}`, color: pc.stalled ? p.warn : p.text2 }
  }
  if (!a.now) return null
  const cat = category(a.now.type, a.now.tool)
  const time = pc?.tool && pc.toolMs >= 30_000 ? ` · ${duration(pc.toolMs / 1000)}` : ""
  return { text: `${GLYPH[cat] ?? "·"} ${itemLabel(a.now)}${time}`, color: pc?.stalled ? p.warn : p.dim }
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
  let x = c.text(0, 0, " T3 ", p.bg, { bg: p.accent, attr: BOLD })
  x += c.text(x, 0, " AGENTS ", p.bright, { attr: BOLD })
  x += 1
  if (st.paused) x += c.text(x, 0, "‖ PAUSED", p.warn, { attr: BOLD })
  else if (st.error) x += c.text(x, 0, blink(now) ? "↻ RETRYING" : "  RETRYING", p.hot, { attr: BOLD })
  else {
    // The dot breathes on each refresh: bright right after a snapshot, fading out.
    const age = Math.min(1, (now - snap.at) / (st.interval * 1000))
    x += c.text(x, 0, "●", mix(p.bright, p.accent, age))
    x += c.text(x + 1, 0, `LIVE ${st.interval}s`, p.text2) + 1
  }
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

function kpis(c: Canvas, y: number, W: number, big: boolean, st: ViewState, agents: Agent[], live: Agent[], waiting: Agent[],
  usage: Provider[], p: Palette, now: number, paces: Map<string, Pace>): number {
  const windowMs = WINDOWS[st.windowIndex] * 60_000
  const all = agents.flatMap((a) => a.events)
  const recent = all.filter((e) => e.t >= now - 5 * 60_000 && TOOL(e.cat))
  const rate = recent.length / 5
  const fails = all.filter((e) => e.failed && e.t >= now - windowMs)
  const lastFail = fails.length ? Math.max(...fails.map((e) => e.t)) : null
  const peakPool = live.length ? live : agents
  const peak = peakPool.reduce<Agent | null>((best, a) => ((ratio(a) ?? -1) > (best ? ratio(best) ?? -1 : -1) ? a : best), null)
  const peakRatio = peak ? ratio(peak) : null
  const today = usage.reduce((n, u) => n + u.today, 0)

  type Tile = { label: string; big: string; color: Color; sub: (x: number, y: number, w: number) => void; alert?: boolean }
  const tiles: Tile[] = [
    { label: "ACTIVE", big: String(live.length), color: live.length ? p.bright : p.dim, sub: (x, yy, w) => {
      if (!live.length) return void c.text(x, yy, "nothing running", p.dim)
      // One mark per agent: a spinner while it works, a still ◌ once it goes quiet.
      const quiet = live.filter((a) => paces.get(a.id)?.quiet).length
      const room = quiet ? w - 9 : w
      let col = 0
      for (const a of live) {
        if (col + 2 > room) break
        const pc = paces.get(a.id)
        if (pc?.quiet) c.set(x + col, yy, "◌", pc.stalled ? p.warn : p.text2)
        else c.set(x + col, yy, spinner(now, col), pc?.stalled ? p.warn : ramp(p, 0.6 + 0.4 * Math.sin(now / 300 + col)))
        col += 2
      }
      const long = live.some((a) => paces.get(a.id)?.quiet && paces.get(a.id)?.stalled)
      if (quiet) c.text(x + col, yy, `${quiet} quiet`, long ? p.warn : p.text2, { max: w - col })
    } },
    { label: "NEEDS YOU", big: String(waiting.length), color: waiting.length ? p.hot : p.dim, alert: waiting.length > 0,
      sub: (x, yy, w) => void c.text(x, yy, waiting.length ? "◆ " + fit(waiting[0].title, w - 2) : "nothing pending",
        waiting.length ? p.hot : p.dim, { max: w }) },
    { label: "TOOLS / MIN", big: rate >= 10 ? String(Math.round(rate)) : rate.toFixed(1), color: rate ? p.fg : p.dim, sub: (x, yy, w) => {
      const counts = Array(w).fill(0)
      for (const e of all) {
        if (e.t < now - windowMs || !TOOL(e.cat)) continue
        counts[Math.min(w - 1, Math.floor(((e.t - (now - windowMs)) / windowMs) * w))]++
      }
      const max = Math.max(1, ...counts)
      counts.forEach((n, i) => c.set(x + i, yy, n ? level(n, max) : "▁", n ? ramp(p, 0.3 + 0.7 * (n / max)) : p.track))
    } },
    { label: "PEAK CONTEXT", big: peakRatio === null ? "--" : Math.round(peakRatio * 100) + "%",
      color: peakRatio === null ? p.dim : zoneColor(p, Math.max(0.5, peakRatio)), sub: (x, yy, w) =>
        void c.text(x, yy, peak && peakRatio !== null ? fit(peak.title, w) : "not reported", p.dim, { max: w }) },
    { label: "TOKENS TODAY", big: today ? tokens(today) : "--", color: today ? p.fg : p.dim, sub: (x, yy, w) => {
      const shown = usage.filter((u) => u.today)
      if (!shown.length) return void c.text(x, yy, "no records", p.dim)
      let col = 0
      for (const u of shown) {
        const t = `${u.id} ${tokens(u.today)}`
        if (col + textWidth(t) + 2 > w) break
        c.set(x + col, yy, "●", p.provider[usageFamily(u.id)])
        col += 2 + c.text(x + col + 2, yy, t, p.dim) + 2
      }
    } },
    { label: "FAILED", big: String(fails.length), color: fails.length && lastFail && now - lastFail < 10 * 60_000 ? p.hot : fails.length ? p.warn : p.dim,
      sub: (x, yy, w) => void c.text(x, yy, lastFail ? `last ${duration((now - lastFail) / 1000)} ago` : "clean run", p.dim, { max: w }) },
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
    if (t.alert) for (let yy = y; yy < y + h; yy++) c.set(x, yy, "▌", blink(now) ? p.hot : mix(p.hot, p.panel, 0.6), p.panel)
    c.text(x + 2, y, t.label, t.alert ? p.hot : p.dim, { attr: BOLD, max: w - 3 })
    // A value too wide for its tile falls back to text on the big digits' baseline.
    if (big && bigWidth(t.big) <= w - 3) drawBig(c, x + 2, y + 1, t.big, t.color)
    else c.text(x + 2, y + valueRows, t.big, t.color, { attr: BOLD, max: w - 3 })
    t.sub(x + 2, y + valueRows + gap + 1, w - 3)
  })
  return h
}

function limits(c: Canvas, y: number, W: number, providers: Provider[], p: Palette, now: number): number {
  const rows = providers.flatMap((u) => u.limits.map((l, i) => ({ u, l, first: i === 0 }))).slice(0, 6)
  const cols = W >= 180 && rows.length > 1 ? 2 : 1
  const perCol = Math.ceil(rows.length / cols)
  const colW = Math.floor((W - 2 - (cols - 1) * 3) / cols)
  section(c, 1, y, W - 2, "LIMITS", p, "│ marks time elapsed in each window", p.faint)
  rows.forEach(({ u, l, first }, n) => {
    const col = Math.floor(n / perCol)
    limitRow(c, 1 + col * (colW + 3), y + 1 + (n % perCol), colW, u, l, first || n % perCol === 0, p, now)
  })
  return perCol + 1
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
  const reset = l.resetsAt ? "↻ " + duration(Math.max(0, (l.resetsAt - now) / 1000)).replace(/ \d+s$/, "") : ""
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
  if (pace) c.text(x + 9, y, pace, hot || ahead ? p.hot : p.faint)
}

function list(c: Canvas, frame: Frame, st: ViewState, p: Palette, now: number): number {
  const { x: X, y: Y, w: W, h: H } = frame.list
  const rows = frame.rows
  const windowMs = WINDOWS[st.windowIndex] * 60_000
  const liveN = rows.filter((r) => isLive(r.agent)).length
  section(c, X + 1, Y, W - 2, "AGENTS", p, `${rows.length} shown · ${liveN} live`)
  const bodyY = Y + 1
  const bodyH = H - 1
  const card = rows.length * 2 <= bodyH - 1 || W < 70
  const per = card ? 2 : 1
  const visible = Math.max(1, Math.floor((bodyH - 1) / per))
  // Scroll only when the selection leaves the visible window.
  const index = Math.max(0, rows.findIndex((r) => r.agent.id === st.selected))
  if (index < st.listScroll) st.listScroll = index
  if (index >= st.listScroll + visible) st.listScroll = index - visible + 1
  st.listScroll = Math.max(0, Math.min(st.listScroll, rows.length - visible))
  const first = st.listScroll
  // Column geometry.
  const barW = card ? Math.max(10, Math.min(30, Math.floor(W * 0.26))) : Math.max(8, Math.min(18, Math.floor(W * 0.13)))
  const nameW = card ? 0 : Math.max(14, Math.min(40, Math.floor(W * 0.32)))
  const laneX = card ? X + 6 + barW + 6 : X + 4 + nameW + 2 + barW + 6
  const laneW = Math.max(0, X + W - laneX - (card ? 1 : 8))
  const laneMax = Math.max(3, ...rows.map((r) => Math.max(0, ...laneData(r.agent, Math.max(1, laneW), windowMs, now).counts)))
  // Time axis aligned with the lanes.
  const axisY = bodyY
  if (laneW >= 12) {
    const label = (m: number) => (m >= 60 ? `-${+(m / 60).toFixed(1)}h` : `-${Math.round(m)}m`)
    const mins = WINDOWS[st.windowIndex]
    c.text(laneX, axisY, label(mins), p.faint)
    c.textRight(laneX + laneW, axisY, "now", p.dim)
    if (laneW >= 30) c.text(laneX + Math.floor(laneW / 2) - 2, axisY, label(mins / 2), p.faint)
  }
  c.text(X + 2, axisY, card ? "CONTEXT" : "AGENT", p.faint)
  if (!card) c.text(X + 4 + nameW + 2, axisY, "CONTEXT", p.faint)
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
    frame.hits.push({ y, x0: X, x1: X + W, id: a.id, area: "list" })
    if (per === 2) frame.hits.push({ y: y + 1, x0: X, x1: X + W, id: a.id, area: "list" })
    const pc = frame.paces.get(a.id)
    const s = statusOf(a, now, p, pc)
    const attention = needsYou(a)
    const edge = selected || attention ? "▌" : linked ? "▎" : " "
    const edgeColor = attention ? p.hot : selected ? p.accent : p.fg
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
    const liveRow = isLive(a) && !attention
    const nameColor = attention ? p.fg : liveRow ? p.fg : BAD.has(a.status) ? p.text2 : p.text2
    const ageText = liveRow || attention ? duration(since(a.started, now)) : a.ended ? duration(since(a.ended, now)) + " ago" : ""
    const frac = st.tween.get(a.id) ?? 0
    const r = ratio(a)
    const pctText = r === null ? "  --" : `${Math.round(r * 100)}%`.padStart(4)
    const pctColor = r === null ? p.faint : r >= 0.9 ? p.hot : r >= 0.75 ? p.warn : p.text2
    if (per === 2) {
      const right = X + W - 1
      const meta = `● ${shortModel(a.model)}  ${ageText}`
      const metaW = textWidth(meta)
      const nameMax = Math.max(6, right - metaW - 2 - (x + 2))
      const name = fit(a.title, nameMax) + fold
      const used = shimmer(c, x + 2, y, name, nameColor, p, now, liveRow, liveRow || attention ? BOLD : 0, rowBg)
      const actionX = x + 2 + used + 2
      const actionW = right - metaW - 2 - actionX
      const act = liveRow || attention ? doing(a, pc, p) : null
      if (act && actionW > 8) c.text(actionX, y, fit(act.text, actionW), act.color, { bg: rowBg })
      c.textRight(right, y, meta, p.dim, { bg: rowBg })
      c.set(right - metaW, y, "●", p.provider[familyOf(a)], rowBg)
      const shift = Math.max(0, row.depth - 2) * 2
      const bx = X + 6 + shift
      const bw = Math.max(4, barW - shift)
      if (row.hasChildren && !st.collapsed.has(a.id) && x < bx) c.set(x, y + 1, "│", p.rule, rowBg)
      if (r === null) c.text(bx, y + 1, fit(a.remote ? "remote · stats on other machine" : "context not reported", bw + 5), p.faint, { bg: rowBg })
      else {
        contextBar(c, bx, y + 1, bw, frac, liveRow, now, p, selected ? mix(p.track, p.sel, 0.4) : p.track)
        c.text(bx + bw + 1, y + 1, pctText, pctColor, { bg: rowBg, attr: BOLD })
      }
    } else {
      const name = fit(a.title, Math.max(4, X + 4 + nameW - (x + 2))) + fold
      shimmer(c, x + 2, y, name, nameColor, p, now, liveRow, liveRow || attention ? BOLD : 0, rowBg)
      const bx = X + 4 + nameW + 2
      if (r === null) c.text(bx, y, fit("--", barW), p.faint, { bg: rowBg })
      else {
        contextBar(c, bx, y, barW, frac, liveRow, now, p, selected ? mix(p.track, p.sel, 0.4) : p.track)
        c.text(bx + barW + 1, y, pctText, pctColor, { bg: rowBg, attr: BOLD })
      }
      const age = ageText.replace(" ago", "")
      c.textRight(X + W - 1, y, age, p.dim, { bg: rowBg })
      c.set(X + W - 3 - textWidth(age), y, "●", p.provider[familyOf(a)], rowBg)
    }
    const fam = familyOf(a)
    if (laneW >= 8) drawLane(c, laneX, y + per - 1, laneData(a, laneW, windowMs, now), laneMax, liveRow, now, p, rowBg,
      fam === "other" ? null : p.provider[fam])
  })
  if (first > 0) c.textRight(X + W - 1, bodyY, `▲ ${first} more`, p.dim)
  const below = rows.length - first - visible
  if (below > 0) c.textRight(X + W - 1, Y + H - 1, `▼ ${below} more`, p.dim)
  return bodyY + 1 + Math.min(visible, rows.length - first) * per
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
  const start = now - windowMs
  const slots = plot.w
  const counts = new Array(slots).fill(0)
  const byFam = new Array(W).fill(0).map(() => ({ claude: 0, openai: 0, grok: 0, other: 0 } as Record<Family, number>))
  const fails = new Array(W).fill(false)
  for (const a of agents) {
    const fam = familyOf(a)
    for (const e of a.events) {
      if (e.t < start || e.t > now || !TOOL(e.cat)) continue
      const i = Math.min(slots - 1, Math.floor(((e.t - start) / windowMs) * slots))
      counts[i]++
      byFam[i >> 1][fam]++
      if (e.failed) fails[i >> 1] = true
    }
  }
  const max = Math.max(1, ...counts)
  counts.forEach((n, i) => n && plot.column(i, Math.max(0.08, n / max)))
  const perMin = max / (windowMs / slots / 60_000)
  section(c, X, Y, W, "PULSE", p, `tool calls, all agents · peak ${perMin >= 10 ? Math.round(perMin) : perMin.toFixed(1)}/min`)
  const tops = new Array(W).fill(h)
  for (let col = 0; col < W; col++) {
    const peak = Math.max(counts[col * 2] ?? 0, counts[col * 2 + 1] ?? 0)
    if (peak) tops[col] = Math.min(h - 1, h - Math.ceil(Math.max(0.08, peak / max) * h))
  }
  plot.draw(c, X, Y + 1, (col, row) => {
    if (fails[col] && row === tops[col]) return p.hot
    const f = byFam[col]
    const fam = (Object.keys(f) as Family[]).reduce((a, b) => (f[b] > f[a] ? b : a), "other")
    const lift = 0.1 + 0.3 * (1 - row / h)
    return fam === "other" ? ramp(p, 0.4 + 0.6 * (1 - row / h)) : mix(p.provider[fam], p.bright, lift)
  })
  for (let x = X; x < X + W; x++) if (c.bgAt(x, Y + h) === p.bg) c.set(x, Y + h, "▔", p.track)
  const label = (m: number) => (m >= 60 ? `-${+(m / 60).toFixed(1)}h` : `-${Math.round(m)}m`)
  c.text(X, Y + h + 1, label(WINDOWS[st.windowIndex]), p.faint)
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
    const top = r.y + titleBlock(c, r.x, r.y, W, a, p, now, frame.paces.get(a.id))
    const b = blocks(c, r, a, st, p, now, frame)
    if (W >= 100) {
      // Wide: measurements on the left, what the agent is doing on the right.
      const cw = Math.floor((W - 3) / 2)
      let ly = top
      for (const blk of [b.context, b.growth, b.tokens, b.cache]) ly += blk(r.x, ly, cw, r.y + r.h - ly)
      let ry = top
      for (const blk of [b.plan, b.activity, b.tools]) ry += blk(r.x + cw + 3, ry, W - cw - 3, r.y + r.h - ry)
      for (let yy = top; yy < Math.max(ly, ry) - 1; yy++) c.set(r.x + cw + 1, yy, "┊", p.track)
      return Math.max(ly, ry) - r.y
    }
    let y = top
    for (const blk of [b.context, b.plan, b.activity, b.tools, b.growth, b.tokens, b.cache]) y += blk(r.x, y, W, r.y + r.h - y)
    return y - r.y
  })
}

function titleBlock(c: Canvas, X: number, Y: number, W: number, a: Agent, p: Palette, now: number, pc?: Pace): number {
  let y = Y
  const s = statusOf(a, now, p, pc)
  const attention = needsYou(a)
  c.text(X, y++, fit(a.title, W), p.bright, { attr: BOLD })
  const opts = a.options.filter((o) => o && o.id && o.value !== undefined).map((o) => `${o.id} ${o.value}`)
  c.set(X, y, "●", p.provider[familyOf(a)])
  c.text(X + 2, y++, fit([a.model, a.project, a.kind, ...opts, a.branch ? "⎇ " + a.branch : ""].filter(Boolean).join(" · "), W - 2), p.dim)
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
    c.text(X, y++, fit(`  now  ${act.text}`, W), act.color === p.dim ? p.text2 : act.color)
    if (why) c.text(X, y++, fit(why, W), p.dim)
  }
  if (a.remote) c.text(X, y++, fit(`  remote on ${a.remote.label ?? "another environment"}; context and activity live there`, W), p.accent)
  return y - Y + 1
}

function blocks(c: Canvas, r: Rect, a: Agent, st: ViewState, p: Palette, now: number, frame: Frame): Record<string, Block> {
  const liveRow = isLive(a) && !needsYou(a)
  const call = a.turn.tokenUsage ?? null
  return {
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
      contextBar(c, X, y++, W, st.tween.get(a.id) ?? rr, liveRow, now, p)
      for (const [t, label, tone] of [[0.5, "50", p.faint], [0.75, "75", p.warn], [0.9, "90", p.hot]] as const) {
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
      if (usage?.hasSubagents) c.text(X, y++, fit("includes subagent usage; not added again elsewhere", W), p.faint)
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
      c.textRight(X + W, Y, label, left > 0 ? p.text2 : p.faint)
      if (left > 0) frame.animating = true
      return 2
    },
    plan(X, Y, W) {
      if (!a.plan?.length) return 0
      let y = Y
      const done = a.plan.filter((q) => q.status === "completed").length
      section(c, X, y++, W, "PLAN", p, `${done}/${a.plan.length} done`)
      const each = W / a.plan.length
      a.plan.forEach((q, i) => {
        const tone = q.status === "completed" ? p.accent : q.status === "in_progress" ? (blink(now, 900) ? p.bright : p.accent) : p.track
        const end = Math.round((i + 1) * each) - (each >= 3 ? 1 : 0)
        for (let k = Math.round(i * each); k < Math.max(end, Math.round(i * each) + 1); k++) c.set(X + k, y, "▀", tone)
      })
      y++
      for (const q of a.plan.slice(0, 8)) {
        const [g, tone] = q.status === "completed" ? ["✓", p.dim] : q.status === "in_progress" ? ["▸", p.bright] : ["○", p.text2]
        c.text(X, y, g, tone)
        c.text(X + 2, y++, fit(q.text, W - 2), tone)
      }
      if (a.plan.length > 8) c.text(X + 2, y++, `+${a.plan.length - 8} more`, p.faint)
      if (a.plan.some((q) => q.status === "in_progress")) frame.animating = true
      return y + 1 - Y
    },
    tools(X, Y, W) {
      const entries = Object.entries(a.mix).sort((m, n) => n[1] - m[1])
      if (!entries.length) return 0
      let y = Y
      const total = entries.reduce((n, [, v]) => n + v, 0)
      section(c, X, y++, W, "TOOLS", p, `${total} this run${a.failures ? ` · ${a.failures} failed` : ""}`, a.failures ? p.hot : p.dim)
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
      // Newest first; brightness fades with age and new entries flash in.
      const feed = a.items.filter((i) => i.type !== "reasoning" && i.type !== "assistant_message")
      let y = Y
      section(c, X, y++, W, "ACTIVITY", p, "latest run")
      if (!feed.length) {
        c.text(X, y++, "No tool activity recorded for this run.", p.dim)
        return y + 1 - Y
      }
      const limit = Math.max(6, Math.min(16, room - 2))
      for (const item of feed.slice(0, limit)) {
        const key = item.thread_id + item.updated_at + item.type + item.label + item.command
        if (!st.firstSeen.has(key)) st.firstSeen.set(key, now - st.bootAt < 2500 ? 0 : now)
        const seen = st.firstSeen.get(key)!
        const flash = seen ? Math.max(0, 1 - (now - seen) / 1600) : 0
        if (flash > 0) frame.animating = true
        const running = LIVE.has(item.status)
        const failed = item.status === "failed"
        const age = since(running ? item.started_at ?? item.updated_at : item.updated_at, now) ?? 0
        const cat = category(item.type, item.tool)
        const fade = running ? p.bright : age < 15 ? p.fg : age < 90 ? p.text2 : age < 600 ? p.dim : p.faint
        const bg = flash > 0 ? mix(p.bg, p.accent, 0.35 * flash) : undefined
        if (bg !== undefined) c.fill(X, y, W, 1, bg)
        c.text(X, y, running ? spinner(now) : failed ? "✗" : GLYPH[cat] ?? "·", failed ? p.hot : running ? p.bright : mix(fade, p.accent, 0.3), { bg })
        c.text(X + 2, y, duration(age).replace(/ \d+s$/, "").padStart(6), running ? p.text2 : p.faint, { bg })
        c.text(X + 9, y++, fit(itemLabel(item), W - 9), failed ? mix(p.hot, fade, 0.3) : fade, { bg })
      }
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
    const tag = fit("⇢ " + shortModel(pts[i].model || "handoff"), 16)
    c.text(cx + 1 + textWidth(tag) <= X + W ? cx + 1 : cx - textWidth(tag), y, tag, p.dim)
  }
  c.textRight(X + axis - 1, y, "100", p.faint)
  c.textRight(X + axis - 1, y + h - 1, "0", p.faint)
  if (h >= 4) c.textRight(X + axis - 1, y + Math.floor(h / 2), "50", p.faint)
  c.text(X + axis, y + h, "first turn", p.faint)
  c.textRight(X + W, y + h, "latest", p.faint)
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
    c.text(1, y, fit("SOURCE ERROR · last snapshot kept · " + st.error, W - 2), p.hot)
    return
  }
  const keys: [string, string][] = [["↑↓", "select"], ["⏎", "open in T3"], ["␣", "fold"], ["a", "active"],
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
  c.textRight(W - 1, y, st.demo ? "demo data · made up" : `read-only · ${duration(Math.max(0, (now - snap.at) / 1000))} ago`, p.faint)
}

function help(c: Canvas, p: Palette, W: number, H: number) {
  c.tint(0, 0, W, H, (f, b) => [mix(f, p.bg, 0.7), mix(b, p.bg, 0.5)])
  const keys: [string, string][] = [
    ["↑↓ j k", "select agent"], ["⏎ o", "open the thread in T3 Code"], ["␣ ← →", "fold / expand"], ["a", "active agents only"], ["t", "lane 15m · 30m · 1h · 3h"],
    ["J K PgDn", "scroll details"], ["f", "follow the thread open in T3"], ["p", "pause updates"], ["x", "CRT effects"],
    ["r", "refresh now"],
    ["mouse", "click selects, wheel scrolls"], ["q Esc", "quit (Esc closes help)"],
  ]
  const legend: [string, string][] = [
    ["⠹ ◆ ✓ ✗ ⊘", "running · needs you · done · failed · interrupted"],
    ["█████▌", "context used; warm past 75%, hot past 90%"],
    ["▂▄█", "lane: tool and model activity per time slice"],
    ["tint", "lane: a run was in progress"],
    ["┃  ▌", "lane: a prompt you sent · live edge"],
    ["│", "limit meter: time elapsed; fill beyond it is ahead of pace"],
    ["▎ band", "the thread open in T3 Code and its subagents"],
    ["◌", "running, but nothing new for 2m; warm after 8m"],
    ["❯ · 6m", "a tool call's run time; warm past 5m"],
  ]
  const two = W >= 84
  const bw = Math.min(W - 2, two ? 80 : 64)
  const keyRows = two ? Math.ceil(keys.length / 2) : keys.length
  const bh = Math.min(H - 2, keyRows + legend.length + 9)
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
  c.text(bx + 2, by, " T3 AGENTS · HELP ", p.bright, { attr: BOLD })
  c.clipped(bx + 1, by + 1, bw - 2, bh - 2, 0, () => {
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
  })
}
