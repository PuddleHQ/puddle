#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/vm-tests-report.sh. The `log` cases feed it nextest's own output (lines as nextest 0.9.146 prints
# them for a failing test, a test killed by the per-test limit, a run stopped by the run deadline, and a slow run
# that passes), the `run` cases a fake `gh` that answers the two API calls with canned JSON through the real
# `--jq` filter. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/vm-tests-report.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/vm-tests-report-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "vm-tests-report-test: FAIL: $*" >&2; fail=1; }
command -v jq >/dev/null || { echo "vm-tests-report-test: jq is required" >&2; exit 1; }

# report <log> [partition]: stdout in $tmp/out, the step summary in $tmp/sum
report() {
    : >"$tmp/sum"
    GITHUB_STEP_SUMMARY="$tmp/sum" RUNNER_TEMP="$tmp/rt" "$script" log "$1" "${2:-1}" >"$tmp/out" 2>&1
    echo $? >"$tmp/rc"
}
errors() { grep -c '^::error' "$tmp/out"; }
has() { grep -qF -- "$2" "$1" || bad "$3: '$2' missing from $(basename "$1"): $(cat "$1")"; }
hasnt() { ! grep -qF -- "$2" "$1" || bad "$3: '$2' must not be in $(basename "$1")"; }
mkdir "$tmp/rt"

# --- a failing test and a test killed by the per-test limit (real nextest output, final copy included)
cat >"$tmp/fail.log" <<'E'
────────────
 Nextest run ID 7d3384ee with nextest profile: vm
    Starting 4 tests across 1 binary
        FAIL [   0.002s] (1/4) nx vm_fails
  stdout ───

    running 1 test
    test vm_fails ... FAILED

        SLOW [>  1.000s] (───) nx vm_hangs
        SLOW [>  2.000s] (───) nx vm_hangs
 TERMINATING [>  3.000s] (───) nx vm_hangs
     TIMEOUT [   3.005s] (2/4) nx vm_hangs
  stdout ───

    running 1 test

    (test timed out)

        PASS [   0.003s] (3/4) nx vm_ok
────────────
     Summary [   6.015s] 4 tests run: 1 passed, 1 failed, 2 timed out, 0 skipped
        FAIL [   0.002s] (1/4) nx vm_fails
     TIMEOUT [   3.005s] (2/4) nx vm_hangs
error: test run failed
E
report "$tmp/fail.log" 3
[ "$(cat "$tmp/rc")" = 0 ] || bad "exit code $(cat "$tmp/rc"), want 0"
[ "$(errors)" = 2 ] || bad "two failed tests give two annotations, got $(errors): $(cat "$tmp/out")"
has "$tmp/out" '::error title=vm-linux partition 3: FAIL::nx vm_fails ended FAIL after 0.002s' "failed test is named"
has "$tmp/out" '::error title=vm-linux partition 3: TIMEOUT::nx vm_hangs ended TIMEOUT after 3.005s' "timed-out test is named"
hasnt "$tmp/out" 'DEADLINE' "no deadline in this run"
# shellcheck disable=SC2016 # why: the backticks are literal Markdown
has "$tmp/sum" '| TIMEOUT | 3.005 | `nx vm_hangs` |' "summary row"
has "$tmp/sum" 'vm-linux partition 3' "summary names the partition"

# --- the same lines in colour (CARGO_TERM_COLOR=always, as the workflow runs)
sed 's/^\( *\)\(FAIL\|TIMEOUT\|PASS\)\( .*\)$/\1\x1b[31;1m\2\x1b[0m\3/' "$tmp/fail.log" >"$tmp/color.log"
report "$tmp/color.log"
[ "$(errors)" = 2 ] || bad "coloured log: $(errors) annotations, want 2: $(cat "$tmp/out")"

# --- the run deadline stops the last test (the shape of the 2026-10-30 queue run: slow passes, one test cut)
cat >"$tmp/deadline.log" <<'E'
        PASS [  65.838s] (19/27) puddle-host::vm_git vm_git_a
        SLOW [>120.000s] (─────) puddle-vm-tests::vm_ca_trust vm_ca_trust_java
        PASS [ 224.377s] (24/27) puddle-vm-tests::vm_ca_trust vm_ca_trust_java
        SLOW [>120.000s] (─────) puddle-vm-tests::vm_dev_cert vm_dev_cert_dotnet
  Cancelling due to global timeout: 1 test still running
     SIGTERM [ 206.237s] (26/27) puddle-vm-tests::vm_dev_cert vm_dev_cert_dotnet
  stdout ───

    (test aborted with signal 15: SIGTERM)

────────────
     Summary [1680.068s] 26/27 tests run: 25 passed (4 slow), 1 failed, 802 skipped
        SLOW [ 224.377s] (24/27) puddle-vm-tests::vm_ca_trust vm_ca_trust_java
     SIGTERM [ 206.237s] (26/27) puddle-vm-tests::vm_dev_cert vm_dev_cert_dotnet
warning: 1/27 tests were not run due to global timeout
error: test run failed
E
echo 'runner speed: sha256 of 1 GiB 0.5s' >"$tmp/rt/runner-speed.txt"
report "$tmp/deadline.log"
[ "$(errors)" = 2 ] || bad "deadline: $(errors) annotations, want the stopped test and the deadline: $(cat "$tmp/out")"
has "$tmp/out" 'puddle-vm-tests::vm_dev_cert vm_dev_cert_dotnet ended SIGTERM after 206.237s' "the test running at the deadline is named"
has "$tmp/out" 'stopped at the run deadline::Cancelling due to global timeout: 1 test still running; warning: 1/27 tests were not run' "the deadline is named with the count not run"
hasnt "$tmp/out" 'vm_ca_trust_java' "a slow test that passed is not an error"
# shellcheck disable=SC2016 # why: the backticks are literal Markdown
has "$tmp/sum" '- 224.377s `puddle-vm-tests::vm_ca_trust vm_ca_trust_java`' "the slowest passing test is listed"
has "$tmp/sum" 'Runner speed before the tests: runner speed: sha256 of 1 GiB 0.5s' "the runner figures are appended"
rm -f "$tmp/rt/runner-speed.txt"

# --- no test failed (the job failed in another step): says so, no error annotation
cat >"$tmp/clean.log" <<'E'
        PASS [   0.003s] (1/1) nx vm_ok
────────────
     Summary [   0.5s] 1 test run: 1 passed, 0 skipped
E
report "$tmp/clean.log"
has "$tmp/out" 'no test failed::nextest reports no failed test; look at the failing step' "clean log is said, not blamed on a test"
printf '     Summary [   0.0s] 0 tests run: 0 passed, 831 skipped\nerror: no tests to run\n' >"$tmp/none.log"
report "$tmp/none.log" 3
has "$tmp/out" 'nextest reports no failed test (error: no tests to run)' "nextest's own error line is quoted"

# --- the log ends without a summary (nextest died): names the last line
printf '        PASS [   0.003s] (1/9) nx vm_ok\nsome last output\n' >"$tmp/nosum.log"
report "$tmp/nosum.log"
has "$tmp/out" 'no nextest summary::the log ends without a summary; last line: some last output' "no summary"

# --- no log at all, and an empty one
report "$tmp/missing.log" 2
has "$tmp/out" '::error title=vm-linux partition 2: no test log::' "missing log"
: >"$tmp/empty.log"
report "$tmp/empty.log" 2
has "$tmp/out" 'partition 2: no test log' "empty log"

# --- many failures: ten annotations (GitHub shows few), the summary keeps every row
for i in $(seq 1 14); do printf '        FAIL [   1.000s] (%s/14) nx vm_t%s\n' "$i" "$i"; done >"$tmp/many.log"
echo '     Summary [  14.0s] 14 tests run: 0 passed, 14 failed' >>"$tmp/many.log"
report "$tmp/many.log"
[ "$(errors)" = 10 ] || bad "many failures: $(errors) annotations, want 10"
[ "$(grep -c '^| FAIL ' "$tmp/sum")" = 14 ] || bad "many failures: summary rows $(grep -c '^| FAIL ' "$tmp/sum"), want 14"

# --- a test name with a workflow-command character does not break the annotation
printf '        FAIL [   1.000s] (1/1) nx vm_a%%b\n' >"$tmp/pct.log"
report "$tmp/pct.log"
has "$tmp/out" 'nx vm_a%25b ended FAIL' "percent is escaped"

# --- run mode: the result job reads back what the partitions named, through a fake gh
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# gh api <path> --jq <filter>: answers from canned JSON through the real filter.
path=$2 filter=""
while [ "$#" -gt 0 ]; do [ "$1" = --jq ] && filter=$2; shift; done
case $path in
repos/o/r/actions/runs/7/jobs*) jq -r "$filter" "$FAKE_DIR/jobs.json" ;;
repos/o/r/check-runs/11/annotations) jq -r "$filter" "$FAKE_DIR/ann11.json" ;;
repos/o/r/check-runs/12/annotations) jq -r "$filter" "$FAKE_DIR/ann12.json" ;;
repos/o/r/actions/jobs/11) jq -r "$filter" "$FAKE_DIR/job11.json" ;;
repos/o/r/actions/jobs/12) jq -r "$filter" "$FAKE_DIR/job12.json" ;;
*) exit 1 ;;
esac
GH
chmod +x "$tmp/bin/gh"
cat >"$tmp/jobs.json" <<'E'
{"jobs":[
 {"id":10,"name":"vm-linux build (test archive, guest agent)","conclusion":"success"},
 {"id":11,"name":"vm-linux tests (partition 1 of 4)","conclusion":"cancelled"},
 {"id":12,"name":"vm-linux tests (partition 2 of 4)","conclusion":"cancelled"},
 {"id":13,"name":"vm-linux tests (partition 3 of 4)","conclusion":"success"}]}
E
cat >"$tmp/ann11.json" <<'E'
[{"title":"","message":"The run was canceled by @github-actions[bot]."},
 {"title":"vm-linux partition 1: SIGTERM","message":"puddle-vm-tests::vm_dev_cert vm_dev_cert_dotnet ended SIGTERM after 206.237s"},
 {"title":"vm-linux partition 1: stopped at the run deadline","message":"Cancelling due to global timeout: 1 test still running"}]
E
echo '[{"title":"","message":"The run was canceled by @github-actions[bot]."}]' >"$tmp/ann12.json"
echo '{"steps":[{"name":"checkout","conclusion":"success"},{"name":"VM tests (tier K, partition 1 of 4)","conclusion":"failure"}]}' >"$tmp/job11.json"
echo '{"steps":[{"name":"VM tests (tier K, partition 2 of 4)","conclusion":"cancelled"}]}' >"$tmp/job12.json"
: >"$tmp/sum"
PATH="$tmp/bin:$PATH" FAKE_DIR="$tmp" GITHUB_STEP_SUMMARY="$tmp/sum" "$script" run o/r 7 >"$tmp/out" 2>&1
has "$tmp/out" 'vm-linux tests (partition 1 of 4): cancelled; failed step: VM tests (tier K, partition 1 of 4)' "run mode names the cancelled job and its failed step"
has "$tmp/out" 'vm-linux partition 1: SIGTERM: puddle-vm-tests::vm_dev_cert vm_dev_cert_dotnet ended SIGTERM after 206.237s' "run mode prints the named test"
has "$tmp/out" 'vm-linux partition 1: stopped at the run deadline' "run mode prints the deadline"
has "$tmp/out" 'vm-linux tests (partition 2 of 4): cancelled' "the second partition is listed"
has "$tmp/out" 'no test was named' "a partition without a name says so"
hasnt "$tmp/out" 'partition 3' "a green partition is not listed"
hasnt "$tmp/out" 'canceled by' "annotations of the platform are not repeated"
has "$tmp/sum" '**vm-linux tests (partition 1 of 4)**' "run mode writes the summary"

echo '{"jobs":[{"id":13,"name":"vm-linux tests (partition 3 of 4)","conclusion":"success"}]}' >"$tmp/jobs.json"
PATH="$tmp/bin:$PATH" FAKE_DIR="$tmp" GITHUB_STEP_SUMMARY="$tmp/sum" "$script" run o/r 7 >"$tmp/out" 2>&1
has "$tmp/out" 'no vm-linux test job failed or was cancelled in run 7' "an all-green run"
PATH="$tmp/bin:$PATH" FAKE_DIR="$tmp" "$script" run o/r 99 >"$tmp/out" 2>&1 || bad "an unanswered API call must not fail the step"
has "$tmp/out" 'could not list the jobs of run 99' "an unanswered API call is said"

[ "$fail" = 0 ] && echo ok
exit "$fail"
