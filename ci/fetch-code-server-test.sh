#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/fetch-code-server.sh against a small release served from a folder (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fetch-code-server-test: FAIL: $1" >&2; exit 1; }

# A release the way upstream packs it: a top folder, a licence, the launcher, node and a shim link.
top=code-server-9.9.9-linux-amd64
mkdir -p "$tmp/src/$top/bin" "$tmp/src/$top/lib" "$tmp/src/$top/node_modules/.bin" "$tmp/site/v9.9.9"
printf '#!/bin/sh\n' >"$tmp/src/$top/bin/code-server"
printf 'node' >"$tmp/src/$top/lib/node"
printf '{"version":"9.9.9"}' >"$tmp/src/$top/package.json"
printf 'MIT' >"$tmp/src/$top/LICENSE"
ln -s ../x/y.js "$tmp/src/$top/node_modules/.bin/semver"
asset=$top.tar.gz
tar -czf "$tmp/site/v9.9.9/$asset" -C "$tmp/src" "$top"
sum=$(sha256sum <"$tmp/site/v9.9.9/$asset" | cut -d' ' -f1)

# The script reads ci/code-server.sha256 beside itself: run a copy next to a test pin list.
mkdir -p "$tmp/ci"
cp "$here/fetch-code-server.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/code-server.sha256
export CODE_SERVER_BASE_URL="file://$tmp/site"

printf '# comment\n9.9.9 %s %s\n' "$asset" "$sum" >"$pins"
"$tmp/ci/fetch-code-server.sh" "$tmp/out" 2>/dev/null || fail "a pinned release did not install"
[ -x "$tmp/out/bin/code-server" ] || [ -f "$tmp/out/bin/code-server" ] || fail "no launcher"
[ -f "$tmp/out/lib/node" ] && [ -f "$tmp/out/LICENSE" ] || fail "files missing"
[ ! -e "$tmp/out/node_modules/.bin/semver" ] || fail "the command shim was unpacked"
grep -q "\"version\": \"9.9.9\"" "$tmp/out/code-server.json" || fail "no version in code-server.json"
grep -q "$sum" "$tmp/out/code-server.json" || fail "no checksum in code-server.json"

"$tmp/ci/fetch-code-server.sh" "$tmp/out" 2>/dev/null && fail "a folder with content was overwritten"

printf '9.9.9 %s %s\n' "$asset" "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
if "$tmp/ci/fetch-code-server.sh" "$tmp/bad" 2>"$tmp/err"; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"

printf '1.0.0 other %s\n' "$sum" >"$pins"
if "$tmp/ci/fetch-code-server.sh" "$tmp/none" 9.9.9 2>"$tmp/err"; then fail "an unpinned version was accepted"; fi
grep -q "nothing pinned" "$tmp/err" || fail "the missing pin is not named"

# A release server that fails its first N requests, then serves the folder (ci/flaky-server.py).
serve() { # serve <fails> <name>: starts the server, sets $port and $srv_pid
    : >"$tmp/$2.log"
    python3 -I "$here/flaky-server.py" "$tmp/site" "$1" "$tmp/$2.log" >"$tmp/$2.port" &
    srv_pid=$!
    n=0
    until [ -s "$tmp/$2.port" ]; do
        n=$((n + 1))
        [ "$n" -lt 200 ] || fail "the fake server did not start"
        kill -0 "$srv_pid" 2>/dev/null || fail "the fake server died"
        sleep 0.05
    done
    port=$(cat "$tmp/$2.port")
}
printf '9.9.9 %s %s\n' "$asset" "$sum" >"$pins"
export FETCH_RETRY_BASE_S=0

serve 2 twice
CODE_SERVER_BASE_URL="http://127.0.0.1:$port" "$tmp/ci/fetch-code-server.sh" "$tmp/retried" 2>"$tmp/err" || { kill "$srv_pid"; fail "a server that fails twice was not survived: $(cat "$tmp/err")"; }
kill "$srv_pid"
[ "$(wc -l <"$tmp/twice.log")" -eq 3 ] || fail "expected 3 requests, saw $(wc -l <"$tmp/twice.log")"
[ "$(grep -c 'download attempt . of 8 failed (curl exit' "$tmp/err")" -eq 2 ] || fail "the two failed tries are not printed with curl's exit code"
[ -f "$tmp/retried/lib/node" ] || fail "no install after retries"

serve 99 always
if FETCH_ATTEMPTS=3 CODE_SERVER_BASE_URL="http://127.0.0.1:$port" "$tmp/ci/fetch-code-server.sh" "$tmp/never" 2>"$tmp/err"; then kill "$srv_pid"; fail "a server that always fails was accepted"; fi
kill "$srv_pid"
[ "$(wc -l <"$tmp/always.log")" -eq 3 ] || fail "expected exactly 3 tries, saw $(wc -l <"$tmp/always.log")"
grep -q '^::error title=fetch-code-server download failed::3 tries failed for .*curl exit' "$tmp/err" || fail "the final failure has no annotation naming curl's exit code"
unset FETCH_RETRY_BASE_S

echo "fetch-code-server-test: ok"
