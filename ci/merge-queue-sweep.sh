#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Puts a pull request into the merge queue when auto-merge is armed on it, it is ready (merge state CLEAN: every required
# check green) and it still is not in the queue. GitHub sometimes never hands an armed, green pull request to a
# ruleset merge queue (community discussion 209348, since Sep 2026; seen here on a Dependabot PR armed by the workflow
# token, and a human-armed one, which a disable and a re-arm queued within 20 s). Arming alone is not enough, so
# .github/workflows/merge-queue-sweep.yml runs this every few minutes.
#
# Per stuck PR it first asks to merge the head commit directly (`gh pr merge <n> --squash --match-head-commit <sha>`: on a
# queue branch this adds the PR to the queue), and when that fails it disables auto-merge and arms it again. A PR is stuck only when
# its last check finished more than SWEEP_MIN (default 10) minutes ago, so a PR that just went green is left to GitHub.
# Reads PR metadata only; never runs PR code. A failing `gh` call for one PR is reported and the exit code is 1 after the loop.
# Usage: ci/merge-queue-sweep.sh   Needs GH_TOKEN, GITHUB_REPOSITORY. Env: SWEEP_MIN, SWEEP_NOW (epoch, tests). Self-test: ci/merge-queue-sweep-test.sh.
set -uo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
now=${SWEEP_NOW:-$(date -u +%s)}
min=${SWEEP_MIN:-10}

# shellcheck disable=SC2016 # GraphQL variables, not shell ones
query='query($owner: String!, $name: String!) { repository(owner: $owner, name: $name) {
  pullRequests(states: OPEN, baseRefName: "develop", first: 100) { nodes {
    number headRefOid isDraft mergeStateStatus autoMergeRequest { enabledAt } mergeQueueEntry { state }
    commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes { __typename ... on CheckRun { completedAt } } } } } } } } } } }'
raw=$(gh api graphql -F owner="${repo%%/*}" -F name="${repo#*/}" -f query="$query") || { echo "merge-queue-sweep: could not read the pull requests of $repo" >&2; exit 1; }

stuck=$(jq -r --argjson now "$now" --argjson min "$min" '
    .data.repository.pullRequests.nodes[]
    | select(.autoMergeRequest != null and .mergeQueueEntry == null and (.isDraft | not) and (.mergeStateStatus | IN("CLEAN", "HAS_HOOKS")))
    | ([.commits.nodes[0].commit.statusCheckRollup.contexts.nodes[]? | .completedAt // empty | fromdateiso8601] | max) as $last
    | select($last != null and ($now - $last) > $min * 60)
    | "\(.number)\t\(.headRefOid)"' <<<"$raw") || { echo "merge-queue-sweep: unexpected answer from GitHub" >&2; exit 1; }

rc=0
[ -n "$stuck" ] || { echo "merge-queue-sweep: no armed, green PR outside the merge queue"; exit 0; }
while IFS=$'\t' read -r n sha; do
    echo "merge-queue-sweep: PR #$n is armed and CLEAN but not in the merge queue; adding it"
    if gh pr merge "$n" -R "$repo" --squash --match-head-commit "$sha"; then continue; fi
    echo "merge-queue-sweep: PR #$n: direct merge request failed; re-arming auto-merge"
    if gh pr merge "$n" -R "$repo" --disable-auto && gh pr merge "$n" -R "$repo" --auto --squash --match-head-commit "$sha"; then continue; fi
    echo "merge-queue-sweep: PR #$n could not be queued" >&2
    rc=1
done <<<"$stuck"
exit $rc
