// T3 Agents: live visual dashboard for local T3 Code agents and subagents.
import { homedir } from "node:os"
import { join } from "node:path"
import { parseArgs } from "node:util"
// Only the interactive dashboard loads OpenTUI; --once and --status stay light.
import { clean, duration, fit, shortModel } from "./format.ts"
import { Source, tree } from "./source.ts"
import { line, watch } from "./status.ts"

const USAGE = `Usage: t3-agents [--window] [--once | --status [--watch]] [--db PATH] [--interval SECONDS]

  --window          open in a dedicated terminal window, or focus it (launcher only)
  --once            print a plain-text snapshot and exit
  --status          print a one-line JSON status (used by the Omarchy bar widget)
  --watch           with --status, print a new line whenever the status changes
  --db PATH         T3 statev2.sqlite (default: \${T3CODE_HOME:-~/.t3}/userdata/statev2.sqlite)
  --interval SECS   data refresh period, minimum 0.5 (default 2)
`

function bar(frac: number, w: number): string {
  const units = Math.round(Math.max(0, Math.min(1, frac)) * w * 8)
  return ("█".repeat(Math.floor(units / 8)) + ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][units % 8]).padEnd(w, "░")
}

function once(source: Source) {
  const snap = source.snapshot()
  const live = snap.agents.filter((a) => a.status === "running").length
  const waiting = snap.agents.filter((a) => a.requests.length || a.status === "waiting").length
  console.log(`T3 AGENTS · read-only · ${new Date().toTimeString().slice(0, 8)} · ${live} running · ${waiting} waiting · ${snap.agents.length} agents`)
  const glyph: Record<string, string> = { running: "●", completed: "✓", failed: "✗", error: "✗", interrupted: "⊘", waiting: "◆" }
  for (const row of tree(snap.agents, new Set())) {
    const a = row.agent
    const prefix = row.guides.map((g) => (g ? "│ " : "  ")).join("") + (row.depth ? (row.last ? "└─" : "├─") : "")
    const { usedTokens: used, maxTokens: max } = a.context
    const ctx = typeof used === "number" && typeof max === "number" && max > 0
      ? `${bar(used / max, 16)} ${String(Math.round((used / max) * 100)).padStart(3)}%` : "context n/a"
    const name = fit(clean(prefix + (glyph[a.status] ?? "·") + " " + a.title), 44).padEnd(44)
    const age = a.ended ? duration((Date.now() - Date.parse(a.ended)) / 1000) + " ago" : duration(a.started ? (Date.now() - Date.parse(a.started)) / 1000 : null)
    console.log(`${name}  ${fit(shortModel(a.model), 16).padEnd(16)}  ${a.status.padEnd(11)}  ${ctx}  ${age}`)
  }
}

async function main() {
  let args
  try {
    args = parseArgs({ options: {
      db: { type: "string" }, interval: { type: "string", default: "2" },
      once: { type: "boolean", default: false }, help: { type: "boolean", short: "h", default: false },
      status: { type: "boolean", default: false }, watch: { type: "boolean", default: false },
    } }).values
  } catch (err) {
    process.stderr.write(`${err instanceof Error ? err.message : err}\n\n${USAGE}`)
    process.exit(2)
  }
  if (args.help) return void process.stdout.write(USAGE)
  const db = args.db ?? join(process.env.T3CODE_HOME ?? join(homedir(), ".t3"), "userdata/statev2.sqlite")
  const interval = Math.max(0.5, Number(args.interval) || 2)
  const source = new Source(db)
  if (args.status) {
    if (args.watch) return void watch(source)
    return void process.stdout.write(line(source) + "\n")
  }
  if (args.once) {
    try {
      return once(source)
    } catch (err) {
      process.stderr.write(`T3 source error: ${err instanceof Error ? err.message : err}\n`)
      process.exit(1)
    }
  }
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    process.stderr.write("Run in a terminal, or use --once for a plain-text snapshot.\n")
    process.exit(1)
  }
  const { App } = await import("./app.ts")
  const app = new App(source, interval)
  const fail = (err: unknown) => {
    app.renderer?.destroy()
    process.stderr.write(`t3-agents crashed: ${err instanceof Error ? err.stack : err}\n`)
    process.exit(1)
  }
  process.on("uncaughtException", fail)
  process.on("unhandledRejection", fail)
  await app.run()
  process.exit(0)
}

await main()
