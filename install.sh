#!/usr/bin/env bash
# Install, update or remove the Rust T3 Agents build for the current user. Safe to rerun.
#
# Everything this writes is keyed by --name: the command ~/.local/bin/NAME, the program
# directory ${XDG_DATA_HOME:-~/.local/share}/NAME/, the launcher entry NAME.desktop and the
# window's app id. Two names therefore never touch each other's files. It is the Rust
# replacement for oracle/install.sh: no Node.js, no npm and no network.
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="$HOME/.local/bin"

marker='Managed by t3-agents-rust (github.com/0xBreadguy/t3-agents)'
# The Node installer's own marker. Its files may be replaced, so an existing Node install
# upgrades in place; anything without a marker of ours or of the original is left alone.
original_marker='Managed by t3-agents (github.com/0xBreadguy/t3-agents)'
old_marker='Managed by omarchy-config/tools/t3-agents' # earlier installs; replaced on upgrade

name='t3-agents'
title=''
binary=''
uninstall=0

usage() {
  cat <<'USAGE'
Usage: install.sh [--name NAME] [--title TITLE] [--binary PATH] [--uninstall]

  --name NAME      command, program directory, launcher entry and window app id
                   (default: t3-agents)
  --title TITLE    display name in the app launcher (default: T3 Agents, or
                   "T3 Agents (NAME)" for another name)
  --binary PATH    install a prebuilt t3-agents-term instead of building one
  --uninstall      remove what an install with this name wrote, and nothing else
USAGE
}

die() {
  printf '%s\n' "$1" >&2
  exit 1
}

while (($#)); do
  case $1 in
    --name) (($# >= 2)) || die '--name needs a value'; name=$2; shift 2 ;;
    --name=*) name=${1#*=}; shift ;;
    --title) (($# >= 2)) || die '--title needs a value'; title=$2; shift 2 ;;
    --title=*) title=${1#*=}; shift ;;
    --binary) (($# >= 2)) || die '--binary needs a value'; binary=$2; shift 2 ;;
    --binary=*) binary=${1#*=}; shift ;;
    --uninstall) uninstall=1; shift ;;
    --help | -h) usage; exit 0 ;;
    *) die "Unknown option '$1'" ;;
  esac
done

# A stand-in HOME does not move the XDG folders. Desktop sessions, Omarchy's among them, export
# XDG_DATA_HOME and XDG_CACHE_HOME as absolute paths, so a trial run that only replaces HOME
# would install into, or uninstall from, the real account's folders. That happened once and
# replaced a working install. So: when HOME is not this account's own home folder, neither
# variable may point into the account's home unless it is also under HOME.
account_home=''
if account=$(id -un 2>/dev/null) && [[ $account =~ ^[A-Za-z0-9_][A-Za-z0-9._-]*$ ]]; then
  account_home=$(eval "printf '%s' ~$account" 2>/dev/null) || account_home=''
  [[ $account_home == /* ]] || account_home=''
  account_home=${account_home%/}
fi
inside() { [[ -n $2 && ($1 == "$2" || $1 == "$2"/*) ]]; }
if [[ -n $account_home && ${HOME%/} != "$account_home" ]]; then
  for variable in XDG_DATA_HOME XDG_CACHE_HOME; do
    folder=${!variable:-}
    folder=${folder%/}
    if inside "$folder" "$account_home" && ! inside "$folder" "${HOME%/}"; then
      die "HOME is $HOME, but $variable is $folder, inside this account's own home folder. Unset $variable or point it under HOME, then run this again. Nothing was changed."
    fi
  done
fi

# The name becomes a command, a directory and an app id, so keep it to file-name characters.
[[ $name =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] ||
  die "The installer needs a --name that is safe as a file name, got '$name'."
if [[ -z $title ]]; then
  if [[ $name == 't3-agents' ]]; then
    title='T3 Agents'
  else
    title="T3 Agents ($name)"
  fi
fi

app_dir="$data_dir/$name"
desktop_file="$data_dir/applications/$name.desktop"
launcher="$bin_dir/$name"
app_id="TUI.$name" # TUI.* is the convention Omarchy treats as a terminal app

has_marker() { [[ -f $1 && ! -L $1 ]] && grep -qF -e "$marker" -- "$1"; }

# The bar keeps the status watcher it started, even after that program's files are replaced or
# removed, and it starts a new one a few seconds after one exits. So end this install's
# watchers: the bar then runs whatever the command points at now, with no shell restart.
# Only processes whose command line is this install's own program with --status --watch.
end_watchers() {
  command -v pkill >/dev/null || return 0
  local dir=$app_dir escaped='' i ch
  for ((i = 0; i < ${#dir}; i++)); do
    ch=${dir:i:1}
    case $ch in
      [].[\*^\$+?\(\)\{\}\|\\]) escaped+="\\$ch" ;;
      *) escaped+=$ch ;;
    esac
  done
  pkill -u "$UID" -f -- "${escaped}/(t3-agents-term|src/main\\.ts) --status --watch" 2>/dev/null || true
}
has_original_marker() { [[ -f $1 && ! -L $1 ]] && grep -qF -e "$original_marker" -e "$old_marker" -- "$1"; }

if ((uninstall)); then
  removed=0
  for path in "$launcher" "$desktop_file"; do
    [[ -e $path ]] || continue
    if has_marker "$path"; then
      rm -f -- "$path"
      printf 'Removed %s\n' "$path"
      removed=1
    else
      printf 'Leaving %s: it was not created by this installer.\n' "$path" >&2
    fi
  done
  if [[ -e $app_dir ]]; then
    if [[ -d $app_dir && ! -L $app_dir ]] && has_marker "$app_dir/.t3-agents-install"; then
      rm -rf -- "$app_dir"
      printf 'Removed %s\n' "$app_dir"
      removed=1
    else
      printf 'Leaving %s: it was not created by this installer.\n' "$app_dir" >&2
    fi
  fi
  end_watchers
  if ((removed)); then
    printf 'Removed %s.\n' "$title"
  else
    printf 'Nothing to remove for %s.\n' "$name"
  fi
  exit 0
fi

# Prerequisites, before anything is copied.
if [[ -n $binary ]]; then
  if [[ ! -f $binary ]] || [[ ! -x $binary ]]; then
    printf 'The file given to --binary is not an executable: %s\n' "$binary" >&2
    exit 1
  fi
else
  cargo_bin=$(type -P cargo || true)
  if [[ -z $cargo_bin ]]; then
    printf 'cargo is required to build t3-agents-term (install Rust from https://rustup.rs, or pass --binary PATH).\n' >&2
    exit 1
  fi
fi

for path in "$launcher" "$desktop_file"; do
  if { [[ -e $path ]] || [[ -L $path ]]; } && ! has_marker "$path" && ! has_original_marker "$path"; then
    printf 'Refusing to overwrite existing file: %s\n' "$path" >&2
    exit 1
  fi
done

# An existing program directory may be this installer's, the Node installer's (upgrade in
# place), or empty. Anything else is not ours to delete.
if [[ -L $app_dir ]]; then
  printf 'Refusing to overwrite symbolic link: %s\n' "$app_dir" >&2
  exit 1
fi
if [[ -e $app_dir ]]; then
  if [[ ! -d $app_dir ]]; then
    printf 'Refusing to overwrite existing path: %s\n' "$app_dir" >&2
    exit 1
  fi
  if has_marker "$app_dir/.t3-agents-install"; then
    : # ours: the rerun path
  elif has_original_marker "$launcher" || has_original_marker "$desktop_file"; then
    : # the Node installer's: upgraded below, with no node_modules left behind
  elif [[ -z $(ls -A -- "$app_dir") ]]; then
    : # empty
  else
    printf 'Refusing to overwrite existing directory: %s\n' "$app_dir" >&2
    printf 'It has no marker from this installer or the Node.js installer; remove it yourself if it is a T3 Agents install.\n' >&2
    exit 1
  fi
fi

# The build, unless a prebuilt binary was given.
if [[ -z $binary ]]; then
  # Build outside the checkout. This repository is also an installed Omarchy plugin, and the
  # plugin validator rejects parts of a Cargo build directory, so the default is the user's
  # cache. An existing CARGO_TARGET_DIR still wins.
  if [[ -z ${CARGO_TARGET_DIR:-} ]]; then
    CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/t3-agents/target"
    export CARGO_TARGET_DIR
  fi
  printf 'Building t3-agents-term (release)...\n' >&2
  (cd -- "$source_dir" && "$cargo_bin" build --release --locked -p t3-agents-term)
  target_dir=$CARGO_TARGET_DIR
  [[ $target_dir == /* ]] || target_dir="$source_dir/$target_dir"
  binary="$target_dir/release/t3-agents-term"
  [[ -x $binary ]] || die "The build did not produce $binary."
fi

install -d -- "$bin_dir" "$data_dir/applications"
# Recreate the program directory, so an upgrade from the Node installer leaves no
# node_modules, src or lockfile behind.
rm -rf -- "$app_dir"
install -d -- "$app_dir"

program_tmp=$(mktemp "$app_dir/.t3-agents-term.XXXXXX")
trap 'rm -f -- "$program_tmp"' EXIT
install -m 755 -- "$binary" "$program_tmp"
mv -f -- "$program_tmp" "$app_dir/t3-agents-term"
trap - EXIT
printf '%s\n' "$marker" >"$app_dir/.t3-agents-install"

# The JSON the bar widget can show when the program is gone; valid JSON whatever --title was.
status_error="t3-agents-term not found. Rerun the $title installer."
status_error=${status_error//\\/\\\\}
status_error=${status_error//\"/\\\"}
status_error=${status_error//$'\n'/ }
status_error=${status_error//$'\r'/ }
status_error=${status_error//$'\t'/ }

# Written beside the old launcher and moved into place, so the bar widget never
# runs a half-written file.
launcher_tmp=$(mktemp "$bin_dir/.$name.XXXXXX")
trap 'rm -f -- "$launcher_tmp"' EXIT
{
  printf '#!/usr/bin/env bash\n# %s\nset -euo pipefail\n' "$marker"
  printf 'name=%q # command name and app-id suffix\napp_id=%q # app id of the dashboard window\ntitle=%q # display name\nstatus_error=%q # JSON reason when the program is gone\n' \
    "$name" "$app_id" "$title" "$status_error"
  cat <<'SH'
app="${XDG_DATA_HOME:-$HOME/.local/share}/$name"
program="$app/t3-agents-term"
if [[ ! -x $program ]]; then
  printf '%s: t3-agents-term not found (it was at %s at install time). Rerun the %s installer.\n' "$name" "$program" "$title" >&2
  # The bar widget reads stdout: give it a reason it can show.
  if [[ " $* " == *' --status '* ]]; then
    printf '{"v":1,"running":0,"waiting":0,"failed":0,"agents":[],"error":"%s"}\n' "$status_error"
  fi
  exit 127
fi
run=("$program")
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
    exec foot --app-id="$app_id" --title="$title" "${run[@]}" "$@"
  elif command -v xdg-terminal-exec >/dev/null; then
    exec xdg-terminal-exec --app-id="$app_id" --title="$title" -- "${run[@]}" "$@"
  elif [[ -n ${TERMINAL:-} ]]; then
    read -ra terminal <<<"$TERMINAL"
    exec "${terminal[@]}" -e "${run[@]}" "$@"
  fi
  message="$name --window needs foot, xdg-terminal-exec or a TERMINAL variable. Or run $name inside any terminal."
  printf '%s\n' "$message" >&2
  # Started from the app launcher, stderr goes nowhere anyone reads.
  if [[ ! -t 2 ]] && command -v notify-send >/dev/null; then notify-send "$title" "$message" || true; fi
  exit 1
fi
exec "${run[@]}" "$@"
SH
} >"$launcher_tmp"
chmod 755 "$launcher_tmp"
mv -f -- "$launcher_tmp" "$launcher"
trap - EXIT

# Desktop Entry Exec has its own quoting rules, distinct from shell quoting.
command_path="$bin_dir/$name"
quoted=${command_path//\\/\\\\}
quoted=${quoted//\"/\\\"}
quoted=${quoted//\`/\\\`}
quoted=${quoted//\$/\\\$}
quoted=${quoted//%/%%}
desktop_tmp=$(mktemp "$data_dir/applications/.$name.XXXXXX")
trap 'rm -f -- "$desktop_tmp"' EXIT
{
  printf '# %s\n' "$marker"
  printf '[Desktop Entry]\nType=Application\nName=%s\n' "$title"
  printf 'Comment=Unofficial live dashboard for T3 Code agents, context and limits\n'
  printf 'Exec="%s" --window\nIcon=utilities-system-monitor\nTerminal=false\nCategories=Development;\n' "$quoted"
  printf 'StartupWMClass=%s\n' "$app_id"
} >"$desktop_tmp"
chmod 644 "$desktop_tmp"
mv -f -- "$desktop_tmp" "$desktop_file"
trap - EXIT

end_watchers
printf 'Installed %s. Run %s or %s --window.\n' "$title" "$name" "$name"
case ":$PATH:" in
  *":$bin_dir:"* | *":$bin_dir/:"*) ;;
  *) printf 'Note: %s is not on your PATH, so run %s or add that folder to PATH.\n' "$bin_dir" "$command_path" ;;
esac
