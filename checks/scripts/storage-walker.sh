#!/usr/bin/env bash
# CI-suitable synthetic correctness gate. The benchmark is separately opt-in:
#   python3 checks/bench/storage.py bench --help
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${CARGO_TARGET_DIR:=$HOME/.cache/t3-agents-checks/storage-walker-target}"
export CARGO_TARGET_DIR
cargo build --release --locked --manifest-path checks/storage-walker/Cargo.toml
args=()
if (( $# )); then args=(--root "$1"); fi
exec python3 checks/bench/storage.py check \
  --binary "$CARGO_TARGET_DIR/release/storage-walker-check" "${args[@]}"
