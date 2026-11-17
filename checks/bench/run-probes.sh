#!/bin/bash
# Incremental-reader probes for the real-schema (t3) scale tiers, both engines.
# Results land in checks/generated/scale/results/probes-$TAG/.
set -u
cd "$(dirname "$0")/../.."
BIN=target/release/t3-agents-core
DIR="${SCALE_DIR:-checks/generated/scale/t3}"
TAG=$(echo "$DIR" | tr / -)
OUT=checks/generated/scale/results/probes-$TAG
mkdir -p "$OUT"
NOW=1794484800000
uptime > "$OUT/load.txt"
for tier in "$@"; do
  node checks/bench/incremental-probes.ts --db "$DIR/$tier.sqlite" --now-ms $NOW \
    > "$OUT/$tier-ts.json" 2> "$OUT/$tier-ts.err"
  echo "$tier ts rc=$?" >> "$OUT/runner.log"
  "$BIN" --bench-probe-incremental --db "$DIR/$tier.sqlite" --now-ms $NOW \
    > "$OUT/$tier-rs.json" 2> "$OUT/$tier-rs.err"
  echo "$tier rs rc=$?" >> "$OUT/runner.log"
done
uptime >> "$OUT/load.txt"
echo done
