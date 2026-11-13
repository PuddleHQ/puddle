#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puts a pull request into the merge queue when auto-merge is armed on it, it is ready (merge state CLEAN: every required
# check green) and it still is not in the queue. GitHub sometimes never hands an armed, green pull request to a
# ruleset merge queue (community discussion 209348, since Sep 2026; seen here on a Dependabot PR armed by the workflow
# token, and a human-armed one, which a disable and a re-arm queued within 20 s). Arming alone is not enough, so
# .github/workflows/merge-queue-sweep.yml runs this every few minutes.
#
# Per stuck PR it adds the head commit to the queue with GraphQL `enqueuePullRequest`, reads the PR back, and when it is still
# outside the queue disables auto-merge and arms it again (what a human did to unstick a PR), reads it back again, and fails
# loudly when that did not queue it either. `gh pr merge <n> --squash` is not used: it was seen to only (re)enable
# auto-merge on a queue branch, so for an already armed PR it changed nothing, printed "merge strategy is set by the merge queue" and exited 0.
# A PR is stuck only when its last check finished more than SWEEP_MIN (default 10) minutes ago, so a PR that just went green is
# left to GitHub. Reads PR metadata only; never runs PR code. A PR that could not be queued is reported and the exit code is 1 after the loop.
# Usage: ci/merge-queue-sweep.sh   Needs GH_TOKEN, GITHUB_REPOSITORY. Env: SWEEP_MIN, SWEEP_NOW (epoch, tests),
# SWEEP_VERIFY_WAIT (seconds to wait before reading a PR back, default 10). Self-test: ci/merge-queue-sweep-test.sh.
set -uo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
now=${SWEEP_NOW:-$(date -u +%s)}
min=${SWEEP_MIN:-10}
wait=${SWEEP_VERIFY_WAIT:-10}

# shellcheck disable=SC2016 # GraphQL variables, not shell ones
query='query($owner: String!, $name: String!) { repository(owner: $owner, name: $name) {
  pullRequests(states: OPEN, baseRefName: "develop", first: 100) { nodes {
    id number headRefOid isDraft mergeStateStatus autoMergeRequest { enabledAt } mergeQueueEntry { state }
    commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes { __typename ... on CheckRun { completedAt } } } } } } } } } } }'
raw=$(gh api graphql -F owner="${repo%%/*}" -F name="${repo#*/}" -f query="$query") || { echo "merge-queue-sweep: could not read the pull requests of $repo" >&2; exit 1; }

stuck=$(jq -r --argjson now "$now" --argjson min "$min" '
    .data.repository.pullRequests.nodes[]
    | select(.autoMergeRequest != null and .mergeQueueEntry == null and (.isDraft | not) and (.mergeStateStatus | IN("CLEAN", "HAS_HOOKS")))
    | ([.commits.nodes[0].commit.statusCheckRollup.contexts.nodes[]? | .completedAt // empty | fromdateiso8601] | max) as $last
    | select($last != null and ($now - $last) > $min * 60)
    | "\(.number)\t\(.headRefOid)\t\(.id)\t\(.mergeStateStatus)"' <<<"$raw") || { echo "merge-queue-sweep: unexpected answer from GitHub" >&2; exit 1; }

# shellcheck disable=SC2016 # GraphQL variables, not shell ones
enqueue='mutation($id: ID!, $sha: GitObjectID!) { enqueuePullRequest(input: {pullRequestId: $id, expectedHeadOid: $sha}) { mergeQueueEntry { state } } }'
# shellcheck disable=SC2016 # GraphQL variables, not shell ones
readback='query($id: ID!) { node(id: $id) { ... on PullRequest { mergeQueueEntry { state } } } }'
queued() {
    sleep "$wait"
    gh api graphql -F id="$1" -f query="$readback" --jq '.data.node.mergeQueueEntry != null' 2>/dev/null | grep -qx true
}

rc=0
[ -n "$stuck" ] || { echo "merge-queue-sweep: no armed, green PR outside the merge queue"; exit 0; }
while IFS=$'\t' read -r n sha id state; do
    echo "merge-queue-sweep: PR #$n is armed and $state but not in the merge queue; adding it"
    gh api graphql -F id="$id" -F sha="$sha" -f query="$enqueue" >/dev/null || echo "merge-queue-sweep: PR #$n: enqueuePullRequest failed" >&2
    if queued "$id"; then echo "merge-queue-sweep: PR #$n is in the queue"; continue; fi
    echo "merge-queue-sweep: PR #$n: still outside the queue; re-arming auto-merge"
    gh pr merge "$n" -R "$repo" --disable-auto && gh pr merge "$n" -R "$repo" --auto --squash --match-head-commit "$sha"
    if queued "$id"; then echo "merge-queue-sweep: PR #$n is in the queue"; continue; fi
    echo "merge-queue-sweep: PR #$n could not be queued" >&2
    rc=1
done <<<"$stuck"
exit $rc
