// Subscription limits from the records Omarchy's agents plugin keeps in
// ~/.local/state/omarchy/agents/usage/ (written by omarchy-agent-usage-update).
import { readdirSync, readFileSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

export const USAGE_DIR = join(homedir(), ".local/state/omarchy/agents/usage")

export interface Limit {
  label: string
  title: string | null // model-scoped allowance, if any
  percent: number // 0..1 of the allowance used
  resetsAt: number | null
  windowMs: number | null
  /** Fraction of the window already elapsed; usage ahead of this is "over pace". */
  elapsed: number | null
}

export interface Provider {
  id: string
  name: string
  tier: string
  limits: Limit[]
  today: number
  models: { model: string; tokens: number }[] // today, largest first
  stale: boolean
}

export function windowLength(label: string): number | null {
  const m = /(\d+)\s*-?\s*(hour|day|week|month)/i.exec(label)
  if (!m) return /weekly/i.test(label) ? 7 * 86_400_000 : /monthly/i.test(label) ? 30 * 86_400_000 : null
  const unit = { hour: 3_600_000, day: 86_400_000, week: 7 * 86_400_000, month: 30 * 86_400_000 }[m[2].toLowerCase() as "hour"]
  return Number(m[1]) * unit
}

export function parseProvider(record: Record<string, any>, now = Date.now()): Provider | null {
  if (!record || record.ready === false) return null
  const limits: Limit[] = []
  for (const entry of Array.isArray(record.limits) ? record.limits : []) {
    const pct = Number(entry?.percent)
    if (!Number.isFinite(pct) || pct < 0) continue
    const resetsAt = Date.parse(entry.resetsAt ?? "")
    const reset = Number.isFinite(resetsAt) ? resetsAt : null
    const windowMs = windowLength(String(entry.label ?? ""))
    // A window that has already reset reads as unused, matching the bar panel.
    const expired = reset !== null && reset <= now
    const elapsed = reset !== null && windowMs ? Math.max(0, Math.min(1, 1 - (reset - now) / windowMs)) : null
    limits.push({ label: String(entry.label ?? "Limit"), title: entry.title ? String(entry.title) : null,
      percent: expired ? 0 : Math.min(1, pct), resetsAt: reset, windowMs, elapsed: expired ? 0 : elapsed })
  }
  const today = Number(record.todayTotalTokens) || 0
  if (!limits.length && !today) return null
  const models = Object.entries(record.todayTokensByModel ?? {})
    .map(([model, n]) => ({ model, tokens: Number(n) || 0 })).filter((m) => m.tokens > 0).sort((a, b) => b.tokens - a.tokens)
  return { id: String(record.id ?? "agent"), name: String(record.name ?? record.id ?? "Agent"),
    tier: String(record.tierLabel ?? ""), limits, today, models, stale: Boolean(record.limitsStale) }
}

export class UsageReader {
  private dir: string
  private checked = 0
  private signature = ""
  providers: Provider[] = []

  constructor(dir = USAGE_DIR) {
    this.dir = dir
  }

  poll(now = Date.now()): Provider[] {
    if (now - this.checked < 5000) return this.providers
    this.checked = now
    try {
      const files = readdirSync(this.dir).filter((f) => f.endsWith(".json")).sort()
      const signature = files.map((f) => f + statSync(join(this.dir, f)).mtimeMs).join("|")
      if (signature !== this.signature) {
        this.signature = signature
        this.cache = files.map((f) => {
          try {
            return JSON.parse(readFileSync(join(this.dir, f), "utf8"))
          } catch {
            return null
          }
        })
      }
      // Re-derive every poll so elapsed/expired track the clock.
      this.providers = this.cache.map((r) => parseProvider(r, now)).filter((p): p is Provider => p !== null)
        .sort((a, b) => b.today - a.today)
    } catch {
      this.providers = []
    }
    return this.providers
  }

  private cache: Record<string, any>[] = []
}
