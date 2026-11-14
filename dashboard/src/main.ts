// T3 Agents: live visual dashboard for local T3 Code agents and subagents.
import { constants, homedir } from "node:os"
import { join } from "node:path"
import { parseArgs } from "node:util"
// Only the interactive dashboard loads OpenTUI; --once and --status stay light.
import { clean, clock, fit, shortModel, span, when } from "./format.ts"
import { Source, SourceError, isLive, isRunning, needsYou, tree, type Agent } from "./source.ts"
import { animationsOff } from "./calm.ts"
import { line, watch } from "./status.ts"

const USAGE = `Usage: t3-agents [--window] [--once | --status [--watch]] [--db PATH | --demo] [--interval SECONDS] [--calm]

  --window          open in a dedicated terminal window, or focus it (launcher only)
  --once            print a plain-text snapshot and exit
  --status          print a one-line JSON status (used by the Omarchy bar widget)
  --watch           with --status, print a new line whenever the status changes
  --db PATH         T3 statev2.sqlite (default: \${T3CODE_HOME:-~/.t3}/userdata/statev2.sqlite)
  --demo            show made-up agents and usage instead of yours, to try it out or take
                    screenshots; reads no T3 or Omarchy data except the theme
  --interval SECS   data refresh period, minimum 0.5 (default 2)
  --calm            start in calm mode: nothing animates (on by default when Omarchy's
                    animations are off; m toggles it)
`

function bar(frac: number, w: number): string {
  const units = Math.round(Math.max(0, Math.min(1, frac)) * w * 8)
  return ("█".repeat(Math.floor(units / 8)) + ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][units % 8]).padEnd(w, "░")
}

const DONE: Record<string, string> = { completed: "✓", failed: "✗", error: "✗", interrupted: "⊘" }
const mark = (a: Agent) => (needsYou(a) ? "◆" : isRunning(a) ? "●" : DONE[a.status] ?? "·")

function once(source: Source, demo = false) {
  const snap = source.snapshot()
  const now = Date.now()
  const live = snap.agents.filter(isRunning).length
  const waiting = snap.agents.filter(needsYou).length
  console.log(`T3 AGENTS · ${demo ? "demo data" : "read-only"} · ${new Date().toTimeString().slice(0, 8)} · ${live} running · ${waiting} waiting · ${snap.agents.length} agents`)
  for (const row of tree(snap.agents, new Set())) {
    const a = row.agent
    const prefix = row.guides.map((g) => (g ? "│ " : "  ")).join("") + (row.depth ? (row.last ? "└─" : "├─") : "")
    const { usedTokens: used, maxTokens: max } = a.context
    const ctx = typeof used === "number" && typeof max === "number" && max > 0
      ? `${bar(used / max, 16)} ${String(Math.round((used / max) * 100)).padStart(3)}%` : "context n/a"
    const name = fit(clean(prefix + mark(a) + " " + a.title), 44).padEnd(44)
    // How long the run took and when, as the dashboard's list and detail pane say it.
    const start = a.started ? Date.parse(a.started) : NaN
    const counting = isLive(a) || needsYou(a)
    const end = counting || !a.ended ? null : Date.parse(a.ended)
    const age = !Number.isFinite(start) ? "--"
      : !counting && end === null ? `${"--".padStart(5)}  started ${clock(start, now)}`
      : `${span(((end ?? now) - start) / 1000).padStart(5)}  ${when(start, end, now)}`
    console.log(`${name}  ${fit(shortModel(a.model), 16).padEnd(16)}  ${a.status.padEnd(11)}  ${ctx}  ${age}`)
  }
}

/** What to try when T3's database can't be read, for someone at a terminal. */
function advice(err: unknown, origin: "--db" | "T3CODE_HOME" | "default"): string {
  if (!(err instanceof SourceError)) return `t3-agents: ${err instanceof Error ? err.message : err}\n`
  const where = {
    "--db": "Check the path given to --db.",
    T3CODE_HOME: `T3CODE_HOME is set to ${process.env.T3CODE_HOME}: check that it is T3's data folder, or unset it to use ~/.t3.`,
    default: "If T3 keeps its data somewhere else, set T3CODE_HOME to that folder or pass --db PATH.",
  }[origin]
  const lines = err.kind === "missing" ? [
    `no T3 database at ${err.path}`,
    "Is T3 Code installed, and has it been opened at least once?",
    where,
  ] : err.kind === "unreadable" ? [
    `can't read the T3 database at ${err.path}: ${err.detail}`,
    "Check that it is T3 Code's statev2.sqlite and that your user can read it.",
    where,
  ] : [
    `unsupported T3 schema in ${err.path}; ${err.detail}`,
    "T3 Code may have changed its database in an update: check for a newer T3 Agents.",
  ]
  lines.push("To try the dashboard without T3 data: t3-agents --demo")
  return `t3-agents: ${lines[0]}\n` + lines.slice(1).map((l) => `  ${l}\n`).join("")
}

async function main() {
  let args
  try {
    args = parseArgs({ options: {
      db: { type: "string" }, interval: { type: "string", default: "2" },
      once: { type: "boolean", default: false }, help: { type: "boolean", short: "h", default: false },
      status: { type: "boolean", default: false }, watch: { type: "boolean", default: false },
      demo: { type: "boolean", default: false }, calm: { type: "boolean", default: false },
    } }).values
    if (args.demo && args.db) throw new Error("--demo and --db cannot be used together")
  } catch (err) {
    process.stderr.write(`${err instanceof Error ? err.message : err}\n\n${USAGE}`)
    process.exit(2)
  }
  if (args.help) return void process.stdout.write(USAGE)
  // A demo builds its own database in a temporary directory, removed on exit.
  const kit = args.demo ? await import("./demo.ts") : null
  const demo = kit && new kit.Demo()
  if (demo && !(process.stdin.isTTY && process.stdout.isTTY && !args.once && !args.status)) {
    // Without the dashboard nothing else handles these; exiting this way runs the cleanup.
    for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"] as const) process.once(sig, () => process.exit(128 + constants.signals[sig]))
  }
  // An empty T3CODE_HOME counts as unset, as a relative path here would be a surprise.
  const db = demo?.path ?? args.db ?? join(process.env.T3CODE_HOME || join(homedir(), ".t3"), "userdata/statev2.sqlite")
  const interval = Math.max(0.5, Number(args.interval) || 2)
  const source = new Source(db)
  if (args.status) {
    if (args.watch) return void watch(source)
    return void process.stdout.write(line(source) + "\n")
  }
  if (args.once) {
    try {
      return once(source, demo !== null)
    } catch (err) {
      process.stderr.write(advice(err, args.db ? "--db" : process.env.T3CODE_HOME ? "T3CODE_HOME" : "default"))
      process.exit(1)
    }
  }
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    process.stderr.write("Run in a terminal, or use --once for a plain-text snapshot.\n")
    process.exit(1)
  }
  const { App } = await import("./app.ts")
  const calm = args.calm || animationsOff()
  const app = new App(source, interval, kit && demo ? { usage: new kit.DemoUsage(demo.epoch), demo: true, calm } : { calm })
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
