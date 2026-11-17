// Semantic oracle calls the pinned product's pure view; it never starts a terminal renderer.
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { Canvas } from "../../oracle/src/canvas.ts"
import { needsYou, pace, type Snapshot } from "../../oracle/src/source.ts"
import { derivePalette } from "../../oracle/src/theme.ts"
import { draw, initialState, nextScope, nextWaiting, observe, pick, showEarlier, step, toggle,
  type Frame, type ViewState } from "../../oracle/src/view.ts"
import { SPEC, snapshot } from "./fixtures.ts"

export const PALETTE = derivePalette({ background: "#101315", foreground: "#cacccc", accent: "#798186",
  selection: "#343d41", red: "#565d60", yellow: "#d9dbdc", bright_red: "#de6145", bright_yellow: "#c9c2b4" })

export type Action = { key: string } | { select: string }
export const actionName = (a: Action): string => "key" in a ? a.key : "pick:" + a.select
export function semantic(st: ViewState, frame: Frame) {
  return { selected: st.selected, scope: st.scope, collapsed: [...st.collapsed].sort(), expanded: [...st.expanded].sort(),
    rows: frame.rows.map((r) => ({ id: r.id, depth: r.depth, stage: r.stage, kept: r.kept, tucked: r.tucked,
      earlier: (r.earlier ?? []).map((a) => a.id) })) }
}

export function action(st: ViewState, frame: Frame, snap: Snapshot, name: string) {
  const now = snap.at
  const ids = frame.rows.map((r) => r.id)
  const row = frame.rows.find((r) => r.id === st.selected)
  const move = (delta: number) => {
    if (!ids.length) return
    const index = Math.max(0, ids.indexOf(st.selected ?? ""))
    pick(st, ids[Math.max(0, Math.min(ids.length - 1, index + delta))], now)
  }
  if (name.startsWith("pick:")) pick(st, name.slice(5), now)
  else if (name === "down" || name === "j") move(1)
  else if (name === "up" || name === "k") move(-1)
  else if (name === "home") move(-1e6)
  else if (name === "end") move(1e6)
  else if (name === "n") nextWaiting(st, snap.agents, now)
  else if (name === "a") nextScope(st, now)
  else if (name === "pagedown") st.detailScroll = Math.min(frame.detailMax, st.detailScroll + 5)
  else if (name === "pageup") st.detailScroll = Math.max(0, st.detailScroll - 5)
  else if (name === "t") st.windowIndex = (st.windowIndex + 1) % 4
  else if (name === "m") st.calm = !st.calm
  else if (row?.earlier && ["space", "right", "enter"].includes(name)) showEarlier(st, row, now)
  else if (name === "space" && row) toggle(st, row)
  else if (name === "right" && row) step(st, row, 1)
  else if (name === "left" && row) {
    const parent = row.earlier ? row.agent.id : row.agent.parent
    if (!step(st, row, -1) && parent) pick(st, parent, now)
  }
}

export function replay(snap: Snapshot, actions: string[] = [], width = 150, height = 70, error?: string) {
  const st = initialState(2)
  st.demo = true; st.calm = true; st.bootAt = 0; st.follow = false
  if (error) {
    st.error = error
    st.cause = { kind: "schema", path: snap.path, detail: error }
  }
  observe(st, snap, snap.at)
  const canvas = new Canvas(width, height)
  let frame = draw(canvas, st, snap, [], PALETTE, snap.at)
  const frames = [{ action: "initial", ...semantic(st, frame) }]
  for (const key of actions) {
    action(st, frame, snap, key)
    frame = draw(canvas, st, snap, [], PALETTE, snap.at)
    frames.push({ action: key, ...semantic(st, frame) })
  }
  return { frames, text: canvas.toText(), canvas, state: st, frame }
}

// This additional summary is independent from pixels and distinguishes quiet from failed/waiting.
export function facts(snap: Snapshot) {
  return { agents: snap.agents.length,
    waiting: snap.agents.filter(needsYou).map((a) => a.id).sort(),
    failed: snap.agents.filter((a) => ["failed", "error"].includes(a.status)).map((a) => a.id).sort(),
    quiet: snap.agents.filter((a) => pace(a, snap.at)?.quiet).map((a) => a.id).sort(),
    eventCount: snap.agents.reduce((n, a) => n + a.events.length, 0),
    historyCount: snap.agents.reduce((n, a) => n + a.history.length, 0) }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2)
  const name = args[0] ?? "edge"
  const actions = args.includes("--no-actions") ? [] : SPEC.actions.map(actionName)
  const result = replay(snapshot(name), actions, 150, 70, name === "error" ? "synthetic schema failure" : undefined)
  if (args.includes("--text")) process.stdout.write(result.text + "\n")
  else console.log(JSON.stringify({ version: 1, case: name, facts: facts(snapshot(name)), frames: result.frames }))
}
