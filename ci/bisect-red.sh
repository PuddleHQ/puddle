#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Finds which commit turned a non-required workflow red. Usage:
#   ci/bisect-red.sh <workflow-file> [--good <sha>] --bad <sha> [--max <n>]
#   ci/bisect-red.sh <workflow-file> --cleanup
# The commits after `--good` up to `--bad` are the suspects (`--good` defaults to the commit of the last green
# scheduled run of the workflow on develop). `--bad` is known red, so the workflow is dispatched on each commit
# before it: a branch `bisect/<workflow>-<sha7>` is created at that commit (GitHub only dispatches a branch or a
# tag) and the workflow runs there. The first suspect whose run is red is the culprit; when every earlier one is
# green, `--bad` is. It refuses more than `--max` (default 6) commits, since every run costs the workflow's full
# runner time. `--cleanup` deletes the branches of that workflow. Needs `gh` with `actions: write` and `contents:
# write`. Self-test: ci/bisect-red-test.sh (BISECT_GH replaces `gh`).
set -uo pipefail

gh=${BISECT_GH:-gh}
repo=${GITHUB_REPOSITORY:-}
[ -n "$repo" ] || repo=$("$gh" repo view --json nameWithOwner --jq .nameWithOwner) || exit 2
usage() { echo "usage: bisect-red.sh <workflow-file> [--good <sha>] --bad <sha> [--max <n>] | <workflow-file> --cleanup" >&2; exit 2; }

[ "$#" -ge 2 ] || usage
workflow=$1
shift
stem=${workflow%.yml}
good="" bad="" max=6 cleanup=0
while [ "$#" -gt 0 ]; do
    case $1 in
    --good) [ "$#" -ge 2 ] || usage; good=$2; shift ;;
    --bad) [ "$#" -ge 2 ] || usage; bad=$2; shift ;;
    --max) [ "$#" -ge 2 ] || usage; max=$2; shift ;;
    --cleanup) cleanup=1 ;;
    *) usage ;;
    esac
    shift
done
case $max in '' | *[!0-9]*) usage ;; esac

if [ "$cleanup" = 1 ]; then
    refs=$("$gh" api "repos/$repo/git/matching-refs/heads/bisect/$stem-" --jq '.[].ref') || exit 1
    for ref in $refs; do
        "$gh" api -X DELETE "repos/$repo/git/$ref" >/dev/null || exit 1
        echo "deleted $ref"
    done
    exit 0
fi

[ -n "$bad" ] || usage
if [ -z "$good" ]; then
    good=$("$gh" run list -R "$repo" --workflow "$workflow" --branch develop --event schedule --status success --limit 1 \
        --json headSha --jq '.[0].headSha // ""') || exit 1
    [ -n "$good" ] || { echo "bisect-red: no green scheduled run of $workflow; pass --good <sha>" >&2; exit 2; }
fi

commits=$("$gh" api "repos/$repo/compare/$good...$bad" --jq '.commits[].sha') || exit 1
[ -n "$commits" ] || { echo "bisect-red: $bad is not ahead of $good" >&2; exit 2; }
# the last commit is $bad, already red
suspects=$(printf '%s\n' "$commits" | sed '$d')
count=$(printf '%s\n' "$commits" | wc -l)
if [ "$count" -gt "$max" ]; then
    echo "bisect-red: $count commits between $good and $bad, over --max $max; narrow --good or raise --max" >&2
    exit 2
fi
echo "bisect-red: $count commit(s) in $good...$bad; $bad is red, running $workflow on the $((count - 1)) before it"
for sha in $suspects; do
    branch="bisect/$stem-${sha:0:7}"
    "$gh" api -X POST "repos/$repo/git/refs" -f "ref=refs/heads/$branch" -f "sha=$sha" >/dev/null || exit 1
    "$gh" workflow run "$workflow" -R "$repo" --ref "$branch" || exit 1
    echo "dispatched $workflow on $sha ($branch)"
done
echo "Watch: gh run list -R $repo --workflow $workflow --event workflow_dispatch. The first red commit is the culprit; $bad is when all are green."
echo "Then: ci/bisect-red.sh $workflow --cleanup"
