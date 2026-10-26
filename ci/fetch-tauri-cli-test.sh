#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/fetch-tauri-cli.sh against a small release served from a folder (no network). The
# archive here is a tarball: the script unpacks either kind by the pinned asset's extension.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fetch-tauri-cli-test: FAIL: $1" >&2; exit 1; }

mkdir -p "$tmp/src" "$tmp/site/tauri-cli-v9.9.9"
printf 'program' >"$tmp/src/cargo-tauri"
asset=cargo-tauri-test.tgz
tar -czf "$tmp/site/tauri-cli-v9.9.9/$asset" -C "$tmp/src" cargo-tauri
sum=$(sha256sum <"$tmp/site/tauri-cli-v9.9.9/$asset" | cut -d' ' -f1)

# The script reads ci/tauri-cli.sha256 beside itself: run a copy next to a test pin list.
mkdir -p "$tmp/ci"
cp "$here/fetch-tauri-cli.sh" "$tmp/ci/"
pins=$tmp/ci/tauri-cli.sha256
export TAURI_CLI_BASE_URL="file://$tmp/site"

printf '# comment\n9.9.9 %s %s\n' "$asset" "$sum" >"$pins"
"$tmp/ci/fetch-tauri-cli.sh" "$tmp/out" 2>/dev/null || fail "a pinned release did not install"
[ -f "$tmp/out/cargo-tauri" ] || fail "no cargo-tauri in the folder"

printf '9.9.9 %s %s\n' "$asset" "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
if "$tmp/ci/fetch-tauri-cli.sh" "$tmp/bad" 2>"$tmp/err"; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"
[ ! -e "$tmp/bad" ] || fail "a download that failed its checksum was unpacked"

printf '1.0.0 other %s\n' "$sum" >"$pins"
if "$tmp/ci/fetch-tauri-cli.sh" "$tmp/none" 9.9.9 2>"$tmp/err"; then fail "an unpinned version was accepted"; fi
grep -q "nothing pinned" "$tmp/err" || fail "the missing pin is not named"

echo "fetch-tauri-cli-test: ok"
