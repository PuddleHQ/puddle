#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puts the pinned Forgejo release binary in <dest-dir>/forgejo, for the lab (PUDDLE_FORGEJO_BIN): a
# real Git forge the tests start for one run, with its own SSH server and an account made for the
# run. The download must match the SHA-256 pinned in ci/forgejo.sha256.
# Usage: ci/fetch-forgejo.sh <dest-dir> [<version> [windows]]    version defaults to the first pinned one for the platform
# `windows` fetches the pinned Gitea Windows release instead (Forgejo ships none) and installs it as
# <dest-dir>/forgejo.exe; the lab runs either through the same fixture.
# FORGEJO_BASE_URL replaces https://codeberg.org/forgejo/forgejo/releases/download and GITEA_BASE_URL
# https://dl.gitea.com/gitea (tests);
# FETCH_ATTEMPTS and FETCH_RETRY_BASE_S set the retry window (ci/fetch-retry.sh).
# Network failures are retried (infrastructure) and each retry is printed; a checksum mismatch never is.
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
if [ "$platform" = windows ]; then
    url=${GITEA_BASE_URL:-https://dl.gitea.com/gitea}/$version/$asset
    installed=forgejo.exe
else
    url=${FORGEJO_BASE_URL:-https://codeberg.org/forgejo/forgejo/releases/download}/v$version/$asset
    installed=forgejo
fi
download fetch-forgejo "$url" "$work/$asset" 600
got=$(sha256sum < "$work/$asset" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "fetch-forgejo: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
mkdir -p "$dest"
cp "$work/$asset" "$dest/$installed"
chmod 755 "$dest/$installed"
echo "fetch-forgejo: $asset verified ($got), installed as $dest/$installed" >&2
