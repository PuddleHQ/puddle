#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The red signal for runs that no ruleset requires (.github/workflows/nightly-signal.yml). Usage:
#   ci/nightly-issue.sh <workflow name> <conclusion> <run url> <head sha>
# Keeps one issue per workflow, titled "Nightly red: <workflow>". A failed run (failure, timed_out,
# startup_failure) opens it, or rewrites its body when it is already open; a successful run closes it with a
# comment; any other conclusion (cancelled, skipped, ...) touches nothing, because a cancelled run says nothing
# about the code. The body names the last green nightly commit and the helper that finds the commit that broke it
# (ci/bisect-red.sh). Exits 1 when `gh` fails, so a broken signal is itself a red run.
#
# Environment: GH_TOKEN (issues: write, actions: read), GITHUB_REPOSITORY; NIGHTLY_GH replaces `gh` in the
# self-test (ci/nightly-issue-test.sh).
set -uo pipefail

[ "$#" = 4 ] || { echo "usage: nightly-issue.sh <workflow name> <conclusion> <run url> <head sha>" >&2; exit 2; }
workflow=$1 conclusion=$2 run_url=$3 sha=$4
gh=${NIGHTLY_GH:-gh}
title="Nightly red: $workflow"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/nightly-issue.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

find_issue() {
    "$gh" issue list --state open --search "in:title \"$title\"" --json number,title --limit 20 |
        jq -r --arg t "$title" 'map(select(.title == $t)) | first | .number // empty'
}

case $conclusion in
success)
    number=$(find_issue) || exit 1
    if [ -n "$number" ]; then
        "$gh" issue close "$number" --comment "The nightly $workflow run is green again: $run_url ($sha). Closing." || exit 1
        echo "nightly-issue: green; closed #$number"
    else
        echo "nightly-issue: green; no open issue"
    fi
    exit 0
    ;;
failure | timed_out | startup_failure) ;;
*)
    echo "nightly-issue: conclusion '$conclusion' says nothing about the code; no issue touched"
    exit 0
    ;;
esac

green=$("$gh" run list --workflow "$workflow.yml" --branch develop --event schedule --status success --limit 1 \
    --json headSha --jq '.[0].headSha // ""') || exit 1
{
    echo "The nightly \`$workflow\` run on \`$sha\` is red: $run_url"
    echo
    if [ -n "$green" ]; then
        echo "The last green nightly of this workflow ran on \`$green\`. The culprit is one of the commits in"
        echo "\`$green...$sha\`; to find it, run \`ci/bisect-red.sh $workflow.yml --good $green --bad $sha\`, which runs the workflow"
        echo "on each commit between them (about one runner-hour in all for three commits); \`--cleanup\` removes its branches."
    else
        echo "No green nightly of this workflow is listed, so the suspects are not known."
        echo "Run \`ci/bisect-red.sh $workflow.yml --good <sha> --bad $sha\` with the last commit you trust."
    fi
    echo
    echo "This workflow is not a required check, so nothing blocked the commits that landed meanwhile."
    echo "The nightly rewrites this issue on every red run and closes it on the first green one."
} >"$tmp/body.md"

number=$(find_issue) || exit 1
if [ -n "$number" ]; then
    "$gh" issue edit "$number" --body-file "$tmp/body.md" || exit 1
    echo "nightly-issue: updated #$number"
else
    "$gh" issue create --title "$title" --body-file "$tmp/body.md" || exit 1
    echo "nightly-issue: opened a new issue"
fi
