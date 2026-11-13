// Compact status for the Omarchy bar widget: counts plus the agents that matter now.
// `--status` prints one JSON line; `--status --watch` keeps printing a line each
// time the status changes, checking the database only when T3 has written to it.
import { statSync } from "node:fs"
import { clean, shortModel } from "./format.ts"
import { isLive, needsYou, ms, category, type Snapshot, type Source } from "./source.ts"
import { family } from "./theme.ts"

export interface StatusAgent {
  title: string
  state: string // "waiting" or the run status
  ctx: number | null // context used, 0..1
  model: string
  family: string
  now: string | null // tool category of the current action
}

export interface Status {
  v: 1
  running: number
  waiting: number
  failed: number // failed tool calls in the last ten minutes
  agents: StatusAgent[] // waiting first, then running, newest first; at most 8
  error?: string
}

const FAILED_WINDOW_MS = 10 * 60_000

export function status(snap: Snapshot, now = Date.now()): Status {
  const waiting = snap.agents.filter(needsYou)
  const live = snap.agents.filter((a) => isLive(a) && !needsYou(a))
  const newest = (a: { started: string | null }, b: { started: string | null }) => (ms(b.started) ?? 0) - (ms(a.started) ?? 0)
  const failed = snap.agents.reduce((n, a) => n + a.events.filter((e) => e.failed && e.t >= now - FAILED_WINDOW_MS).length, 0)
  const agents = [...waiting.sort(newest), ...live.sort(newest)].slice(0, 8).map((a) => {
    const { usedTokens: used, maxTokens: max } = a.context
    return {
      title: clean(a.title).slice(0, 80),
      state: needsYou(a) ? "waiting" : a.status,
      ctx: typeof used === "number" && typeof max === "number" && max > 0 ? Math.round(Math.min(1, used / max) * 100) / 100 : null,
      model: shortModel(clean(a.model)).slice(0, 32),
      family: family(a.model, a.provider_id ?? a.run?.provider),
      now: a.now ? category(a.now.type, a.now.tool) : null,
    }
  })
  return { v: 1, running: live.length, waiting: waiting.length, failed, agents }
}

function signature(path: string): string {
  // SQLite in WAL mode writes the -wal file first; either changing means new data.
  return [path, path + "-wal"].map((p) => {
    try {
      const s = statSync(p)
      return `${s.mtimeMs}:${s.size}`
    } catch {
      return "-"
    }
  }).join("|")
}

export function line(source: Source, now = Date.now()): string {
  try {
    return JSON.stringify(status(source.snapshot(now), now))
  } catch (err) {
    return JSON.stringify({ v: 1, running: 0, waiting: 0, failed: 0, agents: [], error: err instanceof Error ? err.message : String(err) })
  }
}

/** Emit a JSON line whenever the status changes; exit when the reader goes away. */
export function watch(source: Source, write: (s: string) => boolean | void = (s) => process.stdout.write(s), intervalMs = 2000) {
  const parent = process.ppid
  let seen = ""
  let last = ""
  let checked = 0
  process.stdout.on("error", () => process.exit(0)) // the bar closed the pipe
  const tick = () => {
    if (process.ppid !== parent) process.exit(0) // the shell went away without closing the pipe
    const now = Date.now()
    const sig = signature(source.path)
    // Re-read when T3 wrote, and every 30 s so the ten-minute failure window can age out.
    if (sig === seen && now - checked < 30_000) return
    seen = sig
    checked = now
    const next = line(source, now)
    if (next !== last) {
      last = next
      write(next + "\n")
    }
  }
  tick()
  return setInterval(tick, intervalMs)
}
