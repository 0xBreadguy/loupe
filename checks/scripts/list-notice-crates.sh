#!/usr/bin/env bash
# Print the dependency versions to compare with THIRD-PARTY-NOTICES.md.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo tree --locked -p t3-agents-term --edges normal --prefix none --format '{p}' | sed -E 's/ \(\*\)$//; s/ \(proc-macro\)$//' | sort -u
