// Records the 39 cases of `full-read-parity.mjs`: the full reader on five adversarial databases
// (T3's schema, table scans, covering indexes, nodes without a primary key, a malformed payload),
// each at three clocks after each of three rounds of ties, deletes and retimes. The database at
// every state travels as SQL text; the expected snapshot travels whole, and where the original
// fails the fixture records that it fails.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/full-read-parity.mjs"), [], "full-read.json.gz",
  "39 cases of the full reader: five schema and plan variants across three rounds and three clocks, with hundreds of rows tied exactly at the 3,000-item and 8,000-event cuts and on the three-hour window edge. Each case carries its database as SQL text and the original's whole normalized snapshot, or the failure the original reported.")
