// Records the semantic state of 124 cases of `parity.ts`: every snapshot in the workload, with
// and without each of the 19 standard actions, plus four snapshots from the burst. The expected
// frames and the input snapshots both travel in the fixture; `crates/core/tests/golden_semantic.rs`
// replays them through `--semantic` with only `cargo test`.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/parity.ts"), [], "semantic.json.gz",
  "Selected row, scope, collapsed and expanded sets and the complete row list (id, depth, stage, kept, tucked, earlier) after each prefix of the 19 standard actions for demo, empty, error, edge, large and rapid, plus four burst snapshots: 124 frames.")
