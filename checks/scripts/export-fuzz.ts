// Records the 945 action prefixes of `semantic-fuzz.ts`: 45 seeded cases, each with every prefix
// of its 20 generated actions. The generated snapshots and the action lists travel in the
// fixture, so `crates/core/tests/golden_fuzz.rs` replays them with only `cargo test`.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/semantic-fuzz.ts"), [], "fuzz.json.gz",
  "945 action prefixes: 45 seeded cases of 10 to 32 agents (including malformed topology, cycles and orphans) replayed through 21 prefixes each, comparing selected, scope, fold and expand sets and every row's fields. The generated snapshots and action lists travel in the fixture.")
