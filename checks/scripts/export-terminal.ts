// Records the real-terminal gate (`terminal-parity.ts`) from the TypeScript original at main
// `2f1d553`, so `crates/term/tests/golden_terminal.rs` can check `t3-agents-term` inside tmux
// without Node. Needs tmux, the original's dependencies and a release build of the terminal
// program (the gate also runs it). Takes about ten minutes a run, twice.
//
//   node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/export-terminal.ts
import { join } from "node:path"
import { ROOT, exportGate } from "./golden.ts"

exportGate(join(ROOT, "scripts/terminal-parity.ts"), [], "terminal.json.gz",
  "What a real terminal shows and is left in, for the 551 cases of the tmux gate: 14 screen cases of 38 captures each (every cell, with colour and weight, after the keys, mouse events and resize recorded with them), a fifteenth of 12 captures after bracketed pastes (what the original shows when text, escape sequences and 100 KB are pasted, and when a paste arrives in pieces), what the original leaves behind on six ways of exiting (four, and two after a bracketed paste of quit keys), its behaviour at odd sizes, on TERM=dumb and inside another tmux, and what it prints when it is not on a terminal. Also the synthetic database the db cases read.")
