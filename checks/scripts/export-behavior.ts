// Records the 104 timed frames of `behavior-parity.ts`: eight event scripts over the demo
// snapshot, each frame's state, motion tweens and a digest of every cell plane. The scripts
// travel in the fixture (they carry their own updated snapshots), so
// `crates/core/tests/golden_behavior.rs` can replay them through `--behavior-script` with only
// `cargo test`.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/behavior-parity.ts"), [], "behavior.json.gz",
  "104 timed frames across eight scripts: clock, retargeting, help and freeze, arrivals and acknowledgement, notices and expiry, calm and reduced-motion overrides, the quiet scheduler and the CRT phase toggles. Each frame keeps its state, notice, picked, firstSeen, shimmer, tween, acked, waits, fps and FNV-1a digests of the ch, fg, bg and attribute planes, so a one-cell difference fails without storing 2.9 million numbers.")
