#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/capture-upgrade-fixture.sh: a data folder is kept with its expected.json, a kept
# folder is never overwritten, a bad name and a folder that is no data folder are refused.
set -uo pipefail

script=$(cd "$(dirname "$0")" && pwd)/capture-upgrade-fixture.sh
tmp=$(mktemp -d "${TMPDIR:-/tmp}/capture-upgrade-fixture-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "capture-upgrade-fixture-test: FAIL: $*" >&2; fail=1; }

mkdir -p "$tmp/data/settings" "$tmp/keep"
echo db >"$tmp/data/puddle.db"
echo '{"version":2,"workspaces":[{"id":"shop"},{"id":"tools"}]}' >"$tmp/data/workspaces.json"
echo '{}' >"$tmp/data/settings/global.json"
export UPGRADE_FIXTURES="$tmp/keep"

"$script" "$tmp/data" v0.1.0 pypi.org api.example.com >/dev/null 2>&1 || bad "a data folder: exit $?"
[ "$(cat "$tmp/keep/v0.1.0/expected.json" 2>/dev/null)" = '{"workspaces":["shop","tools"],"rules":["pypi.org","api.example.com"]}' ] ||
    bad "expected.json: $(cat "$tmp/keep/v0.1.0/expected.json" 2>&1)"
[ -f "$tmp/keep/v0.1.0/data/settings/global.json" ] && [ -f "$tmp/keep/v0.1.0/data/puddle.db" ] || bad "the files were not copied"

"$script" "$tmp/data" v0.1.0 2>/dev/null && bad "a kept folder was overwritten"
"$script" "$tmp/data" '../x' 2>/dev/null && bad "a name with a slash was accepted"
mkdir "$tmp/empty"
"$script" "$tmp/empty" v0.2.0 2>/dev/null && bad "a folder with no data was kept"
"$script" "$tmp/data" v0.3.0 >/dev/null 2>&1
[ "$(cat "$tmp/keep/v0.3.0/expected.json")" = '{"workspaces":["shop","tools"],"rules":[]}' ] || bad "no rules: $(cat "$tmp/keep/v0.3.0/expected.json")"

if [ "$fail" = 0 ]; then echo "capture-upgrade-fixture-test: ok"; else exit 1; fi
