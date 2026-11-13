#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/deny-fresh.sh with a stand-in for cargo-deny that keeps its own "index": `fetch` copies
# the upstream list of yanked versions into it, `check` judges the fixture lock file
# (ci/fixtures/yanked.lock) against that index only. Cases: a stale index lets the yanked crate pass when the
# check runs alone (the bug); through the script the fetch comes first and the check fails on the yanked
# crate; a database older than a day prints its age and a fresh one does not; no database prints that; a
# failed fetch warns and still runs the check on the old copy; DENY_FETCH=0 does not fetch.
# Not seen: the real cargo-deny and crates.io (the real fetch is the one command this script runs).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/deny-fresh-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "deny-fresh-test: $*" >&2
    exit 1
}

printf 'async-executor 1.14.0\n' >"$tmp/upstream"
cat >"$tmp/deny" <<'STUB'
#!/bin/sh
case "$1" in
fetch)
    echo "fetch $*" >>"$STATE/calls"
    [ "${FETCH_FAILS:-0}" = 1 ] && exit 1
    cp "$STATE/upstream" "$STATE/index"
    ;;
*)
    echo "check $*" >>"$STATE/calls"
    status=0
    while read -r name ver; do
        if grep -A1 "^name = \"$name\"\$" "$LOCK" | grep -q "^version = \"$ver\"\$"; then
            echo "error[yanked]: detected yanked crate $name $ver"
            status=1
        fi
    done <"$STATE/index"
    exit $status
    ;;
esac
STUB
chmod +x "$tmp/deny"
export STATE="$tmp" LOCK="$here/fixtures/yanked.lock"

db="$tmp/dbs"
mkdir -p "$db/advisory-db-x/.git"
touch "$db/advisory-db-x/.git/FETCH_HEAD"
run() { DENY="$tmp/deny" DENY_DB_DIR="$db" "$here/deny-fresh.sh" check advisories 2>&1; }
reset() { : >"$tmp/index"; : >"$tmp/calls"; }

# The bug: the check alone, on a stale index, passes.
reset
"$tmp/deny" check advisories >/dev/null || fail "stand-in: a stale index should let the yanked crate pass"

reset
if out=$(run); then fail "after a fetch the yanked crate must fail the check: $out"; fi
printf '%s\n' "$out" | grep -q 'yanked crate async-executor 1.14.0' || fail "no yanked report: $out"
[ "$(head -n1 "$tmp/calls")" = "fetch fetch all" ] || fail "the fetch did not come first: $(cat "$tmp/calls")"
if printf '%s\n' "$out" | grep -q 'hours ago'; then fail "a database fetched just now printed an age: $out"; fi

touch -d '3 days ago' "$db/advisory-db-x/.git/FETCH_HEAD"
reset
out=$(run) || true
printf '%s\n' "$out" | grep -q 'last fetched 72 hours ago' || fail "no age for an old database: $out"

mv "$db" "$tmp/dbs-gone"
reset
out=$(run) || true
printf '%s\n' "$out" | grep -q 'no advisory database fetched yet' || fail "no notice for a missing database: $out"

# Offline: the fetch fails, the script says so and still runs the check (on an empty index, which passes).
reset
out=$(FETCH_FAILS=1 run) || fail "a failed fetch must still run the check on the old copy: $out"
printf '%s\n' "$out" | grep -q 'warning: the fetch failed' || fail "no warning for a failed fetch: $out"
grep -q '^check ' "$tmp/calls" || fail "the check did not run after a failed fetch"

reset
out=$(DENY_FETCH=0 run) || fail "DENY_FETCH=0 run failed: $out"
if grep -q '^fetch ' "$tmp/calls"; then fail "DENY_FETCH=0 still fetched"; fi

echo "deny-fresh-test: ok"
