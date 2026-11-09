#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Puts the pinned Forgejo release binary in <dest-dir>/forgejo, for the lab (PUDDLE_FORGEJO_BIN): a
# real Git forge the tests start for one run, with its own SSH server and an account made for the
# run. The download must match the SHA-256 pinned in ci/forgejo.sha256.
# Usage: ci/fetch-forgejo.sh <dest-dir> [<version>]    version defaults to the first pinned one
# FORGEJO_BASE_URL replaces https://codeberg.org/forgejo/forgejo/releases/download (tests);
# FETCH_ATTEMPTS and FETCH_RETRY_BASE_S set the retry window (ci/fetch-retry.sh).
# Network failures are retried (infrastructure) and each retry is printed; a checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: $0 <dest-dir> [<version>]" >&2; exit 2; }
dest=$1
version=${2:-}

line=$(awk -v v="$version" '!/^#/ && NF >= 3 && $2 !~ /^forgejo-src-/ && (v == "" || $1 == v) { print; exit }' "$here/forgejo.sha256")
[ -n "$line" ] || { echo "fetch-forgejo: nothing pinned for ${version:-any version} in ci/forgejo.sha256" >&2; exit 1; }
version=${line%% *}
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
base=${FORGEJO_BASE_URL:-https://codeberg.org/forgejo/forgejo/releases/download}
download fetch-forgejo "$base/v$version/$asset" "$work/$asset" 600
got=$(sha256sum < "$work/$asset" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "fetch-forgejo: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
mkdir -p "$dest"
cp "$work/$asset" "$dest/forgejo"
chmod 755 "$dest/forgejo"
echo "fetch-forgejo: $asset verified ($got), installed as $dest/forgejo" >&2
