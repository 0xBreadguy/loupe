// Records the bar's status line, byte for byte, from the TypeScript original at main `2f1d553`
// so `crates/core/tests/golden_status.rs` can check the Rust build with only `cargo test`.
// Same 347 cases as status-parity.ts: the recorder is a no-op unless T3_RECORD is set.
//
//   node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/export-status.ts
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/status-parity.ts"), [], "status.json.gz",
  "The status line for 347 cases: fixture snapshots (including 300 seeded fuzz cases), four database error lines with the path replaced by {path}, and 1,206 s of the demo watcher's timer replay.")
