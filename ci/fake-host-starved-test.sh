#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/fake-host-starved.sh with a stub test binary that holds a host.lock open (and lends a copy of the
# descriptor to a child, as a forked process would), fails on a chosen run and prints the refusal text of a held data
# folder. Checks: the loop runs every run, the first failure's log and the holders (the stub and its child) are kept,
# a green loop exits 0 and leaves no failure files, and no busy loop or sampler outlives the script. Not seen: the real
# fake_host binary and GitHub.
set -euo pipefail
command -v taskset >/dev/null || { echo "fake-host-starved-test: skipped, taskset is Linux only (the job runs on ubuntu)"; exit 0; }
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fake-host-starved-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fake-host-starved-test: $*" >&2; exit 1; }

# The stub counts its runs in $STUB_DIR/count and fails on run $STUB_FAIL_ON.
cat >"$tmp/fake_host_stub" <<'STUB'
#!/usr/bin/env bash
n=$(($(cat "$STUB_DIR/count" 2>/dev/null || echo 0) + 1))
echo "$n" >"$STUB_DIR/count"
echo $$ >>"$STUB_DIR/stub.pids"
mkdir -p "$STUB_DIR/data"
exec 3>"$STUB_DIR/data/host.lock"
sleep 0.4 &   # a child that inherited descriptor 3, as a forked copy does
sleep 0.4
if [ "$n" = "${STUB_FAIL_ON:-0}" ]; then
    echo "test a_stopped_host_restarts ... FAILED"
    echo "thread 'x' panicked at fake_host.rs:1:1: DataFolder(Held { dir: 'x', holder: Some($$) })"
    exit 101
fi
echo "test result: ok"
STUB
chmod +x "$tmp/fake_host_stub"

cpu=$(taskset -cp $$ | sed 's/.*: //; s/[-,].*//')
run() { # run <out> <fail-on> ; prints the script's output, returns its status
    rm -rf "$tmp/state"
    mkdir -p "$tmp/state"
    STUB_DIR="$tmp/state" STUB_FAIL_ON="$2" FAKE_HOST_BIN="$tmp/fake_host_stub" FAKE_HOST_DIR="$tmp" FAKE_HOST_OUT="$1" \
        FAKE_HOST_RUNS=3 FAKE_HOST_CPUS="$cpu" FAKE_HOST_BUSY=2 "$here/fake-host-starved.sh"
}

# count only this script's process group: another worker's stress loop on the same machine must not count
own_group=$(ps -o pgid= -p $$ | tr -d ' ')
busy_loops() { pgrep -f -g "$own_group" 'while :; do :; done' | sort || true; }
busy_before=$(busy_loops)
rc=0
run "$tmp/out-red" 2 >"$tmp/red.txt" 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "a failing run must exit 1, got $rc: $(cat "$tmp/red.txt")"
[ "$(cat "$tmp/state/count")" = 3 ] || fail "every run must happen after a failure"
grep -q '^run 2 of 3: FAILED' "$tmp/red.txt" || fail "the failing run is named: $(cat "$tmp/red.txt")"
grep -q 'holder: Some(' "$tmp/out-red/first-failure.log" || fail "first-failure.log lacks the refusal"
grep -q 'host.lock holder named by the refusal: holder: Some(' "$tmp/red.txt" || fail "the refusal's holder is printed"
stub_pid=$(sed -n 2p "$tmp/state/stub.pids")
holders=$tmp/out-red/first-failure-holders.txt
grep -q "^$stub_pid " "$holders" || fail "the stub ($stub_pid) must be a sampled holder: $(cat "$holders")"
[ "$(wc -l <"$holders")" -ge 2 ] || fail "the child holding a copy must be sampled too: $(cat "$holders")"
grep -q "^[0-9]* $stub_pid " "$holders" || fail "the copy must show the stub as its parent: $(cat "$holders")"
grep -q '^FAILED=1$' "$tmp/out-red/result.env" || fail "result.env: $(cat "$tmp/out-red/result.env")"
grep -q '^FIRST_FAILED_RUN=2$' "$tmp/out-red/result.env" || fail "result.env names the first failed run"

# a green loop
run "$tmp/out-green" 0 >"$tmp/green.txt" 2>&1 || fail "a green loop must exit 0: $(cat "$tmp/green.txt")"
[ ! -e "$tmp/out-green/first-failure.log" ] || fail "a green loop leaves no failure log"
grep -q '^FAILED=0$' "$tmp/out-green/result.env" || fail "green result.env"

# nothing leaked: no busy loop (`while :; do :; done` pinned by this script) and no sampler is left
sleep 1
# only a loop that appeared during this run is a leak: an earlier test's loop can still be dying when this one starts
leaked=$(comm -13 <(echo "$busy_before") <(busy_loops))
[ -z "$leaked" ] || fail "a busy loop outlived the script: $leaked"
! pgrep -f "sample_holders $tmp" >/dev/null || fail "a sampler outlived the script"
echo "fake-host-starved-test: ok"
