#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/fetch-gateway.sh against a small release served from a folder (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fetch-gateway-test: FAIL: $1" >&2; exit 1; }

# A release the way JetBrains packs it: one top folder holding bin/gateway.sh.
mkdir -p "$tmp/site" "$tmp/pack/Gateway-1/bin" "$tmp/ci"
printf '#!/bin/sh\n' >"$tmp/pack/Gateway-1/bin/gateway.sh"
chmod +x "$tmp/pack/Gateway-1/bin/gateway.sh"
tar -czf "$tmp/site/Gateway-1.tar.gz" -C "$tmp/pack" Gateway-1
sum=$(sha256sum <"$tmp/site/Gateway-1.tar.gz" | cut -d' ' -f1)
zero=$(printf 0 | sha256sum | cut -d' ' -f1)

# The script reads ci/gateway.sha256 beside itself: run a copy next to a test pin list.
cp "$here/fetch-gateway.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/gateway.sha256
export GATEWAY_BASE_URL="file://$tmp/site" FETCH_RETRY_BASE_S=0 FETCH_ATTEMPTS=2

printf '# comment\n1 Gateway-1.tar.gz %s\n' "$sum" >"$pins"
"$tmp/ci/fetch-gateway.sh" "$tmp/out" 2>/dev/null || fail "the pinned release did not install"
[ -x "$tmp/out/bin/gateway.sh" ] || fail "the release was not unpacked without its top folder"

"$tmp/ci/fetch-gateway.sh" 2>/dev/null && fail "a missing destination was accepted"

printf '1 Gateway-1.tar.gz %s\n' "$zero" >"$pins"
if "$tmp/ci/fetch-gateway.sh" "$tmp/bad" 2>"$tmp/err"; then fail "a changed release was accepted"; fi
grep -q "checksum mismatch for Gateway-1.tar.gz" "$tmp/err" || fail "the mismatch is not named"

printf '# nothing\n' >"$pins"
if "$tmp/ci/fetch-gateway.sh" "$tmp/none" 2>"$tmp/err"; then fail "an empty pin list was accepted"; fi
grep -q "nothing pinned" "$tmp/err" || fail "the missing pin is not named"

printf '1 Missing.tar.gz %s\n' "$sum" >"$pins"
if "$tmp/ci/fetch-gateway.sh" "$tmp/gone" 2>"$tmp/err"; then fail "a download that fails was accepted"; fi
grep -q '^::error title=fetch-gateway download failed::2 tries failed' "$tmp/err" || fail "the failed download has no annotation"

echo "fetch-gateway-test: ok"
