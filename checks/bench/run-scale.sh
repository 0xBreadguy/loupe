#!/bin/bash
# Alternating TS/Rust source-read measurements for the scale lab.
# Exploratory when the machine is busy: rerun on a quiet one for figures to keep.
# SCALE_DIR chooses the database set: default is the real-schema (t3) tiers;
# SCALE_DIR=checks/generated/scale re-runs the demo-schema lab.
set -u
cd "$(dirname "$0")/../.."
BIN=target/release/loupe-core
DIR="${SCALE_DIR:-checks/generated/scale/t3}"
TAG=$(echo "$DIR" | tr / -)
OUT=checks/generated/scale/results/$TAG
mkdir -p "$OUT"
NOW=1794484800000
uptime > "$OUT/load.txt"

run() { # tier ops warmup
  local tier=$1 ops=$2 warm=$3
  for i in 1 2 3 4 5; do
    node checks/bench/source-baseline.ts --db $DIR/$tier.sqlite --now-ms $NOW --ops $ops --warmup $warm > "$OUT/$tier-ts-$i.json" 2> "$OUT/$tier-ts-$i.err"
    echo "$tier ts $i rc=$?" >> "$OUT/runner.log"
    "$BIN" --bench-source $ops --db $DIR/$tier.sqlite --now-ms $NOW > "$OUT/$tier-rs-$i.json" 2> "$OUT/$tier-rs-$i.err"
    echo "$tier rs $i rc=$?" >> "$OUT/runner.log"
  done
}
rm -f "$OUT/runner.log"
run s 30 3
run m 15 3
run l 10 2
run xl 6 2
run xxl 4 1
uptime >> "$OUT/load.txt"
echo done
