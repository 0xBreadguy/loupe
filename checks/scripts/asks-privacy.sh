#!/usr/bin/env bash
# Isolated synthetic integration gate; no installed binary or real T3 home.
set -euo pipefail
cd "$(dirname "$0")/../.."
scratch=$(mktemp -d "$HOME/asks-check.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
mkdir -p "$scratch"/{tmp,target,tmux,t3}
export TMPDIR="$scratch/tmp" SQLITE_TMPDIR="$scratch/tmp" CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$scratch/target}" TMUX_TMPDIR="$scratch/tmux" T3CODE_HOME="$scratch/t3"
cargo build --release --locked
result=0
ASKS_STRICT=1 python3 checks/scripts/asks_sql_guard.py || result=1
python3 checks/scripts/asks_privacy.py || result=1
exit "$result"
