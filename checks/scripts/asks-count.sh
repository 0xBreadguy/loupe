#!/usr/bin/env bash
# Hold the product's asks count to the recorded corpus.
#
#   checks/scripts/asks-count.sh            compare every case
#   checks/scripts/asks-count.sh --json     print {"case-id": count} for
#                                           checks/scripts/asks-corpus.py --actual
#   checks/scripts/asks-count.sh PATH.json  use another corpus, same shape
#
# The corpus is checks/fixtures/asks-counts.json, made from the prototype by
# checks/scripts/asks-corpus.py --write --reference PATH.
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${CARGO_TARGET_DIR:=$PWD/checks/generated/asks-count-target}"
export CARGO_TARGET_DIR
exec cargo run --release --quiet --locked --manifest-path checks/asks-count/Cargo.toml -- "$@"
