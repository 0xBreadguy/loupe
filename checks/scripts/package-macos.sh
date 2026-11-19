#!/usr/bin/env bash
# Build t3-agents-term for macOS and package it for release:
# checks/generated/macos-release/t3-agents-VERSION-universal-apple-darwin.tar.gz and SHA256SUMS beside it.
# The program is built for aarch64-apple-darwin and x86_64-apple-darwin and joined with lipo, so one archive serves
# every Mac. With --native-only it builds only this Mac's architecture (for a Mac without the other Rust target) and
# names the archive t3-agents-VERSION-aarch64-apple-darwin.tar.gz or ...-x86_64-apple-darwin.tar.gz.
# The archive unpacks into one folder holding the program, LICENSE, THIRD-PARTY-NOTICES.md, README and BUILD-INFO
# (the commit, the Rust version, the architectures, the minimum macOS, the dynamic dependencies, the signature).
# There is no install.sh: it is written for Linux and has not been run on macOS. Nothing is published.
# The hand-started workflow .github/workflows/macos-release.yml runs this on a GitHub runner.
# Plain bash that also runs under macOS's /bin/bash 3.2.
set -euo pipefail
cd "$(dirname "$0")/../.."

native_only=0
for arg in "$@"; do
  case "$arg" in
    --native-only) native_only=1;;
    -h|--help) echo 'Usage: checks/scripts/package-macos.sh [--native-only]'; exit 0;;
    *) echo "Unknown argument: $arg (usage: checks/scripts/package-macos.sh [--native-only])" >&2; exit 1;;
  esac
done

for tool in cargo rustc git python3 tar gzip shasum lipo vtool otool codesign file sort awk grep uname; do
  command -v "$tool" >/dev/null || { echo "Missing required tool: $tool (the Xcode command line tools provide most of them)" >&2; exit 1; }
done
[[ $(uname -s) == Darwin ]] || { echo 'macOS is required' >&2; exit 1; }

host=$(rustc -vV | awk '/^host: / {print $2}')
case "$host" in aarch64-apple-darwin|x86_64-apple-darwin) ;; *) echo "Unsupported Rust host: $host" >&2; exit 1;; esac

if [[ $native_only == 1 ]]; then
  targets=$host
  label=$host
else
  targets='aarch64-apple-darwin x86_64-apple-darwin'
  label=universal-apple-darwin
  for target in $targets; do
    libdir=$(rustc --target "$target" --print target-libdir)
    if ! ls "$libdir"/libstd-*.rlib >/dev/null 2>&1; then
      echo "The Rust target $target is not installed. Run: rustup target add $target" >&2
      echo 'Or build only this Mac'\''s architecture with: checks/scripts/package-macos.sh --native-only' >&2
      exit 1
    fi
  done
fi

# 11.0 is the first macOS release for Apple silicon and the floor Rust sets for aarch64-apple-darwin. It is used for
# both architectures, so every slice of the program asks for the same system. Read back from the binary below.
export MACOSX_DEPLOYMENT_TARGET=11.0

version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"] == "t3-agents-term"))')
out="$PWD/checks/generated/macos-release"
folder="t3-agents-$version-$label"
archive="$folder.tar.gz"
rm -rf -- "$out/$folder"
rm -f -- "$out/$archive" "$out/SHA256SUMS"
mkdir -p "$out/$folder"
export CARGO_TARGET_DIR="$out/target"

# Every system library the program may link: under /usr/lib only, as the first column of otool -L (minus the file's own line).
check_deps() { # $1 = binary, $2 = architecture; prints otool -L, fails on anything else
  local deps
  deps=$(otool -arch "$2" -L "$1" | sed '1d')
  printf '%s\n' "$deps"
  if printf '%s\n' "$deps" | awk 'NF {print $1}' | grep -Ev '^/usr/lib/[^ ]+\.dylib$'; then
    echo "Unexpected dynamic dependency in the $2 program" >&2; return 1
  fi
}
min_macos() { vtool -arch "$2" -show-build "$1" | awk '$1 == "minos" {print $2}'; } # $1 = binary, $2 = architecture

thin=''
for target in $targets; do
  cargo build --release --locked -p t3-agents-term --target "$target"
  thin="$thin $CARGO_TARGET_DIR/$target/release/t3-agents-term"
done

program="$out/$folder/t3-agents-term"
if [[ $native_only == 1 ]]; then
  cp "${thin# }" "$program"
  expected_archs=${host%%-*}
  [[ $expected_archs == aarch64 ]] && expected_archs=arm64
else
  # shellcheck disable=SC2086 # the two paths contain no spaces
  lipo -create -output "$program" $thin
  expected_archs='arm64 x86_64'
fi

# lipo (and a copy) drop or invalidate the linker's signature; sign again, ad hoc, with a fixed identifier.
codesign --force --sign - --identifier t3-agents-term "$program"
codesign --verify --strict --all-architectures "$program"

archs=$(lipo -archs "$program")
[[ $(printf '%s\n' $archs | sort | tr '\n' ' ') == "$(printf '%s\n' $expected_archs | sort | tr '\n' ' ')" ]] \
  || { echo "Unexpected architectures: $archs (wanted: $expected_archs)" >&2; exit 1; }
echo "Architectures: $archs"

deps=''
minos=''
for arch in $archs; do
  echo "Dynamic dependencies ($arch):"
  one=$(check_deps "$program" "$arch")
  printf '%s\n' "$one"
  deps="$deps$arch:
$one
"
  m=$(min_macos "$program" "$arch")
  echo "Minimum macOS ($arch): $m"
  [[ $m == "$MACOSX_DEPLOYMENT_TARGET" ]] || { echo "The $arch program asks for macOS $m, not $MACOSX_DEPLOYMENT_TARGET" >&2; exit 1; }
  minos=$m
done

signature=$(codesign -dv --verbose=2 "$program" 2>&1 | grep -E '^((Identifier|Format|Signature|TeamIdentifier)=|CodeDirectory )' || true)
[[ $signature == *'Signature=adhoc'* ]] || { echo 'The program is not ad hoc signed' >&2; exit 1; }
printf 'Signature:\n%s\n' "$signature"

cp LICENSE THIRD-PARTY-NOTICES.md "$out/$folder/"
cat > "$out/$folder/README" <<EOF
T3 Agents $version for macOS ($label)

This is a preview of the standalone macOS dashboard.

T3 Agents shows your T3 Code agents in the terminal. T3 Code must be installed
on this same Mac: the program reads its local database, read-only.

Try it, from this folder, on made-up data (it touches none of yours):

  ./t3-agents-term --demo

Keep it: copy the program to a folder on your PATH, for example

  mkdir -p ~/.local/bin
  cp t3-agents-term ~/.local/bin/t3-agents

then run t3-agents. If ~/.local/bin is not on your PATH, add it to your shell
configuration yourself; nothing here changes it for you.

The program is signed ad hoc, not by an Apple developer account. If macOS blocks
a copy downloaded with a browser, open System Settings > Privacy & Security and
allow it there, or remove the quarantine mark from the program yourself.

THIRD-PARTY-NOTICES.md contains third-party notices.

BUILD-INFO says which commit this was built from, the architectures, the
oldest macOS it runs on, and its signature.
EOF
{
  printf 'Commit: %s\n' "$(git rev-parse HEAD)"
  if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then echo 'Working tree: dirty'; else echo 'Working tree: clean'; fi
  rustc -vV
  printf 'Target: %s\nArchitectures: %s\nMinimum macOS: %s\n' "$label" "$archs" "$minos"
  printf 'Dynamic dependencies:\n%s' "$deps"
  printf 'Signature:\n%s\n' "$signature"
} > "$out/$folder/BUILD-INFO"

# No macOS metadata (._ files, extended attributes) and no local user or group names in the archive.
COPYFILE_DISABLE=1 tar -C "$out" --no-xattrs --uid 0 --gid 0 --uname root --gname wheel -cf - "$folder" | gzip -n > "$out/$archive"
(cd "$out" && shasum -a 256 "$archive" > SHA256SUMS)
printf 'Archive: %s\n' "$out/$archive"
