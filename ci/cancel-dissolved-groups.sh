#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Cancels the runs of merge groups the queue has dissolved. When an entry leaves the queue without a failed job
# (removed by hand, dequeued by a newer push, a lost race), ci/cancel-group.sh never runs, and the old group keeps
# its four workflows going on runners nothing needs; one such group held 216 runner minutes. .github/workflows/
# merge-queue-sweep.yml runs this every few minutes.
#
# A run is cancelled only when all of these hold:
#   - its event is merge_group and it is not completed;
#   - its branch is gh-readonly-queue/... (no pull request or push run is ever touched);
#   - its head commit is not the group commit of any entry now in the queue, and the PR number in the branch name
#     (pr-<n>-<sha>) is not that of an entry whose group commit is not known yet (a group just created);
#   - it was created more than SWEEP_GROUP_MIN (default 15) minutes ago, so a group whose entry the queue does not
#     list yet is never cut down.
# When the queue cannot be read, or lists more entries than one page, nothing is cancelled (an incomplete queue
# would make live groups look dissolved).
# Usage: ci/cancel-dissolved-groups.sh   Needs GH_TOKEN (`actions: write`), GITHUB_REPOSITORY.
# Env: SWEEP_GROUP_MIN, SWEEP_NOW (epoch, tests), SWEEP_BASE (default develop), DRY_RUN=1 (print, cancel nothing).
# Best effort: exits 0 even when a cancel fails (the run finished meanwhile); exits 1 only when GitHub cannot be read.
# Self-test: ci/cancel-dissolved-groups-test.sh.
set -uo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
now=${SWEEP_NOW:-$(date -u +%s)}
min=${SWEEP_GROUP_MIN:-15}
base=${SWEEP_BASE:-develop}

# shellcheck disable=SC2016 # GraphQL variables, not shell ones
query='query($owner: String!, $name: String!, $branch: String!) { repository(owner: $owner, name: $name) {
  mergeQueue(branch: $branch) { entries(first: 100) { pageInfo { hasNextPage } nodes { headCommit { oid } pullRequest { number } } } } } }'
queue=$(gh api graphql -F owner="${repo%%/*}" -F name="${repo#*/}" -F branch="$base" -f query="$query") \
    || { echo "cancel-dissolved-groups: could not read the merge queue of $repo" >&2; exit 1; }
if [ "$(jq -r '.data.repository.mergeQueue // empty | .entries.pageInfo.hasNextPage' <<<"$queue" 2>/dev/null)" != false ]; then
    echo "cancel-dissolved-groups: no readable single-page merge queue for $base; leaving every group alone" >&2
    exit 0
fi
live_shas=$(jq -r '.data.repository.mergeQueue.entries.nodes[] | .headCommit.oid // empty' <<<"$queue") || exit 1
pending_prs=$(jq -r '.data.repository.mergeQueue.entries.nodes[] | select(.headCommit == null) | .pullRequest.number' <<<"$queue") || exit 1

runs=$(gh run list --repo "$repo" --event merge_group --limit 200 --json databaseId,status,headBranch,headSha,createdAt) \
    || { echo "cancel-dissolved-groups: could not list the merge group runs of $repo" >&2; exit 1; }
stale=$(jq -r --argjson now "$now" --argjson min "$min" '
    .[] | select(.status != "completed" and (.headBranch | startswith("gh-readonly-queue/")) and (($now - (.createdAt | fromdateiso8601)) > $min * 60))
    | "\(.databaseId)\t\(.headBranch)\t\(.headSha)"' <<<"$runs") || { echo "cancel-dissolved-groups: unexpected run list" >&2; exit 1; }

n=0
while IFS=$'\t' read -r id branch sha; do
    [ -n "$id" ] || continue
    if grep -qxF -- "$sha" <<<"$live_shas"; then continue; fi
    pr=${branch##*/pr-}
    pr=${pr%%-*}
    if [ -n "$pending_prs" ] && grep -qxF -- "$pr" <<<"$pending_prs"; then continue; fi
    n=$((n + 1))
    if [ "${DRY_RUN:-0}" = 1 ]; then
        echo "cancel-dissolved-groups: would cancel run $id of dissolved group $branch"
    elif gh run cancel "$id" --repo "$repo"; then
        echo "cancel-dissolved-groups: cancelled run $id of dissolved group $branch"
    else
        echo "cancel-dissolved-groups: run $id was not cancelled (already done)" >&2
    fi
done <<<"$stale"
[ "$n" -gt 0 ] || echo "cancel-dissolved-groups: no dissolved group has active runs"
exit 0
