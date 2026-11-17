// Records the 18 clock phases of `demo-parity.mjs`: the whole snapshot the original's demo builds
// at each offset, normalized the way the gate compares it. `crates/core/tests/golden_demo.rs`
// asks the Rust build for the same snapshot with `--demo-snapshot`.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/demo-parity.mjs"), [], "demo.json.gz",
  "18 demo clock phases from -1 s to 1 h, each the original's whole snapshot (agents, events, plans, histories, requests, item metadata) with null object members and the root path removed, which is exactly what the live gate compares.")
