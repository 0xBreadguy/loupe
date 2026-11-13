.pragma library

// Pure helpers for the T3 Agents bar widget, shared by the QML and its tests.

var SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
var STATES = { waiting: true, running: true, active: true, starting: true, preparing: true, queued: true, pending: true }

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

function unavailable(reason) {
  return { running: 0, waiting: 0, failed: 0, agents: [], error: plain(reason || "unavailable", 120) }
}

// One line from `t3-agents --status --watch`, validated and bounded; null when unusable.
function parse(line) {
  var raw
  try {
    raw = JSON.parse(String(line).slice(0, 65536))
  } catch (e) {
    return null
  }
  if (!raw || typeof raw !== "object" || raw.v !== 1) return null
  if (raw.error) return unavailable(raw.error)
  var agents = []
  var list = Array.isArray(raw.agents) ? raw.agents.slice(0, 8) : []
  for (var i = 0; i < list.length; i++) {
    var a = list[i] || {}
    var ctx = Number(a.ctx)
    agents.push({
      title: plain(a.title, 60) || "Untitled",
      state: STATES[a.state] === true ? a.state : "running",
      ctx: a.ctx === null || a.ctx === undefined || !isFinite(ctx) ? null : Math.max(0, Math.min(1, ctx)),
      model: plain(a.model, 24)
    })
  }
  return { running: count(raw.running), waiting: count(raw.waiting), failed: count(raw.failed), agents: agents, error: "" }
}

// The bar mark: a diamond when an agent needs you, a spinner while agents run.
function glyph(status, frame, animate) {
  var ok = status && !status.error
  if (ok && status.waiting > 0) return "◆"
  if (ok && status.running > 0 && animate) return SPIN[Math.abs(Math.floor(frame)) % SPIN.length]
  return "⠿"
}

// "◆ Title · opus 5.5 · 36%"; formatNumber keeps digits in the user's language.
function agentLine(a, formatNumber) {
  var parts = [(a.state === "waiting" ? "◆ " : "⠿ ") + a.title]
  if (a.model) parts.push(a.model)
  if (a.ctx !== null) {
    var pct = Math.round(a.ctx * 100)
    parts.push((formatNumber ? formatNumber(pct) : String(pct)) + "%")
  }
  return parts.join(" · ")
}
