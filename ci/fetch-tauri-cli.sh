#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Puts the pinned Tauri command line (`cargo-tauri.exe`, the tool that builds the MSI) in <dest-dir>
# so `cargo tauri` finds it on PATH. The download must match the SHA-256 pinned in
# ci/tauri-cli.sha256; building it with `cargo install` takes about ten minutes, this takes seconds.
# Usage: ci/fetch-tauri-cli.sh <dest-dir> [<version>]    version defaults to the first pinned one
# TAURI_CLI_BASE_URL replaces https://github.com/tauri-apps/tauri/releases/download (tests).
# Works in Linux sh and Git Bash. Network failures are retried (infrastructure) and each retry is
# printed; a checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: $0 <dest-dir> [<version>]" >&2; exit 2; }
dest=$1
version=${2:-}

line=$(awk -v v="$version" '!/^#/ && NF >= 3 && (v == "" || $1 == v) { print; exit }' "$here/tauri-cli.sha256")
[ -n "$line" ] || { echo "fetch-tauri-cli: nothing pinned for ${version:-any version} in ci/tauri-cli.sha256" >&2; exit 1; }
version=${line%% *}
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
base=${TAURI_CLI_BASE_URL:-https://github.com/tauri-apps/tauri/releases/download}
url="$base/tauri-cli-v$version/$asset"
download fetch-tauri-cli "$url" "$work/$asset" 300
# Hash from stdin: for a path with backslashes, Git Bash's sha256sum puts a backslash before the hash.
got=$(sha256sum < "$work/$asset" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "fetch-tauri-cli: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
mkdir -p "$dest"
case "$asset" in
*.zip) unzip -q -o "$work/$asset" -d "$dest" ;;
*) tar -xzf "$work/$asset" -C "$dest" ;;
esac
bin=$dest/cargo-tauri
[ -f "$bin" ] || [ -f "$bin.exe" ] || { echo "fetch-tauri-cli: $asset has no cargo-tauri" >&2; exit 1; }
echo "fetch-tauri-cli: $asset verified ($got), installed in $dest" >&2
