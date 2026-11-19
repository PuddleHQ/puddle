#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fails when the Tauri command line does not read the WiX toolset the way ci/fetch-wix.sh seeds it. Tauri
# compiles the toolset's URL, SHA-256 and folder name (`tauri\WixTools314` under the user's cache folder) into
# the binary, so the check reads those constants out of the very binary that builds the MSI: if Tauri moves to
# another WiX version or folder, the seeded copy would be ignored and Tauri would download again, unseen.
# Usage: ci/check-wix-pin.sh <cargo-tauri binary>
set -eu

here=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 1 ] && [ -f "$1" ] || { echo "usage: $0 <cargo-tauri binary>" >&2; exit 2; }
bin=$1
line=$(awk '!/^#/ && NF >= 3 { print; exit }' "$here/wix.sha256")
# shellcheck disable=SC2086
set -- $line
version=$1
url=$2
sha=$3
# 3.14.1 -> WixTools314 (major and minor, as tauri-bundler names it).
folder=WixTools$(printf %s "$version" | cut -d. -f1,2 | tr -d .)
for needle in "$url" "$sha" "$folder"; do
    grep -aqF -- "$needle" "$bin" || {
        echo "check-wix-pin: $bin does not contain '$needle': Tauri reads another WiX than ci/wix.sha256 pins; update the pin (and the folder in ci/build-msi.sh) from tauri-bundler's windows/msi/mod.rs" >&2
        exit 1
    }
done
echo "check-wix-pin: $bin reads $folder, $url and $sha, as pinned" >&2
