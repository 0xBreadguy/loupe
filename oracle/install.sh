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
# The dashboard window's app id. TUI.* is the convention Omarchy's window rules
# treat as a terminal app (Super+C copies, terminal opacity) without making it
# float (only TUI.float floats), and it is neither Omarchy's nor T3's name.
app_id=TUI.t3-agents

# OpenTUI's native core runs under Node.js through its FFI support (26.4+).
node_bin=$(type -P node || true)
if [[ -z $node_bin ]]; then
  printf 'Node.js 26.4 or later is required (for example: mise use -g node@latest).\n' >&2
  exit 1
fi
[[ $node_bin == /* ]] || node_bin="$PWD/$node_bin"
if ! "$node_bin" -e 'const [a, b] = process.versions.node.split(".").map(Number); process.exit(a > 26 || (a === 26 && b >= 4) ? 0 : 1)'; then
  printf 'Node.js 26.4 or later is required; found %s at %s.\n' "$("$node_bin" --version)" "$node_bin" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  printf 'npm is required to install the pinned dependencies; it usually comes with Node.js.\n' >&2
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

# Runtime dependencies are pinned by the lockfile; reinstall only when it changes.
if [[ ! -d "$app_dir/node_modules" ]] || ! cmp -s "$app_dir/package-lock.json" "$app_dir/.installed-lock"; then
  (cd "$app_dir" && npm ci --omit=dev --no-fund --no-audit --loglevel=error)
  cp "$app_dir/package-lock.json" "$app_dir/.installed-lock"
fi

# Written beside the old launcher and moved into place, so the bar widget never
# runs a half-written file.
launcher=$(mktemp "$bin_dir/.t3-agents.XXXXXX")
trap 'rm -f "$launcher"' EXIT
{
  printf '#!/usr/bin/env bash\n# %s\nset -euo pipefail\n' "$marker"
  # Desktop shells and bar widgets often start without the PATH an interactive
  # shell builds (mise, nvm), so keep the Node.js this installer checked.
  printf 'installed_node=%q # the Node.js the installer checked\napp_id=%q # app id of the dashboard window\n' "$node_bin" "$app_id"
  cat <<'SH'
app="${XDG_DATA_HOME:-$HOME/.local/share}/t3-agents"
# The checked Node.js first: a project's version manager can put an older one on PATH.
if [[ -x $installed_node ]]; then
  node=$installed_node
elif ! node=$(type -P node); then
  printf 't3-agents: Node.js not found (it was at %s at install time). Rerun the T3 Agents installer.\n' "$installed_node" >&2
  # The bar widget reads stdout: give it a reason it can show.
  if [[ " $* " == *' --status '* ]]; then
    printf '{"v":1,"running":0,"waiting":0,"failed":0,"agents":[],"error":"Node.js not found. Rerun the T3 Agents installer."}\n'
  fi
  exit 127
fi
run=("$node" --experimental-ffi --disable-warning=ExperimentalWarning "$app/src/main.ts")
if [[ "${1:-}" == '--window' ]]; then
  shift
  # One dashboard window: focus it when it is already open, as omarchy-launch-or-focus does.
  if [[ -n ${HYPRLAND_INSTANCE_SIGNATURE:-} ]] && command -v hyprctl >/dev/null && command -v jq >/dev/null; then
    address=$(hyprctl clients -j 2>/dev/null | jq -r --arg id "$app_id" '.[] | select(.class == $id) | .address' 2>/dev/null | head -n1 || true)
    if [[ $address =~ ^0x[0-9a-f]+$ ]]; then
      # Current Hyprland takes a Lua dispatch; older releases take focuswindow.
      if hyprctl dispatch "hl.dsp.focus({ window = \"address:$address\" })" >/dev/null 2>&1 ||
        hyprctl dispatch focuswindow "address:$address" >/dev/null 2>&1; then
        exit 0
      fi
    fi
  fi
  if command -v foot >/dev/null; then
    exec foot --app-id="$app_id" --title='T3 Agents' "${run[@]}" "$@"
  elif command -v xdg-terminal-exec >/dev/null; then
    exec xdg-terminal-exec --app-id="$app_id" --title='T3 Agents' -- "${run[@]}" "$@"
  elif [[ -n ${TERMINAL:-} ]]; then
    read -ra terminal <<<"$TERMINAL"
    exec "${terminal[@]}" -e "${run[@]}" "$@"
  fi
  message='t3-agents --window needs foot, xdg-terminal-exec or a TERMINAL variable. Or run t3-agents inside any terminal.'
  printf '%s\n' "$message" >&2
  # Started from the app launcher, stderr goes nowhere anyone reads.
  if [[ ! -t 2 ]] && command -v notify-send >/dev/null; then notify-send 'T3 Agents' "$message" || true; fi
  exit 1
fi
exec "${run[@]}" "$@"
SH
} >"$launcher"
chmod 755 "$launcher"
mv -f "$launcher" "$bin_dir/t3-agents"
trap - EXIT

# Desktop Entry Exec has its own quoting rules, distinct from shell quoting.
"$node_bin" - "$desktop_file" "$bin_dir/t3-agents" "$marker" "$app_id" <<'JS'
const { writeFileSync } = require("node:fs")
const [file, command, marker, appId] = process.argv.slice(2)
const quoted = command.replace(/[\\"`$]/g, (c) => "\\" + c).replace(/%/g, "%%")
writeFileSync(file, `# ${marker}\n[Desktop Entry]\nType=Application\nName=T3 Agents\n` +
  "Comment=Unofficial live dashboard for T3 Code agents, context and limits\n" +
  `Exec="${quoted}" --window\nIcon=utilities-system-monitor\nTerminal=false\nCategories=Development;\n` +
  `StartupWMClass=${appId}\n`)
JS

printf 'Installed T3 Agents. Run t3-agents or t3-agents --window.\n'
case ":$PATH:" in
  *":$bin_dir:"* | *":$bin_dir/:"*) ;;
  *) printf 'Note: %s is not on your PATH, so run %s or add that folder to PATH.\n' "$bin_dir" "$bin_dir/t3-agents" ;;
esac
