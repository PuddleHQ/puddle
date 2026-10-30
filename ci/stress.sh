#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The nightly stress job's runner (.github/workflows/stress.yml). Runs one suite family many times
# on a starved machine, so a test that only fails under load fails here instead of in the merge queue,
# and leaves the result for ci/stress-yield.py. Self-test: ci/stress-test.sh.
#
#   ci/stress.sh rust         nextest --stress-count on the fast in-process suites (fake_host, the
#                             proxy suites, the agent's capture tests)
#   ci/stress.sh playwright   Playwright --repeat-each on chromium (CDP CPU throttle) and webkit
#   ci/stress.sh vitest       vitest with shuffled order and every test repeated
#
# The tools run pinned to STRESS_CPUS with STRESS_BUSY busy loops on the same CPUs. rust: one CPU and 6 loops,
# the level at which the slowloris flake fixed in 11403e5b fails 5 of 5 (2 CPUs with 2 loops did not see it, 5 of 5
# passed); playwright and vitest: 2 CPUs and 2 loops.
# The build comes first and runs unpinned. Output goes to $STRESS_OUT/<kind>/ (default target/stress):
#   report.*        the tool's own machine-readable report (junit.xml, playwright.json)
#   result.env      KIND, EXIT, WALL_S, REPORT, ARGS: what the tool did, for stress-yield.py
# The script exits with the tool's status, so a failure found is a red run (and so an issue, through
# nightly-signal.yml). Overrides, for the proof run and for local use:
#   STRESS_CPUS  STRESS_BUSY  STRESS_COUNT (20 rust, 3 playwright, 5 vitest)  STRESS_FILTER (a nextest
#   filter expression)  STRESS_PROJECTS (playwright, default "chromium webkit")  STRESS_THROTTLE (4)
#   STRESS_PARTITION (rust: m/n, one nth of the tests)  STRESS_ARGS (extra arguments for Playwright, e.g. spec files, to narrow a proof run)
set -euo pipefail

kind=${1:?usage: stress.sh <rust|playwright|vitest>}
root=$(cd "$(dirname "$0")/.." && pwd)
out=${STRESS_OUT:-$root/target/stress}/$kind
cpus="" busy=""   # set per kind below
mkdir -p "$out"
rm -f "$out/result.env" # a run that dies early must not leave the last run's result

busy_pids=()
stop_busy() {
    local pid
    for pid in ${busy_pids[@]+"${busy_pids[@]}"}; do
        kill "$pid" 2>/dev/null || true
    done
}
trap stop_busy EXIT

# STRESS_BUSY busy loops, or one per CPU in the list ("0-1", "0,2", "3") when unset, all pinned to the list.
start_busy() {
    local n=0 part
    local IFS=,
    for part in $cpus; do
        case $part in
        *-*) n=$((n + ${part#*-} - ${part%-*} + 1)) ;;
        *) n=$((n + 1)) ;;
        esac
    done
    n=${busy:-$n}
    while [ "$n" -gt 0 ]; do
        taskset -c "$cpus" sh -c 'while :; do :; done' &
        busy_pids+=("$!")
        n=$((n - 1))
    done
}

# Runs "$@" pinned and under the busy loops; sets exit_code and wall_s.
starved() {
    start_busy
    local began=$SECONDS
    exit_code=0
    taskset -c "$cpus" "$@" || exit_code=$?
    wall_s=$((SECONDS - began))
    stop_busy
    busy_pids=()
}

write_result() { # write_result <report-file> <args-note>
    {
        echo "KIND=$kind"
        echo "EXIT=$exit_code"
        echo "WALL_S=$wall_s"
        echo "REPORT=$1"
        echo "ARGS=$2"
    } >"$out/result.env"
}

case $kind in
rust)
    cpus=${STRESS_CPUS:-0} busy=${STRESS_BUSY:-6}
    count=${STRESS_COUNT:-20}
    filter=${STRESS_FILTER:-'binary(fake_host) | package(puddle-proxy) | (package(puddle-agent) & test(capture))'}
    cd "$root"
    args=(--profile ci -p puddle-host -p puddle-proxy -p puddle-agent -E "$filter")
    # STRESS_PARTITION=1/2 runs half of the tests (a job per half, each on its own runner).
    [ -z "${STRESS_PARTITION:-}" ] || args+=(--partition "count:$STRESS_PARTITION")
    cargo nextest run "${args[@]}" --no-run
    # Nextest 0.9.146 writes one junit.xml per profile; the path is below.
    starved cargo nextest run "${args[@]}" --stress-count "$count"
    junit=$(find target/nextest/ci -name junit.xml | head -n 1)
    cp "$junit" "$out/report.xml" 2>/dev/null || true
    write_result "report.xml" "stress-count=$count cpus=$cpus busy=$busy partition=${STRESS_PARTITION:-all} filter=$filter"
    ;;
playwright)
    cpus=${STRESS_CPUS:-0-1} busy=${STRESS_BUSY:-2}
    count=${STRESS_COUNT:-3}
    projects=${STRESS_PROJECTS:-chromium webkit}
    cd "$root/ui"
    project_args=()
    for p in $projects; do project_args+=(--project "$p"); done
    # shellcheck disable=SC2206 # extra arguments, split on purpose
    extra=(${STRESS_ARGS:-})
    # CI=1 for forbidOnly, and no html reporter: the json one is the record. --project narrows the tests but not
    # globalSetup's warm-up, which starts every browser in the config: PUDDLE_E2E_PROJECTS narrows that too (a job
    # that installed one browser would otherwise die in the warm-up of the other, run 40987999249).
    starved env CI=1 PUDDLE_E2E_PROJECTS="${projects// /,}" PUDDLE_E2E_CPU_THROTTLE="${STRESS_THROTTLE:-4}" PUDDLE_E2E_WORKERS="${STRESS_WORKERS:-2}" \
        PLAYWRIGHT_JSON_OUTPUT_NAME="$out/report.json" \
        npx playwright test "${project_args[@]}" --repeat-each "$count" --reporter=list,json \
        ${extra[@]+"${extra[@]}"}
    write_result "report.json" "repeat-each=$count projects=$projects cpus=$cpus busy=$busy throttle=${STRESS_THROTTLE:-4}"
    ;;
vitest)
    cpus=${STRESS_CPUS:-0-1} busy=${STRESS_BUSY:-2}
    count=${STRESS_COUNT:-5}
    seed=${STRESS_SEED:-$(date -u +%Y%m%d)}
    cd "$root/ui"
    # No coverage: the thresholds are for the normal gate and a shuffled order must not trip them.
    starved npx vitest run --sequence.shuffle --sequence.seed="$seed" --repeats "$count" \
        --reporter=default --reporter=junit --outputFile.junit="$out/report.xml"
    write_result "report.xml" "repeats=$count seed=$seed cpus=$cpus busy=$busy"
    ;;
*)
    echo "usage: stress.sh <rust|playwright|vitest>" >&2
    exit 2
    ;;
esac

# The tool's status is the job's: a failing test is a red run.
exit "$exit_code"
