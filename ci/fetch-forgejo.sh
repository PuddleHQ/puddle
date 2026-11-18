#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puts the pinned Forgejo release binary in <dest-dir>/forgejo, for the lab (PUDDLE_FORGEJO_BIN): a
# real Git forge the tests start for one run, with its own SSH server and an account made for the
# run. The download must match the SHA-256 pinned in ci/forgejo.sha256.
# Usage: ci/fetch-forgejo.sh <dest-dir> [<version> [windows]]    version defaults to the first pinned one for the platform
# `windows` fetches the pinned Gitea Windows release instead (Forgejo ships none) and installs it as
# <dest-dir>/forgejo.exe; the lab runs either through the same fixture.
# Where the file comes from, in order: FORGEJO_CACHE_DIR (a folder CI restores between runs; a cached
# file is hashed against the pin like a download, a bad one is ignored), the upstream release, then the
# PuddleHQ/ci-mirror release `<forgejo|gitea>-v<version>` (unmodified upstream binaries, with their
# licence and source pointer), so a run does not depend on one host being up. The pin is the only
# trust: a file from any of them is installed only when it matches.
# FORGEJO_BASE_URL replaces https://codeberg.org/forgejo/forgejo/releases/download, GITEA_BASE_URL
# https://dl.gitea.com/gitea and FORGEJO_MIRROR_URL https://github.com/PuddleHQ/ci-mirror/releases/download
# (tests); FETCH_ATTEMPTS, FETCH_SOURCE_ATTEMPTS and FETCH_RETRY_BASE_S set the retry window
# (ci/fetch-retry.sh). Network failures are retried (infrastructure) and each retry is printed; a
# checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -ge 1 ] && [ "$#" -le 3 ] || { echo "usage: $0 <dest-dir> [<version> [windows]]" >&2; exit 2; }
dest=$1
version=${2:-}
platform=${3:-linux}
case "$platform" in
    linux) kind='^forgejo-.*-linux-amd64$' ;;
    windows) kind='^gitea-.*-windows-amd64[.]exe$' ;;
    *) echo "fetch-forgejo: the platform is linux or windows, not $platform" >&2; exit 2 ;;
esac

line=$(awk -v v="$version" -v kind="$kind" '!/^#/ && NF >= 3 && $2 ~ kind && (v == "" || $1 == v) { print; exit }' "$here/forgejo.sha256")
[ -n "$line" ] || { echo "fetch-forgejo: nothing pinned for ${version:-any version} on $platform in ci/forgejo.sha256" >&2; exit 1; }
version=${line%% *}
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mirror=${FORGEJO_MIRROR_URL:-https://github.com/PuddleHQ/ci-mirror/releases/download}
if [ "$platform" = windows ]; then
    url=${GITEA_BASE_URL:-https://dl.gitea.com/gitea}/$version/$asset
    mirror=$mirror/gitea-v$version/$asset
    installed=forgejo.exe
else
    url=${FORGEJO_BASE_URL:-https://codeberg.org/forgejo/forgejo/releases/download}/v$version/$asset
    mirror=$mirror/forgejo-v$version/$asset
    installed=forgejo
fi
hash_of() { sha256sum < "$1" | cut -d' ' -f1; }
cache=${FORGEJO_CACHE_DIR:-}
cached=
if [ -n "$cache" ] && [ -f "$cache/$asset" ]; then
    if [ "$(hash_of "$cache/$asset")" = "$want" ]; then
        cp "$cache/$asset" "$work/$asset"
        cached=1
        echo "fetch-forgejo: using the cached $asset (checksum matches the pin)" >&2
    else
        echo "fetch-forgejo: ignoring the cached $asset (checksum differs from the pin), downloading" >&2
    fi
fi
[ -n "$cached" ] || download_from fetch-forgejo "$work/$asset" 600 "$url" "$mirror"
got=$(hash_of "$work/$asset")
if [ "$got" != "$want" ]; then
    echo "fetch-forgejo: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
if [ -n "$cache" ] && [ -z "$cached" ]; then
    mkdir -p "$cache"
    cp "$work/$asset" "$cache/$asset"
fi
mkdir -p "$dest"
cp "$work/$asset" "$dest/$installed"
chmod 755 "$dest/$installed"
echo "fetch-forgejo: $asset verified ($got), installed as $dest/$installed" >&2
