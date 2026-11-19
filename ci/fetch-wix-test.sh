#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/fetch-wix.sh and ci/check-wix-pin.sh against a small fake WiX zip served from a folder or a
# flaky local server (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fetch-wix-test: FAIL: $1" >&2; exit 1; }

files="candle.exe candle.exe.config darice.cub light.exe light.exe.config wconsole.dll winterop.dll wix.dll WixUIExtension.dll WixUtilExtension.dll"
mkdir -p "$tmp/src" "$tmp/site"
for f in $files; do printf 'x' >"$tmp/src/$f"; done
# shellcheck disable=SC2086
python3 -I -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[2], "w"); [z.write(sys.argv[1] + "/" + n, n) for n in sys.argv[3:]]; z.close()' "$tmp/src" "$tmp/site/wix-test.zip" $files
sum=$(sha256sum <"$tmp/site/wix-test.zip" | cut -d' ' -f1)

# The scripts read ci/wix.sha256 beside themselves: run copies next to a test pin.
mkdir -p "$tmp/ci"
cp "$here/fetch-wix.sh" "$here/check-wix-pin.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/wix.sha256
export WIX_BASE_URL="file://$tmp/site"
export FETCH_RETRY_BASE_S=0
printf '# comment\n3.14.1 https://example.invalid/wix-test.zip %s\n' "$sum" >"$pins"

# 1. a pinned zip installs, with every file Tauri checks
"$tmp/ci/fetch-wix.sh" "$tmp/out/WixTools314" 2>/dev/null || fail "a pinned zip did not install"
for f in $files; do [ -f "$tmp/out/WixTools314/$f" ] || fail "$f missing after install"; done

# 2. a complete folder is left alone (no download: the site is gone)
WIX_BASE_URL="file://$tmp/nowhere" "$tmp/ci/fetch-wix.sh" "$tmp/out/WixTools314" 2>"$tmp/err" || fail "a complete folder was not left alone"
grep -q "nothing to do" "$tmp/err" || fail "the no-op is not said"

# 3. an incomplete folder is replaced
rm "$tmp/out/WixTools314/light.exe"
"$tmp/ci/fetch-wix.sh" "$tmp/out/WixTools314" 2>/dev/null || fail "an incomplete folder was not repaired"
[ -f "$tmp/out/WixTools314/light.exe" ] || fail "light.exe not restored"

# 4. a changed download is refused and nothing is unpacked
printf '3.14.1 https://example.invalid/wix-test.zip %s\n' "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
if "$tmp/ci/fetch-wix.sh" "$tmp/bad/WixTools314" 2>"$tmp/err"; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"
[ ! -e "$tmp/bad" ] || fail "a download that failed its checksum was unpacked"
printf '3.14.1 https://example.invalid/wix-test.zip %s\n' "$sum" >"$pins"

# 5. the cache serves the file when the host is gone, and a bad cache entry is ignored
mkdir -p "$tmp/cache"
FETCH_CACHE_DIR="$tmp/cache" "$tmp/ci/fetch-wix.sh" "$tmp/c1/WixTools314" 2>/dev/null || fail "first run with a cache dir failed"
[ -f "$tmp/cache/wix-test.zip" ] || fail "the verified zip was not kept in the cache"
WIX_BASE_URL="file://$tmp/nowhere" FETCH_ATTEMPTS=1 FETCH_CACHE_DIR="$tmp/cache" "$tmp/ci/fetch-wix.sh" "$tmp/c2/WixTools314" 2>"$tmp/err" || fail "the cache did not serve a run with the host gone"
grep -q "using the cached" "$tmp/err" || fail "the cache hit is not said"
printf 'junk' >"$tmp/cache/wix-test.zip"
FETCH_CACHE_DIR="$tmp/cache" "$tmp/ci/fetch-wix.sh" "$tmp/c3/WixTools314" 2>"$tmp/err" || fail "a bad cache entry was not survived"
grep -q "ignoring the cached" "$tmp/err" || fail "the bad cache entry is not named"

# 6. a server that fails twice is survived (the 2026-11-18 case: HTTP 500 once), one that always fails is not
serve() { # serve <fails> <name>: starts the server, sets $port and $srv_pid
    : >"$tmp/$2.log"
    python3 -I "$here/flaky-server.py" "$tmp/site" "$1" "$tmp/$2.log" >"$tmp/$2.port" &
    srv_pid=$!
    n=0
    until [ -s "$tmp/$2.port" ]; do
        n=$((n + 1))
        [ "$n" -lt 1200 ] || fail "the fake server did not start"
        kill -0 "$srv_pid" 2>/dev/null || fail "the fake server died"
        sleep 0.05
    done
    port=$(cat "$tmp/$2.port")
}
serve 2 twice
WIX_BASE_URL="http://127.0.0.1:$port" "$tmp/ci/fetch-wix.sh" "$tmp/r/WixTools314" 2>"$tmp/err" || { kill "$srv_pid"; fail "a server that fails twice was not survived: $(cat "$tmp/err")"; }
kill "$srv_pid"
[ "$(wc -l <"$tmp/twice.log")" -eq 3 ] || fail "expected 3 requests, saw $(wc -l <"$tmp/twice.log")"
[ -f "$tmp/r/WixTools314/candle.exe" ] || fail "no install after retries"
serve 99 always
if FETCH_ATTEMPTS=3 WIX_BASE_URL="http://127.0.0.1:$port" "$tmp/ci/fetch-wix.sh" "$tmp/never/WixTools314" 2>"$tmp/err"; then kill "$srv_pid"; fail "a server that always fails was accepted"; fi
kill "$srv_pid"
[ "$(wc -l <"$tmp/always.log")" -eq 3 ] || fail "expected exactly 3 tries, saw $(wc -l <"$tmp/always.log")"
grep -q '^::error title=fetch-wix download failed::3 tries failed' "$tmp/err" || fail "the final failure has no annotation"

# 7. check-wix-pin: a binary that names the pinned URL, hash and folder passes; each missing one fails
url=https://example.invalid/wix-test.zip
printf 'junk %s junk %s junk WixTools314 junk' "$url" "$sum" >"$tmp/tauri-good"
"$tmp/ci/check-wix-pin.sh" "$tmp/tauri-good" 2>/dev/null || fail "a matching binary was refused"
printf 'junk %s junk %s junk WixTools3141 junk' "$url" "$sum" >"$tmp/tauri-other-folder"
"$tmp/ci/check-wix-pin.sh" "$tmp/tauri-other-folder" 2>/dev/null || fail "a longer folder name that contains the pinned one was refused"
printf 'junk %s junk %s junk WixTools315 junk' "$url" "$sum" >"$tmp/tauri-bad-folder"
if "$tmp/ci/check-wix-pin.sh" "$tmp/tauri-bad-folder" 2>"$tmp/err"; then fail "a binary reading another folder was accepted"; fi
grep -q "WixTools314" "$tmp/err" || fail "the missing folder is not named"
printf 'junk %s junk WixTools314' "$sum" >"$tmp/tauri-bad-url"
if "$tmp/ci/check-wix-pin.sh" "$tmp/tauri-bad-url" 2>/dev/null; then fail "a binary with another URL was accepted"; fi
printf 'junk %s junk WixTools314' "$url" >"$tmp/tauri-bad-sha"
if "$tmp/ci/check-wix-pin.sh" "$tmp/tauri-bad-sha" 2>/dev/null; then fail "a binary with another hash was accepted"; fi

echo "fetch-wix-test: ok"
