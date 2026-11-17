#!/usr/bin/env bash
# Existing installs update by running dashboard/install.sh, so this path stays. The product's
# installer now lives at the repository root; hand it the same arguments.
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
exec "$source_dir/install.sh" "$@"
