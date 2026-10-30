#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Fail fast in the merge queue: when a job of a merge group fails, cancel every run of that group that has not
# completed (the other three workflows and the rest of this one), so the queue entry is rebuilt after the fix
# instead of after the slowest job. The result jobs of the cancelled runs run `if: always()` and report red,
# so the cancel can never pass as green (ci/scope.sh fails a verdict on a cancelled job).
#
# usage: ci/cancel-group.sh <event> <job-status> <ref> <run-id>
#   event       github.event_name; anything but merge_group does nothing
#   job-status  job.status of the calling job; anything but failure does nothing (a skipped or cancelled job,
#               for example an infrastructure cancel, never cascades)
#   ref         github.ref; only refs/heads/gh-readonly-queue/... is touched, and the runs are those of that
#               one branch, so no other group and no pull request run is ever cancelled
#   run-id      github.run_id; its run is cancelled last, after the others
# env: GH_TOKEN with `actions: write`, GITHUB_REPOSITORY. DRY_RUN=1 lists and prints, cancels nothing.
# Before cancelling it annotates the calling job with the failed job and step names (the cancel turns the job into
# `cancelled`, which hides them). Best effort: always exits 0 (a failed cancel only leaves the old, slower behaviour). Safe to run from two
# failed jobs at once: a run that completed or was cancelled meanwhile is skipped, a lost race is ignored.
set -uo pipefail

event=${1:-} status=${2:-} ref=${3:-} own=${4:-}
repo=${GITHUB_REPOSITORY:-}

[ "$event" = merge_group ] || { echo "cancel-group: event '$event' is not merge_group, nothing to do"; exit 0; }
[ "$status" = failure ] || { echo "cancel-group: job status '$status' is not failure, nothing to do"; exit 0; }
branch=${ref#refs/heads/}
case $branch in
gh-readonly-queue/?*) ;;
*) echo "cancel-group: ref '$ref' is not a merge queue branch, nothing to do"; exit 0 ;;
esac
[ -n "$repo" ] || { echo "cancel-group: GITHUB_REPOSITORY is not set" >&2; exit 0; }

list() {
    gh run list --repo "$repo" --branch "$branch" --limit 100 --json databaseId,status \
        --jq '.[] | select(.status != "completed") | .databaseId'
}

# The cancel ends this job as `cancelled` and the group's result checks report only that word, so say here, in an
# annotation (it outlives the cancel), which job and step failed. Best effort like everything below.
named=$(gh api "repos/$repo/actions/runs/$own/jobs?per_page=100" \
    --jq '[.jobs[] | select(any(.steps[]?; .conclusion == "failure")) | "\(.name): \([.steps[] | select(.conclusion == "failure") | .name] | join(", "))"] | join("; ")' 2>/dev/null) || named=""
echo "::error title=merge group cancelled: this job failed first::${named:-no failed step could be read} (run $own)"

ids=$(list) || { echo "cancel-group: could not list the runs of $branch" >&2; exit 0; }
[ -n "$ids" ] || { echo "cancel-group: no run of $branch is still active"; exit 0; }

# Own run last: cancelling it ends this very job.
ordered=$(printf '%s\n' "$ids" | grep -vx -- "$own"; printf '%s\n' "$ids" | grep -x -- "$own")
for id in $ordered; do
    if [ "${DRY_RUN:-0}" = 1 ]; then
        echo "cancel-group: would cancel run $id of $branch"
    elif gh run cancel "$id" --repo "$repo"; then
        echo "cancel-group: cancelled run $id of $branch"
    else
        echo "cancel-group: run $id was not cancelled (already done or cancelled)" >&2
    fi
done
exit 0
