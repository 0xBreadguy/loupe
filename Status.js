.pragma library

// Pure helpers for the Loupe bar widget, shared by the QML and its tests.

var SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
var BEAT_MS = 100
var CAUSES = { watcher: true, launcher: true, exited: true, service: true }
var STATES = { waiting: true, running: true, active: true, starting: true, preparing: true, queued: true, pending: true }
// The model's provider, which picks its hue, and what the agent is doing now:
// the categories the status line can send. Anything else falls back.
var FAMILIES = { claude: true, openai: true, grok: true, gemini: true, other: true }
var NOW = { shell: true, read: true, edit: true, web: true, agent: true, ask: true, think: true, reply: true,
  you: true, note: true, tool: true, other: true }

// Text from another process: no control or bidi characters and nothing markup-shaped.
function plain(value, max) {
  var s = String(value === undefined || value === null ? "" : value)
    .replace(/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028-\u202e\u2066-\u2069\ufeff]/g, " ")
    .replace(/</g, "‹").replace(/>/g, "›")
    .replace(/\s+/g, " ").trim()
  if (max && s.length > max) s = s.slice(0, max - 1) + "…"
  return s
}

function count(n) {
  n = Number(n)
  return isFinite(n) ? Math.max(0, Math.min(999, Math.floor(n))) : 0
}

// No status to show, and why: the watcher reported an error ("watcher"), could
// not start ("launcher"), stopped ("exited", with its exit code), or the
// plugin's service is missing ("service").
function unavailable(reason, cause, code) {
  return { running: 0, waiting: 0, failed: 0, agents: [], error: plain(reason || "unavailable", 120),
    cause: CAUSES[cause] === true ? cause : "watcher", code: typeof code === "number" && isFinite(code) ? code : null }
}

// The spinner frame at a moment. The dashboard uses the same beat, so the bar
// and the dashboard tick in phase.
function frameAt(ms) {
  return Math.floor(ms / BEAT_MS) % SPIN.length
}

// One line from `loupe --status --watch`, validated and bounded; null when unusable.
function parse(line) {
  var raw
  try {
    raw = JSON.parse(String(line).slice(0, 65536))
  } catch (e) {
    return null
  }
  if (!raw || typeof raw !== "object" || raw.v !== 1) return null
  if (raw.error) return unavailable(raw.error, "watcher")
  var agents = []
  var list = Array.isArray(raw.agents) ? raw.agents.slice(0, 8) : []
  for (var i = 0; i < list.length; i++) {
    var a = list[i] || {}
    var ctx = Number(a.ctx)
    agents.push({
      title: plain(a.title, 60) || "Untitled",
      state: STATES[a.state] === true ? a.state : "running",
      ctx: a.ctx === null || a.ctx === undefined || !isFinite(ctx) ? null : Math.max(0, Math.min(1, ctx)),
      model: plain(a.model, 24),
      family: FAMILIES[a.family] === true ? a.family : "other",
      // null means the agent has gone quiet. An activity this version doesn't
      // know is still activity, so it becomes "other" rather than quiet.
      now: a.now === null || a.now === undefined ? null : (NOW[a.now] === true ? a.now : "other")
    })
  }
  return { running: count(raw.running), waiting: count(raw.waiting), failed: count(raw.failed), agents: agents, error: "" }
}

// A braille glyph as the bitmask of its raised dots (bit 0 is dot 1 ... bit 7 is
// dot 8, Unicode's order), so the bar can draw the dashboard's spinner as
// geometry whose footprint never changes between frames.
function dots(glyph) {
  var c = String(glyph || "").charCodeAt(0)
  return c >= 0x2800 && c <= 0x28ff ? c - 0x2800 : 0
}

// The dots lit for a spinner frame. With animations off it is one fixed frame,
// which still differs from idle, where nothing is lit.
function spinDots(frame, animate) {
  return dots(SPIN[animate ? Math.abs(Math.floor(frame)) % SPIN.length : 2])
}

// The bar mark's lit dots: the spinner while agents run and none need you;
// nothing otherwise.
function litDots(status, frame, animate) {
  if (!status || status.error || status.waiting > 0 || !(status.running > 0)) return 0
  return spinDots(frame, animate)
}

// An agent working on something right now: not waiting on you, not queued, and
// with a current activity. The card draws it with a spinner; a quiet one gets a
// still ring.
function working(a) {
  return !!a && a.state !== "waiting" && a.state !== "queued" && a.state !== "pending"
    && a.now !== null && a.now !== undefined
}

// Whether an open card has anything to spin: its header's cell, which spins as
// the bar's does, or a working agent's row. When nothing does, nothing ticks.
function cardSpins(status) {
  if (!status || status.error) return false
  if (litDots(status, 0, true) !== 0) return true
  var agents = status.agents || []
  for (var i = 0; i < agents.length; i++) if (working(agents[i])) return true
  return false
}

// Gemini's hue, by the dashboard's rule but from the theme's own colors, as the
// other hues here are: the first of the theme's blue, its bright_blue and the
// dashboard's stock blues that reads as blue and stands at least 12 apart
// (OKLab ΔE × 100) from the hues Claude, OpenAI and Grok take here (magenta,
// green, blue). Otherwise the stock blue farthest from the nearest of them.
var GEMINI_STOCK = ["#4285f4", "#2867e4", "#83c4ff", "#1e4eac", "#113585"]

function oklab(hex) {
  var n = parseInt(String(hex).slice(1), 16)
  var c = [16, 8, 0].map(function (shift) {
    var s = ((n >> shift) & 255) / 255
    return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4)
  })
  var l = Math.cbrt(0.4122214708 * c[0] + 0.5363325363 * c[1] + 0.0514459929 * c[2])
  var m = Math.cbrt(0.2119034982 * c[0] + 0.6806995451 * c[1] + 0.1073969566 * c[2])
  var s = Math.cbrt(0.0883024619 * c[0] + 0.2817188376 * c[1] + 0.6299787005 * c[2])
  return [0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s]
}

function deltaE(a, b) {
  var p = oklab(a), q = oklab(b)
  return 100 * Math.sqrt(Math.pow(p[0] - q[0], 2) + Math.pow(p[1] - q[1], 2) + Math.pow(p[2] - q[2], 2))
}

function readsBlue(hex) {
  var c = oklab(hex)
  var h = Math.atan2(c[2], c[1]) * 180 / Math.PI
  if (h < 0) h += 360
  return h >= 230 && h <= 275 && Math.sqrt(c[1] * c[1] + c[2] * c[2]) >= 0.08
}

function geminiHue(named) {
  named = named || {}
  var others = [named.magenta, named.green, named.blue].filter(function (c) { return !!c })
  var nearest = function (c) {
    return others.reduce(function (d, o) { return Math.min(d, deltaE(c, o)) }, Infinity)
  }
  var candidates = [named.blue, named.bright_blue].filter(function (c) { return !!c }).concat(GEMINI_STOCK)
  for (var i = 0; i < candidates.length; i++)
    if (readsBlue(candidates[i]) && nearest(candidates[i]) >= 12) return candidates[i]
  return GEMINI_STOCK.reduce(function (best, c) { return nearest(c) > nearest(best) ? c : best })
}
