#!/usr/bin/env bash
# Build loupe-term for this machine's own architecture and package it for release:
# checks/generated/linux-release/TARGET/loupe-VERSION-TARGET.tar.gz and SHA256SUMS beside it.
# The archive unpacks into one folder holding the program, install.sh, loupe.svg, LICENSE, THIRD-PARTY-NOTICES.md, README and
# BUILD-INFO (the commit, the Rust version, the target, the glibc it needs). Nothing is published.
# The hand-started workflow .github/workflows/linux-release.yml runs this on GitHub's runners.
set -euo pipefail
cd "$(dirname "$0")/../.."
for tool in cargo rustc git python3 tar gzip sha256sum objdump ldd sort awk grep uname; do
  command -v "$tool" >/dev/null || { echo "Missing required tool: $tool" >&2; exit 1; }
done
[[ $(uname -s) == Linux ]] || { echo 'Linux is required' >&2; exit 1; }
target=$(rustc -vV | awk '/^host: / {print $2}')
case "$target" in x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;; *) echo "Unsupported native target: $target" >&2; exit 1;; esac
[[ $(uname -m) == "${target%%-*}" ]] || { echo 'Rust host does not match machine architecture' >&2; exit 1; }
version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"] == "loupe-term"))')
out="$PWD/checks/generated/linux-release/$target"
folder="loupe-$version-$target"
rm -rf -- "$out/$folder"
mkdir -p "$out/$folder"
export CARGO_TARGET_DIR="$PWD/checks/generated/linux-release/target"
cargo build --release --locked -p loupe-term --target "$target"
binary="$CARGO_TARGET_DIR/$target/release/loupe-term"
echo 'Dynamic dependencies:'
deps=$(ldd "$binary")
printf '%s\n' "$deps"
if printf '%s\n' "$deps" | awk '/=>/ {print $1} /^[[:space:]]*\// {print $1}' | grep -Ev '^(libc\.so\.6|libm\.so\.6|libgcc_s\.so\.1|libpthread\.so\.0|libdl\.so\.2|librt\.so\.1|ld-linux[^/]*\.so\.[0-9]+|/lib[^ ]*/ld-linux[^ ]*\.so\.[0-9]+)$'; then
  echo 'Unexpected dynamic dependency' >&2; exit 1
fi
if [[ $deps == *'not found'* ]]; then echo 'Missing dynamic dependency' >&2; exit 1; fi
glibc=$(objdump -T "$binary" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sort -Vu | tail -n 1)
[[ -n $glibc ]] || { echo 'Could not determine glibc requirement' >&2; exit 1; }
echo "Highest glibc symbol version: $glibc"
cp "$binary" install.sh loupe.svg LICENSE THIRD-PARTY-NOTICES.md "$out/$folder/"
printf 'Loupe %s for %s\n\nInstall for the current user, from this folder:\n\n  ./install.sh --binary ./loupe-term\n\nRemove it again:\n\n  ./install.sh --uninstall\n\nTHIRD-PARTY-NOTICES.md contains third-party notices.\n\nBUILD-INFO says which commit this was built from and which glibc it needs.\n' "$version" "$target" > "$out/$folder/README"
{
  printf 'Commit: %s\n' "$(git rev-parse HEAD)"
  if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then echo 'Working tree: dirty'; else echo 'Working tree: clean'; fi
  rustc -vV
  printf 'Target: %s\nHighest glibc symbol version: %s\n' "$target" "$glibc"
  printf 'Dynamic dependencies:\n%s\n' "$deps"
} > "$out/$folder/BUILD-INFO"
archive="$folder.tar.gz"
tar --owner=0 --group=0 --numeric-owner -C "$out" -czf "$out/$archive" "$folder"
(cd "$out" && sha256sum "$archive" > SHA256SUMS)
printf 'Archive: %s\n' "$out/$archive"
