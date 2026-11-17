#!/usr/bin/env bash
# Draw the original and the Rust dashboard in a real terminal emulator's window, one after the
# other, and compare the two windows pixel by pixel after every step of the same input script.
#
# Everything happens in an isolated omabox desktop: never on a real one. The data is the synthetic
# emoji database (emoji-db.ts) under a pinned clock, in calm mode, so nothing moves by itself.
#
#   checks/scripts/window-parity.sh --terminal foot --terminal ghostty
#
#   --terminal NAME   foot, ghostty, kitty, alacritty, wezterm or sakura (VTE); repeat for several
#                     (default: foot). An X11-only terminal such as xterm cannot run here: the
#                     isolated desktop has no X server.
#   --titles SET      the thread titles: emoji (everyday titles, the default) or clusters-1,
#                     clusters-2, clusters-3 (every kind of grapheme cluster the layout
#                     handles, a page at a time)
#   --ssh-like        give the app only what a login over SSH would carry: TERM, HOME, PATH and
#                     LANG, none of the variables a terminal sets to name itself
#   --term NAME       tell the app TERM is NAME, whatever the terminal set
#   --binary PATH     the Rust dashboard, somewhere inside this checkout: the box sees nothing
#                     else of the machine (default: target/release/t3-agents-term)
#   --terminals DIR   a folder holding unpacked terminal packages (usr/bin, usr/lib, usr/share).
#                     A terminal found there runs from there; otherwise it must be installed.
#                     (default: checks/generated/terminals/root)
#   --out DIR         where the shots, byte captures and the report go
#                     (default: checks/generated/window-parity)
#   --box NAME        use this running box and leave it up (default: start one, stop it after)
#
# Exit 0 when no pixel differs in any step of any terminal and at least half the steps changed
# the screen (so the input demonstrably landed), 1 otherwise, 2 for a broken run.
# The byte capture of each run (what the app wrote to the terminal and nothing else) is kept
# beside the shots as NAME.bytes (without script(1)'s header lines), what the terminal sent back (its answers to the start-up
# questions, then the keys and clicks of the steps) as NAME.in, and the terminal's size in cells
# as NAME.size.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
terminals=()
binary="$repo/target/release/t3-agents-term"
troot="$repo/checks/generated/terminals/root"
out="$repo/checks/generated/window-parity"
box=''
titles=emoji
ssh_like=0
term=''
while (($#)); do
  case $1 in
    --terminal) terminals+=("$2"); shift 2 ;;
    --binary) binary=$(realpath -- "$2"); shift 2 ;;
    --terminals) troot=$(realpath -- "$2"); shift 2 ;;
    --out) out=$(realpath -m -- "$2"); shift 2 ;;
    --box) box=$2; shift 2 ;;
    --titles) titles=$2; shift 2 ;;
    --ssh-like) ssh_like=1; shift ;;
    --term) term=$2; shift 2 ;;
    *) printf 'unknown option %s\n' "$1" >&2; exit 2 ;;
  esac
done
((${#terminals[@]})) || terminals=(foot)
[[ -x $binary ]] || { printf 'no Rust dashboard at %s; build it or pass --binary\n' "$binary" >&2; exit 2; }
command -v omabox >/dev/null || { printf 'omabox is not installed; this check only runs in an isolated desktop\n' >&2; exit 2; }
node=(node --experimental-ffi --disable-warning=ExperimentalWarning)
now=1794484800000

# One step per entry: what to send, and a name for the shot taken after it.
#   t:TEXT typed text   k:KEY a named key   c:X,Y a click   s:X,Y,DY a wheel scroll
steps=(
  't:j|down' 't:j|down-again' 't:n|next-waiting' 't:G|last' 't:g|first' 't:a|scope-active'
  't:a|scope-sidebar' 't:a|scope-all' 't:t|lane' 't:J|details-down' 't:K|details-up' 't:?|help'
  't:j|help-scroll' 'k:Escape|help-closed' 'k:Down|arrow-down' 'k:Up|arrow-up' 'k:Next|page-down'
  't:p|paused' 't:p|resumed' 'c:300,520|click-row' 's:300,520,15|wheel-list' 's:1400,620,15|wheel-details'
)

mkdir -p "$out"
rm -f "$out"/*.png "$out"/*.bytes "$out"/*.in "$out"/*.size "$out"/report.json "$out"/titles.json
work=$(mktemp -d -p "$out" work.XXXXXX)
started_box=0
cleanup() {
  rm -rf -- "$work"
  if ((started_box)); then omabox down -b "$box" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

mkdir "$work/t3-home" "$work/db"
T3CODE_HOME="$work/t3-home" "${node[@]}" "$repo/checks/scripts/emoji-db.ts" "$work/db" "$titles" >"$out/titles.json"

if [[ -z $box ]]; then
  box=$(omabox up --new --net isolated --clean 2>&1 | grep -oE "box '[^']+' up" | head -1 | cut -d"'" -f2)
  [[ -n $box ]] || { printf 'could not start a box\n' >&2; exit 2; }
  started_box=1
fi
home=$(omabox -b "$box" path --home)
cp "$work/db/statev2.sqlite" "$home/window-parity.sqlite"
mkdir -p "$home/window-parity-t3"

ob() { omabox -b "$box" "$@"; }

# launcher APP ID: a script in the box that starts the app and, when it quits, records the
# terminal's size (a window is 80 by 24 for an instant before the desktop sizes it).
launcher() {
  local app=$1 id=$2 file="$home/$2.sh" run wrap=''
  [[ -z $term ]] || wrap="env TERM=$term "
  ((!ssh_like)) || wrap="env -i TERM=${term:-\$TERM} HOME=\$HOME PATH=\$PATH LANG=\${LANG:-C.UTF-8} T3CODE_HOME=/home/sbx/window-parity-t3 T3_FIXED_NOW=$now "
  if [[ $app == original ]]; then
    run="node --experimental-ffi --disable-warning=ExperimentalWarning --import file://$repo/checks/scripts/fixed-clock.mjs $repo/oracle/src/main.ts"
  else
    run=$binary
  fi
  cat >"$file" <<EOF
#!/bin/sh
export T3CODE_HOME=/home/sbx/window-parity-t3 T3_FIXED_NOW=$now
$wrap$run --calm --interval 30 --db /home/sbx/window-parity.sqlite
code=\$?
stty size >/home/sbx/$id.size
exit \$code
EOF
  chmod 755 "$file"
}

# open TERMINAL ID: start the terminal on the launcher, with the app's bytes captured by script(1).
open_window() {
  local terminal=$1 id=$2 bin env=() inner=(script -q -I "/home/sbx/$2.in" -O "/home/sbx/$2.bytes" -c "/home/sbx/$2.sh")
  if [[ -x $troot/usr/bin/$terminal ]]; then
    bin=$troot/usr/bin/$terminal
    env=(env "LD_LIBRARY_PATH=$troot/usr/lib" "TERMINFO_DIRS=$troot/usr/share/terminfo:/usr/share/terminfo")
  else
    bin=$(command -v "$terminal") || { printf '%s is neither in %s nor installed\n' "$terminal" "$troot" >&2; return 2; }
  fi
  case $terminal in
    foot) ob run -d --wait -- "${env[@]}" "$bin" "--app-id=$id" "--title=$id" "${inner[@]}" ;;
    ghostty) ob run -d --wait -- "${env[@]}" "$bin" "--class=com.t3agents.$id" "--title=$id" --gtk-single-instance=false --confirm-close-surface=false -e "${inner[@]}" ;;
    kitty) ob run -d --wait -- "${env[@]}" "$bin" --class "$id" --title "$id" -o confirm_os_window_close=0 -o update_check_interval=0 -o enable_audio_bell=no "${inner[@]}" ;;
    alacritty) ob run -d --wait -- "${env[@]}" "$bin" --class "$id" --title "$id" -e "${inner[@]}" ;;
    # WezTerm's own tab bar shows the running process's name and settles late; it is not the app's.
    wezterm) ob run -d --wait -- "${env[@]}" "$bin" -n --config enable_tab_bar=false start --always-new-process --class "$id" -- "${inner[@]}" ;;
    # sakura is a thin wrapper around VTE, the terminal inside GNOME Terminal, Ptyxis and Tilix.
    # GTK 3 on Wayland takes the window's app id from the program name, not from an option.
    sakura) ob run -d --wait -- "${env[@]}" "$bin" --name "$id" --class "$id" -t "$id" -e "${inner[@]}" ;;
    *) printf 'unknown terminal %s\n' "$terminal" >&2; return 2 ;;
  esac >/dev/null 2>&1
}

shot() { # shot ID NAME
  local path
  ob wait still --window "$1" >/dev/null 2>&1 || true
  path=$(ob shot --window "$1" 2>/dev/null | tail -1)
  [[ -s $path ]] || { printf 'no shot of %s\n' "$1" >&2; return 2; }
  cp -- "$path" "$out/$2.png"
  rm -f -- "$path"
}

drive() { # drive TERMINAL APP
  local terminal=$1 app=$2 id="w${1:0:1}${2:0:1}$$" n=0 step what name kind arg x y dy
  launcher "$app" "$id"
  open_window "$terminal" "$id"
  shot "$id" "$terminal-$app-00-start"
  for step in "${steps[@]}"; do
    what=${step%%|*} name=${step#*|} kind=${what%%:*} arg=${what#*:}
    n=$((n + 1))
    case $kind in
      t) ob keys --window "$id" -t "$arg" ;;
      k) ob keys --window "$id" "$arg" ;;
      c) IFS=, read -r x y <<<"$arg"; ob click --window "$id" "$x" "$y" ;;
      s) IFS=, read -r x y dy <<<"$arg"; ob scroll --window "$id" "$x" "$y" "$dy" ;;
    esac >/dev/null 2>&1 || { printf 'could not send %s to %s in %s\n' "$what" "$app" "$terminal" >&2; return 2; }
    ob wait for 400ms >/dev/null 2>&1
    shot "$id" "$(printf '%s-%s-%02d-%s' "$terminal" "$app" "$n" "$name")"
  done
  ob keys --window "$id" -t q >/dev/null 2>&1
  ob wait window "$id" --gone >/dev/null 2>&1 || { printf '%s in %s did not quit on q\n' "$app" "$terminal" >&2; return 2; }
  cp -- "$home/$id.bytes" "$out/$terminal-$app.bytes"
  cp -- "$home/$id.size" "$out/$terminal-$app.size"
  cp -- "$home/$id.in" "$out/$terminal-$app.in"
}

for terminal in "${terminals[@]}"; do
  drive "$terminal" original
  drive "$terminal" rust
  printf 'drove %s\n' "$terminal" >&2
done

python3 - "$out" "${terminals[@]}" <<'EOF'
import json, pathlib, sys
from PIL import Image, ImageChops

out, terminals = pathlib.Path(sys.argv[1]), sys.argv[2:]
report, failed = {}, False
for terminal in terminals:
    sizes = {app: (out / f"{terminal}-{app}.size").read_text().split() for app in ("original", "rust")}
    steps, changed, previous = [], 0, None
    for first in sorted(out.glob(f"{terminal}-original-*.png")):
        data = first.read_bytes()
        changed += previous is not None and data != previous
        previous = data
        name = first.name[len(f"{terminal}-original-"):-4]
        second = out / f"{terminal}-rust-{name}.png"
        a, b = Image.open(first).convert("RGB"), Image.open(second).convert("RGB")
        if a.size != b.size:
            steps.append({"step": name, "differs": "window sizes", "original": a.size, "rust": b.size})
            continue
        width, height = a.size
        red, green, blue = ImageChops.difference(a, b).split()
        any_channel = ImageChops.lighter(ImageChops.lighter(red, green), blue)
        count = width * height - any_channel.histogram()[0]
        entry = {"step": name, "pixels": width * height, "pixels_that_differ": count}
        if count:
            left, top, right, bottom = any_channel.getbbox()
            entry["rows"] = [top, bottom - 1]
            entry["columns"] = [left, right - 1]
        steps.append(entry)
    differing = [s for s in steps if s.get("pixels_that_differ") or s.get("differs")]
    report[terminal] = {
        "cells_rows_by_columns": sizes,
        "steps_compared": len(steps),
        "steps_that_differ": len(differing),
        "pixels_compared": sum(s.get("pixels", 0) for s in steps),
        "pixels_that_differ": sum(s.get("pixels_that_differ", 0) for s in steps),
        "steps_that_changed_the_original_screen": changed,
        "differing_steps": differing,
    }
    # A run whose input never landed would compare one screen with itself over and over.
    failed = failed or bool(differing) or sizes["original"] != sizes["rust"] or changed < len(steps) // 2
(out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report, indent=2))
sys.exit(1 if failed else 0)
EOF
