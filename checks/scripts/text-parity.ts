// Engine text semantics: every small pure helper compared against the TypeScript oracle in
// oracle/src. The Rust side reads one JSON value per stdin line (`--probe-helper NAME`) and
// prints one JSON result per line; this script feeds the same arguments to the original and
// fails on any difference.
//
// Lone surrogates are excluded: they can only arrive through a JavaScript string, never through
// JSON or SQLite text, which is all these helpers read.
import {
  cellWidth, textWidth, fit, wrap, tokens, duration, span, clock, when, shortModel, clean,
} from '../../oracle/src/format.ts'
import { family, hex, mix, luminance, contrast, chroma, ramp, reach } from '../../oracle/src/theme.ts'
import { category } from '../../oracle/src/source.ts'
import { GLYPH, itemLabel, spinner } from '../../oracle/src/view.ts'
import { threadRef, threadKey } from '../../oracle/src/identity.ts'
import { threadLink, agentThreadLink, t3Thread } from '../../oracle/src/focus.ts'
import { windowLength } from '../../oracle/src/usage.ts'
import { level, spark, bigWidth } from '../../oracle/src/canvas.ts'
import { spawnSync } from 'node:child_process'
import { enabled as recording, recordGroup, recordKept, recordHidden } from './text-record.ts'

const binary = process.env.T3_NATIVE_BINARY ?? 'target/release/loupe-core'
const MAX_BUFFER = 512 * 1024 * 1024

function probe(name: string, payload: string): string[] {
  const run = spawnSync(binary, ['--probe-helper', name], { input: payload, encoding: 'utf8', maxBuffer: MAX_BUFFER })
  if (run.error || run.status !== 0) {
    const detail = run.stderr ? `: ${String(run.stderr).trim()}` : ''
    const why = run.error ? String((run.error as any).message ?? run.error) : `exit ${run.status}`
    throw new Error(`${name}: ${why}${detail}`)
  }
  const out = run.stdout
  return out === '' ? [] : out.replace(/\n$/, '').split('\n')
}

/** JSON has no NaN or infinities; they travel as their JavaScript spellings. Doubles that are
 * not safe integers also travel as strings, because serde_json does not round every decimal
 * literal correctly; Rust's f64 parser does. */
function wire(value: any): any {
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) return String(value)
    if (Number.isSafeInteger(value) && !Object.is(value, -0)) return value
    return String(value)
  }
  if (Array.isArray(value)) return value.map(wire)
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {}
    for (const key of Object.keys(value)) out[key] = wire(value[key])
    return out
  }
  return value
}

function same(a: any, b: any): boolean {
  if (typeof a === 'number' && typeof b === 'number') return a === b || (Number.isNaN(a) && Number.isNaN(b))
  if (Array.isArray(a) || Array.isArray(b)) {
    return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((x, i) => same(x, b[i]))
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const keys = Object.keys(a)
    return keys.length === Object.keys(b).length && keys.every((k) => same(a[k], b[k]))
  }
  return a === b
}

/** Expected values in their comparable form: numbers stay numbers, non-finite becomes the
 * JavaScript spelling JSON can carry. */
function canonical(value: any): any {
  if (typeof value === 'number') return Number.isFinite(value) ? value : String(value)
  if (Array.isArray(value)) return value.map(canonical)
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {}
    for (const key of Object.keys(value)) out[key] = canonical(value[key])
    return out
  }
  return value
}

function compare(name: string, inputs: any[], expected: (input: any) => any, show: (input: any) => string = (x) => JSON.stringify(wire(x))) {
  const payload = inputs.map((x) => JSON.stringify(wire(x))).join('\n') + '\n'
  const actual = probe(name, payload)
  const wanted: any[] = [] // only filled when recording (T3_RECORD), see text-record.ts
  let differences = 0
  for (let i = 0; i < inputs.length; i++) {
    let got: any
    try { got = JSON.parse(actual[i]) } catch { got = actual[i] }
    const want = canonical(expected(inputs[i]))
    if (recording) wanted.push(want)
    if (!same(got, want)) {
      if (differences < 5) console.log(`  ${show(inputs[i])}: Node ${JSON.stringify(want)}, Rust ${actual[i]}`)
      differences++
    }
  }
  console.log(`${name}: ${inputs.length} inputs, ${differences} differ`)
  if (actual.length !== inputs.length || differences) process.exitCode = 1
  recordGroup(name, payload, wanted)
}

// ── clean and display width (already exhaustive) ─────────────────────────────
{
  const count = 0x110000
  const input = Array.from({ length: count }, (_, i) => i).join('\n') + '\n'
  const run = spawnSync(binary, ['--probe-hidden'], { input, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 })
  if (run.error || run.status !== 0) throw run.error ?? new Error(run.stderr)
  const actual = run.stdout.trimEnd().split('\n')
  let differences = 0
  for (let cp = 0; cp < count; cp++) if (actual[cp] !== (+/\p{C}/u.test(String.fromCodePoint(cp))).toString()) differences++
  console.log(`clean: ${count} inputs, ${differences} differ`)
  if (actual.length !== count || differences) process.exitCode = 1
  recordHidden(input, count, (cp) => /\p{C}/u.test(String.fromCodePoint(cp)))
}

function compareWidth(name: string, inputs: (number | string)[], expected: (input: any) => number) {
  const run = spawnSync(binary, ['--probe-width'], {
    input: inputs.map((x) => JSON.stringify(x)).join('\n') + '\n', encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
  })
  if (run.error || run.status !== 0) throw run.error ?? new Error(run.stderr)
  const actual = run.stdout.trimEnd().split('\n')
  let differences = 0
  for (let i = 0; i < inputs.length; i++) {
    const want = expected(inputs[i])
    if (actual[i] !== String(want)) {
      if (differences < 5) console.log(`  ${JSON.stringify(inputs[i])}: Node ${want}, Rust ${actual[i]}`)
      differences++
    }
  }
  console.log(`${name}: ${inputs.length} inputs, ${differences} differ`)
  if (actual.length !== inputs.length || differences) process.exitCode = 1
}

// ── seeded inputs ────────────────────────────────────────────────────────────
function makeRandom(seed: number) {
  return () => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed }
}

const pool: string[] = [
  // ASCII and punctuation
  'a', 'Z', 'z', 'A', '0', '9', '5', ' ', '/', '\\', '-', '_', '.', ':', '+', '%', '~', '!', '*',
  "'", '(', ')', '"', '@', '#', '&', '=', '<', '>', '|', '$', ',', ';', '?', '^', '`', '{', '}', '[', ']',
  // whitespace, all kinds
  '\t', '\n', '\v', '\f', '\r', '\u0085', '\u00a0', '\u1680', '\u2000', '\u2001', '\u2002', '\u2003',
  '\u2004', '\u2005', '\u2006', '\u2007', '\u2008', '\u2009', '\u200a', '\u200b', '\u2028', '\u2029',
  '\u202f', '\u205f', '\u3000', '\ufeff',
  // combining marks and format characters
  '\u0301', '\u036f', '\u20d0', '\u200e', '\u200f', '\u202a', '\u2066', '\u2069', '\u00ad',
  // controls
  '\u0000', '\u0001', '\u001f', '\u007f', '\u009f',
  // CJK and fullwidth forms
  '中', '日', '。', '、', 'Ａ', 'Ｚ', '１', '９', '！', '　',
  // emoji: ZWJ sequences, variation selectors, flags
  '😀', '👩\u200d💻', '\ufe0f', '🇺🇸', '🇯🇵', '🏳️\u200d🌈', '🧑\u200d🚀', '♥\ufe0f',
  // astral letters
  '𐐀', '𝐷', '𝕬', '𞤀',
  // unassigned code points
  '\u0378', '\u0380', '\u05ff', '\u1abf', '\u2fe0', '\U000e0000',
  // a couple of words so wrapping sees multi-character tokens
  'word', 'thread', '2026-11-12T11:00:00Z', 'claude-opus-4-5', 'gpt-5', 'e-1',
]

const LENGTHS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 15, 20, 24, 29, 30, 31, 32, 39, 40, 41, 59, 60, 61, 62, 63, 64, 65, 100, 120]

function randomStrings(count: number, seed: number): string[] {
  const random = makeRandom(seed)
  const strings: string[] = []
  for (let i = 0; i < count; i++) {
    const length = LENGTHS[random() % LENGTHS.length]
    let s = ''
    for (let j = 0; j < length; j++) s += pool[random() % pool.length]
    strings.push(s)
  }
  return strings
}

const strings = randomStrings(20000, 0x51ed5eed)
const alphabet = Array.from({ length: 0x7f - 0x20 }, (_, i) => String.fromCharCode(0x20 + i))

// ── 1. fitting and wrapping ─────────────────────────────────────────────────
// The original `wrap` never terminates when a word wider than `max` contains a character wider
// than `max`: no character fits `head`, so the word is pushed unchanged forever. Rust takes such
// a character anyway. Those inputs have no oracle result, so they are excluded here and noted
// in engine-semantics.md.
function wrapTerminates(s: string, max: number): boolean {
  if (max <= 0) return true
  for (const word of s.split(/\s+/).filter(Boolean)) {
    if (textWidth(word) > max && [...word].some((ch) => cellWidth(ch.codePointAt(0)!) > max)) return false
  }
  return true
}
for (let max = 0; max <= 60; max++) {
  compare('fit', strings.map((s) => [s, max]), ([s, m]) => fit(s, m), ([s, m]) => `${JSON.stringify(s)} @ ${m}`)
  const wrapping = strings.filter((s) => wrapTerminates(s, max))
  if (recording) recordKept(`wrap max=${max}`, strings.map((s) => wrapTerminates(s, max)))
  compare('wrap', wrapping.map((s) => [s, max]), ([s, m]) => wrap(s, m), ([s, m]) => `${JSON.stringify(s)} @ ${m}`)
}
// Widths around zero-width and wide characters.
const widthVectors = ['中', 'a中b', '\u0301a', '😀😀', '🇺🇸x', 'a\tb', 'a\u0085b', 'a\ufeffb', '\u200b']
if (recording) recordKept('wrap vectors', widthVectors.flatMap((s) => [0, 1, 2, 3, 4].map((max) => wrapTerminates(s, max))))
for (const s of widthVectors) {
  for (let max = 0; max <= 4; max++) {
    compare('fit', [[s, max]], ([s, m]) => fit(s, m))
    if (wrapTerminates(s, max)) compare('wrap', [[s, max]], ([s, m]) => wrap(s, m))
  }
}

// ── 2. compact numbers and durations ────────────────────────────────────────
const numberBoundaries = [
  0, -0, 0.4, 0.5, 1, 1.5, -0.5, -1.5, 2.5, 9.5, 59, 59.9, 60, 61, 599, 3600, 3661, 86399, 86400,
  86399.9, 172800, 1000, 1249, 1250, 9999, 99950, 99999, 100000, 999999, 1e6, 1.25e6, 9.95e6, 1e8,
  1.005e8, 1e9, 1.5e9, 9.99e9, 1e12, 1e15, 1e21, 1e300, -1, -1e9, -1e300, NaN, Infinity, -Infinity,
]
function numberSweep(count: number, seed: number): number[] {
  const random = makeRandom(seed)
  const values = [...numberBoundaries]
  while (values.length < count) {
    const kind = random() % 7
    const unit = random() / 4294967296
    if (kind === 0) values.push((random() % 2000001) - 1000000)
    else if (kind === 1) values.push(((random() % 2000000) - 1000000) / 1000)
    else if (kind === 2) values.push(unit * 10 ** ((random() % 41) - 10))
    else if (kind === 3) values.push(-unit * 10 ** ((random() % 41) - 10))
    else if (kind === 4) values.push(Math.floor(unit * 10 ** (random() % 25)))
    else if (kind === 5) values.push(unit * 1e6 - 5e5)
    else values.push(unit * 86400 * 2)
  }
  return values
}
const numbers = numberSweep(20000, 0x0ddba11)
compare('number_string', numbers.map((n) => [n]), ([n]) => String(n))
compare('tokens', numbers.map((n) => [n]), ([n]) => tokens(n))
compare('duration', numbers.map((n) => [n]), ([n]) => duration(n))
compare('span', numbers.map((n) => [n]), ([n]) => span(n))
compare('tokens', [[null]], ([n]) => tokens(n))

// Local time: probes run the whole gate under several TZ values. Keep timestamps inside the
// range Date and libc localtime both define (here 1969..2027).
const HOUR = 3_600_000
const clockTimes: number[] = [
  0, -1, 1, -60000, -60000.5, -59999.5, -1000, 999, -86400000, 86399999,
  Date.UTC(2024, 1, 29, 12), Date.UTC(2025, 11, 31, 23, 59, 59, 999), Date.UTC(2026, 0, 1, 0, 0, 0, 1),
]
// US spring forward 2026-03-08, US fall back 2026-11-01, EU spring 2026-03-29, EU fall 2026-10-25.
for (const anchor of [
  Date.UTC(2026, 2, 7, 0), Date.UTC(2026, 2, 8, 6), Date.UTC(2026, 2, 9, 12),
  Date.UTC(2026, 9, 31, 0), Date.UTC(2026, 10, 1, 6), Date.UTC(2026, 10, 2, 12),
  Date.UTC(2026, 2, 28, 22), Date.UTC(2026, 2, 29, 4), Date.UTC(2026, 2, 30, 6),
  Date.UTC(2026, 9, 24, 20), Date.UTC(2026, 9, 25, 4), Date.UTC(2026, 9, 26, 6),
  Date.UTC(2025, 11, 31, 18), Date.UTC(2026, 0, 1, 6),
]) {
  for (let k = -8; k <= 8; k++) clockTimes.push(anchor + k * HOUR / 2)
}
const clockInputs: any[] = []
for (const t of clockTimes) for (const day of [t, t + HOUR, t - 86400000, t + 86400000, 0]) clockInputs.push([t, day])
compare('clock', clockInputs, ([t, day]) => clock(t, day))
const whenInputs: any[] = []
clockTimes.forEach((t, i) => {
  if (i % 2) return
  for (const end of [null, t + 5 * 60000, t + 2 * HOUR]) whenInputs.push([t, end, t + 10 * 60000])
  whenInputs.push([t, t + 60000, t + 86400000 * 2])
})
compare('when', whenInputs, ([start, end, now]) => when(start, end, now))

// ── 3. model names and families ─────────────────────────────────────────────
const modelVectors = [
  '', 'gpt-5', 'claude-opus-4-5', 'claude-3-5-sonnet', 'CLAUDE-OPUS-4-5', 'o3', 'o3-mini',
  'gpt-4o-1-2', 'a-1-2', 'x--1-2', 'claude-', 'gpt-', '-1-2', '1-2', 'a-1-2-3', 'a-1-2x',
  'claude-gpt-x', 'claude-İ-4-5', 'ｃｌａｕｄｅ-4-5', 'gpt-４-５', 'İ-1-2', 'a-١٢-٣٤',
  'a-1-2\n', ' a-1-2', '-1-2-', 'a-１２-３４', 'İ', 'ß', 'ẞ', 'K', 'Å', 'Σ', 'ΑΣ',
]
const shortModelInputs = [...modelVectors, ...strings.slice(0, 20000)].map((s) => [s])
compare('short_model', shortModelInputs, ([s]) => shortModel(s))

const familyVectors: [string | null, string | null][] = [
  ['claude-opus-4-5', null], ['Claude', null], ['CLAUDE', null], ['opus 4', null], ['SONNET', null],
  ['haiku', null], ['FABLE', null], ['anthropic', null], ['Anthropic Claude', null],
  ['gpt-5', null], ['GPT-5', null], ['gpt-4o', null], ['codex', null], ['CODEX', null], ['openai', null],
  ['o1', null], ['o9', null], ['O3', null], ['o10', null], ['ao1', null], ['o1o', null], ['o0', null],
  ['o1-', null], ['x-o9', null], ['o1_x', null], ['o2.o3', null],
  ['grok', null], ['GROK-4', null], ['xai', null], ['XAI', null], ['xAI', null], ['grok-xai', null],
  [null, 'claude'], [null, 'gpt'], [null, 'nope'], ['', 'claude'], ['', ''], ['nope', 'grok'],
  ['İ', null], ['ẞ', null], ['Σ', null], ['K', null], ['Å', null], ['ﬀ', null], ['İclaude', null],
  ['claudeİ', null], ['ＧＰＴ', null], ['ＣＬＡＵＤＥ', null], ['claude\u0000', null], ['claude\n', null],
]
{
  const random = makeRandom(0xfa111)
  for (let i = 0; i < 20000; i++) {
    const model = strings[(i * 7 + random()) % strings.length]
    const hint = random() % 3 === 0 ? pool[random() % pool.length] + (random() % 2 ? 'claude' : 'gpt') : null
    familyVectors.push([model, hint])
  }
}
compare('family', familyVectors, ([model, hint]) => family(model ?? undefined, hint ?? undefined))

const categoryVectors: [string, string | null][] = []
for (const kind of [
  'command_execution', 'file_change', 'web_search', 'subagent', 'handoff', 'user_input_request',
  'secret_request', 'link_request', 'reasoning', 'assistant_message', 'user_message', 'notification',
  'dynamic_tool', 'other', 'COMMAND_EXECUTION', '', 'dynamic_tool ', 'İ', 'unknown',
]) {
  categoryVectors.push([kind, null])
}
for (const tool of [
  null, '', 'read', 'Read', 'READ', 'glob', 'grep', 'ls', 'view', 'view_image', 'notebookread',
  'edit', 'write', 'patch', 'Edit', 'rewrite', 'patcher', 'web', 'fetch', 'search', 'browse', 'fetcher',
  'agent', 'task', 'delegate', 'spawn', 'agents', 'tasks', 'subagent', 'Agent', 'mcp__x__read',
  'tool', 'İedit', 'editİ', 'REWRITE', 'ＷＥＢ', 'web\u0000', ' web', 'read ', 'read\n', 'READ_IMAGE',
  'multi_edit', 'WriteFile', 'search_web', 'spawn_agent', 'delegation',
]) {
  categoryVectors.push(['dynamic_tool', tool])
}
{
  const random = makeRandom(0xca7e90)
  for (let i = 0; i < 20000; i++) {
    const kind = random() % 5 === 0 ? strings[(i * 3) % strings.length] : 'dynamic_tool'
    const tool = strings[(i * 11 + random()) % strings.length]
    categoryVectors.push([kind, random() % 6 === 0 ? null : tool])
  }
}
compare('category', categoryVectors, ([kind, tool]) => category(kind, tool ?? undefined))

// ── 4. identity and links ───────────────────────────────────────────────────
// encodeURIComponent over every code point (lone surrogates cannot reach a Rust string).
for (let start = 0; start < 0x110000; start += 200000) {
  const inputs: string[] = []
  for (let cp = start; cp < Math.min(start + 200000, 0x110000); cp++) {
    if (cp >= 0xd800 && cp <= 0xdfff) continue
    inputs.push(String.fromCodePoint(cp))
  }
  compare('encode_uri_component', inputs.map((s) => [s]), ([s]) => encodeURIComponent(s), ([s]) => JSON.stringify(s))
}
compare('encode_uri_component', strings.map((s) => [s]), ([s]) => encodeURIComponent(s))
for (const s of ['', ' ', '!', '*', "'", '(', ')', '~', '-', '_', '.', '%', '/', '🍞', 'a b']) {
  compare('encode_uri_component', [[s]], ([s]) => encodeURIComponent(s))
}

const envPool = [...alphabet, '\u00a0', '\ufeff', '\u0085', '中', '🍞', '\u0000', '\u007f', '']
function pickString(random: () => number, poolIn: string[], maxLength: number): string {
  const length = random() % (maxLength + 1)
  let s = ''
  for (let i = 0; i < length; i++) s += poolIn[random() % poolIn.length]
  return s
}
const linkInputs: any[] = [
  ['', ''], ['e-1', 'x'], ['e-1', 'thread:task:a%3Ab/c'], ['日本', '🍞/!'], ['e', '🍞'.repeat(512)],
  ['e', '🍞'.repeat(513)], ['e', '🍞'.repeat(512) + 'a'], ['e', 'a'.repeat(1024)], ['e', 'a'.repeat(1025)],
  ['e'.repeat(128), 'x'], ['e'.repeat(129), 'x'], ['a b', 'c d'], ['\u0000', '\u0001'],
]
{
  const random = makeRandom(0x11a5c)
  for (let i = 0; i < 20000; i++) {
    linkInputs.push([pickString(random, envPool, 20), pickString(random, envPool, 30)])
  }
  for (let i = 0; i < 200; i++) {
    linkInputs.push([pickString(random, envPool, 5), pickString(random, ['a', '🍞', '中'], 600)])
  }
}
compare('thread_link', linkInputs, ([env, id]) => threadLink(env, id))
compare('thread_key', linkInputs, ([env, id]) => threadKey({ environmentId: env, threadId: id }))
compare('environment_id_valid', joinPool(20000, 0x1d5), ([s]) => /^[\w-]{1,128}$/.test(s))
function joinPool(count: number, seed: number): any[] {
  const random = makeRandom(seed)
  const inputs: any[] = []
  for (const s of ['', 'e', 'e-1', 'e_1', '.', '/', 'a b', 'é', '中', 'a'.repeat(128), 'a'.repeat(129), '\ufeffe', 'e\u0085']) inputs.push([s])
  while (inputs.length < count) {
    const alphabetIn = random() % 2 ? [...alphabet, 'é', '中', '\ufeff', '\u0085'] : alphabet
    inputs.push([pickString(random, alphabetIn, 140)])
  }
  return inputs
}

const refInputs: any[] = [
  null, 0, 1, true, false, 'x', ' {} ', [], [1], {},
  { environmentId: 'e', threadId: 't' }, { environmentId: '', threadId: 't' },
  { environmentId: 'e', threadId: '' }, { environmentId: 'e', threadId: '   ' },
  { environmentId: 'e', threadId: '\ufeff' }, { environmentId: 'e', threadId: '\u0085' },
  { environmentId: 'e', threadId: ' \u0085 ' }, { environmentId: 'e', threadId: 'a\u0000b' },
  { environmentId: 'e', threadId: 'a\nb' }, { environmentId: 'e', threadId: 'a\u007fb' },
  { environmentId: 'e', threadId: 'a'.repeat(1024) }, { environmentId: 'e', threadId: 'a'.repeat(1025) },
  { environmentId: 'e', threadId: '🍞'.repeat(512) }, { environmentId: 'e', threadId: '🍞'.repeat(512) + 'a' },
  { environmentId: 'a'.repeat(128), threadId: 't' }, { environmentId: 'a'.repeat(129), threadId: 't' },
  { environmentId: 'a b', threadId: 't' }, { environmentId: 'é', threadId: 't' },
  { environmentId: 1, threadId: 't' }, { environmentId: 'e', threadId: 1 },
  { environmentId: null, threadId: 't' }, { environmentId: 'e', threadId: null },
  { environmentId: 'e', threadId: 'x', extra: 1 },
]
{
  const random = makeRandom(0x7ef5)
  const envs = ['', 'e', 'e-1', 'a'.repeat(128), 'a'.repeat(129), 'a b', 'é', '中', '\ufeff', 'e_1-2']
  for (let i = 0; i < 20000; i++) {
    const value: any = random() % 8 === 0 ? { environmentId: envs[random() % envs.length] }
      : { environmentId: envs[random() % envs.length], threadId: pickString(random, envPool, 40) }
    if (random() % 11 === 0) value.threadId = pickString(random, ['a', '🍞', '中', ' ', '\u0085', '\ufeff'], random() % 60)
    refInputs.push(value)
  }
}
compare('thread_ref', refInputs.map((v) => [v]), ([value]) => {
  const ref = threadRef(value)
  return ref ? [ref.environmentId, ref.threadId] : null
})

function makeAgent(random: () => number, index: number): any {
  const titles = ['Fix the build', 'Notes / ideas', 'Remote', 'Explore', 'Ünïcode 🍞', 'a  b', '', ' x ']
  const projects = ['dotfiles', 'other', 'loupe', 'proj 🍞', '', ' x ', 'a / b']
  const updated = ['2026-11-12', '2026-11-13', '', 'z', '\uffff', '\ue000', '𐀀', '\u2028', '2026-11-12T11:00:00Z']
  const threadId = random() % 3 === 0 ? `sub-${index}` : `id-${index}`
  const id = `id-${index}`
  const remote = random() % 4 === 0
    ? { environmentId: random() % 5 ? 'machine-b' : '../bad', threadId: random() % 5 ? 'parent' : ' ' }
    : null
  return {
    id,
    thread_id: random() % 2 ? id : threadId,
    title: titles[random() % titles.length],
    project: projects[random() % projects.length],
    updated: random() % 4 === 0 ? updated[random() % updated.length] : `2026-11-${10 + (random() % 9)}`,
    remote,
  }
}
function dirty(random: () => number, s: string): string {
  const wrapIn = ['\u0000', '\u00a0', '\ufeff', ' ', '\u0085', '\t', '\u200b', '\u2028'][random() % 8]
  if (random() % 3 === 0) return s
  if (random() % 2) return [...s].map((c) => (random() % 4 === 0 ? wrapIn + c : c)).join('')
  return wrapIn + s + wrapIn
}
const titleInputs: any[] = [['', []], ['Fix the build', []]]
{
  const random = makeRandom(0x7171e)
  for (let i = 0; i < 8000; i++) {
    const agents = Array.from({ length: 1 + (random() % 4) }, (_, k) => makeAgent(random, k))
    const chosen = agents[random() % agents.length]
    const kind = random() % 3
    const title = kind === 0 ? chosen.title
      : kind === 1 ? `${chosen.project} / ${chosen.title}`
        : pool[random() % pool.length] + chosen.title
    titleInputs.push([dirty(random, title), agents])
  }
}
compare('thread_for_title', titleInputs, ([title, agents]) => t3Thread(title, agents))

const agentLinkInputs: any[] = []
{
  const random = makeRandom(0xa9e17)
  for (let i = 0; i < 4000; i++) {
    const agent = makeAgent(random, i)
    const local = random() % 4 === 0 ? null : pickString(random, envPool, 10)
    agentLinkInputs.push([agent, local])
  }
}
compare('agent_thread_link', agentLinkInputs, ([agent, local]) => agentThreadLink(agent, local))

// ── 5. delegation ordering (localeCompare over timestamp/id characters) ─────
{
  const random = makeRandom(0xc011a7e)
  const pairs: any[] = []
  const timestamps = []
  for (let i = 0; i < 4000; i++) {
    const y = 2024 + (random() % 4)
    const mo = String(1 + (random() % 12)).padStart(2, '0')
    const d = String(1 + (random() % 28)).padStart(2, '0')
    const h = String(random() % 24).padStart(2, '0')
    const mi = String(random() % 60).padStart(2, '0')
    const se = String(random() % 60).padStart(2, '0')
    timestamps.push(`${y}-${mo}-${d}T${h}:${mi}:${se}.${String(random() % 1000).padStart(3, '0')}Z`)
  }
  for (let i = 0; i < 20000; i++) {
    const kind = random() % 4
    let a: string
    let b: string
    if (kind === 0) {
      a = timestamps[random() % timestamps.length]
      b = timestamps[random() % timestamps.length]
    } else if (kind === 1) {
      const base = timestamps[random() % timestamps.length]
      b = base
      a = base.slice(0, random() % base.length)
    } else {
      const length = () => random() % 12
      const build = () => {
        let s = ''
        for (let j = 0; j < length(); j++) s += alphabet[random() % alphabet.length]
        return s
      }
      a = build()
      b = build()
    }
    pairs.push([a, b])
  }
  // The three known disagreements, plus case-only pairs. Non-ASCII (`é`) is out of the modelled
  // alphabet and is not claimed; see engine-semantics.md.
  for (const [a, b] of [['a', 'B'], ['a-b', 'a_b'], ['a', 'A'], ['A', 'a'], ['aA', 'Aa'], ['a', 'ab'], ['', 'a'], ['a', '']]) {
    pairs.push([a, b])
  }
  compare('locale_compare', pairs, ([a, b]) => Math.sign(a.localeCompare(b)), ([a, b]) => `${JSON.stringify(a)} vs ${JSON.stringify(b)}`)
}

// ── 6. remaining string and number helpers ─────────────────────────────────
const windowVectors: string[] = [
  '', 'hour', 'Hour', 'HOUR', '5-hour', '5 hour', '5hours', '5  hours', '5 - hour', '5- hour',
  '5 -hour', '12-hourly', '1 day', '1day', '2 days', '3 week', '2 weeks', '1 month', '3 months',
  'weekly', 'Weekly', 'biweekly', 'WEEKLY', 'monthly', 'Monthly', 'week', 'day', 'hour',
  'x1y2hour', '12x3hour', '1 hour 2 day', '0 hour', '999999999999999999999999 hour',
  '1\u00a0hour', '1\u202fhour', '1\ufeffhour', '1\u0085hour', '1\thour', '1\nhour', '\u00a01 hour',
  '1\u200bhour', '١ hour', '１２ hour', '1 ⁻ hour', '5-hour limit', 'limit: 5-hour session',
  '1 hour\u0000', '1-hour\n', '7 days', '30 days', '24 hours',
]
{
  const random = makeRandom(0x1d0b)
  for (let i = 0; i < 20000; i++) {
    if (i % 2) { windowVectors.push(pool.length ? strings[i % strings.length] : '') }
    else {
      const n = ['0', '1', '5', '12', '999', '123456789', '0'.repeat(1 + (random() % 40))][random() % 7]
      const gap = ['', ' ', '\t', '\u00a0', '\ufeff', '\u0085', '-', ' - ', ' -', '-  '][random() % 9]
      const unit = ['hour', 'Hour', 'HOUR', 'hours', 'day', 'DAYS', 'week', 'WEEKLY', 'month', 'Monthly', 'x'][random() % 11]
      windowVectors.push(`${pool[random() % pool.length]}${n}${gap}${unit}${pool[random() % pool.length]}`)
    }
  }
}
compare('window_length', windowVectors.map((s) => [s]), ([s]) => windowLength(s))

const hexVectors: string[] = ['', '#', '#000000', '#ffffff', '#FFFFFF', '000000', '123456', '  #123456  ', '#12345', '#1234567', '#gggggg', ' #AbCdEf ', '\ufeff#123456', '#123456\n', '#123456é', '0x123456', '＃123456']
{
  const random = makeRandom(0x4e6)
  for (let i = 0; i < 20000; i++) {
    const length = random() % 12
    let s = random() % 2 ? '#' : ''
    const chars = '0123456789abcdefABCDEFgG #\t\ufeffé中'
    for (let j = 0; j < length; j++) s += chars[random() % chars.length]
    hexVectors.push(s)
  }
}
compare('hex', hexVectors.map((s) => [s]), ([s]) => hex(s))

const colors: number[] = [0, 1, 0x010203, 0xffffff, 0x7aa2f7, 0xde6145, 0x101315, 0x808080]
const random = makeRandom(0xc0105)
while (colors.length < 12000) colors.push(random() & 0xffffff)
const colorPairs: any[] = colors.map((c) => [c, colors[(c * 7919) % colors.length], (c % 1001) / 1000])
compare('mix', colorPairs, ([a, b, t]) => mix(a, b, t))
compare('luminance', colors.map((c) => [c]), ([c]) => luminance(c))
compare('contrast', colorPairs.map(([a, b]) => [a, b]), ([a, b]) => contrast(a, b))
compare('chroma', colors.map((c) => [c]), ([c]) => chroma(c))
const palette = { accent: 0x7aa2f7, bright: 0xe0e5fa, bg: 0x1a1b26 } as any
const rampInputs: any[] = []
for (const t of [-1, -0.5, -0, 0, 0.01, 0.54, 0.55, 0.56, 0.99, 1, 1.5, 2, NaN, Infinity, -Infinity]) rampInputs.push([t])
for (let i = 0; i < 20000; i++) rampInputs.push([(random() / 4294967296) * 3 - 1])
compare('ramp', rampInputs, ([t]) => ramp(palette, t))

const reachInputs: any[] = []
{
  const randomReach = makeRandom(0x4eac4)
  const surfacesPool = [[0x101315, 0x171a1c], [0x1a1b26], [0x101315, 0x171a1c, 0x33467c], [0xffffff], []]
  for (const target of [0, 1, 2.4, 3, 4.5, 7, 21]) {
    for (const [from, to] of [[0x565d60, 0xffffff], [0xcacccc, 0xffffff], [0xde6145, 0x000000], [0x8b5896, 0xffffff]]) {
      reachInputs.push([from, to, target, surfacesPool[0]])
    }
  }
  while (reachInputs.length < 12000) {
    const from = randomReach() & 0xffffff
    const to = randomReach() & 0xffffff
    const target = (randomReach() % 2100) / 100
    reachInputs.push([from, to, target, surfacesPool[randomReach() % surfacesPool.length]])
  }
}
compare('reach', reachInputs, ([from, to, target, surfaces]) => reach(from, to, target, surfaces))

const levelInputs: any[] = []
for (const v of [0, -0, 0.001, 0.1, 0.5, 1, 2, 7, 8, 1e9, -1e9, Infinity, -Infinity]) {
  for (const max of [0, 1, 2, 8, 100, 1e9, -1, -100]) levelInputs.push([v, max])
}
for (let i = 0; i < 20000; i++) levelInputs.push([(random() / 4294967296) * 100 - 50, (random() / 4294967296) * 1000])
compare('level', levelInputs, ([v, max]) => level(v, max))

const sparkInputs: any[] = [[[], null]]
for (let i = 0; i < 5000; i++) {
  const values = Array.from({ length: random() % 9 }, () => (random() / 4294967296) * 200 - 20)
  const max = random() % 3 === 0 ? null : (random() / 4294967296) * 300 - 10
  sparkInputs.push([values, max])
}
compare('spark', sparkInputs, ([values, max]) => spark(values, max === null ? undefined : max).join(''))

const bigWidthInputs: any[] = []
for (const s of ['', '0', '1.2K', '99%', '--', 'abc', '中', '🍞', '.', '-', '1 2']) bigWidthInputs.push([s, false], [s, true])
for (const s of strings.slice(0, 20000)) bigWidthInputs.push([s, false], [s, true])
compare('big_width', bigWidthInputs, ([s, large]) => bigWidth(s, large ? 'shadow' : 'plain'))

// ── 7. formatting helpers the details pane used to keep privately ───────────
// These stay out of the recorded fixture: their numeric vectors are generated here rather than
// by a Rust port of the generator, and the live gate is what checks them. Everything above is
// recorded as before, so text.json.gz does not change.
if (!recording) {
  // where `{:.0}` and `toFixed` part company: a half rounds up in JavaScript, to even in Rust.
  const ties: number[] = [
    0, -0, 0.5, 1.5, 2.5, 3.5, -0.5, -1.5, -2.5, 0.49999999999999994, 2 ** 52 + 0.5,
    112500, 112.5, 1250, 12500, 999950, 999999.5, 1e6 - 0.5, 1e9 - 0.5, 1e21, 1e21 - 1,
    1e300, -1e300, 5e-324, NaN, Infinity, -Infinity,
  ]
  {
    const tieRandom = makeRandom(0x71e5)
    for (let i = 0; i < 20000; i++) ties.push((tieRandom() % 200001) / 2 - 50000)
    for (let i = 0; i < 5000; i++) ties.push((tieRandom() / 4294967296) * 2e7 - 1e7)
  }
  compare('js_round', ties.map((x) => [x]), ([x]) => Math.round(x))
  compare('to_fixed', ties.flatMap((x) => [[x, 0], [x, 1], [x, 2]]), ([x, d]) => x.toFixed(d))
  compare('fixed_number', ties.flatMap((x) => [[x, 0], [x, 1]]), ([x, d]) => String(+x.toFixed(d)))
  compare('short_duration', ties.map((x) => [x]), ([x]) => duration(x).replace(/ \d+s$/, ''))
  compare('coarse', ties.map((x) => [x]), ([x]) => duration(x).split(' ')[0])
  // `now` is a wall clock: non-negative and finite. Outside that, JavaScript's index is undefined.
  compare('spinner', ties.filter((x) => x >= 0 && Number.isFinite(x)).map((x) => [x]), ([x]) => spinner(x))
  const categoryNames = ['shell', 'read', 'edit', 'web', 'agent', 'ask', 'think', 'reply', 'you', 'note', 'tool', 'other', '', 'SHELL', 'web_search', 'unknown category']
  compare('glyph', categoryNames.map((c) => [c]), ([c]) => GLYPH[c] ?? '·')
  /** `view.ts`'s private `ratio(a)` (line 97), over a context instead of an agent. */
  const ratioOf = (used: any, max: any): number | null =>
    typeof used === 'number' && typeof max === 'number' && max > 0 ? Math.max(0, Math.min(1, used / max)) : null
  /** `view.ts`'s private `whole(v)` (line 653). */
  const wholeOf = (v: string): string => {
    const m = /^(\d+\.\d+)(\D*)$/.exec(v)
    return m && Number(m[1]) >= 1 ? String(Math.round(Number(m[1]))) + m[2] : v
  }
  compare('ratio', [
    [null, null], [null, 1], [1, null], [1, 0], [1, -1], [0, 0], [0, 1], [-1, 1], [2, 1],
    [1, 1], [0.125, 1], [112500, 1000000], [NaN, 1], [1, NaN], [Infinity, 1], [1, Infinity],
  ], ([u, m]) => ratioOf(u, m))
  compare('whole', [
    '', '0', '0.5', '1.0', '1.4', '1.5', '1.49', '1.', '1.x', '12.5%', '-1.5', '1.5x2', ' 1.5',
    '1.5 ', '10.25 %', '1.5\u200b', '999999999999999999999.5', 'a1.5', '1.5a', '1.5.5',
  ].map((v) => [v]), ([v]) => wholeOf(v))
  compare('limit_label', [
    '', 'Pro (5h)', 'a (b) c', '(x)', 'a(b)', 'a (b) (c)', 'a  (b)', 'a (b) c (d)', 'no parens',
    'a (b', 'a ) b (c)', '\u00a0(x)', 'a\t(b)', 'a (b) )', '(a) (b)',
  ].map((v) => [v]), ([v]) => v.replace(/\s*\(.*\)/, ''))
  const gateHome = process.env.HOME ?? ''
  const labelTexts = [
    '', ' ', '\t ', '\nsecond', '  \nls', 'a', ' a ', 'a\nb', 'web_search', 'a\u200bb', '\u200b',
    '\ufeffx', '\u0085x', `${gateHome}`, `${gateHome}/x`, `${gateHome}/a\nb`, `  ${gateHome}/x`,
    `a ${gateHome}/b`, `${gateHome}/a ${gateHome}/b`, `${gateHome}name/x`, `x ${gateHome}`,
    `mid ${gateHome} and ${gateHome} again`, '/home/user.name[x]/x', '/home/userXname[x]/y',
  ]
  const labelTypes = ['command_execution', 'file_change', 'reasoning', 'assistant_message',
    'web_search', 'subagent', 'handoff', 'user_input_request', 'dynamic_tool', 'browser_session', '', 'UPPER_CASE']
  const labelItems = labelTypes.flatMap((type) => labelTexts.map((s) => [{
    thread_id: 'synthetic', run_id: null, node_id: null, provider_thread_id: null, type, status: 'completed',
    updated_at: '2026-11-16T00:00:00.000Z', started_at: null, label: s, command: s, file: s, tool: null,
  }, gateHome]))
  compare('item_label', labelItems, ([item]) => itemLabel(item as any), ([item]) => JSON.stringify(item))
  // `view.ts`'s private `tidy(s, home)` (line 108), including both HOME-unset quirks.
  const tidyOf = (s: string, home: string): string =>
    clean(s.split('\n')[0]).replace(home ? new RegExp('^' + home.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')) : /^$/, '~').trim()
  const tidyInputs: any[] = []
  for (const home of [gateHome, '/home/user', '/home/user.name[x]', '']) {
    for (const s of labelTexts) tidyInputs.push([s, home])
  }
  compare('tidy', tidyInputs, ([s, home]) => tidyOf(s, home))
}
