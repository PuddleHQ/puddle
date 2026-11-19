#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puts the WiX toolset Tauri's MSI bundler needs in <dest-dir>, the folder Tauri reads
# (`%LOCALAPPDATA%\tauri\WixTools314`: tauri-bundler's `dirs::cache_dir()/tauri/WixTools314`; ci/build-msi.sh
# passes it). When Tauri finds the folder complete it never downloads; when it does not, it downloads once with
# no retry, and a GitHub 500 then fails the whole msi chain. The zip must match the SHA-256 pinned in
# ci/wix.sha256. Source order: FETCH_CACHE_DIR (CI restores it keyed on the pin), then the download with the
# retry window of ci/fetch-retry.sh.
# Usage: ci/fetch-wix.sh <dest-dir>      WIX_BASE_URL replaces the pinned URL's folder (tests).
# Works in Linux sh and Git Bash. A folder that already has every file Tauri checks is left alone.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -eq 1 ] || { echo "usage: $0 <dest-dir>" >&2; exit 2; }
dest=$1

# The files tauri-bundler's WIX_REQUIRED_FILES checks; a folder missing one is deleted and downloaded again.
required="candle.exe candle.exe.config darice.cub light.exe light.exe.config wconsole.dll winterop.dll wix.dll WixUIExtension.dll WixUtilExtension.dll"
wix_complete() {
    for f in $required; do [ -f "$1/$f" ] || return 1; done
}

line=$(awk '!/^#/ && NF >= 3 { print; exit }' "$here/wix.sha256")
[ -n "$line" ] || { echo "fetch-wix: nothing pinned in ci/wix.sha256" >&2; exit 1; }
# shellcheck disable=SC2086
set -- $line
url=$2
want=$3
if [ -n "${WIX_BASE_URL:-}" ]; then url="$WIX_BASE_URL/${url##*/}"; fi
name=${url##*/}

if wix_complete "$dest"; then
    echo "fetch-wix: $dest already has the WiX files, nothing to do" >&2
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if ! cache_get fetch-wix "$name" "$want" "$work/$name"; then
    download fetch-wix "$url" "$work/$name" 300
    # Hash from stdin: for a path with backslashes, Git Bash's sha256sum puts a backslash before the hash.
    got=$(sha256sum < "$work/$name" | cut -d' ' -f1)
    if [ "$got" != "$want" ]; then
        echo "fetch-wix: checksum mismatch for $name: got $got, pinned $want" >&2
        exit 1
    fi
    cache_put "$name" "$work/$name"
fi
mkdir -p "$work/tree"
unzip -q -o "$work/$name" -d "$work/tree"
wix_complete "$work/tree" || { echo "fetch-wix: $name lacks a file Tauri checks ($required)" >&2; exit 1; }
# An incomplete folder is replaced whole, as Tauri does.
[ ! -e "$dest" ] || rm -r "$dest"
mkdir -p "$(dirname "$dest")"
mv "$work/tree" "$dest"
echo "fetch-wix: $name verified ($want), installed in $dest" >&2
