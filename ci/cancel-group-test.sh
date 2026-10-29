#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/cancel-group.sh with a fake `gh` over a table of runs ("<branch> <id> <status>" lines in
# $FAKE_RUNS). `gh run list` answers the non-completed runs of --branch (what the real --jq prints); `gh run
# cancel` logs the id and marks the run completed (a cancelled run is completed), or fails for the ids in $FAKE_REFUSE. Prints ok only when
# every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/cancel-group.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/cancel-group-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "cancel-group-test: FAIL: $*" >&2; fail=1; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_LOG"
[ "${FAKE_FAIL:-}" = 1 ] && exit 1
case "$1 $2" in
"run list")
    branch=""
    while [ "$#" -gt 0 ]; do [ "$1" = --branch ] && branch=$2; shift; done
    awk -v b="$branch" '$1 == b && $3 != "completed" { print $2 }' "$FAKE_RUNS"
    ;;
"run cancel")
    case " ${FAKE_REFUSE:-} " in *" $3 "*) exit 1 ;; esac
    awk -v id="$3" '$2 == id { $3 = "completed" } { print }' "$FAKE_RUNS" >"$FAKE_RUNS.new" && mv "$FAKE_RUNS.new" "$FAKE_RUNS"
    ;;
*) exit 1 ;;
esac
GH
chmod +x "$tmp/bin/gh"

G=gh-readonly-queue/develop/pr-7-aaa
OTHER=gh-readonly-queue/develop/pr-8-bbb
# Runs of group G: own run 101 (in progress), 102 queued, 103 completed green, 104 in progress; group OTHER: 201.
table() {
    printf '%s 101 in_progress\n%s 102 queued\n%s 103 completed\n%s 104 in_progress\n%s 201 in_progress\n' "$G" "$G" "$G" "$G" "$OTHER" >"$tmp/runs"
}

# call <args...>: runs the script on a fresh log, with the fake gh and the table in place; prints the cancelled ids.
call() {
    : >"$tmp/log"
    PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_LOG="$tmp/log" FAKE_RUNS="$tmp/runs" "$script" "$@" >/dev/null 2>&1
    echo $? >"$tmp/rc"
    grep '^run cancel ' "$tmp/log" | awk '{print $3}' | tr '\n' ' ' | sed 's/ $//'
}

expect() { # <label> <want cancelled ids> <got>
    [ "$(cat "$tmp/rc")" = 0 ] || bad "$1: exit $(cat "$tmp/rc"), want 0"
    [ "$3" = "$2" ] || bad "$1: cancelled '$3', want '$2'"
}

table
got=$(call merge_group failure "refs/heads/$G" 101)
expect "first failure cancels the group, own run last" "102 104 101" "$got"
grep -q -- '--branch gh-readonly-queue/develop/pr-7-aaa' "$tmp/log" || bad "the listing is not scoped to the group's branch"
grep -qE '^run cancel 201' "$tmp/log" && bad "cancelled a run of another group"
grep -qE '^run cancel 103' "$tmp/log" && bad "cancelled a completed run"

got=$(call merge_group failure "refs/heads/$G" 104)
expect "second failed job, run again: nothing left to cancel" "" "$got"
grep -qx "$OTHER 201 in_progress" "$tmp/runs" || bad "touched the other group's run"

table
got=$(call merge_group failure "refs/heads/$G" 104)
expect "own run is whichever run id is passed" "101 102 104" "$got"

for st in skipped cancelled success; do
    table
    got=$(call merge_group "$st" "refs/heads/$G" 101)
    expect "job status $st does nothing" "" "$got"
done

table
for ev in pull_request push workflow_dispatch schedule ""; do
    got=$(call "$ev" failure "refs/heads/$G" 101)
    expect "event '$ev' does nothing" "" "$got"
done
got=$(call merge_group failure refs/heads/develop 101)
expect "a develop ref is not a queue branch" "" "$got"
got=$(call merge_group failure refs/pull/7/merge 101)
expect "a pull request ref is not a queue branch" "" "$got"
got=$(call merge_group failure refs/heads/gh-readonly-queue/ 101)
expect "an empty queue branch name does nothing" "" "$got"

# A green group: every run completed, a failed job elsewhere cannot exist, but the call stays a no-op.
printf '%s 101 completed\n%s 102 completed\n' "$G" "$G" >"$tmp/runs"
got=$(call merge_group failure "refs/heads/$G" 101)
expect "all runs completed" "" "$got"

# A run that cannot be cancelled (finished meanwhile): the others are still cancelled, exit stays 0.
table
: >"$tmp/log"
PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_LOG="$tmp/log" FAKE_RUNS="$tmp/runs" FAKE_REFUSE=102 "$script" merge_group failure "refs/heads/$G" 101 >/dev/null 2>&1
rc=$?
[ "$rc" = 0 ] || bad "refused cancel: exit $rc"
grep -q '^run cancel 104' "$tmp/log" || bad "a refused cancel stopped run 104"
grep -q '^run cancel 101' "$tmp/log" || bad "a refused cancel stopped the own run"

# gh failing to list: exit 0, nothing cancelled.
table
: >"$tmp/log"
PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_LOG="$tmp/log" FAKE_RUNS="$tmp/runs" FAKE_FAIL=1 "$script" merge_group failure "refs/heads/$G" 101 >/dev/null 2>&1 || bad "list failure: nonzero exit"
grep -q '^run cancel' "$tmp/log" && bad "cancelled after a failed listing"

# DRY_RUN lists and cancels nothing.
table
: >"$tmp/log"
out=$(PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_LOG="$tmp/log" FAKE_RUNS="$tmp/runs" DRY_RUN=1 "$script" merge_group failure "refs/heads/$G" 101 2>&1)
grep -q '^run cancel' "$tmp/log" && bad "DRY_RUN cancelled"
[ "$(printf '%s\n' "$out" | grep -c 'would cancel')" = 3 ] || bad "DRY_RUN did not name the 3 active runs: $out"

[ "$fail" = 0 ] && echo ok
exit "$fail"
