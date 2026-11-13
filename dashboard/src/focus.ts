// The link between this dashboard and T3 Code's desktop app, both ways.
//
// Which thread T3 Code is showing: the desktop app titles its window
// "project / thread" (just "thread" without a project, its own name when no
// thread is open), so the title names the thread. Hyprland announces title and
// focus changes on its event socket: nothing is polled, nothing is written, and
// only T3 Code windows' titles are kept.
import { spawn } from "node:child_process"
import { readFileSync } from "node:fs"
import { createConnection, type Socket } from "node:net"
import { dirname, join } from "node:path"
import { clean } from "./format.ts"
import type { Agent } from "./source.ts"

const T3_CLASS = /t3code/i // com.t3tools.T3Code; never this dashboard (org.omarchy.t3-agents)
const RETRY_MS = 10_000
const strip = (address: string) => address.trim().replace(/^0x/, "")
const norm = (s: string) => clean(s).trim()

/** The app thread a T3 window title names, preferring "project / thread" over a bare title, then the newest. */
export function t3Thread(title: string | null, agents: Agent[]): string | null {
  const t = title === null ? "" : norm(title)
  if (!t) return null
  let best: Agent | null = null
  let bestRank = 0
  for (const a of agents) {
    if (a.id !== a.thread_id) continue // native subagents have no T3 page of their own
    const name = norm(a.title)
    const rank = t === `${norm(a.project)} / ${name}` ? 2 : t === name ? 1 : 0
    if (rank > bestRank || (rank > 0 && rank === bestRank && (a.updated ?? "") > (best?.updated ?? ""))) {
      best = a
      bestRank = rank
    }
  }
  return best?.id ?? null
}

/** Hyprland's socket directory for this session, or null outside Hyprland. */
export function hyprDir(env: NodeJS.ProcessEnv = process.env): string | null {
  const sig = env.HYPRLAND_INSTANCE_SIGNATURE
  const run = env.XDG_RUNTIME_DIR
  return sig && run && /^[\w.-]+$/.test(sig) ? join(run, "hypr", sig) : null
}

/** One request on Hyprland's command socket (for example "j/clients"); it replies and closes. */
function request(path: string, command: string, timeoutMs = 1500): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    const sock = createConnection(path)
    sock.setTimeout(timeoutMs, () => sock.destroy(new Error("Hyprland did not answer")))
    sock.on("connect", () => sock.write(command))
    sock.on("data", (d: Buffer) => chunks.push(d))
    sock.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")))
    sock.on("error", reject)
  })
}

interface Client { address?: string; class?: string; title?: string; focusHistoryID?: number }

export class T3Window {
  /** The title of the most recently focused T3 Code window, or null when there is none. */
  title: string | null = null
  private windows = new Map<string, string>() // address (no 0x) -> title, T3 windows only
  private current: string | null = null
  private events: Socket | null = null
  private retry: ReturnType<typeof setTimeout> | null = null
  private buffer = ""
  private stopped = false
  private syncing: Promise<void> | null = null

  private onChange: (title: string | null) => void
  readonly dir: string | null

  constructor(onChange: (title: string | null) => void, dir: string | null = hyprDir()) {
    this.onChange = onChange
    this.dir = dir
  }

  get available(): boolean {
    return this.dir !== null
  }

  start() {
    if (this.dir) this.connect()
  }

  stop() {
    this.stopped = true
    if (this.retry) clearTimeout(this.retry)
    this.events?.destroy()
  }

  private connect() {
    if (this.stopped || !this.dir) return
    // Subscribe before reading the window list so no change falls between them.
    const sock = createConnection(join(this.dir, ".socket2.sock"))
    this.events = sock
    sock.setEncoding("utf8")
    sock.unref()
    sock.on("connect", () => void this.sync())
    sock.on("data", (d: string) => this.feed(d))
    sock.on("error", () => {})
    sock.on("close", () => {
      if (this.stopped) return
      this.retry = setTimeout(() => this.connect(), RETRY_MS)
      this.retry.unref()
    })
  }

  /** Re-read every window; the T3 window focused most recently is the current one. */
  sync(): Promise<void> {
    if (!this.dir) return Promise.resolve()
    this.syncing ??= request(join(this.dir, ".socket.sock"), "j/clients")
      .then((reply) => {
        const clients = JSON.parse(reply) as Client[]
        this.windows.clear()
        let best: Client | null = null
        for (const c of Array.isArray(clients) ? clients : []) {
          if (!c.address || !T3_CLASS.test(c.class ?? "")) continue
          this.windows.set(strip(c.address), c.title ?? "")
          if (!best || (c.focusHistoryID ?? 1e9) < (best.focusHistoryID ?? 1e9)) best = c
        }
        this.current = best?.address ? strip(best.address) : null
        this.update()
      })
      .catch(() => {}) // keep the last title; the next event tries again
      .finally(() => {
        this.syncing = null
      })
    return this.syncing
  }

  private feed(data: string) {
    this.buffer += data
    let nl: number
    while ((nl = this.buffer.indexOf("\n")) >= 0) {
      this.event(this.buffer.slice(0, nl))
      this.buffer = this.buffer.slice(nl + 1)
    }
    if (this.buffer.length > 1 << 16) this.buffer = "" // never an event; do not grow
  }

  /** One socket2 line, "EVENT>>DATA". */
  event(line: string) {
    const sep = line.indexOf(">>")
    if (sep < 0) return
    const name = line.slice(0, sep)
    const data = line.slice(sep + 2)
    if (name === "windowtitlev2") {
      const comma = data.indexOf(",")
      const address = strip(data.slice(0, comma))
      if (comma > 0 && this.windows.has(address)) {
        this.windows.set(address, data.slice(comma + 1))
        this.update()
      }
    } else if (name === "activewindowv2") {
      const address = strip(data)
      if (this.windows.has(address) && address !== this.current) {
        this.current = address
        this.update()
      }
    } else if (name === "closewindow") {
      if (this.windows.has(strip(data))) void this.sync()
    } else if (name === "openwindow") {
      if (T3_CLASS.test(data)) void this.sync()
    }
  }

  private update() {
    const title = this.current === null ? null : this.windows.get(this.current) ?? null
    if (title === this.title) return
    this.title = title
    this.onChange(title)
  }
}

// ── Opening a thread in T3 Code ─────────────────────────────────────────────
// T3 Code registers t3code:// links. A running app receives
// t3code://threads/ENVIRONMENT/THREAD, shows that thread and raises its window;
// the path is built the way T3 builds its own thread links.

/** This installation's environment id, kept beside the database T3 writes. */
export function environmentId(dbPath: string): string | null {
  try {
    const id = readFileSync(join(dirname(dbPath), "environment-id"), "utf8").trim()
    return /^[\w-]{1,128}$/.test(id) ? id : null
  } catch {
    return null
  }
}

export function threadLink(environment: string, threadId: string): string | null {
  if (!environment || !threadId || threadId.length > 1024) return null
  return `t3code://threads/${encodeURIComponent(environment)}/${encodeURIComponent(threadId)}`
}

export type Spawner = typeof spawn

/** Hand a t3code:// link to the desktop's handler; resolves once the opener has started. */
export function openLink(link: string, run: Spawner = spawn): Promise<void> {
  if (!link.startsWith("t3code://")) return Promise.reject(new Error("not a T3 Code link"))
  return new Promise((resolve, reject) => {
    const child = run("xdg-open", [link], { detached: true, stdio: "ignore" })
    child.once("error", (err: NodeJS.ErrnoException) =>
      reject(new Error(err.code === "ENOENT" ? "xdg-open is not installed" : err.message)))
    child.once("spawn", () => {
      child.unref()
      resolve()
    })
  })
}
