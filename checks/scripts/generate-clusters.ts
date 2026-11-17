// Generates crates/core/src/cluster_tables.rs: the per-code-point Unicode properties OpenTUI 0.8.3
// uses to group a run of text into grapheme clusters and give each a width (see
// crates/core/src/cluster.rs).
//
// The input is the Unicode 17.0 data OpenTUI's native library is built from: the `uucode-opentui`
// copy inside `packages/native/src/vendor/zig-deps.tar.gz` of OpenTUI's v0.8.3 tag. To fetch it
// (into the ignored checks/generated/dl):
//
//   git clone --depth 1 --branch v0.8.3 https://github.com/anomalyco/opentui checks/generated/dl/opentui-0.8.3
//   mkdir -p checks/generated/dl/zig-deps
//   tar -xzf checks/generated/dl/opentui-0.8.3/packages/native/src/vendor/zig-deps.tar.gz -C checks/generated/dl/zig-deps uucode-opentui
//
//   node checks/scripts/generate-clusters.ts [ucd-dir]
//
// Each property is derived the way uucode derives it (src/components.zig in that copy):
// - the grapheme break class, from GraphemeBreakProperty.txt, emoji-data.txt and the InCB values in
//   DerivedCoreProperties.txt, in uucode's order of precedence: Emoji_Modifier, Emoji_Modifier_Base,
//   Extended_Pictographic, then InCB (Extend: ZWJ or extend; Linker; Consonant), then the original
//   class (an Extend with InCB None is ZWNJ);
// - East Asian Width, from DerivedEastAsianWidth.txt with its @missing Wide ranges;
// - the general category marks Mn, Mc, Me, from DerivedGeneralCategory.txt;
// - Default_Ignorable_Code_Point, from DerivedCoreProperties.txt;
// - wcwidth_zero_in_grapheme (flag 16), the property OpenTUI's Ghostty width method reads: uucode's
//   own width is 0 (a Cc, Cs, Zl or Zp, or a default-ignorable except the soft hyphen), or the code
//   point is an emoji modifier, Mn, Me, or a Hangul V or T or a Prepend.
import { readFileSync, writeFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
const UCD = resolve(process.argv[2] ?? join(REPO, "checks/generated/dl/zig-deps/uucode-opentui/ucd"))
const N = 0x110000

function lines(rel: string): string[] {
  let text: string
  try {
    text = readFileSync(join(UCD, rel), "utf8")
  } catch {
    throw new Error(`${join(UCD, rel)} is missing; see the header of this script for how to fetch OpenTUI's Unicode data`)
  }
  return text.split("\n")
}
function header(rel: string): string {
  return lines(rel)[0]!.replace(/^#\s*/, "").trim()
}
// `cp` or `a..b`, inclusive.
function range(s: string): [number, number] {
  const [a, b] = s.trim().split("..")
  return [parseInt(a!, 16), parseInt((b ?? a)!, 16)]
}
// Data lines as [range, fields...] with comments stripped.
function* data(rel: string): Generator<[[number, number], string[]]> {
  for (const line of lines(rel)) {
    const body = line.replace(/#.*/, "").trim()
    if (!body) continue
    const parts = body.split(";").map((p) => p.trim())
    yield [range(parts[0]!), parts.slice(1)]
  }
}

// Grapheme break, as uucode's GraphemeBreak enum.
const GB = ["other", "control", "prepend", "cr", "lf", "regional_indicator", "spacing_mark", "l", "v", "t", "lv", "lvt", "zwj", "zwnj", "extended_pictographic", "emoji_modifier_base", "emoji_modifier", "icb_extend", "icb_linker", "icb_consonant"] as const
const ORIGINAL: Record<string, string> = { Prepend: "prepend", CR: "cr", LF: "lf", Control: "control", Extend: "extend", Regional_Indicator: "regional_indicator", SpacingMark: "spacing_mark", L: "l", V: "v", T: "t", LV: "lv", LVT: "lvt", ZWJ: "zwj" }
const original = new Array<string>(N).fill("other")
for (const [[a, b], [prop]] of data("auxiliary/GraphemeBreakProperty.txt")) {
  const v = ORIGINAL[prop!]
  if (!v) throw new Error(`unknown Grapheme_Cluster_Break ${prop}`)
  for (let cp = a; cp <= b; cp++) original[cp] = v
}
const emoji = { Emoji_Modifier: new Uint8Array(N), Emoji_Modifier_Base: new Uint8Array(N), Extended_Pictographic: new Uint8Array(N) }
for (const [[a, b], [prop]] of data("emoji/emoji-data.txt")) {
  const set = emoji[prop as keyof typeof emoji]
  if (set) for (let cp = a; cp <= b; cp++) set[cp] = 1
}
const incb = new Array<string>(N).fill("None")
const ignorable = new Uint8Array(N)
for (const [[a, b], [prop, value]] of data("DerivedCoreProperties.txt")) {
  if (prop === "InCB") for (let cp = a; cp <= b; cp++) incb[cp] = value!
  if (prop === "Default_Ignorable_Code_Point") for (let cp = a; cp <= b; cp++) ignorable[cp] = 1
}
const gb = new Uint8Array(N)
for (let cp = 0; cp < N; cp++) {
  let v: string
  if (emoji.Emoji_Modifier[cp]) v = "emoji_modifier"
  else if (emoji.Emoji_Modifier_Base[cp]) v = "emoji_modifier_base"
  else if (emoji.Extended_Pictographic[cp]) v = "extended_pictographic"
  else if (incb[cp] === "Extend") v = cp === 0x200d ? "zwj" : "icb_extend"
  else if (incb[cp] === "Linker") v = "icb_linker"
  else if (incb[cp] === "Consonant") v = "icb_consonant"
  else if (original[cp] === "extend") {
    if (cp !== 0x200c) throw new Error(`Extend with InCB None that is not ZWNJ: ${cp.toString(16)}`)
    v = "zwnj"
  } else v = original[cp]!
  gb[cp] = GB.indexOf(v as (typeof GB)[number])
}

// East Asian Width: Neutral by default, the @missing Wide ranges, then the data lines in order.
const wide = new Uint8Array(N)
for (const line of lines("extracted/DerivedEastAsianWidth.txt")) {
  const missing = line.match(/^#\s*@missing:\s*([0-9A-F.]+)\s*;\s*(\w+)/)
  if (missing) {
    const [a, b] = range(missing[1]!)
    if (missing[2] === "Wide") for (let cp = a; cp <= b; cp++) wide[cp] = 1
    else if (missing[2] !== "Neutral") throw new Error(`unknown @missing width ${missing[2]}`)
    continue
  }
  const body = line.replace(/#.*/, "").trim()
  if (!body) continue
  const [r, w] = body.split(";").map((p) => p.trim())
  const [a, b] = range(r!)
  for (let cp = a; cp <= b; cp++) wide[cp] = w === "W" || w === "F" ? 1 : 0
}

const gc = new Array<string>(N).fill("Cn")
for (const [[a, b], [cat]] of data("extracted/DerivedGeneralCategory.txt")) for (let cp = a; cp <= b; cp++) gc[cp] = cat!

// Flags, as cluster.rs reads them.
const WIDE = 1, MN = 2, MARK = 4, IGNORABLE = 8, ZERO_IN_GRAPHEME = 16
const GB_ZERO = new Set([GB.indexOf("v"), GB.indexOf("t"), GB.indexOf("prepend"), GB.indexOf("emoji_modifier")])
const ranges: [number, number, number, number][] = []
for (let cp = 0; cp < N; cp++) {
  const zero = ["Cc", "Cs", "Zl", "Zp", "Mn", "Me"].includes(gc[cp]!) || (ignorable[cp] && cp !== 0xad) || GB_ZERO.has(gb[cp]!)
  const flags = (zero ? ZERO_IN_GRAPHEME : 0) | (wide[cp] ? WIDE : 0) | (gc[cp] === "Mn" ? MN : 0) | (gc[cp] === "Mn" || gc[cp] === "Mc" || gc[cp] === "Me" ? MARK : 0) | (ignorable[cp] ? IGNORABLE : 0)
  const last = ranges[ranges.length - 1]
  if (last && last[1] === cp - 1 && last[2] === gb[cp] && last[3] === flags) last[1] = cp
  else ranges.push([cp, cp, gb[cp]!, flags])
}

const sources = ["auxiliary/GraphemeBreakProperty.txt", "emoji/emoji-data.txt", "DerivedCoreProperties.txt", "extracted/DerivedEastAsianWidth.txt", "extracted/DerivedGeneralCategory.txt"].map(header)
const out = `// Generated by checks/scripts/generate-clusters.ts from the Unicode data OpenTUI 0.8.3 is built
// with (its vendored uucode copy). Do not edit by hand.
//
${sources.map((s) => `// ${s}`).join("\n")}
//
// One row per run of code points with the same properties: (first, last, grapheme break class,
// flags). The class is an index into [\`Gb\`](super::Gb)'s order; the flags are
// 1 East Asian Width W or F, 2 general category Mn, 4 Mn, Mc or Me, 8 Default_Ignorable_Code_Point, 16 wcwidth_zero_in_grapheme.
pub(super) static PROPS: &[(u32, u32, u8, u8)] = &[
${ranges.map(([a, b, g, f]) => `    (0x${a.toString(16)}, 0x${b.toString(16)}, ${g}, ${f}),`).join("\n")}
];
`
const path = join(REPO, "crates/core/src/cluster_tables.rs")
writeFileSync(path, out)
console.log(`${path}: ${ranges.length} ranges from ${UCD}`)
