// OpenTUI host: one full-screen custom renderable that flushes the canvas,
// app-wide keys, mouse selection, adaptive frame rate and optional CRT effects.
import {
  CRTRollingBarEffect, RGBA, Renderable, VignetteEffect, applyScanlines, createCliRenderer,
  type CliRenderer, type KeyEvent, type MouseEvent, type OptimizedBuffer, type RenderContext,
} from "@opentui/core"
import { CONTINUATION, Canvas } from "./canvas.ts"
import { T3Window, environmentId, openLink, threadLink } from "./focus.ts"
import { keyName } from "./keys.ts"
import { Source, type Snapshot } from "./source.ts"
import { ThemeWatcher } from "./theme.ts"
import { UsageReader } from "./usage.ts"
import { WINDOWS, draw, initialState, observe, trouble, type Frame, type ViewState } from "./view.ts"

/**
 * Frame rates. Idle keeps the clock ticking; the beat matches the spinners'
 * 100 ms frames; smooth is only for the moments a tween, a name shimmer or the
 * boot wipe is in flight.
 */
export const FPS = { idle: 2, beat: 10, smooth: 30 } as const

/** Spend frames only on what is moving. Calm mode and the help overlay stay at the idle rate. */
export function frameRate(frame: Frame | null, st: ViewState): number {
  if (!frame || st.calm || st.help) return FPS.idle
  if (frame.smooth) return FPS.smooth
  return frame.beat || st.fx ? FPS.beat : FPS.idle
}

const colors = new Map<number, RGBA>()
function rgba(c: number): RGBA {
  let v = colors.get(c)
  if (!v) {
    if (colors.size > 8192) colors.clear() // animated blends create many one-off colors
    v = RGBA.fromInts((c >> 16) & 255, (c >> 8) & 255, c & 255, 255)
    colors.set(c, v)
  }
  return v
}

/** Draw the canvas as runs of identically styled cells (few native calls per frame). */
export function flush(canvas: Canvas, buffer: OptimizedBuffer, ox: number, oy: number) {
  const { w, h, ch, fg, bg, at } = canvas
  for (let y = 0; y < h; y++) {
    let x = 0
    while (x < w) {
      const i = y * w + x
      const f = fg[i]
      const b = bg[i]
      const a = at[i]
      let s = ""
      let end = x
      while (end < w) {
        const j = y * w + end
        if (fg[j] !== f || bg[j] !== b || at[j] !== a) break
        const cp = ch[j]
        if (cp !== CONTINUATION) s += String.fromCodePoint(cp)
        else if (!s) s = " " // orphaned half of a wide character
        end++
      }
      buffer.drawText(s, ox + x, oy + y, rgba(f), rgba(b), a)
      x = end
    }
  }
}

/** Optional CRT look: scanlines, vignette and a slow rolling bar (colors only). */
export function crt(): (buffer: OptimizedBuffer, dt: number) => void {
  const vignette = new VignetteEffect(0.45)
  const bar = new CRTRollingBarEffect(0.35, 0.12, 0.18, 0.4)
  return (buffer, dt) => {
    applyScanlines(buffer, 0.78, 2)
    vignette.apply(buffer)
    bar.apply(buffer, dt)
  }
}

class Surface extends Renderable {
  host: App
  constructor(ctx: RenderContext, host: App) {
    super(ctx, {
      id: "t3-agents", width: "100%", height: "100%",
      onMouseDown: (e: MouseEvent) => host.click(e),
      onMouseScroll: (e: MouseEvent) => host.wheel(e),
    })
    this.host = host
  }

  protected renderSelf(buffer: OptimizedBuffer): void {
    this.host.paint(buffer, this.screenX, this.screenY, this.width, this.height)
  }
}

export interface AppOptions {
  usage?: Pick<UsageReader, "poll"> // limits and token counts; default: Omarchy's usage records
  demo?: boolean // made-up data: no link to the T3 Code window, and opening a thread only says what it would do
  calm?: boolean // start in calm mode: no spinners, tweens, shimmer, boot wipe or CRT effects, at the idle frame rate
}

export class App {
  readonly canvas = new Canvas()
  readonly st: ViewState
  readonly theme = new ThemeWatcher()
  readonly usage: Pick<UsageReader, "poll">
  readonly source: Source
  readonly demo: boolean
  snap: Snapshot = { agents: [], at: 0, path: "" }
  frame: Frame | null = null
  renderer: CliRenderer | null = null
  private timer: ReturnType<typeof setInterval> | null = null
  private fx = crt()
  // Follows the T3 Code window's title, so the thread open there is highlighted here.
  // Not in a demo: real window titles would never name made-up threads.
  readonly t3: T3Window | null

  constructor(source: Source, interval: number, options: AppOptions = {}) {
    this.source = source
    this.usage = options.usage ?? new UsageReader()
    this.demo = options.demo ?? false
    this.st = initialState(interval)
    this.st.calm = options.calm ?? false
    this.t3 = this.demo ? null : new T3Window((title) => {
      this.st.t3Title = title
      this.renderer?.requestRender()
    })
    this.st.t3Link = this.t3?.available ?? false
    this.st.demo = this.demo
    this.refresh()
  }

  refresh() {
    try {
      this.snap = this.source.snapshot()
      this.st.error = null
      this.st.cause = null
      observe(this.st, this.snap, Date.now()) // what this snapshot brought is what flashes
    } catch (err) {
      this.st.error = err instanceof Error ? err.message : String(err)
      this.st.cause = trouble(err, this.source.path) // the agent list says what to try
    }
    this.renderer?.requestRender()
  }

  async run() {
    const renderer = await createCliRenderer({ exitOnCtrlC: true, targetFps: FPS.idle, maxFps: FPS.smooth, useMouse: true, autoFocus: false })
    this.renderer = renderer
    renderer.root.add(new Surface(renderer, this))
    renderer.keyInput.on("keypress", (k: KeyEvent) => this.key(k))
    this.timer = setInterval(() => {
      if (!this.st.paused) this.refresh()
    }, this.st.interval * 1000)
    renderer.start()
    this.t3?.start()
    await new Promise<void>((resolve) => renderer.once("destroy", () => resolve()))
    if (this.timer) clearInterval(this.timer)
    this.t3?.stop()
  }

  quit() {
    if (this.timer) clearInterval(this.timer)
    this.renderer?.destroy()
  }

  paint(buffer: OptimizedBuffer, x: number, y: number, w: number, h: number) {
    const now = Date.now()
    this.theme.poll()
    this.canvas.resize(w, h)
    this.frame = draw(this.canvas, this.st, this.snap, this.usage.poll(now), this.theme.palette, now)
    flush(this.canvas, buffer, x, y)
    const fps = frameRate(this.frame, this.st)
    if (this.renderer && this.renderer.targetFps !== fps) this.renderer.targetFps = fps
  }

  private ids(): string[] {
    return (this.frame?.rows ?? []).map((r) => r.agent.id)
  }

  private select(delta: number) {
    const ids = this.ids()
    if (!ids.length) return
    const i = Math.max(0, ids.indexOf(this.st.selected ?? ""))
    const next = ids[Math.max(0, Math.min(ids.length - 1, i + delta))]
    if (next !== this.st.selected) {
      this.st.selected = next
      this.st.detailScroll = 0
    }
  }

  private scrollDetail(delta: number) {
    this.st.detailScroll = Math.max(0, Math.min(this.frame?.detailMax ?? 0, this.st.detailScroll + delta))
  }

  key(k: KeyEvent) {
    const st = this.st
    const seq = k.sequence
    const name = keyName(k.name) // numpad keys act like their main-keyboard twins
    const row = this.frame?.rows.find((r) => r.agent.id === st.selected)
    if (name === "escape" || seq === "q") {
      if (st.help && name === "escape") st.help = false
      else return this.quit()
    } else if (seq === "?" || seq === "h") st.help = !st.help
    else if (name === "down" || seq === "j") this.select(1)
    else if (name === "up" || seq === "k") this.select(-1)
    else if (name === "home" || seq === "g") this.select(-1e6)
    else if (name === "end" || seq === "G") this.select(1e6)
    else if (name === "pagedown" || seq === "J") this.scrollDetail(5)
    else if (name === "pageup" || seq === "K") this.scrollDetail(-5)
    else if (name === "space" && row?.hasChildren) {
      if (st.collapsed.has(row.agent.id)) st.collapsed.delete(row.agent.id)
      else st.collapsed.add(row.agent.id)
    } else if (name === "left" && row) {
      if (row.hasChildren && !st.collapsed.has(row.agent.id)) st.collapsed.add(row.agent.id)
      else if (row.agent.parent) st.selected = row.agent.parent
    } else if (name === "right" && row) st.collapsed.delete(row.agent.id)
    else if (name === "return" || name === "enter" || seq === "o") this.openInT3()
    else if (seq === "a") st.activeOnly = !st.activeOnly
    else if (seq === "f") {
      st.follow = !st.follow
      if (st.follow) st.t3Linked = null // select the T3 thread again on the next frame
    }
    else if (seq === "t") st.windowIndex = (st.windowIndex + 1) % WINDOWS.length
    else if (seq === "p") st.paused = !st.paused
    else if (seq === "r" && !st.paused) this.refresh()
    else if (seq === "x") {
      if (st.calm) this.note("Calm mode keeps CRT effects off · m leaves calm mode")
      else this.setFx(!st.fx)
    } else if (seq === "m") this.setCalm(!st.calm)
    else return
    this.renderer?.requestRender()
  }

  /** Show the selected agent's thread in T3 Code (a subagent opens the thread it works in). */
  private openInT3() {
    const st = this.st
    const a = this.snap.agents.find((x) => x.id === st.selected)
    if (!a) return
    const note = (text: string, error = false) => {
      st.notice = { text, at: Date.now(), error }
      this.renderer?.requestRender()
    }
    const thread = this.snap.agents.find((x) => x.id === a.thread_id) ?? a
    if (this.demo) return note(`Demo: would open ${thread.title} in T3 Code`)
    const env = environmentId(this.source.path)
    const link = env ? threadLink(env, a.thread_id) : null
    if (!link) return note("Cannot open in T3: no environment id beside the T3 database", true)
    // T3 then shows the thread; following keeps this selection because it is in that thread's family.
    openLink(link).then(
      () => note(`Opening in T3 · ${thread.title}`),
      (err: Error) => note(`Cannot open in T3: ${err.message}`, true),
    )
  }

  private setFx(on: boolean) {
    if (on === this.st.fx) return
    this.st.fx = on
    if (on) this.renderer?.addPostProcessFn(this.fx)
    else this.renderer?.removePostProcessFn(this.fx)
  }

  /** Calm mode: spinners hold a still glyph, values jump instead of tweening, no wipe, shimmer or CRT. */
  setCalm(on: boolean) {
    this.st.calm = on
    if (on) this.setFx(false)
    this.note(on ? "Calm mode: nothing moves but the data · m to leave" : "Calm mode off")
  }

  private note(text: string) {
    this.st.notice = { text, at: Date.now(), error: false }
  }

  click(e: MouseEvent) {
    if (this.st.help) {
      this.st.help = false
      return void this.renderer?.requestRender()
    }
    const hit = this.frame?.hits.find((h) => h.y === e.y && e.x >= h.x0 && e.x < h.x1)
    if (hit && hit.id !== this.st.selected) {
      this.st.selected = hit.id
      this.st.detailScroll = 0
      this.renderer?.requestRender()
    }
  }

  wheel(e: MouseEvent) {
    const d = this.frame?.detail
    const down = e.scroll?.direction === "down"
    if (d && e.x >= d.x && e.x < d.x + d.w && e.y >= d.y && e.y < d.y + d.h) this.scrollDetail(down ? 3 : -3)
    else this.select(down ? 1 : -1)
    this.renderer?.requestRender()
  }
}
