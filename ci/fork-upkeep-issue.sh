#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Keeps the tracking issue of the weekly fork upkeep run (.github/workflows/fork-upkeep.yml). Usage:
#   ci/fork-upkeep-issue.sh <report.md> <exit code of ci/fork-upkeep.py report> <run url>
# Exit code 2 (findings: a conflict, an unlabelled commit, a stale label) opens the issue, or rewrites its body
# when it is already open; exit code 0 closes the open issue with a comment; any other code (a crash, no
# network) touches nothing and fails, because an outage must not close an issue about a real conflict.
#
# Environment: GH_TOKEN (issues: write), GITHUB_REPOSITORY; FORK_UPKEEP_GH replaces `gh` in the self-test
# (ci/fork-upkeep-issue-test.sh).
set -uo pipefail

[ "$#" = 3 ] || { echo "usage: fork-upkeep-issue.sh <report.md> <exit code> <run url>" >&2; exit 2; }
report=$1 code=$2 run_url=$3
gh=${FORK_UPKEEP_GH:-gh}
title="Fork upkeep: the forks do not replay on upstream"

find_issue() {
    "$gh" issue list --state open --search "in:title \"$title\"" --json number,title --limit 20 |
        jq -r --arg t "$title" 'map(select(.title == $t)) | first | .number // empty'
}

case $code in
0)
    number=$(find_issue) || exit 1
    if [ -n "$number" ]; then
        "$gh" issue close "$number" --comment "Every fork commit is labelled and replays on upstream in $run_url; closing." || exit 1
        echo "fork-upkeep-issue: clean; closed #$number"
    else
        echo "fork-upkeep-issue: clean; no open issue"
    fi
    exit 0
    ;;
2) ;;
*)
    echo "fork-upkeep-issue: the report exited $code; no issue touched" >&2
    exit 1
    ;;
esac

[ -s "$report" ] || { echo "fork-upkeep-issue: no report at $report" >&2; exit 1; }
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fork-upkeep-issue.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
{
    cat "$report"
    echo
    echo "Run: $run_url. The weekly run rewrites this issue and closes it on the first clean one."
} >"$tmp/body.md"

number=$(find_issue) || exit 1
if [ -n "$number" ]; then
    "$gh" issue edit "$number" --body-file "$tmp/body.md" || exit 1
    echo "fork-upkeep-issue: updated #$number"
else
    "$gh" issue create --title "$title" --body-file "$tmp/body.md" || exit 1
    echo "fork-upkeep-issue: opened a new issue"
fi
