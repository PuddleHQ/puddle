#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/github-rest/check.sh and slice.py against a description served from a folder (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "github-rest check-test: FAIL: $1" >&2; exit 1; }

python=python3
command -v python3 >/dev/null 2>&1 || python=python
"$python" -I "$here/slice.py" self-test >/dev/null || fail "slice.py self-test failed"

mkdir -p "$tmp/gh/ci/github-rest" "$tmp/site/abc123/d" "$tmp/site/main/d"
cp "$here/check.sh" "$here/slice.py" "$tmp/gh/ci/github-rest/"
cp "$here/../fetch-retry.sh" "$tmp/gh/ci/"
cat >"$tmp/site/abc123/d/api.json" <<'JSON'
{"openapi":"3.0.3","info":{"version":"1"},"paths":{"/user":{"get":{"responses":{"200":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/u"}}}}}}},"/user/repos":{"get":{"responses":{"200":{"content":{"application/json":{"schema":{"type":"array"}}}}}}},"/user/orgs":{"get":{"responses":{"200":{"content":{"application/json":{"schema":{"type":"array"}}}}}}}},"components":{"schemas":{"u":{"type":"object"},"other":{"type":"string"}}}}
JSON
sed 's/"type":"object"/"type":"string"/' "$tmp/site/abc123/d/api.json" >"$tmp/site/main/d/api.json"
sum=$(sha256sum <"$tmp/site/abc123/d/api.json" | cut -d' ' -f1)
printf 'abc123 d/api.json %s\n' "$sum" >"$tmp/gh/ci/github-rest/pin.txt"
"$python" -I "$here/slice.py" write "$tmp/site/abc123/d/api.json" "$tmp/slice.json"

export RAW_BASE_URL="file://$tmp/site" SLICE_FILE="$tmp/slice.json" FETCH_ATTEMPTS=1
"$tmp/gh/ci/github-rest/check.sh" >/dev/null 2>&1 || fail "a matching slice was refused"

printf '{}\n' >"$tmp/slice.json.bad"
if SLICE_FILE="$tmp/slice.json.bad" "$tmp/gh/ci/github-rest/check.sh" >/dev/null 2>"$tmp/err"; then fail "a slice that differs was accepted"; fi
grep -q "not what the pinned description gives" "$tmp/err" || fail "the stale slice is not named"

printf 'abc123 d/api.json %s\n' "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$tmp/gh/ci/github-rest/pin.txt"
if "$tmp/gh/ci/github-rest/check.sh" >/dev/null 2>"$tmp/err"; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"

printf 'abc123 d/api.json %s\n' "$sum" >"$tmp/gh/ci/github-rest/pin.txt"
if "$tmp/gh/ci/github-rest/check.sh" --latest >/dev/null 2>"$tmp/err"; then fail "a changed latest description was accepted"; fi
grep -q "GitHub changed its description" "$tmp/err" || fail "the drift is not named"
cp "$tmp/site/abc123/d/api.json" "$tmp/site/main/d/api.json"
"$tmp/gh/ci/github-rest/check.sh" --latest >/dev/null 2>&1 || fail "an unchanged latest description was refused"

# The real pin: a commit, a path and 64 hex digits.
line=$(grep -v '^#' "$here/pin.txt" | head -n 1)
# shellcheck disable=SC2086 # the pin line is three words, split on purpose
set -- $line
printf %s "$1" | grep -Eq '^[0-9a-f]{40}$' || fail "the pinned commit is not 40 hex digits: $line"
printf %s "$3" | grep -Eq '^[0-9a-f]{64}$' || fail "the pinned checksum is not 64 hex digits: $line"
