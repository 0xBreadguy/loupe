// A synthetic T3 database whose thread titles carry text that needs grapheme clustering.
// It is the built-in demo with its threads retitled. window-parity.sh draws it in real terminal
// windows.
//
//   node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/emoji-db.ts OUT_DIR [SET]
//
// SET is `emoji` (the default): titles a person might type, with joined families, flags, skin
// tones, keycaps, variation selectors, combining marks and wide text (the demo has nine threads,
// so the first nine of them). Or `clusters-1`,
// `clusters-2`, `clusters-3`: every kind of grapheme cluster the layout handles, from
// cluster-titles.ts, a page at a time because the demo has fewer threads than there are titles.
//
// Set T3CODE_HOME to an empty folder first; the demo must never look at a real T3 home.
import { copyFileSync } from "node:fs"
import { join } from "node:path"
import { DatabaseSync } from "node:sqlite"
import { Demo } from "../../oracle/src/demo.ts"
import { CLUSTERS } from "./cluster-titles.ts"

export const NOW = 1_794_484_800_000
const EMOJI = [
  "family \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466} build",
  "flag \u{1f3f3}\u{fe0f}\u{200d}\u{1f308} and \u{1f1ef}\u{1f1f5} \u{1f1e7}\u{1f1f7} check",
  "thumbs \u{1f44d}\u{1f3fd} \u{1f44b}\u{1f3ff} review",
  "keycaps 1\u{fe0f}\u{20e3} 2\u{fe0f}\u{20e3} 3\u{fe0f}\u{20e3} steps",
  "heart \u{2764}\u{fe0f} vs \u{2764} text",
  "technologist \u{1f9d1}\u{200d}\u{1f4bb} pairing",
  "e\u{301} a\u{308} o\u{302} combining",
  "wide \u{65e5}\u{672c}\u{8a9e} and \u{d55c}\u{ad6d}\u{c5b4} mix",
  "bread \u{1f35e}\u{1f35e}\u{1f35e} run",
  "astronaut \u{1f469}\u{1f3fb}\u{200d}\u{1f680} launch",
  "pirate \u{1f3f4}\u{200d}\u{2620}\u{fe0f} branch",
  "zero width a\u{200b}b\u{200d}c joiner",
]
const out = process.argv[2]
const set = process.argv[3] ?? "emoji"
const page = /^clusters-([1-9])$/.exec(set)
if (!out || (set !== "emoji" && !page)) throw new Error("usage: emoji-db.ts OUT_DIR [emoji|clusters-N]")

const demo = new Demo({ now: NOW, refreshMs: 0 })
copyFileSync(demo.path, join(out, "statev2.sqlite"))
copyFileSync(join(demo.dir, "environment-id"), join(out, "environment-id"))
demo.stop()
const db = new DatabaseSync(join(out, "statev2.sqlite"))
const rows = db.prepare("SELECT thread_id FROM orchestration_v2_projection_threads ORDER BY thread_id").all() as { thread_id: string }[]
const titles = page ? CLUSTERS.slice((Number(page[1]) - 1) * rows.length, Number(page[1]) * rows.length) : EMOJI
if (!titles.length) throw new Error(`${set}: there are only ${Math.ceil(CLUSTERS.length / rows.length)} pages of cluster titles`)
const retitle = db.prepare("UPDATE orchestration_v2_projection_threads SET payload_json = json_set(payload_json, '$.title', ?) WHERE thread_id = ?")
rows.forEach((row, i) => { if (i < titles.length) retitle.run(titles[i], row.thread_id) })
db.close()
console.log(JSON.stringify({ set, threads: rows.length, retitled: Math.min(rows.length, titles.length), now: NOW }))
