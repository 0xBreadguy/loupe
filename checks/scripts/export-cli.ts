// Records the product command line, original vs native, for the 75 cases of cli-parity.ts:
// standard output bytes, exit code and standard error. `crates/core/tests/golden_cli.rs` replays
// them against the Rust build; the synthetic databases travel in the fixture as SQL text.
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/cli-parity.ts"), [], "cli.json.gz",
  "The command line for 75 cases: --help, --once over three demo phases in three timezones, the cross-machine and title databases, T3CODE_HOME and HOME origins, database errors at each origin, usage errors, --status and the no-terminal path. Recorded stdout and stderr are hex so bytes compare exactly.")
