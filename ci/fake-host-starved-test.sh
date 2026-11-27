#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/fake-host-starved.sh with a stub test binary that holds a host.lock open (and lends a copy of the
# descriptor to a child, as a forked process would), fails on chosen runs (STUB_FAIL_ON, a list), each with its own test name, and prints the refusal text of a held data
# folder. Checks: the loop runs every run, every failing run's log and the holders (the stub and its child) are kept,
# a green loop exits 0 and leaves no failure files, and no busy loop or sampler outlives the script. Not seen: the real
# fake_host binary and GitHub.
set -euo pipefail
command -v taskset >/dev/null || { echo "fake-host-starved-test: skipped, taskset is Linux only (the job runs on ubuntu)"; exit 0; }
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fake-host-starved-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
# shellcheck disable=SC1091 # the helper sits in scripts/
. "$(dirname "$0")/../scripts/test-fail.sh"

# The stub counts its runs in $STUB_DIR/count and fails on run $STUB_FAIL_ON.
cat >"$tmp/fake_host_stub" <<'STUB'
#!/usr/bin/env bash
# The loop's second invocation of a run is the churn tests alone: it is not a run of its own.
if [ "$1" = "churn::" ]; then
    echo "stub churn args: $*" >>"$STUB_DIR/churn.args"
    echo "test result: ok"
    exit 0
fi
[ "$*" = "--skip churn::" ] || { echo "stub: first invocation lacks --skip churn:: ($*)"; exit 3; }
n=$(($(cat "$STUB_DIR/count" 2>/dev/null || echo 0) + 1))
echo "$n" >"$STUB_DIR/count"
echo $$ >>"$STUB_DIR/stub.pids"
mkdir -p "$STUB_DIR/data"
exec 3>"$STUB_DIR/data/host.lock"
sleep 30 &   # a child that inherited descriptor 3, as a forked copy does
child=$!
# Stay alive until the sampler has recorded the stub and the child: a sampler that is starved (a loaded machine) must
# not decide whether the child was seen. Bounded, so a sampler that never records fails the test instead of hanging it.
for _ in $(seq 300); do
    grep -q "^$$ " "$STUB_HOLDERS" 2>/dev/null && grep -q "^$child " "$STUB_HOLDERS" 2>/dev/null && break
    sleep 0.1
done
kill "$child" 2>/dev/null
case " ${STUB_FAIL_ON:-0} " in *" $n "*)
    echo "test race_of_run_$n ... FAILED"
    echo "thread 'x' panicked at fake_host.rs:1:1: DataFolder(Held { dir: 'x', holder: Some($$) })"
    exit 101 ;;
esac
echo "test result: ok"
STUB
chmod +x "$tmp/fake_host_stub"

cpu=$(taskset -cp $$ | sed 's/.*: //; s/[-,].*//')
run() { # run <out> <fail-on> ; prints the script's output, returns its status
    rm -rf "$tmp/state"
    mkdir -p "$tmp/state"
    STUB_DIR="$tmp/state" STUB_HOLDERS="$1/current-holders.txt" STUB_FAIL_ON="$2" FAKE_HOST_BIN="$tmp/fake_host_stub" FAKE_HOST_DIR="$tmp" FAKE_HOST_OUT="$1" \
        FAKE_HOST_RUNS=3 FAKE_HOST_CPUS="$cpu" FAKE_HOST_BUSY=2 "$here/fake-host-starved.sh"
}

rc=0
run "$tmp/out-red" 2 >"$tmp/red.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "a failing run must exit 1, got $rc: $(cat "$tmp/red.txt")"
[ "$(cat "$tmp/state/count")" = 3 ] || fail "every run must happen after a failure"
grep -q '^run 2 of 3: FAILED' "$tmp/red.txt" || fail "the failing run is named: $(cat "$tmp/red.txt")"
grep -q 'holder: Some(' "$tmp/out-red/failure-2.log" || fail "failure-2.log lacks the refusal"
grep -q 'host.lock holder named by the refusal: holder: Some(' "$tmp/red.txt" || fail "the refusal's holder is printed"
stub_pid=$(sed -n 2p "$tmp/state/stub.pids")
holders=$tmp/out-red/failure-2-holders.txt
grep -q "^$stub_pid " "$holders" || fail "the stub ($stub_pid) must be a sampled holder: $(cat "$holders")"
[ "$(wc -l <"$holders")" -ge 2 ] || fail "the child holding a copy must be sampled too: $(cat "$holders")"
grep -q "^[0-9]* $stub_pid " "$holders" || fail "the copy must show the stub as its parent: $(cat "$holders")"
grep -q -- '--test-threads=1' "$tmp/state/churn.args" || fail "the churn tests run alone, one thread: $(cat "$tmp/state/churn.args")"
grep -q '^FAILED=1$' "$tmp/out-red/result.env" || fail "result.env: $(cat "$tmp/out-red/result.env")"
grep -q '^FIRST_FAILED_RUN=2$' "$tmp/out-red/result.env" || fail "result.env names the first failed run"

# two failing runs: both logs are kept and the summary names each run's own failing test
rc=0
summary=$tmp/summary.md
GITHUB_STEP_SUMMARY=$summary run "$tmp/out-two" "1 3" >"$tmp/two.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "two failing runs must exit 1, got $rc: $(cat "$tmp/two.txt")"
for r in 1 3; do
    grep -q "test race_of_run_$r \.\.\. FAILED" "$tmp/out-two/failure-$r.log" || fail "failure-$r.log is missing or lacks its test"
    [ -s "$tmp/out-two/failure-$r-holders.txt" ] || fail "failure-$r-holders.txt is missing"
    grep -q "^- run $r (exit 101): race_of_run_$r" "$summary" || fail "the job summary lacks run $r: $(cat "$summary")"
done
[ ! -e "$tmp/out-two/failure-2.log" ] || fail "a passing run keeps no log"
grep -q '^FAILED=2$' "$tmp/out-two/result.env" || fail "result.env FAILED"
grep -q '^FAILED_RUNS="1 3"$' "$tmp/out-two/result.env" || fail "result.env FAILED_RUNS: $(cat "$tmp/out-two/result.env")"
grep -q '^FIRST_FAILED_RUN=1$' "$tmp/out-two/result.env" || fail "result.env names the first failed run"

# a green loop
run "$tmp/out-green" 0 >"$tmp/green.txt" 2>&1 || fail "a green loop must exit 0: $(cat "$tmp/green.txt")"
[ ! -e "$tmp/out-green/failure-1.log" ] || fail "a green loop leaves no failure log"
grep -q '^FAILED=0$' "$tmp/out-green/result.env" || fail "green result.env"

# nothing leaked: no busy loop (`while :; do :; done` pinned by this script) and no sampler is left
sleep 1
# Only this copy's loops are named (the script puts its output folder on each loop's command line), so a loop of
# another run on the same machine cannot fail this check.
! pgrep -f "fake-host-busy $tmp/" >/dev/null || fail "a busy loop outlived the script"
! pgrep -f "sample_holders $tmp" >/dev/null || fail "a sampler outlived the script"

# A sampler that wakes far less often than the stub lives (a starved machine) must still see the child holding a copy.
FAKE_HOST_SAMPLE_S=0.5 run "$tmp/out-slow" 2 >"$tmp/slow.txt" 2>&1 || true
[ "$(wc -l <"$tmp/out-slow/failure-2-holders.txt")" -ge 2 ] || fail "a slow sampler missed the child: $(cat "$tmp/out-slow/failure-2-holders.txt")"

# Two copies at once on one machine, as parallel checks are: each must pass although the other's loops are running.
if [ -z "${FAKE_HOST_STARVED_TEST_COPY:-}" ]; then
    FAKE_HOST_STARVED_TEST_COPY=1 "$0" >"$tmp/copy-a.txt" 2>&1 &
    pid_a=$!
    FAKE_HOST_STARVED_TEST_COPY=1 "$0" >"$tmp/copy-b.txt" 2>&1 &
    pid_b=$!
    wait "$pid_a" || fail "a copy run beside another failed: $(cat "$tmp/copy-a.txt")"
    wait "$pid_b" || fail "a copy run beside another failed: $(cat "$tmp/copy-b.txt")"
fi
echo "fake-host-starved-test: ok"
