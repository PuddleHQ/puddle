#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The nightly starved loop of the puddle-host fake_host suite (.github/workflows/fake-host-starved.yml). Runs the
# suite's test binary FAKE_HOST_RUNS times in a row, pinned to FAKE_HOST_CPUS beside FAKE_HOST_BUSY busy loops on the
# same CPUs, so a race that needs a starved machine fails here a day after it merged instead of in a merge queue.
#
# Why its own job and not the nextest runs of stress.yml: nextest runs every test in its own process, and a
# descriptor copied by fork() only exists when another test thread of the SAME process spawns a child (git, ssh, ...)
# while a data-folder lock is being released. That window is only there when the binary runs all its tests as threads of
# one process, so this script runs the binary itself, the way plain `cargo test` does.
#
# On a failing run it keeps the log of the FIRST failure (first-failure.log) and prints who held host.lock: the pid
# the refusal names, and the processes a sampler (pinned off the starved CPUs) saw with a host.lock open during that
# run, with their parent, so a forked copy of the test binary shows as a second holder whose parent is the first.
# Exits 1 when any run failed. Output goes to $FAKE_HOST_OUT (default target/fake-host-starved):
#   first-failure.log           the first failing run's log (the logs of passing runs are not kept)
#   first-failure-holders.txt   the processes that had a host.lock open during it: "pid ppid command"
#   result.env                  RUNS, FAILED, FIRST_FAILED_RUN, CPUS, BUSY
# Overrides: FAKE_HOST_RUNS (20)  FAKE_HOST_CPUS (0-1)  FAKE_HOST_BUSY (4)  FAKE_HOST_OUT  FAKE_HOST_BIN (a built test binary,
# skips the build; the self-test ci/fake-host-starved-test.sh uses it)  FAKE_HOST_DIR (the working directory of the runs,
# default crates/puddle-host)  FAKE_HOST_SAMPLE_S (sampler period, 0.02).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
runs=${FAKE_HOST_RUNS:-20}
cpus=${FAKE_HOST_CPUS:-0-1}
busy=${FAKE_HOST_BUSY:-4}
out=${FAKE_HOST_OUT:-$root/target/fake-host-starved}
period=${FAKE_HOST_SAMPLE_S:-0.02}
workdir=${FAKE_HOST_DIR:-$root/crates/puddle-host}
command -v taskset >/dev/null || { echo "fake-host-starved: taskset is Linux only" >&2; exit 2; }
mkdir -p "$out"
# A run that dies early must not leave the last run's result.
rm -f "$out/first-failure.log" "$out/first-failure-holders.txt" "$out/result.env"

bin=${FAKE_HOST_BIN:-}
if [ -z "$bin" ]; then
    # The build runs unpinned and unloaded; only the runs are starved.
    cd "$root"
    # puddle-settings/mise: the managed tools layer is a feature, off in a bare -p build, where every tools_* and mise
    # test of fake_host fails (as in ci/stress.sh).
    bin=$(cargo test -p puddle-host --features puddle-settings/mise --test fake_host --no-run --locked --message-format=json |
        jq -r 'select(.reason == "compiler-artifact" and .target.name == "fake_host" and .executable != null) | .executable' | tail -n 1)
    [ -n "$bin" ] && [ -x "$bin" ] || { echo "fake-host-starved: could not find the fake_host test binary" >&2; exit 2; }
fi

in_list() { # in_list <cpu> <list such as 0-1 or 0,2>
    local part IFS=,
    for part in $2; do
        case $part in
        *-*) { [ "$1" -ge "${part%-*}" ] && [ "$1" -le "${part#*-}" ]; } && return 0 ;;
        *) [ "$1" = "$part" ] && return 0 ;;
        esac
    done
    return 1
}
# The sampler runs on the CPUs the starved set does not use (on all of them when the machine has no others).
total=$(nproc --all)
other=""
for ((c = 0; c < total; c++)); do
    in_list "$c" "$cpus" || other="${other:+$other,}$c"
done
sampler_cpus=${other:-0-$((total - 1))}

busy_pids=()
sampler_pid=""
stop_busy() {
    local pid
    for pid in ${busy_pids[@]+"${busy_pids[@]}"}; do kill "$pid" 2>/dev/null || true; done
    busy_pids=()
}
stop_sampler() {
    [ -z "$sampler_pid" ] || kill "$sampler_pid" 2>/dev/null || true
    sampler_pid=""
}
trap 'stop_busy; stop_sampler' EXIT

# Appends "pid ppid command" for each process that has a host.lock open, once per pid, until killed.
sample_holders() { # sample_holders <file>
    local file=$1 fd pid seen=" " ppid cmd
    : >"$file"
    while :; do
        while IFS= read -r fd; do
            pid=${fd#/proc/}
            pid=${pid%%/*}
            case $seen in *" $pid "*) continue ;; esac
            seen="$seen$pid "
            ppid=$(awk '{print $4}' "/proc/$pid/stat" 2>/dev/null) || ppid="?"
            cmd=$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null) || cmd="?"
            echo "$pid ${ppid:-?} $cmd" >>"$file"
        done < <(find /proc/[0-9]*/fd -maxdepth 1 -lname '*/host.lock' 2>/dev/null)
        sleep "$period"
    done
}

n=0
while [ "$n" -lt "$busy" ]; do
    taskset -c "$cpus" sh -c 'while :; do :; done' fake-host-busy "$out" &
    busy_pids+=("$!")
    n=$((n + 1))
done

failed=0 first=0
log="$out/current.log" holders="$out/current-holders.txt"
for ((i = 1; i <= runs; i++)); do
    taskset -c "$sampler_cpus" bash -c "$(declare -f sample_holders); period=$period; sample_holders $holders" &
    sampler_pid=$!
    rc=0
    (cd "$workdir" && taskset -c "$cpus" "$bin" >"$log" 2>&1) || rc=$?
    stop_sampler
    if [ "$rc" = 0 ]; then
        echo "run $i of $runs: ok"
        continue
    fi
    failed=$((failed + 1))
    echo "run $i of $runs: FAILED (exit $rc)"
    [ "$first" = 0 ] || continue
    first=$i
    cp "$log" "$out/first-failure.log"
    cp "$holders" "$out/first-failure-holders.txt"
    echo "--- first failure: failing tests and panics"
    grep -E -A1 '^test .* FAILED|panicked at' "$out/first-failure.log" | head -n 30 || true
    named=$(grep -Eo 'holder: Some\([0-9]+\)' "$out/first-failure.log" | head -n 1 || true)
    echo "--- host.lock holder named by the refusal: ${named:-none (the failure is not a data-folder refusal)}"
    echo "--- processes seen with a host.lock open during that run (pid ppid command; a holder whose parent is also a holder is a forked copy):"
    if [ -s "$holders" ]; then cat "$holders"; else echo "(none sampled)"; fi
done
stop_busy

{
    echo "RUNS=$runs"
    echo "FAILED=$failed"
    echo "FIRST_FAILED_RUN=$first"
    echo "CPUS=$cpus"
    echo "BUSY=$busy"
} >"$out/result.env"
summary="fake_host starved loop: $failed of $runs runs failed (cpus $cpus, $busy busy loops)"
echo "$summary"
[ -z "${GITHUB_STEP_SUMMARY:-}" ] || echo "### $summary" >>"$GITHUB_STEP_SUMMARY"
[ "$failed" = 0 ]
