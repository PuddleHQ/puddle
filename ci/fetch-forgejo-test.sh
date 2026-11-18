#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/fetch-forgejo.sh against a release served from a folder (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fetch-forgejo-test: FAIL: $1" >&2; exit 1; }
hash_of_file() { sha256sum <"$1" | cut -d' ' -f1; }

asset=forgejo-9.9.9-linux-amd64
mkdir -p "$tmp/site/v9.9.9" "$tmp/ci"
printf '#!/bin/sh\necho forgejo\n' >"$tmp/site/v9.9.9/$asset"
sum=$(sha256sum <"$tmp/site/v9.9.9/$asset" | cut -d' ' -f1)

# The script reads ci/forgejo.sha256 beside itself: run a copy next to a test pin list.
cp "$here/fetch-forgejo.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/forgejo.sha256
export FORGEJO_BASE_URL="file://$tmp/site"

printf '# comment\n9.9.9 %s %s\n' "$asset" "$sum" >"$pins"
"$tmp/ci/fetch-forgejo.sh" "$tmp/out" 2>/dev/null || fail "a pinned release did not install"
[ -x "$tmp/out/forgejo" ] || fail "no executable forgejo"
[ "$("$tmp/out/forgejo")" = forgejo ] || fail "the installed file is not the download"

printf '9.9.9 %s %s\n' "$asset" "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
if "$tmp/ci/fetch-forgejo.sh" "$tmp/bad" 2>"$tmp/err"; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"
[ ! -e "$tmp/bad/forgejo" ] || fail "a changed download was installed"

printf '1.0.0 other %s\n' "$sum" >"$pins"
if "$tmp/ci/fetch-forgejo.sh" "$tmp/none" 9.9.9 2>"$tmp/err"; then fail "an unpinned version was accepted"; fi
grep -q "nothing pinned" "$tmp/err" || fail "the missing pin is not named"

# The Windows pin is Gitea's release, from its own site, installed as forgejo.exe.
wasset=gitea-9.9.9-windows-amd64.exe
mkdir -p "$tmp/gitea/9.9.9"
printf 'MZ stand-in\n' >"$tmp/gitea/9.9.9/$wasset"
wsum=$(sha256sum <"$tmp/gitea/9.9.9/$wasset" | cut -d' ' -f1)
printf '9.9.9 %s %s\n9.9.9 %s %s\n' "$asset" "$sum" "$wasset" "$wsum" >"$pins"
GITEA_BASE_URL="file://$tmp/gitea" "$tmp/ci/fetch-forgejo.sh" "$tmp/win" "" windows 2>/dev/null || fail "a pinned Windows release did not install"
[ -x "$tmp/win/forgejo.exe" ] && [ ! -e "$tmp/win/forgejo" ] || fail "the Windows release is not installed as forgejo.exe alone"
"$tmp/ci/fetch-forgejo.sh" "$tmp/lin" 2>/dev/null || fail "the Linux pin was not found beside a Windows one"
[ -x "$tmp/lin/forgejo" ] || fail "no Linux forgejo"
printf '9.9.9 %s %s\n' "$wasset" "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
if GITEA_BASE_URL="file://$tmp/gitea" "$tmp/ci/fetch-forgejo.sh" "$tmp/badwin" "" windows 2>"$tmp/err"; then fail "a changed Windows download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the Windows mismatch is not named"

# Upstream down: the same file comes from the mirror's `forgejo-v<version>` release, after a printed move.
export FETCH_RETRY_BASE_S=0
mkdir -p "$tmp/mirror/forgejo-v9.9.9" "$tmp/mirror/gitea-v9.9.9"
cp "$tmp/site/v9.9.9/$asset" "$tmp/mirror/forgejo-v9.9.9/$asset"
cp "$tmp/gitea/9.9.9/$wasset" "$tmp/mirror/gitea-v9.9.9/$wasset"
printf '9.9.9 %s %s\n9.9.9 %s %s\n' "$asset" "$sum" "$wasset" "$wsum" >"$pins"
FORGEJO_BASE_URL="file://$tmp/down" FORGEJO_MIRROR_URL="file://$tmp/mirror" "$tmp/ci/fetch-forgejo.sh" "$tmp/mir" 2>"$tmp/err" ||
    fail "the mirror was not used while upstream was down: $(cat "$tmp/err")"
[ "$("$tmp/mir/forgejo")" = forgejo ] || fail "the mirror's file was not installed"
[ "$(grep -c 'download attempt . of 3 failed' "$tmp/err")" -eq 2 ] || fail "upstream did not get three tries: $(cat "$tmp/err")"
grep -q "trying the next host file://$tmp/mirror/forgejo-v9.9.9/$asset" "$tmp/err" || fail "the move to the mirror is not printed"
! grep -q '::error' "$tmp/err" || fail "a download the mirror served left an error annotation"
GITEA_BASE_URL="file://$tmp/down" FORGEJO_MIRROR_URL="file://$tmp/mirror" "$tmp/ci/fetch-forgejo.sh" "$tmp/mirwin" "" windows 2>/dev/null ||
    fail "the Windows release was not taken from the mirror's gitea-v<version> release"
[ -x "$tmp/mirwin/forgejo.exe" ] || fail "no forgejo.exe from the mirror"

# A mirror that serves a different file is refused like upstream: the pin is the only trust.
printf 'tampered\n' >"$tmp/mirror/forgejo-v9.9.9/$asset"
if FORGEJO_BASE_URL="file://$tmp/down" FORGEJO_MIRROR_URL="file://$tmp/mirror" "$tmp/ci/fetch-forgejo.sh" "$tmp/tamp" 2>"$tmp/err"; then
    fail "a changed mirror file was accepted"
fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mirror's mismatch is not named"
[ ! -e "$tmp/tamp/forgejo" ] || fail "a changed mirror file was installed"

# Every host down: one error annotation that names both.
if FETCH_ATTEMPTS=2 FORGEJO_BASE_URL="file://$tmp/down" FORGEJO_MIRROR_URL="file://$tmp/down2" "$tmp/ci/fetch-forgejo.sh" "$tmp/none2" 2>"$tmp/err"; then
    fail "a download no host served exited 0"
fi
[ "$(grep -c '^::error' "$tmp/err")" -eq 1 ] || fail "not exactly one error annotation: $(cat "$tmp/err")"
grep -q "^::error title=fetch-forgejo download failed::every host failed: file://$tmp/down/.*; file://$tmp/down2/" "$tmp/err" ||
    fail "the annotation does not name both hosts: $(cat "$tmp/err")"

# The cache: a verified download is kept; the next run uses it with every host down; a bad copy is ignored.
cache=$tmp/cache
FORGEJO_CACHE_DIR=$cache "$tmp/ci/fetch-forgejo.sh" "$tmp/c1" 2>/dev/null || fail "a cached run did not install"
[ "$(hash_of_file "$cache/$asset")" = "$sum" ] || fail "a verified download was not cached"
FORGEJO_CACHE_DIR=$cache FORGEJO_BASE_URL="file://$tmp/down" FORGEJO_MIRROR_URL="file://$tmp/down2" "$tmp/ci/fetch-forgejo.sh" "$tmp/c2" 2>"$tmp/err" ||
    fail "the cache was not used with every host down: $(cat "$tmp/err")"
grep -q "using the cached $asset" "$tmp/err" || fail "the cache hit is not printed"
[ "$("$tmp/c2/forgejo")" = forgejo ] || fail "the cached file was not installed"
printf 'stale\n' >"$cache/$asset"
FORGEJO_CACHE_DIR=$cache "$tmp/ci/fetch-forgejo.sh" "$tmp/c3" 2>"$tmp/err" || fail "a bad cached copy stopped the download"
grep -q "ignoring the cached $asset" "$tmp/err" || fail "the bad cached copy is not named"
[ "$(hash_of_file "$cache/$asset")" = "$sum" ] || fail "the bad cached copy was not replaced by the verified download"
unset FETCH_RETRY_BASE_S

# The real pin file: first line is well formed (version, asset named after it, 64 hex digits).
line=$(awk '!/^#/ && NF >= 3 && $2 ~ /^forgejo-/ { print; exit }' "$here/forgejo.sha256")
# shellcheck disable=SC2086 # the pin line is three words, split on purpose
set -- $line
[ "$2" = "forgejo-$1-linux-amd64" ] || fail "the pinned asset is not forgejo-<version>-linux-amd64: $line"
printf %s "$3" | grep -Eq '^[0-9a-f]{64}$' || fail "the pinned checksum is not 64 hex digits: $line"

wline=$(awk '!/^#/ && NF >= 3 && $2 ~ /^gitea-/ { print; exit }' "$here/forgejo.sha256")
# shellcheck disable=SC2086 # the pin line is three words, split on purpose
set -- $wline
[ "$2" = "gitea-$1-windows-amd64.exe" ] || fail "the pinned Windows asset is not gitea-<version>-windows-amd64.exe: $wline"
printf %s "$3" | grep -Eq '^[0-9a-f]{64}$' || fail "the pinned Windows checksum is not 64 hex digits: $wline"

echo "fetch-forgejo-test: ok"
