#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/bisect-red.sh with a stub `gh` that answers the compare and last-green lookups from variables
# and records every call: which commits get a branch and a dispatch (all but the red one), the default for --good,
# the --max refusal, cleanup, and usage errors. Not seen: the real API.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/bisect-red-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { echo "bisect-red-test: $*" >&2; exit 1; }

cat >"$tmp/gh" <<'STUB'
#!/bin/sh
echo "$*" >>"$GH_LOG"
case "$1" in
run) printf '%s\n' "${GREEN_SHA:-}" ;;
api)
    case "$*" in
    *compare/*) printf '%s\n' ${COMMITS:-} ;;
    *matching-refs*) printf '%s\n' ${REFS:-} ;;
    esac
    ;;
esac
STUB
chmod +x "$tmp/gh"

# run <want exit> <args...>
run() {
    local want=$1 got=0
    shift
    : >"$tmp/log"
    BISECT_GH="$tmp/gh" GH_LOG="$tmp/log" GITHUB_REPOSITORY=o/r GREEN_SHA="${GREEN_SHA:-}" COMMITS="${COMMITS:-}" REFS="${REFS:-}" \
        "$here/bisect-red.sh" "$@" >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "exit $got, wanted $want for '$*': $(cat "$tmp/out")"
}

COMMITS="aaaaaaa1 bbbbbbb2 ccccccc3"
run 0 vm-linux.yml --good g000000 --bad ccccccc3
[ "$(grep -c '^workflow run ' "$tmp/log")" = 2 ] || fail "must dispatch the two commits before the red one: $(cat "$tmp/log")"
{ grep -q 'ref=refs/heads/bisect/vm-linux-aaaaaaa' "$tmp/log" && grep -q 'sha=bbbbbbb2' "$tmp/log"; } || fail "branches not created per suspect"
grep -q 'ccccccc3' "$tmp/log" && grep -q 'sha=ccccccc3' "$tmp/log" && fail "the red commit must not be dispatched"
grep -q -- '--ref bisect/vm-linux-bbbbbbb' "$tmp/log" || fail "workflow not dispatched on the branch"
grep -q 'compare/g000000...ccccccc3' "$tmp/log" || fail "compare range wrong"

COMMITS="ccccccc3"
run 0 vm-linux.yml --good g000000 --bad ccccccc3
[ "$(grep -c '^workflow run ' "$tmp/log")" = 0 ] || fail "one commit in range: it is the culprit, nothing to dispatch"

COMMITS="a1 a2 a3 a4"
GREEN_SHA=gdefault run 0 vm-linux.yml --bad a4
grep -q 'compare/gdefault...a4' "$tmp/log" || fail "--good must default to the last green scheduled run"
grep -q -- '--event schedule' "$tmp/log" || fail "default --good must come from scheduled runs"
GREEN_SHA="" run 2 vm-linux.yml --bad a4
run 2 vm-linux.yml --good g --bad a4 --max 3
grep -q '^workflow run ' "$tmp/log" && fail "over --max must dispatch nothing"
run 0 vm-linux.yml --good g --bad a4 --max 4
COMMITS=""
run 2 vm-linux.yml --good g --bad a4
run 2 vm-linux.yml --good g
run 2 vm-linux.yml --bogus
run 2 vm-linux.yml --good g --bad a4 --max x
run 2

REFS="refs/heads/bisect/vm-linux-aaaaaaa refs/heads/bisect/vm-linux-bbbbbbb"
run 0 vm-linux.yml --cleanup
[ "$(grep -c -- '-X DELETE repos/o/r/git/refs/heads/bisect/vm-linux-' "$tmp/log")" = 2 ] || fail "cleanup must delete both branches: $(cat "$tmp/log")"
echo "bisect-red-test: ok"
