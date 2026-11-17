// Shared, synthetic Snapshot contract. No live input is accepted by this module.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { Demo } from "../../oracle/src/demo.ts"
import { Source, type Agent, type Item, type Snapshot } from "../../oracle/src/source.ts"

export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..")
export const SPEC = JSON.parse(readFileSync(join(ROOT, "fixtures/workloads.json"), "utf8"))
export const EPOCH = SPEC.epoch as number
const stamp = (t: number) => new Date(t).toISOString()

export function agent(id: string, parent: string | null, status: string, ageMinutes = 0, extra: Partial<Agent> = {}): Agent {
  const live = ["running", "waiting", "queued", "active"].includes(status)
  return {
    id, parent, title: id, kind: parent ? "subagent" : "thread", project: "synthetic-workspace", cwd: "/synthetic/project",
    model: "gpt-6.1-sol", status, run: {}, provider_id: "synthetic-provider", context: { usedTokens: 64000, maxTokens: 100000 },
    turn: {}, updated: stamp(EPOCH - ageMinutes * 60000), started: stamp(EPOCH - (ageMinutes + 10) * 60000),
    ended: live ? null : stamp(EPOCH - ageMinutes * 60000), remote: null,
    shelf: parent ? undefined : { listed: true, settled: false, snoozedUntil: null },
    items: [], recentFailures: [], requests: [], asked: null, thread_id: parent ?? id, branch: "synthetic-only", options: [],
    history: [], events: [], spans: [[EPOCH - 600000, live ? null : EPOCH - ageMinutes * 60000]],
    plan: [{ text: "Inspect synthetic input", status: "completed" }, { text: "Compare the output", status: "in_progress" }],
    mix: {}, failures: 0, now: null, ...extra,
  }
}

export function demoSnapshot(at = EPOCH + 150000): Snapshot {
  const demo = new Demo({ now: EPOCH, refreshMs: 0 })
  try {
    demo.rebuild(at)
    return { ...new Source(demo.path).snapshot(at), path: "synthetic:demo" }
  } finally { demo.stop() }
}

/** Full baseline Demo/Source seed: do not trim histories or events out of the native default. */
export function defaultSnapshot(): Snapshot { return demoSnapshot() }

export function edgeSnapshot(): Snapshot {
  const agents: Agent[] = [
    agent("edge-root", null, "running", 0, { title: "Nested agent tree · 日本語 · e\u0301 · 🍞 · " + "long label ".repeat(16) }),
    agent("edge-working", "edge-root", "running", 0),
    agent("edge-wait", "edge-root", "completed", 500, { requests: ["user_input"], asked: stamp(EPOCH - 300000) }),
    agent("edge-done5", "edge-root", "completed", 5),
    agent("edge-done20", "edge-root", "completed", 20),
    agent("edge-fail40", "edge-root", "failed", 40, { failures: 1 }),
    ...[90, 200, 300, 400].map((age) => agent(`edge-old${age}`, "edge-root", "completed", age)),
    agent("edge-grand", "edge-old300", "failed", 10),
    agent("edge-great", "edge-grand", "completed", 15),
    agent("edge-quiet", null, "running", 15),
    agent("edge-queued", null, "queued", 1),
    agent("edge-settled", null, "completed", 2, { shelf: { listed: true, settled: true, snoozedUntil: null } }),
    agent("edge-snoozed", null, "completed", 3, { shelf: { listed: true, settled: false, snoozedUntil: stamp(EPOCH + 3600000) } }),
    agent("edge-unlisted", null, "completed", 4, { shelf: { listed: false, settled: false, snoozedUntil: null } }),
    agent("edge-interrupted", null, "interrupted", 6),
  ]
  for (const a of agents) {
    a.history = Array.from({ length: 160 }, (_, i) => ({ pct: i / 200, used: i * 500, max: 100000,
      at: EPOCH - (160 - i) * 60000, provider: "synthetic", model: a.model }))
    a.events = Array.from({ length: 160 }, (_, i) => ({ t: EPOCH - (160 - i) * 15000,
      cat: i % 17 === 0 ? "you" : ["shell", "edit", "read", "think"][i % 4], failed: i % 29 === 0 }))
  }
  // Quiet is derived from absence of recent events, not just a fixture label.
  const quiet = agents.find((a) => a.id === "edge-quiet")!
  quiet.events = [{ t: EPOCH - 900000, cat: "read", failed: false }]
  const current = agents.find((a) => a.id === "edge-working")!
  const item: Item = { thread_id: "edge-root", run_id: "synthetic-run", node_id: current.id,
    provider_thread_id: "synthetic-provider", type: "command_execution", status: "running",
    updated_at: stamp(EPOCH - 1000), started_at: stamp(EPOCH - 2000), label: "Synthetic validation",
    command: "cargo test -- synthetic-only", file: "", tool: null }
  current.items = [item]; current.now = item
  return { agents, at: EPOCH, path: "synthetic:edge" }
}

/**
 * Characters `clean()` (oracle/src/format.ts) replaces with a one-column space before
 * drawing, while `fit()`, `wrap()` and `textWidth()` still measure them as zero (or as
 * one of their own): the ones the screen has to compose differently from how it measures.
 */
const HIDDEN = {
  zwsp: "\u200b",
  zwnj: "\u200c",
  zwj: "\u200d",
  lrm: "\u200e",
  rlm: "\u200f",
  shy: "\u00ad",
  wj: "\u2060",
  bom: "\ufeff",
  c0: "\u0001", // a C0 control
  c1: "\u0085", // a C1 control
  pua: "\ue000", // private use
  unassigned: "\u0378", // unassigned
  vs16: "\ufe0f", // variation selector, kept (category Mn) but zero columns
  combining: "\u0301", // combining mark, kept but zero columns
  jp: "\u{1f1ef}\u{1f1f5}", // regional indicators
  skin: "\u{1f3fd}", // skin-tone modifier
} as const
/** A real emoji sequence built from ZWJ. */
const FAMILY = `👨${HIDDEN.zwj}👩${HIDDEN.zwj}👧${HIDDEN.zwj}👦`
const RAINBOW = `🏳${HIDDEN.vs16}${HIDDEN.zwj}🌈`

/**
 * Hidden characters at the start, the middle and the end of data strings, including the
 * two cases that pull apart: a string `fit()` measures as short but `text()` draws wide,
 * and a string long enough that truncation runs through a run of them.
 */
export function hiddenSnapshot(): Snapshot {
  const at = (minutes: number) => stamp(EPOCH - minutes * 60000)
  const agentItem = (extra: Partial<Item>): Item => ({
    thread_id: "hidden-root", run_id: "hidden-run", node_id: null, provider_thread_id: "hidden-provider",
    type: "command_execution", status: "completed", updated_at: at(1), started_at: at(2),
    label: "", command: "", file: "", tool: null, ...extra,
  })
  const root = agent("hidden-root", null, "running", 0, {
    title: `${FAMILY} family build`,
    project: `synthetic${HIDDEN.zwsp}project`,
    model: `gpt${HIDDEN.zwsp}-5.1${HIDDEN.zwj}`,
    branch: `hidden${HIDDEN.zwnj}branch`,
    options: [{ id: `${HIDDEN.lrm}reasoning${HIDDEN.rlm}`, value: `high${HIDDEN.wj}` }],
  })
  const tree = [
    root,
    // A running subagent: the list's activity label follows its title.
    agent("hidden-work", "hidden-root", "running", 0, { title: `${HIDDEN.c0}control ${FAMILY} ${HIDDEN.c1}`, model: `cl${HIDDEN.shy}aude-4-6` }),
    // Waiting on you: its request text and its ask item both reach the screen.
    agent("hidden-wait", "hidden-root", "completed", 2, {
      title: `approve ${RAINBOW} ${HIDDEN.pua}now`,
      requests: [`user${HIDDEN.zwsp}input`],
      asked: at(3),
      project: `🏳${HIDDEN.zwj}project`, model: `gr${HIDDEN.wj}ok-4`,
    }),
    // Folded children: an "earlier" row, its subagents and the overflow row.
    agent("hidden-old10", "hidden-root", "completed", 10, { title: `older ${HIDDEN.bom}run` }),
    agent("hidden-old60", "hidden-root", "completed", 60, { title: `old ${HIDDEN.rlm}run` }),
    agent("hidden-old180", "hidden-root", "failed", 180, { title: `failed ${HIDDEN.zwj}run`, failures: 2 }),
    agent("hidden-old600", "hidden-root", "completed", 600, { title: `ancient ${HIDDEN.shy}run` }),
    // A remote subagent: its label reaches the detail pane.
    agent("hidden-remote", "hidden-root", "completed", 30, {
      title: `remote ${HIDDEN.unassigned}child`,
      remote: { environmentId: "synthetic-environment", threadId: "hidden-remote-thread", label: `other${HIDDEN.zwsp}machine` },
    }),
    // A parent known only by reference.
    agent("hidden-ref", null, "completed", 45, { title: `parent ${HIDDEN.vs16}reference`, referenceOnly: true }),
    // A long title that truncates, through runs of hidden characters.
    agent("hidden-long", null, "running", 1, {
      title: `${"hidden ".repeat(30)}${HIDDEN.zwsp.repeat(24)}tail`,
      model: `un${HIDDEN.pua}known`,
    }),
    // A title `fit()` measures as empty and `text()` draws 40 columns wide.
    agent("hidden-wide", null, "running", 1, { title: `${FAMILY} ${HIDDEN.zwsp.repeat(40)}`, model: `👨${HIDDEN.zwj}👩${HIDDEN.zwj}👧-model` }),
    agent("hidden-queued", null, "queued", 1, { title: `queued ${HIDDEN.combining}e\u0301 ${HIDDEN.jp}` }),
    agent("hidden-settled", null, "completed", 2, { title: `settled ${HIDDEN.skin}👍🏽`, shelf: { listed: true, settled: true, snoozedUntil: null } }),
    agent("hidden-snoozed", null, "completed", 3, { title: `snoozed ${HIDDEN.lrm}${HIDDEN.rlm}`, shelf: { listed: true, settled: false, snoozedUntil: stamp(EPOCH + 3600000) } }),
    agent("hidden-unlisted", null, "completed", 4, { title: `unlisted ${HIDDEN.zwj}`, shelf: { listed: false, settled: false, snoozedUntil: null } }),
    agent("hidden-interrupted", null, "interrupted", 6, { title: `interrupted ${HIDDEN.c0}${HIDDEN.c1}` }),
  ]
  root.items = [
    agentItem({ type: "command_execution", status: "running", command: `cargo test -- ${FAMILY} ${"hidden-argument ".repeat(12)}${HIDDEN.zwsp}`, updated_at: at(0) }),
    agentItem({ type: "file_change", file: `src/${FAMILY}${HIDDEN.zwsp}file${HIDDEN.rlm}.rs` }),
    agentItem({ type: "dynamic_tool", label: `tool ${HIDDEN.pua}label${HIDDEN.zwj}`, tool: `mcp${HIDDEN.zwsp}__tool` }),
    agentItem({ type: "web_search", label: `search ${HIDDEN.shy}text` }),
    agentItem({ type: "subagent", label: `spawn ${HIDDEN.wj}agent` }),
  ]
  root.now = root.items[0]
  root.plan = [
    { text: `inspect ${FAMILY} ${HIDDEN.zwsp}input`, status: "completed" },
    { text: `compare ${"hidden ".repeat(14)}${HIDDEN.zwj}output`, status: "in_progress" },
    ...Array.from({ length: 8 }, (_, i) => ({ text: `step ${i} ${HIDDEN.c0}${HIDDEN.bom}`, status: i < 3 ? "completed" : "pending" })),
  ]
  root.failures = 1
  root.recentFailures = [agentItem({
    type: "command_execution", status: "failed", exit_code: 1, updated_at: at(4),
    command: `false ${FAMILY} ${HIDDEN.zwsp}`,
  })]
  root.history = Array.from({ length: 12 }, (_, i) => ({
    pct: i / 16, used: i * 4000, max: 100000, at: EPOCH - (12 - i) * 60000,
    provider: i < 6 ? `syn${HIDDEN.zwsp}thetic` : `other${HIDDEN.zwj}`,
    model: i < 6 ? `gpt${HIDDEN.zwsp}-5.1` : `cl${HIDDEN.shy}aude-4-6`,
  }))
  root.events = Array.from({ length: 90 }, (_, i) => ({ t: EPOCH - (90 - i) * 20000, cat: ["shell", "edit", "read", "think"][i % 4], failed: i % 23 === 0 }))
  const wait = tree.find((a) => a.id === "hidden-wait")!
  wait.items = [agentItem({
    type: "user_input_request", status: "running", updated_at: at(3),
    label: `confirm ${FAMILY} ${HIDDEN.pua}request`, command: `ask ${HIDDEN.zwsp}command`,
  })]
  wait.now = wait.items[0]
  wait.plan = [{ text: `wait ${HIDDEN.zwj}step`, status: "in_progress" }]
  const rolled = tree.find((a) => a.id === "hidden-old180")!
  rolled.items = [agentItem({ type: "command_execution", status: "failed", exit_code: 2, updated_at: at(181), command: `fail ${HIDDEN.zwsp}${FAMILY}` })]
  rolled.recentFailures = rolled.items
  for (const a of tree) {
    a.history = a.history.length ? a.history : Array.from({ length: 8 }, (_, i) => ({ pct: i / 10, used: i * 5000, max: 100000,
      at: EPOCH - (8 - i) * 60000, provider: `syn${HIDDEN.zwsp}thetic`, model: a.model }))
    a.events = a.events.length ? a.events : Array.from({ length: 8 }, (_, i) => ({ t: EPOCH - (8 - i) * 30000, cat: ["shell", "edit"][i % 2], failed: false }))
    a.mix = { shell: 3, edit: 2, read: 1 }
  }
  return { agents: tree, at: EPOCH, path: `synthetic:hid${HIDDEN.zwsp}den` }
}

/**
 * The home folders `itemLabel` is drawn under (`oracle/src/view.ts`): a plain one, one whose
 * characters are special in a regular expression, and `null` for `HOME` unset. Every state is
 * drawn once under each, so the fixture needs no knowledge of the machine running it.
 */
export const LABEL_HOMES: (string | null)[] = ["/home/user", "/home/user.name[x]", null]

/**
 * `itemLabel`'s inputs: the home folder at the start, the middle, twice and nowhere; a path
 * that only begins with the same characters; first lines that are empty, blank or followed by
 * more lines; every item type, with and without a label. Numbers that reach the details pane
 * sit on rounding ties on purpose (12.5% and a 112,500-token turn), where `toFixed` and Rust's
 * `{:.0}` part company.
 */
export function labelsSnapshot(): Snapshot & { homes: (string | null)[]; actions: string[] } {
  const at = (minutes: number) => stamp(EPOCH - minutes * 60000)
  const A = LABEL_HOMES[0] as string
  const B = LABEL_HOMES[1] as string
  const item = (thread: string, extra: Partial<Item>): Item => ({
    thread_id: thread, run_id: `${thread}-run`, node_id: null, provider_thread_id: `${thread}-provider`,
    type: "dynamic_tool", status: "completed", updated_at: at(1), started_at: at(2),
    label: "", command: "", file: "", tool: null, ...extra,
  })
  const start = `${A}/projects/labels/src/main.rs`
  const middle = `run ${A}/bin/label --check`
  const twice = `first ${A}/one then ${A}/two`
  const exact = A
  const longer = `${A}name/notes.txt` // `/home/username`, which merely begins with `/home/user`
  const regexStart = `${B}/regex/file.txt`
  const regexNear = "/home/userXname[x]/not-the-home.txt" // the unescaped `.` would match this
  // Types and statuses no other fixture has: the view has to derive text or a glyph from each.
  const odd: [string, string, string][] = [
    ["", "", ""], ["", `${A}/untyped label`, ""], ["web_search", `${A}/Web_Search`, ""],
    ["Web_Search", `${A}/mixed type`, ""], ["tool_call_v2", `${A}/invented type`, ""],
    ["SECRET_REQUEST", "", ""], ["user input", `${A}/spaced type`, ""], ["un_known_kind", "", ""],
  ]
  const statuses: [string, string][] = [
    ["unknown", "unknown"], ["settled", "settled"], ["snoozed", "snoozed"], ["cancelled", "cancelled"],
    ["pending", "pending"], ["preparing", "preparing"], ["starting", "starting"], ["queued", "queued"],
    ["invented", "waiting-room"], ["empty", ""], ["mixed", "RuNnInG"], ["upper", "COMPLETED"],
    // Characters JavaScript and Rust must agree on when uppercasing an unseen status.
    ["sharp", "ßeta"], ["dotless", "ıdentity"],
  ]
  const labelled: [string, string][] = [
    ["web_search", start], ["web_search", ""],
    ["subagent", middle], ["subagent", ""],
    ["handoff", twice], ["handoff", ""],
    ["user_input_request", exact], ["user_input_request", ""],
    ["secret_request", longer], ["secret_request", ""],
    ["link_request", regexStart], ["link_request", ""],
    ["user_message", regexNear], ["user_message", ""],
    ["notification", " \t\nsecond line"], ["notification", ""],
    ["dynamic_tool", `tool\u200b${A}/hidden`], ["dynamic_tool", ""],
    ["browser_session", `mid ${A} here and ${A} again`], ["browser_session", ""],
    ["unknown_widget", exact], ["unknown_widget", ""],
  ]
  const commands: string[] = ["", "   ", "\nsecond line", "  \nls -la", `${A}/app/server --watch\nmore`,
    `  ${A}/app`, exact, `printf '${B}'\nnext`, "/home/username/cmd", "/home/userXname[x]/cmd2",
    "\tcmd\t", `${A}/a and ${A}/b`]
  const files: string[] = ["", `${A}/src/lib.rs`, "  ", `${B}/x/y.rs`, "/home/userXname[x]/z.rs", "src/mid.ts"]
  const items = (thread: string): Item[] => [
    ...labelled.map(([type, label]) => item(thread, { type, label, status: "completed" })),
    ...odd.map(([type, label]) => item(thread, { type, label, status: "completed" })),
    ...statuses.map(([name, status]) => item(thread, { type: "command_execution", status, command: `${A}/status-${name}` })),
    ...commands.map((command, i) => item(thread, { type: "command_execution", command, status: i % 3 ? "completed" : "failed", exit_code: i % 3 ? null : 2 })),
    ...files.map((file) => item(thread, { type: "file_change", file })),
    item(thread, { type: "reasoning", label: `${A}/ignored reasoning label` }),
    item(thread, { type: "assistant_message", label: `${A}/ignored reply label` }),
  ]
  // The most recent call of the asked agent: its label is the one the header shows.
  const ask = item("labels-wait", {
    type: "user_input_request", status: "running", updated_at: at(3), started_at: at(3),
    label: `${A}/confirm`, command: `${A}/confirm --release`,
  })
  const wait = agent("labels-wait", null, "waiting", 0, {
    title: `waiting on ${A}/approval`, requests: ["user_input"], asked: at(3),
    items: [ask, ...items("labels-wait").slice(0, 6)], now: ask,
  })
  const root = agent("labels-root", null, "running", 0, {
    title: "home folders",
    context: { usedTokens: 125000, maxTokens: 1000000 }, // 12.5%: Math.round gives 13, {:.0} gives 12
    turn: { tokenUsage: { inputTokens: 112500, outputTokens: 0 } },
  })
  root.items = [
    item("labels-root", { type: "command_execution", status: "running", updated_at: at(0), started_at: at(0), command: `${A}/projects/labels/build --watch` }),
    ...items("labels-root"),
  ]
  root.now = root.items[0]
  root.recentFailures = [
    item("labels-root", { type: "command_execution", status: "failed", exit_code: 1, updated_at: at(4), command: `${A}/bin/old-a\nsecond` }),
    item("labels-root", { type: "file_change", status: "failed", updated_at: at(112.5 / 60), file: `${A}name/old.txt`, run_id: "labels-root-old-run" }),
  ]
  root.failures = 2
  // Two turns of the same provider: the delta is 112,500 tokens, a `toFixed` tie.
  root.history = [225000, 337500].map((used, i) => ({ pct: used / 1000000, used, max: 1000000,
    at: EPOCH - (5 - i) * 60000, provider: "synthetic", model: root.model }))
  root.events = Array.from({ length: 12 }, (_, i) => ({ t: EPOCH - (12 - i) * 20000, cat: ["shell", "edit", "read", "think"][i % 4], failed: i % 7 === 0 }))
  root.plan = [
    { text: `inspect ${A}/plan`, status: "completed" },
    { text: `compare ${A} with ${A} again`, status: "in_progress" },
  ]
  root.mix = { shell: 2, edit: 1, read: 1 }
  const quiet = agent("labels-quiet", null, "running", 25, {
    // Quiet comes from no recent events; its line then follows its last call's label.
    items: [item("labels-quiet", { type: "web_search", updated_at: at(30), label: `${A}/quiet search` })],
    events: [{ t: EPOCH - 900000, cat: "read", failed: false }],
  })
  const sub = agent("labels-sub", "labels-root", "running", 0, {
    title: `sub ${A}/working`,
    // A run status no one has seen: the view reads the run's id, never its status.
    run: { id: "labels-sub-run", status: "unknown" },
    items: [item("labels-sub", { type: "dynamic_tool", status: "running", updated_at: at(0), label: `sub ${A}/middle` })],
  })
  sub.now = sub.items[0]
  const subTypes = agent("labels-sub-types", "labels-root", "running", 0, { title: "sub types",
    run: { id: "labels-sub-types-run", status: "SeTtLeD" } })
  subTypes.items = items("labels-sub-types")
  const types = agent("labels-types", null, "completed", 6, {
    title: `types ${B}`,
    context: { usedTokens: -2e9, maxTokens: 1e9 }, // negative: ratio clamps to 0
    items: items("labels-types"),
  })
  const huge = agent("labels-huge", null, "completed", 8, {
    title: "huge counts",
    context: { usedTokens: 9999990000000, maxTokens: 10000000000000 },
  })
  const fail = agent("labels-fail", null, "failed", 12, {
    title: `failed ${A}/run`, failures: 2,
    recentFailures: [
      item("labels-fail", { type: "command_execution", status: "failed", exit_code: 130, updated_at: at(12), command: `${B}/fail` }),
      item("labels-fail", { type: "file_change", status: "failed", updated_at: at(13), file: exact }),
    ],
  })
  const statusAgents = statuses.map(([name, status]) => {
    const a = agent(`labels-status-${name}`, null, status, 20, {
      title: `status ${name}`,
      run: { id: `labels-status-${name}-run`, status: `${status}\u200b-run` },
      items: [item(`labels-status-${name}`, { type: "subagent", updated_at: at(20), label: `${A}/status ${name}` })],
    })
    a.now = a.items[0]
    return a
  })
  const tree = [wait, root,
    sub, subTypes,
    // Enough finished children that the oldest folds into an "earlier" row.
    ...[40, 60, 190, 200, 260, 400].map((age) => agent(`labels-old${age}`, "labels-root", "completed", age, {
      title: `old ${age}`, items: [item(`labels-old${age}`, { type: "command_execution", command: `${A}/old-${age}` })],
    })),
    quiet, types, huge, fail, ...statusAgents,
  ]
  for (const a of tree) {
    a.history = a.history.length ? a.history : Array.from({ length: 8 }, (_, i) => ({
      pct: i / 10, used: i * 50000, max: 1000000, at: EPOCH - (8 - i) * 60000,
      provider: `syn\u200bthetic`, model: a.model,
    }))
    a.events = a.events.length ? a.events : Array.from({ length: 6 }, (_, i) => ({
      t: EPOCH - (6 - i) * 30000, cat: ["shell", "edit"][i % 2], failed: false,
    }))
  }
  return { agents: tree, at: EPOCH, path: `synthetic:lab${"\u200b"}els`, homes: LABEL_HOMES,
    actions: ["", "down", "end", "home,space", "pagedown", "?",
      ...statuses.map(([name]) => `pick:labels-status-${name}`),
      "pick:labels-sub", "pick:labels-sub-types",
      "pick:labels-wait", "pick:labels-types", "pick:labels-fail", "pick:labels-quiet", "a"] }
}

export function largeSnapshot(count = SPEC.large.agents): Snapshot {
  const template = edgeSnapshot().agents[0]
  const agents = Array.from({ length: count }, (_, i) => agent(`large-${i.toString().padStart(5, "0")}`,
    i % 16 === 0 ? null : `large-${(i - i % 16).toString().padStart(5, "0")}`,
    i % 11 === 0 ? "running" : i % 13 === 0 ? "failed" : "completed", i % 300,
    { title: `Synthetic agent ${i} · ${"long label ".repeat(i % 7)}`, history: structuredClone(template.history),
      events: structuredClone(template.events), requests: i % 31 === 0 ? ["approval"] : [],
      items: Array.from({ length: 8 }, (_, n) => ({ thread_id: "synthetic", run_id: null, node_id: null,
        provider_thread_id: null, type: "file_change", status: "completed", updated_at: stamp(EPOCH - n * 10000),
        started_at: null, label: "synthetic file", command: "", file: `src/file-${n}.rs`, tool: null })) }))
  return { agents, at: EPOCH, path: "synthetic:large" }
}

export function rapidSnapshot(index: number): Snapshot {
  const snap = edgeSnapshot()
  snap.at = EPOCH + index * SPEC.rapid.stepMs
  snap.path = "synthetic:rapid"
  const working = snap.agents.find((a) => a.id === "edge-working")!
  working.context.usedTokens = 64000 + index
  working.updated = stamp(snap.at)
  working.events.push({ t: snap.at, cat: "edit", failed: index % 37 === 0 })
  if (index % 5 === 0) working.requests = ["approval"]
  return snap
}

export function snapshot(name: string, count?: number): Snapshot {
  switch (name) {
    case "demo": return demoSnapshot()
    case "edge": return edgeSnapshot()
    case "hidden": return hiddenSnapshot()
    case "large": return largeSnapshot(count)
    case "rapid": return rapidSnapshot(0)
    case "empty": case "error": return { agents: [], at: EPOCH, path: `synthetic:${name}` }
    default: throw new Error(`Unknown synthetic case ${name}`)
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv[2] === "--write-default") {
    writeFileSync(join(ROOT, "fixtures/demo.json"), JSON.stringify(defaultSnapshot()) + "\n")
    console.log("Wrote deterministic synthetic fixtures/demo.json")
    process.exit(0)
  }
  const out = resolve(process.argv[2] ?? join(ROOT, "generated"))
  mkdirSync(out, { recursive: true })
  // The hidden-character and label fixtures are tracked inputs: the grid gate, their recordings
  // and the Rust replays all read the same bytes, so they are written beside the generator every run.
  writeFileSync(join(ROOT, "fixtures/hidden.json"), JSON.stringify(hiddenSnapshot()) + "\n")
  writeFileSync(join(ROOT, "fixtures/labels.json"), JSON.stringify(labelsSnapshot()) + "\n")
  const manifest: Record<string, unknown> = { version: 1, epoch: EPOCH, cases: {} }
  for (const name of SPEC.cases) {
    const snap = snapshot(name)
    const text = JSON.stringify(snap)
    writeFileSync(join(out, name + ".json"), text + "\n")
    manifest.cases[name] = { agents: snap.agents.length, events: snap.agents.reduce((n, a) => n + a.events.length, 0),
      history: snap.agents.reduce((n, a) => n + a.history.length, 0), bytes: Buffer.byteLength(text) }
  }
  // Compact burst contract: replace the named agent's mutable display fields.
  // Consumers retain the initial arrays and replace the last synthetic event, not grow a real log.
  const updates = Array.from({ length: SPEC.rapid.updates }, (_, index) => {
    const snap = rapidSnapshot(index)
    const a = snap.agents.find((a) => a.id === "edge-working")!
    return { index, at: snap.at, agentId: a.id, usedTokens: a.context.usedTokens, updated: a.updated,
      event: a.events.at(-1), requests: a.requests }
  })
  writeFileSync(join(out, "rapid-events.json"), JSON.stringify({ version: 1, initial: "rapid.json", updates }) + "\n")
  writeFileSync(join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n")
  console.log(JSON.stringify({ out, ...manifest }))
}
