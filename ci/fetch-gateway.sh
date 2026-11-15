#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Unpacks the pinned JetBrains Gateway (Linux x86_64) into <dest-dir>, for the Gateway attach test
# (PUDDLE_GATEWAY_DIR): Gateway's own SSH client and bootstrap, driven headless. The download must
# match the SHA-256 pinned in ci/gateway.sha256.
# Usage: ci/fetch-gateway.sh <dest-dir>
# GATEWAY_BASE_URL replaces https://download.jetbrains.com/idea/gateway (tests);
# FETCH_ATTEMPTS and FETCH_RETRY_BASE_S set the retry window (ci/fetch-retry.sh).
# Network failures are retried (infrastructure) and each retry is printed; a checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -eq 1 ] || { echo "usage: $0 <dest-dir>" >&2; exit 2; }
dest=$1

line=$(awk '!/^#/ && NF >= 3 { print; exit }' "$here/gateway.sha256")
[ -n "$line" ] || { echo "fetch-gateway: nothing pinned in ci/gateway.sha256" >&2; exit 1; }
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
download fetch-gateway "${GATEWAY_BASE_URL:-https://download.jetbrains.com/idea/gateway}/$asset" "$work/$asset" 900
got=$(sha256sum < "$work/$asset" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "fetch-gateway: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
mkdir -p "$dest"
tar -xzf "$work/$asset" -C "$dest" --strip-components=1
[ -x "$dest/bin/gateway.sh" ] || { echo "fetch-gateway: $asset has no bin/gateway.sh" >&2; exit 1; }
echo "fetch-gateway: $asset verified ($got), unpacked in $dest" >&2
