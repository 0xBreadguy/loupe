#!/usr/bin/env bash
# Install or update T3 Agents for the current user. Safe to rerun.
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="$HOME/.local/bin"
app_dir="$data_dir/t3-agents"
desktop_file="$data_dir/applications/t3-agents.desktop"
marker='Managed by t3-agents (github.com/0xBreadguy/t3-agents)'
old_marker='Managed by omarchy-config/tools/t3-agents' # earlier installs; replaced on upgrade

# OpenTUI's native core runs under Node.js through its FFI support (26.4+).
if ! command -v node >/dev/null 2>&1; then
  printf 'Node.js 26.4 or later is required (for example: mise use -g node@latest).\n' >&2
  exit 1
fi
if ! node -e 'const [a, b] = process.versions.node.split(".").map(Number); process.exit(a > 26 || (a === 26 && b >= 4) ? 0 : 1)'; then
  printf 'Node.js 26.4 or later is required; found %s.\n' "$(node --version)" >&2
  exit 1
fi

for path in "$bin_dir/t3-agents" "$desktop_file"; do
  if [[ -e "$path" ]] && ! grep -qF -e "$marker" -e "$old_marker" "$path"; then
    printf 'Refusing to overwrite existing file: %s\n' "$path" >&2
    exit 1
  fi
done

install -d "$app_dir" "$bin_dir" "$data_dir/applications"
install -m 644 "$source_dir/package.json" "$source_dir/package-lock.json" "$app_dir/"
rm -f "$app_dir/README.md" # earlier installs copied it
rm -rf "$app_dir/src"
cp -r "$source_dir/src" "$app_dir/src"
rm -f "$app_dir/t3-agents.py" # version 1 (Python/curses)

# Runtime dependencies are pinned by the lockfile; reinstall only when it changes.
if [[ ! -d "$app_dir/node_modules" ]] || ! cmp -s "$app_dir/package-lock.json" "$app_dir/.installed-lock"; then
  (cd "$app_dir" && npm ci --omit=dev --no-fund --no-audit --loglevel=error)
  cp "$app_dir/package-lock.json" "$app_dir/.installed-lock"
fi

cat > "$bin_dir/t3-agents" <<'SH'
#!/usr/bin/env bash
# Managed by t3-agents (github.com/0xBreadguy/t3-agents)
set -euo pipefail
app="${XDG_DATA_HOME:-$HOME/.local/share}/t3-agents"
run=(node --experimental-ffi --disable-warning=ExperimentalWarning "$app/src/main.ts")
if [[ "${1:-}" == '--window' ]]; then
  shift
  # One dashboard window: focus it when it is already open, as omarchy-launch-or-focus does.
  address=$(hyprctl clients -j 2>/dev/null | jq -r '.[] | select(.class == "org.omarchy.t3-agents") | .address' | head -n1 || true)
  if [[ $address =~ ^0x[0-9a-f]+$ ]]; then
    exec hyprctl dispatch "hl.dsp.focus({ window = \"address:$address\" })" >/dev/null
  fi
  exec foot --app-id=org.omarchy.t3-agents --title='T3 Agents' "${run[@]}" "$@"
fi
exec "${run[@]}" "$@"
SH
chmod 755 "$bin_dir/t3-agents"

# Desktop Entry Exec has its own quoting rules, distinct from shell quoting.
node - "$desktop_file" "$bin_dir/t3-agents" "$marker" <<'JS'
const { writeFileSync } = require("node:fs")
const [file, command, marker] = process.argv.slice(2)
const quoted = command.replace(/[\\"`$]/g, (c) => "\\" + c).replace(/%/g, "%%")
writeFileSync(file, `# ${marker}\n[Desktop Entry]\nType=Application\nName=T3 Agents\n` +
  "Comment=Live visual dashboard for T3 Code agents, context and limits\n" +
  `Exec="${quoted}" --window\nIcon=utilities-system-monitor\nTerminal=false\nCategories=Development;\n`)
JS
printf 'Installed T3 Agents. Run t3-agents or t3-agents --window.\n'
