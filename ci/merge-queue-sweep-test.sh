#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/merge-queue-sweep.sh with a fake `gh` serving a GraphQL answer ($FAKE_PRS) and recording each call:
# which PRs it acts on (armed, CLEAN, not queued, last check older than SWEEP_MIN) and the fallback when the direct
# merge request fails. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/merge-queue-sweep.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/merge-queue-sweep-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "merge-queue-sweep-test: FAIL: $*" >&2; fail=1; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_LOG"
case "$* " in
*" -f query=query(\$id: ID!)"*)
    # read back one PR: in the queue only after the last change was one that works
    last=$(grep -E '^(api graphql .*enqueuePullRequest|pr merge .* --auto)' "$FAKE_LOG" | tail -n 1)
    case $last in
    *enqueuePullRequest*) [ "${FAKE_ENQUEUE_NOOP:-}${FAKE_ENQUEUE_FAIL:-}" != "" ] && echo false || echo true ;;
    *--auto*) [ "${FAKE_REARM_NOOP:-}" = 1 ] && echo false || echo true ;;
    *) echo false ;;
    esac ;;
*" -f query=mutation("*) [ "${FAKE_ENQUEUE_FAIL:-}" = 1 ] && exit 1 ;;
"api graphql"*) [ "${FAKE_READ_FAIL:-}" = 1 ] && exit 1; cat "$FAKE_PRS" ;;
"pr merge"*) [ "${FAKE_REARM_FAIL:-}" = 1 ] && case " $* " in *" --disable-auto "*) exit 1 ;; esac ;;
esac
exit 0
GH
chmod +x "$tmp/bin/gh"

now=1800000000
iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }
# pr <number> <armed 1|0> <queued 1|0> <draft> <state> <minutes since last check>
pr() {
    jq -n --argjson n "$1" --argjson a "$2" --argjson q "$3" --argjson d "$4" --arg s "$5" --arg t "$(iso $((now - $6 * 60)))" \
        '{id: "id\($n)", number: $n, headRefOid: "sha\($n)", isDraft: $d, mergeStateStatus: $s,
          autoMergeRequest: (if $a == 1 then {enabledAt: $t} else null end), mergeQueueEntry: (if $q == 1 then {state: "QUEUED"} else null end),
          commits: {nodes: [{commit: {statusCheckRollup: {contexts: {nodes: [{__typename: "CheckRun", completedAt: $t}, {__typename: "StatusContext"}]}}}}]}}'
}
wrap() { jq -s '{data: {repository: {pullRequests: {nodes: .}}}}'; }

# the calls that change something, one per line: the enqueue mutation as "enqueue <id> <sha>", `gh pr merge` as is
calls() { grep -E 'mutation\(|^pr merge' "$tmp/log" | sed -E 's/^api graphql -F id=([^ ]+) -F sha=([^ ]+) .*/enqueue \1 \2/'; }

# run <label> <want exit> <want calls (newline separated, "" = none)> <extra env...>; the PRs are in $tmp/prs
run() {
    local label=$1 wantrc=$2 wantlog=$3 out rc
    shift 3
    : >"$tmp/log"
    out=$(env "$@" FAKE_PRS="$tmp/prs" FAKE_LOG="$tmp/log" SWEEP_NOW=$now SWEEP_VERIFY_WAIT=0 PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r "$script" 2>&1)
    rc=$?
    [ "$rc" = "$wantrc" ] || bad "$label: exit $rc, wanted $wantrc: $out"
    [ "$(calls)" = "$wantlog" ] || bad "$label: calls were: $(calls | tr '\n' '|') wanted: $(tr '\n' '|' <<<"$wantlog")"
}

e1='enqueue id1 sha1'
e7='enqueue id7 sha7'
r1=$'pr merge 1 -R o/r --disable-auto\npr merge 1 -R o/r --auto --squash --match-head-commit sha1'
r7=$'pr merge 7 -R o/r --disable-auto\npr merge 7 -R o/r --auto --squash --match-head-commit sha7'
{ pr 1 1 0 false CLEAN 30; pr 2 1 1 false CLEAN 30; pr 3 0 0 false CLEAN 30; pr 4 1 0 true CLEAN 30
  pr 5 1 0 false BLOCKED 30; pr 6 1 0 false CLEAN 3; pr 7 1 0 false CLEAN 11; } | wrap >"$tmp/prs"
run "only the armed, clean, unqueued, settled PRs are enqueued" 0 "$e1"$'\n'"$e7" FAKE_X=1
run "SWEEP_MIN widens the wait" 0 "$e1" SWEEP_MIN=20
run "the enqueue call fails: disable and re-arm" 0 "$e1"$'\n'"$r1"$'\n'"$e7"$'\n'"$r7" FAKE_ENQUEUE_FAIL=1
run "the enqueue call succeeds but the PR stays outside: re-arm" 0 "$e1"$'\n'"$r1"$'\n'"$e7"$'\n'"$r7" FAKE_ENQUEUE_NOOP=1
run "re-arm does not queue it either: exit 1" 1 "$e1"$'\n'"$r1"$'\n'"$e7"$'\n'"$r7" FAKE_ENQUEUE_NOOP=1 FAKE_REARM_NOOP=1
run "disable fails: exit 1, no arm" 1 "$e1"$'\n'"pr merge 1 -R o/r --disable-auto"$'\n'"$e7"$'\n'"pr merge 7 -R o/r --disable-auto" FAKE_ENQUEUE_NOOP=1 FAKE_REARM_FAIL=1
run "unreadable GitHub: exit 1, nothing changed" 1 "" FAKE_READ_FAIL=1
{ pr 8 1 1 false CLEAN 30; pr 9 0 0 false CLEAN 30; } | wrap >"$tmp/prs"
run "nothing stuck" 0 "" FAKE_X=1
echo '{"data": {}}' >"$tmp/prs"
run "unexpected answer: exit 1" 1 "" FAKE_X=1

if [ "$fail" = 0 ]; then
    echo "merge-queue-sweep-test: ok"
else
    exit 1
fi
