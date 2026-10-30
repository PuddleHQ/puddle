#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Names the VM tests that failed or were stopped, so a red or cancelled vm-linux check says which test and why
# instead of "cancelled". In the merge queue the first failed partition cancels its own run (ci/cancel-group.sh),
# the job then ends as `cancelled` and the result job only sees that word; an annotation written before the
# cancel survives it, so the partition job writes one per test and the result job reads them back.
#
# usage: ci/vm-tests-report.sh log <nextest-log> <partition>   (partition job, after the tests, on failure)
#        ci/vm-tests-report.sh run <repo> <run-id>             (result job: what each partition recorded)
# `log` reads nextest's output (colours and all), prints a `::error` annotation for every test that did not
# pass (FAIL, TIMEOUT, SIGTERM, ...), one for the run deadline, and a summary table to $GITHUB_STEP_SUMMARY with
# the slowest passing tests (a runner that is slow shows as every test slow, not as one test hanging).
# `run` needs `gh` with `actions: read` and `checks: read`. Both always exit 0: a report must never turn a
# failure into a different one.
set -uo pipefail

mode=${1:-}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

# One line per test that did not pass, in the order they ended: "<STATUS> <seconds> <package::binary> <test>".
# Nextest prints each failure again after its summary, so a test is listed once.
outcomes() {
    sed 's/\x1b\[[0-9;]*m//g' "$1" | awk '
        match($0, /^[[:space:]]*[A-Z][A-Z-]+[[:space:]]+\[[^]]*\][[:space:]]+\([[:space:]]*[0-9]+\/[0-9]+\)[[:space:]]/) {
            split($0, f, /[][()[:space:]]+/)
            # f[2] is the status; a SLOW line with a count is a passing test that crossed the slow mark
            status = f[2]
            if (status == "PASS" || status == "SLOW" || status == "LEAK" || status == "SKIP") next
            line = $0
            sub(/^[^)]*\)[[:space:]]+/, "", line)
            secs = $0; sub(/^[^[]*\[[[:space:]]*/, "", secs); sub(/s\].*$/, "", secs)
            key = status " " line
            if (!(key in seen)) { seen[key] = 1; print status, secs, line }
        }'
}

# "<seconds> <package::binary> <test>" of the slowest passing tests, longest first.
slow_passes() {
    sed 's/\x1b\[[0-9;]*m//g' "$1" | awk '
        /^[[:space:]]*PASS[[:space:]]+\[[^]]*\][[:space:]]+\([[:space:]]*[0-9]+\/[0-9]+\)[[:space:]]/ {
            secs = $0; sub(/^[^[]*\[[[:space:]]*/, "", secs); sub(/s\].*$/, "", secs)
            line = $0; sub(/^[^)]*\)[[:space:]]+/, "", line)
            if (!(line in seen)) { seen[line] = 1; print secs, line }
        }' | sort -rn | head -5
}

# Workflow commands end at a newline and use % for escapes.
esc() { printf '%s' "$1" | sed -e 's/%/%25/g' -e 's/\r/%0D/g' | tr '\n' ' '; }

report_log() {
    local log=${1:-} part=${2:-?} n=0 status secs bin test clean
    if [ ! -s "$log" ]; then
        echo "::error title=vm-linux partition $part: no test log::nextest wrote no log ($log): the run ended before the tests started or the runner died"
        echo "### vm-linux partition $part: no test log" >>"$summary"
        return
    fi
    clean=$(sed 's/\x1b\[[0-9;]*m//g' "$log")
    {
        echo "### vm-linux partition $part: tests that did not pass"
        echo
        echo '| Result | Seconds | Test |'
        echo '|---|---|---|'
    } >>"$summary"
    while read -r status secs bin test; do
        [ -n "$status" ] || continue
        n=$((n + 1))
        [ "$n" -le 10 ] && echo "::error title=vm-linux partition $part: $status::$(esc "$bin $test") ended $status after ${secs}s"
        echo "| $status | $secs | \`$bin $test\` |" >>"$summary"
    done < <(outcomes "$log")
    local cut
    cut=$(grep -E 'Cancelling due to global timeout' <<<"$clean" | head -1 | sed 's/^[[:space:]]*//')
    if [ -n "$cut" ]; then
        local notrun
        notrun=$(grep -E 'tests? (was|were) not run due to global timeout' <<<"$clean" | head -1)
        echo "::error title=vm-linux partition $part: stopped at the run deadline::$(esc "$cut${notrun:+; $notrun}")"
        echo "| DEADLINE | | $cut${notrun:+; $notrun} |" >>"$summary"
    fi
    if [ "$n" = 0 ] && [ -z "$cut" ]; then
        if grep -qE '^[[:space:]]*Summary \[' <<<"$clean"; then
            local err
            err=$(grep -E '^error:' <<<"$clean" | tail -1)
            echo "::error title=vm-linux partition $part: no test failed::nextest reports no failed test${err:+ ($(esc "$err"))}; look at the failing step"
            echo "| none | | nextest reports no failed test${err:+ (\`$err\`)}; look at the failing step |" >>"$summary"
        else
            local last
            last=$(grep -v '^[[:space:]]*$' <<<"$clean" | tail -1 | cut -c1-200)
            echo "::error title=vm-linux partition $part: no nextest summary::the log ends without a summary; last line: $(esc "$last")"
            echo "| none | | no nextest summary; last line: \`$last\` |" >>"$summary"
        fi
    fi
    {
        echo
        echo "Slowest passing tests (every test slow means a slow runner, not one hanging test):"
        echo
        slow_passes "$log" | while read -r secs rest; do echo "- ${secs}s \`$rest\`"; done
        if [ -s "${RUNNER_TEMP:-/nonexistent}/runner-speed.txt" ]; then
            echo
            echo "Runner speed before the tests: $(cat "$RUNNER_TEMP/runner-speed.txt")"
        fi
    } >>"$summary"
}

report_run() {
    local repo=${1:-} run=${2:-} id name conclusion any=0 notes steps
    [ -n "$repo" ] && [ -n "$run" ] || { echo "vm-tests-report: usage: run <repo> <run-id>" >&2; return; }
    local jobs
    jobs=$(gh api "repos/$repo/actions/runs/$run/jobs?per_page=100" \
        --jq '.jobs[] | select(.name | startswith("vm-linux tests")) | select(.conclusion != "success" and .conclusion != "skipped") | "\(.id)\t\(.conclusion)\t\(.name)"') || {
        echo "vm-tests-report: could not list the jobs of run $run" >&2
        return
    }
    while IFS=$'\t' read -r id conclusion name; do
        [ -n "$id" ] || continue
        any=1
        notes=$(gh api "repos/$repo/check-runs/$id/annotations" --jq '.[] | select((.title // "") | startswith("vm-linux partition")) | "  \(.title): \(.message)"' 2>/dev/null)
        steps=$(gh api "repos/$repo/actions/jobs/$id" --jq '[.steps[] | select(.conclusion == "failure") | .name] | join(", ")' 2>/dev/null)
        echo "$name: $conclusion${steps:+; failed step: $steps}"
        if [ -n "$notes" ]; then
            printf '%s\n' "$notes"
        else
            echo "  no test was named: the job ended before its report step (or the run was cancelled from outside)"
        fi
        {
            echo "- **$name**: $conclusion${steps:+, failed step: $steps}"
            if [ -n "$notes" ]; then printf '%s\n' "$notes" | sed 's/^  /  - /'; else echo "  - no test was named"; fi
        } >>"$summary"
    done <<<"$jobs"
    [ "$any" = 1 ] || echo "no vm-linux test job failed or was cancelled in run $run"
}

case $mode in
log) report_log "${2:-}" "${3:-?}" ;;
run) report_run "${2:-}" "${3:-}" ;;
*) echo "usage: ci/vm-tests-report.sh log <nextest-log> <partition> | run <repo> <run-id>" >&2 ;;
esac
exit 0
