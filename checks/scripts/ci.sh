#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export T3CODE_HOME
T3CODE_HOME=$(mktemp -d)
trap 'rm -rf "$T3CODE_HOME"' EXIT

step() {
  local name=$1 start=$SECONDS status
  shift
  printf '\n==> %s\n' "$name"
  if "$@"; then
    printf 'PASS: %s (%ss)\n' "$name" "$((SECONDS - start))"
  else
    status=$?
    printf 'FAIL: %s (%ss, exit %s)\n' "$name" "$((SECONDS - start))" "$status" >&2
    exit "$status"
  fi
}
node_gate() { node --experimental-ffi --disable-warning=ExperimentalWarning "checks/scripts/$@"; }
dashboard() { (cd oracle && "$@"); }

step 'Install dashboard dependencies' dashboard npm ci
step 'Build Rust workspace' cargo build --release --locked
step 'Generate fixtures' node_gate fixtures.ts
step 'Generate replay' node_gate replay.ts
step 'Rust tests' cargo test --release --locked
# The recorded fixtures replay what the original produced, with no Node and no generated inputs.
# golden_terminal drives only the Rust program inside tmux; where tmux is missing it says so and passes.
step 'Recorded fixtures without Node' cargo test --release --locked -p t3-agents-core -p t3-agents-term --test golden --test golden_status --test golden_cli --test golden_semantic --test golden_behavior --test golden_fuzz --test golden_demo --test golden_cross --test golden_full_read --test golden_text --test golden_terminal
step 'Grid parity: demo UTC' env TZ=UTC node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts
step 'Grid parity: edge UTC' env TZ=UTC node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts checks/generated/edge.json
step 'Grid parity: hidden characters UTC' env TZ=UTC node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts checks/fixtures/hidden.json
step 'Grid parity: activity labels UTC' env TZ=UTC node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts checks/fixtures/labels.json
step 'Grid parity: empty UTC' env TZ=UTC node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts checks/generated/empty.json
# A meter's colors depend on its width, and a rounding tie shows at a few widths only.
step 'Grid parity: every width from 60 to 300' env TZ=UTC GATE_GRIDS=60..300x58 'GATE_ACTIONS=|pick:demo-checkout|pick:demo-billing' node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts
step 'Semantic parity' node_gate parity.ts
step 'Demo parity' node_gate demo-parity.mjs
step 'Semantic fuzz' node_gate semantic-fuzz.ts
step 'Behavior parity' node_gate behavior-parity.ts
step 'Cross-machine parity UTC' env TZ=UTC node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/cross-machine-parity.ts
step 'Text parity' node_gate text-parity.ts
step 'Status parity' node_gate status-parity.ts
step 'CLI parity' node_gate cli-parity.ts
step 'Status watcher on changing databases' node_gate status-live-parity.ts
step 'Installer and launcher parity' node_gate install-parity.mjs
step 'T3 schema versions 054 to 096' python3 checks/scripts/schema-compat.py
step 'Real terminal parity (tmux)' node_gate terminal-parity.ts
# The Rust layout of grapheme clusters against the original renderer's own buffer.
step 'Cluster layout fuzz (20,000 seeded strings)' node_gate cluster-fuzz.ts
step 'Terminal capability parity (fake foot on a pty)' node_gate terminal-caps-parity.ts
step 'Grid parity: Asia/Kolkata' env GATE_TZ=Asia/Kolkata node --experimental-ffi --disable-warning=ExperimentalWarning checks/scripts/grid-parity.ts
step 'Dashboard tests' dashboard npm test
step 'Dashboard type check' dashboard npm run check
step 'Whitespace check' git diff --check
