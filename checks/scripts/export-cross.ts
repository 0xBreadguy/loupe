// Records `cross-machine-parity.ts`: a synthetic database with remote parents, a retry that
// resolves to an earlier provider turn and a delegation cycle. The fixture carries the database
// and its machine id, the original's whole snapshot, the 78 full-grid states and the 20 focused
// per-agent states (as plane digests), so `crates/core/tests/golden_cross.rs` replays all of it.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/cross-machine-parity.ts"), [], "cross.json.gz",
  "One cross-machine snapshot, the 78 full-grid states over it and 20 focused per-agent states, each as FNV-1a digests of the ch, fg, bg and attribute planes. The synthetic database and the environment-id file beside it travel as a tree.")
