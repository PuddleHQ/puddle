#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/cancel-dissolved-groups.sh with a fake `gh`: the merge queue comes from $FAKE_QUEUE (GraphQL JSON), the
# runs from $FAKE_RUNS_JSON, and every `gh run cancel` is logged. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/cancel-dissolved-groups.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/cancel-dissolved-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "cancel-dissolved-groups-test: FAIL: $*" >&2; fail=1; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_LOG"
case "$1 $2" in
"api graphql") [ "${FAKE_QUEUE_FAIL:-}" = 1 ] && exit 1; cat "$FAKE_QUEUE" ;;
"run list") [ "${FAKE_LIST_FAIL:-}" = 1 ] && exit 1; cat "$FAKE_RUNS_JSON" ;;
"run cancel") case " ${FAKE_REFUSE:-} " in *" $3 "*) exit 1 ;; esac ;;
esac
exit 0
GH
chmod +x "$tmp/bin/gh"

now=1800000000
iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }
# run <id> <status> <branch> <head sha> <age minutes>
run_json() {
    jq -n --argjson id "$1" --arg s "$2" --arg b "$3" --arg h "$4" --arg t "$(iso $((now - $5 * 60)))" \
        '{databaseId: $id, status: $s, headBranch: $b, headSha: $h, createdAt: $t}'
}
# entry <pr> <group commit or empty>
entry() { jq -n --argjson n "$1" --arg c "$2" '{pullRequest: {number: $n}, headCommit: (if $c == "" then null else {oid: $c} end)}'; }
queue() { jq -s --argjson more "${MORE:-false}" '{data: {repository: {mergeQueue: {entries: {pageInfo: {hasNextPage: $more}, nodes: .}}}}}'; }

# check <label> <want cancelled ids, space separated> <extra env...>; queue in $tmp/q, runs in $tmp/r
check() {
    local label=$1 want=$2 got rc
    shift 2
    : >"$tmp/log"
    env "$@" FAKE_QUEUE="$tmp/q" FAKE_RUNS_JSON="$tmp/r" FAKE_LOG="$tmp/log" SWEEP_NOW=$now PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r "$script" >"$tmp/out" 2>&1
    rc=$?
    got=$(awk '$1 == "run" && $2 == "cancel" { print $3 }' "$tmp/log" | tr '\n' ' ' | sed 's/ $//')
    [ "$got" = "$want" ] || bad "$label: cancelled '$got', wanted '$want'"
    [ "$rc" = "${WANT_RC:-0}" ] || bad "$label: exit $rc, wanted ${WANT_RC:-0}: $(cat "$tmp/out")"
}

g=gh-readonly-queue/develop
{
    run_json 1 in_progress "$g/pr-10-aaa" live10 40          # in the queue: kept
    run_json 2 in_progress "$g/pr-11-bbb" gone11 40          # dissolved: cancelled
    run_json 3 queued "$g/pr-11-bbb" gone11 40               # a queued run of it: cancelled
    run_json 4 completed "$g/pr-12-ccc" gone12 40            # completed: left
    run_json 5 in_progress "$g/pr-13-ddd" fresh13 3          # too young: left
    run_json 6 in_progress "$g/pr-14-eee" gone14 40          # entry 14 has no group commit yet: left
    run_json 7 in_progress "$g/pr-10-old" old10 40           # an older group of an entry that was rebuilt: cancelled
    run_json 8 in_progress "$g/pr-15-fff" stacked15 40       # stacked group of a live entry: kept
    run_json 9 in_progress "feature/x" other 40              # not a queue branch: left
} | jq -s . >"$tmp/r"
{ entry 10 live10; entry 15 stacked15; entry 14 ""; } | queue >"$tmp/q"
check "dissolved groups only" "2 3 7"
check "SWEEP_GROUP_MIN widens the age" "2 3 7" SWEEP_GROUP_MIN=30
check "SWEEP_GROUP_MIN above every age cancels nothing" "" SWEEP_GROUP_MIN=60
check "DRY_RUN cancels nothing" "" DRY_RUN=1
grep -q 'would cancel run 2 ' "$tmp/out" || bad "DRY_RUN does not name run 2"
check "a refused cancel does not stop the rest" "2 3 7" FAKE_REFUSE=3

export MORE=true
{ entry 10 live10; } | queue >"$tmp/q"
check "a second queue page cancels nothing" ""
unset MORE
echo '{"data":{"repository":{"mergeQueue":null}}}' >"$tmp/q"
check "no merge queue cancels nothing" ""
{ entry 10 live10; } | queue >"$tmp/q"
WANT_RC=1 check "unreadable queue: exit 1, nothing cancelled" "" FAKE_QUEUE_FAIL=1
WANT_RC=1 check "unreadable run list: exit 1, nothing cancelled" "" FAKE_LIST_FAIL=1
echo '[]' >"$tmp/r"
check "no runs" ""
{ run_json 1 in_progress "$g/pr-10-aaa" live10 40; } | jq -s . >"$tmp/r"
echo '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}}' >"$tmp/q"
check "an empty queue dissolves every old group" "1"

[ "$fail" = 0 ] && echo "cancel-dissolved-groups-test: ok"
exit "$fail"
