#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/free-disk.sh on a scratch tree: it removes the listed paths only when free space is under the
# threshold, leaves everything else alone, and exits 0 when a path is missing.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
fail() { echo "free-disk-test: FAIL: $1" >&2; exit 1; }

mkdir -p "$tmp/a" "$tmp/b" "$tmp/keep"
head -c 3145728 /dev/zero >"$tmp/a/big"
echo x >"$tmp/keep/file"
run() { # <below GB>
    FREE_DISK_BELOW_GB=$1 FREE_DISK_SUDO='' FREE_DISK_ROOT="$tmp" FREE_DISK_PATHS="$tmp/a $tmp/b $tmp/missing" \
        GITHUB_STEP_SUMMARY="$tmp/summary" "$here/free-disk.sh" >"$tmp/out"
}

run 0
[ -e "$tmp/a" ] || fail "removed a path while free space was above the threshold"
grep -q "nothing to do" "$tmp/out" || fail "no 'nothing to do' line"

run 1000000
[ ! -e "$tmp/a" ] && [ ! -e "$tmp/b" ] || fail "did not remove the listed paths"
[ -e "$tmp/keep/file" ] || fail "removed a path that was not listed"
grep -q "### free disk:" "$tmp/summary" || fail "no summary section"
grep -Eq '^ +3 MiB  .*/a$' "$tmp/summary" || fail "the summary does not name the 3 MiB the first path held"
grep -q "runner still low on disk" "$tmp/out" || fail "no warning while still under the floor"
echo "free-disk-test: ok"
