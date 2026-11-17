import { readFileSync, writeFileSync } from 'node:fs'
if (process.versions.unicode !== '17.0') throw new Error(`Expected Unicode 17.0, got ${process.versions.unicode}`)
const ranges: [number, number][] = []
let start = -1
for (let cp = 0; cp <= 0x110000; cp++) {
  const hidden = cp < 0x110000 && /\p{C}/u.test(String.fromCodePoint(cp))
  if (hidden && start < 0) start = cp
  if (!hidden && start >= 0) { ranges.push([start, cp - 1]); start = -1 }
}
const path = 'crates/core/src/terminal.rs'
const source = readFileSync(path, 'utf8')
const table = `// BEGIN GENERATED HIDDEN RANGES (Unicode 17.0)\npub(crate) static HIDDEN_RANGES: &[(u32, u32)] = &[\n${ranges.map(([a,b]) => `    (0x${a.toString(16)}, 0x${b.toString(16)}),`).join('\n')}\n];\n// END GENERATED HIDDEN RANGES`
if (!source.includes('// BEGIN GENERATED HIDDEN RANGES') || !source.includes('// END GENERATED HIDDEN RANGES')) throw new Error('Missing table markers')
writeFileSync(path, source.replace(/\/\/ BEGIN GENERATED HIDDEN RANGES[\s\S]*?\/\/ END GENERATED HIDDEN RANGES/, table))
