#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/nightly-issue.sh with a stub `gh` (answers `issue list` and `run list` from variables, records
# every write call and keeps the issue body), and a structure check of .github/workflows/nightly-signal.yml: it
# names only workflows that exist, and every workflow with a cron trigger is either named there or listed below
# as covered otherwise, so a new nightly workflow cannot go red unnoticed. Not seen: the real `gh` and the
# workflow_run event.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/nightly-issue-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { echo "nightly-issue-test: $*" >&2; exit 1; }

cat >"$tmp/gh" <<'STUB'
#!/bin/sh
case "$1 $2" in
"issue list") printf '%s\n' "${OPEN_ISSUES:-[]}" ;;
"run list") echo "$*" >>"$GH_LOG"; [ "${RUN_FAIL:-}" = 1 ] && exit 1; printf '%s\n' "${GREEN_SHA:-}" ;;
"issue create") echo "$*" >>"$GH_LOG"; cp "$6" "$tmp_body" ;;
"issue edit") echo "$*" >>"$GH_LOG"; cp "$5" "$tmp_body" ;;
*) echo "$*" >>"$GH_LOG" ;;
esac
STUB
chmod +x "$tmp/gh"

# run <name> <want exit> <conclusion> <open issues json> [GREEN_SHA]; write calls land in $tmp/gh.log
run() {
    local name=$1 want=$2 conclusion=$3 open=$4 green=${5:-} got=0
    : >"$tmp/gh.log"
    : >"$tmp/body.md"
    OPEN_ISSUES=$open GREEN_SHA=$green GH_LOG="$tmp/gh.log" tmp_body="$tmp/body.md" NIGHTLY_GH="$tmp/gh" \
        "$here/nightly-issue.sh" vm-linux "$conclusion" https://example.test/runs/9 bad1234 >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "$name: exit $got, wanted $want: $(cat "$tmp/out")"
}
writes() { grep -c "^issue " "$tmp/gh.log" || true; }
open='[{"number":5,"title":"Nightly red: vm-linux"},{"number":6,"title":"Nightly red: windows"}]'

run "red, no open issue" 0 failure '[]' good5678
grep -q '^issue create --title Nightly red: vm-linux ' "$tmp/gh.log" || fail "red: no issue created: $(cat "$tmp/gh.log")"
{ grep -q 'good5678' "$tmp/body.md" && grep -q 'ci/bisect-red.sh vm-linux.yml --good good5678 --bad bad1234' "$tmp/body.md"; } || fail "red: body lacks the bisect command: $(cat "$tmp/body.md")"
{ grep -q -- '--workflow vm-linux.yml' "$tmp/gh.log" && grep -q -- '--event schedule' "$tmp/gh.log"; } || fail "red: last green not asked for the workflow's scheduled runs"
run "red, no green nightly known" 0 timed_out '[]'
grep -q 'No green nightly' "$tmp/body.md" || fail "red without a green run: body must say the suspects are unknown"
run "red, issue open" 0 failure "$open" good5678
grep -q '^issue edit 5 ' "$tmp/gh.log" && [ "$(writes)" = 1 ] || fail "red with an open issue must edit #5 only: $(cat "$tmp/gh.log")"
run "startup failure counts" 0 startup_failure '[]'
[ "$(writes)" = 1 ] || fail "startup_failure must open an issue"
run "green closes the open issue" 0 success "$open"
grep -q '^issue close 5 ' "$tmp/gh.log" && [ "$(writes)" = 1 ] || fail "green must close #5 only (not the windows one): $(cat "$tmp/gh.log")"
run "green, nothing open" 0 success "$open" # an issue of another workflow stays
run "green, no issue at all" 0 success '[]'
[ "$(writes)" = 0 ] || fail "green with no issue must write nothing"
for c in cancelled skipped neutral action_required; do
    run "$c touches nothing" 0 "$c" "$open" good5678
    [ "$(writes)" = 0 ] || fail "$c must not touch an issue"
done
export RUN_FAIL=1
run "gh run list fails" 1 failure '[]'
unset RUN_FAIL
[ "$(writes)" = 0 ] || fail "a failed lookup must not open a half-filled issue"
[ "$("$here/nightly-issue.sh" a b c >/dev/null 2>&1; echo $?)" = 2 ] || fail "wrong argument count must exit 2"

# Structure of the workflow that calls it.
wf="$here/../.github/workflows/nightly-signal.yml"
[ -f "$wf" ] || fail "no nightly-signal.yml"
listed=$(sed -n '/^    workflows:/,/^    types:/p' "$wf" | sed -n 's/^ *- *\[\?\([a-z-]*\).*/\1/p')
[ -n "$listed" ] || fail "nightly-signal.yml names no workflow"
for n in $listed; do
    grep -q "^name: $n\$" "$here/../.github/workflows/"*.yml || fail "nightly-signal.yml names '$n', which no workflow has"
done
# covered otherwise: ci (its nightly jobs have their own issue: ci/advisories-issue.sh), quality (a standing
# report that never fails), cache-gc (housekeeping, health.sh watches it), the signal itself
covered=" ci quality cache-gc nightly-signal "
for f in "$here/../.github/workflows/"*.yml; do
    grep -q '^  schedule:' "$f" || continue
    n=$(sed -n 's/^name: //p' "$f" | head -1)
    case "$covered" in *" $n "*) continue ;; esac
    printf '%s\n' "$listed" | grep -qx "$n" || fail "workflow '$n' runs on a schedule but nightly-signal.yml does not watch it"
done
echo "nightly-issue-test: ok"
