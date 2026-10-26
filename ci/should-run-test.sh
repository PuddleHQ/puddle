#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/should-run.sh with a fake `gh` that answers from a table of green runs
# (event:sha lines in $FAKE_RUNS) and records each call: the nightly gate with and without
# --events, and the develop push gate (--queue-tested) with every condition it checks, API errors
# included. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/should-run.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/should-run-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "should-run-test: FAIL: $*" >&2; fail=1; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# fake gh run list: counts lines "<event>:<sha>" of $FAKE_RUNS matching --commit and, if given, --event.
echo "$*" >>"$FAKE_LOG"
[ "${FAKE_FAIL:-}" = 1 ] && exit 1
sha="" event=""
while [ "$#" -gt 0 ]; do
    case $1 in
    --commit) sha=$2; shift ;;
    --event) event=$2; shift ;;
    esac
    shift
done
n=$(grep -c "^${event:-[a-z_]*}:$sha$" "$FAKE_RUNS")
[ "$n" -gt 0 ] && echo 1 || echo 0
GH
chmod +x "$tmp/bin/gh"

# run <want output line> <label> <VAR=value...> -- <args...>
run() {
    local want=$1 label=$2 vars=() rc=0
    shift 2
    while [ "$1" != -- ]; do vars+=("$1"); shift; done
    shift
    : >"$tmp/out"
    : >"$tmp/log"
    env -u PUDDLE_QUEUE_SKIP_PUSH -u GITHUB_REF PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r GITHUB_SHA=c1 \
        GITHUB_OUTPUT="$tmp/out" FAKE_LOG="$tmp/log" FAKE_RUNS="$tmp/runs" "${vars[@]}" \
        "$script" "$@" >/dev/null 2>&1 || rc=$?
    if [ "$want" = "exit 2" ]; then
        [ "$rc" = 2 ] || bad "$label: exit $rc, want 2"
        return
    fi
    [ "$rc" = 0 ] || bad "$label: exit $rc"
    [ "$(cat "$tmp/out")" = "$want" ] || bad "$label: output '$(cat "$tmp/out")', want '$want'"
}

printf 'merge_group:c1\npull_request:c9\n' >"$tmp/runs"
# Nightly gate.
run run=true "push never skips" GITHUB_EVENT_NAME=push -- ci.yml
run run=true "dispatch never skips" GITHUB_EVENT_NAME=workflow_dispatch -- windows.yml
run run=false "nightly, a green merge queue run counts" GITHUB_EVENT_NAME=schedule -- vm-linux.yml
run run=true "nightly with --events, a merge queue run does not count" GITHUB_EVENT_NAME=schedule -- windows.yml --events schedule,workflow_dispatch
{ grep -q -- '--event schedule' "$tmp/log" && grep -q -- '--event workflow_dispatch' "$tmp/log"; } || bad "--events did not ask for each event"
printf 'merge_group:c1\nworkflow_dispatch:c1\n' >"$tmp/runs"
run run=false "nightly with --events, a green dispatch counts" GITHUB_EVENT_NAME=schedule -- windows.yml --events schedule,workflow_dispatch
printf 'schedule:c1\n' >"$tmp/runs"
run run=false "nightly with --events, a green nightly counts" GITHUB_EVENT_NAME=schedule -- windows.yml --events schedule,workflow_dispatch
printf 'schedule:c2\n' >"$tmp/runs"
run run=true "nightly, a green run on another commit" GITHUB_EVENT_NAME=schedule -- vm-linux.yml
run run=true "nightly with --events, nothing green" GITHUB_EVENT_NAME=schedule -- windows.yml --events schedule
run "exit 2" "--events without a list" GITHUB_EVENT_NAME=schedule -- windows.yml --events
run "exit 2" "an unknown flag" GITHUB_EVENT_NAME=schedule -- windows.yml --bogus

# Develop push gate.
PUSH=(GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/develop PUDDLE_QUEUE_SKIP_PUSH=on)
printf 'merge_group:c1\n' >"$tmp/runs"
run tested=true "green merge queue run on this commit" "${PUSH[@]}" -- ci.yml --queue-tested
grep -q -- '--event merge_group' "$tmp/log" || bad "--queue-tested did not filter on merge_group runs"
grep -q -- '--status success' "$tmp/log" || bad "--queue-tested did not filter on green runs"
grep -q -- '--workflow ci.yml' "$tmp/log" || bad "--queue-tested did not filter on the workflow"
run tested=false "API error" "${PUSH[@]}" FAKE_FAIL=1 -- ci.yml --queue-tested
run tested=false "variable unset" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/develop -- ci.yml --queue-tested
run tested=false "variable not exactly on" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/develop PUDDLE_QUEUE_SKIP_PUSH=ON -- ci.yml --queue-tested
run tested=false "push to main" GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main PUDDLE_QUEUE_SKIP_PUSH=on -- ci.yml --queue-tested
run tested=false "a dispatch on develop" GITHUB_EVENT_NAME=workflow_dispatch GITHUB_REF=refs/heads/develop PUDDLE_QUEUE_SKIP_PUSH=on -- ci.yml --queue-tested
run tested=false "a nightly on develop" GITHUB_EVENT_NAME=schedule GITHUB_REF=refs/heads/develop PUDDLE_QUEUE_SKIP_PUSH=on -- ci.yml --queue-tested
printf 'merge_group:c2\npush:c1\nschedule:c1\npull_request:c1\n' >"$tmp/runs"
run tested=false "green runs of other events only" "${PUSH[@]}" -- ci.yml --queue-tested
run "exit 2" "--queue-tested with more arguments" "${PUSH[@]}" -- ci.yml --queue-tested x

if [ "$fail" = 0 ]; then
    echo "should-run-test: ok"
else
    exit 1
fi
