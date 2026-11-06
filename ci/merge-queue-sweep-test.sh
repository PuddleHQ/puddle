#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
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
case "$1 $2" in
"api graphql") [ "${FAKE_READ_FAIL:-}" = 1 ] && exit 1; cat "$FAKE_PRS" ;;
"pr merge")
    case " $* " in
    *" --disable-auto "*) [ "${FAKE_REARM_FAIL:-}" = 1 ] && exit 1 ;;
    *" --auto "*) ;;
    *) [ "${FAKE_DIRECT_FAIL:-}" = 1 ] && exit 1 ;;
    esac ;;
esac
exit 0
GH
chmod +x "$tmp/bin/gh"

now=1800000000
iso() { date -u -d "@$1" +%Y-%m-%dT%H:%M:%SZ; }
# pr <number> <armed 1|0> <queued 1|0> <draft> <state> <minutes since last check>
pr() {
    jq -n --argjson n "$1" --argjson a "$2" --argjson q "$3" --argjson d "$4" --arg s "$5" --arg t "$(iso $((now - $6 * 60)))" \
        '{number: $n, headRefOid: "sha\($n)", isDraft: $d, mergeStateStatus: $s,
          autoMergeRequest: (if $a == 1 then {enabledAt: $t} else null end), mergeQueueEntry: (if $q == 1 then {state: "QUEUED"} else null end),
          commits: {nodes: [{commit: {statusCheckRollup: {contexts: {nodes: [{__typename: "CheckRun", completedAt: $t}, {__typename: "StatusContext"}]}}}}]}}'
}
wrap() { jq -s '{data: {repository: {pullRequests: {nodes: .}}}}'; }

# run <label> <want exit> <want log lines (sorted, newline separated, "" = none)> <extra env...>; the PRs are in $tmp/prs
run() {
    local label=$1 wantrc=$2 wantlog=$3 out rc
    shift 3
    : >"$tmp/log"
    out=$(env "$@" FAKE_PRS="$tmp/prs" FAKE_LOG="$tmp/log" SWEEP_NOW=$now PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r "$script" 2>&1)
    rc=$?
    [ "$rc" = "$wantrc" ] || bad "$label: exit $rc, wanted $wantrc: $out"
    [ "$(grep '^pr merge' "$tmp/log")" = "$wantlog" ] || bad "$label: calls were: $(grep '^pr merge' "$tmp/log" | tr '\n' '|') wanted: $(tr '\n' '|' <<<"$wantlog")"
}

{ pr 1 1 0 false CLEAN 30; pr 2 1 1 false CLEAN 30; pr 3 0 0 false CLEAN 30; pr 4 1 0 true CLEAN 30
  pr 5 1 0 false BLOCKED 30; pr 6 1 0 false CLEAN 3; pr 7 1 0 false CLEAN 11; } | wrap >"$tmp/prs"
run "only the armed, clean, unqueued, settled PRs" 0 $'pr merge 1 -R o/r --squash --match-head-commit sha1\npr merge 7 -R o/r --squash --match-head-commit sha7' FAKE_X=1
run "SWEEP_MIN widens the wait" 0 $'pr merge 1 -R o/r --squash --match-head-commit sha1' SWEEP_MIN=20
run "direct request fails: disable and re-arm" 0 $'pr merge 1 -R o/r --squash --match-head-commit sha1\npr merge 1 -R o/r --disable-auto\npr merge 1 -R o/r --auto --squash --match-head-commit sha1\npr merge 7 -R o/r --squash --match-head-commit sha7\npr merge 7 -R o/r --disable-auto\npr merge 7 -R o/r --auto --squash --match-head-commit sha7' FAKE_DIRECT_FAIL=1
run "both fail: exit 1" 1 $'pr merge 1 -R o/r --squash --match-head-commit sha1\npr merge 1 -R o/r --disable-auto\npr merge 7 -R o/r --squash --match-head-commit sha7\npr merge 7 -R o/r --disable-auto' FAKE_DIRECT_FAIL=1 FAKE_REARM_FAIL=1
run "unreadable GitHub: exit 1, nothing merged" 1 "" FAKE_READ_FAIL=1
{ pr 8 1 1 false CLEAN 30; pr 9 0 0 false CLEAN 30; } | wrap >"$tmp/prs"
run "nothing stuck" 0 "" FAKE_X=1
echo '{"data": {}}' >"$tmp/prs"
run "unexpected answer: exit 1" 1 "" FAKE_X=1

if [ "$fail" = 0 ]; then
    echo "merge-queue-sweep-test: ok"
else
    exit 1
fi
