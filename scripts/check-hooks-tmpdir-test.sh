#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Runs the whole `hooks` gate of scripts/check.sh with the longest TMPDIR the socket-length guard lets through, and
# with one byte less: 61 and 60 bytes (the limit without XDG_RUNTIME_DIR; the host's lane paths and long task ids
# land near it). Every script self-test of the gate builds scratch directories below TMPDIR; one that nests a long
# path under the caller's TMPDIR fails here, which is how a self-test that only passes under a short TMPDIR shows up.
# The nested gate skips this script (CHECK_HOOKS_TMPDIR_NESTED), so the run does not recurse.
set -eu
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
unset CARGO_TARGET_DIR CHECK_TMPDIR
[ -z "${CHECK_HOOKS_TMPDIR_NESTED:-}" ] || {
    echo "check-hooks-tmpdir-test: skipped inside its own nested run"
    exit 0
}
here=$(cd "$(dirname "$0")/.." && pwd)
fail() {
    echo "check-hooks-tmpdir-test: $*" >&2
    exit 1
}
# Under /var/tmp, not the caller's TMPDIR, so the base below has room for the exact lengths.
root=$(mktemp -d /var/tmp/chk-hooks.XXXXXX)
trap 'rm -rf "$root"' EXIT

# base_of <n>: a directory whose check.sh temp dir (`<base>/chk.XXXXXX`, 11 bytes more) is n bytes long; made here.
base_of() {
    p="$root/b"
    while [ "${#p}" -lt $(($1 - 11)) ]; do p="${p}x"; done
    mkdir -p "$p"
    echo "$p"
}
for n in 60 61; do
    base=$(base_of "$n")
    log="$root/run-$n.log"
    (cd "$here" && env -u XDG_RUNTIME_DIR CHECK_HOOKS_TMPDIR_NESTED=1 CHECK_TMPDIR="$base" scripts/check.sh hooks) >"$log" 2>&1 ||
        {
            tail -n 30 "$log" >&2
            fail "the hooks gate failed under a $n-byte TMPDIR"
        }
    [ -z "$(ls -A "$base")" ] || fail "the $n-byte run left $(ls "$base") behind"
done
echo "check-hooks-tmpdir-test: ok"
