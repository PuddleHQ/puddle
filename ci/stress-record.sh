#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The last step of each stress job (.github/workflows/stress.yml): adds the night's row for one kind to
# target/stress/yield.tsv (ci/stress-yield.py) and to the job summary, and lists the failing tests.
# Needs STRESS_JOB_START (epoch seconds, set by the job's first step), GITHUB_SHA, GITHUB_RUN_ID.
set -euo pipefail
kind=${1:?usage: stress-record.sh <rust|playwright|vitest>}
here=$(cd "$(dirname "$0")" && pwd)
out=${STRESS_OUT:-$here/../target/stress}
mkdir -p "$out"
summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
{
    echo "### Stress yield ($kind)"
    python3 "$here/stress-yield.py" record "$out" "$kind" --sha "${GITHUB_SHA:?}" --run-id "${GITHUB_RUN_ID:?}" \
        --job-seconds "$(($(date +%s) - ${STRESS_JOB_START:?}))"
} >>"$summary"
# The report file names the failing tests; the same section the summary gets, and one warning each.
if [ -f "$out/$kind/result.env" ]; then
    report=$(sed -n 's/^REPORT=//p' "$out/$kind/result.env")
    [ -f "$out/$kind/$report" ] && python3 "$here/flaky-report.py" --stress "$kind" "$out/$kind/$report" >>"$summary"
fi
exit 0
