#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds the pinned Forgejo for Windows, for the lab (PUDDLE_FORGEJO_BIN): Forgejo publishes no
# Windows release, so the pinned source tarball (ci/forgejo.sha256, the `forgejo-src-<version>.tar.gz`
# line) is downloaded, verified, and compiled with the runner's Go (its toolchain line is fetched by
# Go itself) and a C compiler (the SQLite driver). Puts <dest-dir>/forgejo.exe and the source tree
# <dest-dir>/src beside it: this build embeds no templates or assets, so the lab points
# PUDDLE_FORGEJO_STATIC_ROOT at that tree (crates/puddle-e2e/src/lab/forgejo.rs).
# Usage: ci/build-forgejo-windows.sh <dest-dir> [<version>]    version defaults to the first pinned source
# FORGEJO_BASE_URL replaces https://codeberg.org/forgejo/forgejo/releases/download (tests).
# Also runs on Linux (same Go build), which is how the lab's workflow can be tried without a Windows machine.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: $0 <dest-dir> [<version>]" >&2; exit 2; }
dest=$1
version=${2:-}

line=$(awk -v v="$version" '!/^#/ && NF >= 3 && $2 ~ /^forgejo-src-/ && (v == "" || $1 == v) { print; exit }' "$here/forgejo.sha256")
[ -n "$line" ] || { echo "build-forgejo-windows: no source pinned for ${version:-any version} in ci/forgejo.sha256" >&2; exit 1; }
version=${line%% *}
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
base=${FORGEJO_BASE_URL:-https://codeberg.org/forgejo/forgejo/releases/download}
download build-forgejo-windows "$base/v$version/$asset" "$work/$asset" 600
got=$(sha256sum < "$work/$asset" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "build-forgejo-windows: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
mkdir -p "$dest/src"
tar -xzf "$work/$asset" -C "$dest/src" --strip-components=1
exe=forgejo
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) exe=forgejo.exe ;; esac
(
    cd "$dest/src"
    CGO_ENABLED=1 GOTOOLCHAIN=${GOTOOLCHAIN:-auto} go build \
        -tags 'sqlite sqlite_unlock_notify' \
        -ldflags "-X main.Version=$version -X main.ForgejoVersion=$version" \
        -o "../$exe" .
)
echo "build-forgejo-windows: $asset verified ($got), built as $dest/$exe" >&2
