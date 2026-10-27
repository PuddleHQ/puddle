#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Keeps the repo's Actions caches under GitHub's 10 GB cap, so the 137 MB cargo-xwin entry (a miss
# costs a 14-minute MSVC CRT download) is never evicted. Deletes, newest kept:
#   - on develop/main, all but the newest entry of each rust-cache / msb build / node group (an old
#     Cargo.lock hash leaves a 1-3 GB entry behind that nothing restores any more);
#   - on develop/main, an entry of those groups unused for a week (a group whose key was renamed
#     leaves its last entry behind, and the newest-of-group rule keeps it for ever);
#   - on any other ref (task branches, pull requests), entries not used for two days;
#   - on a merge queue ref (refs/heads/gh-readonly-queue/...), every entry at once: only a rerun of
#     that one queue entry could read it, and the steps that save on a miss on any ref (npm,
#     cargo-xwin, the msb build) would otherwise add hundreds of MB per landing.
# Never touches the cargo-xwin entry on develop (so entries of older cargo-xwin versions stay, about 137 MB each). DRY_RUN=1 lists instead of deleting.
# Self-test: ci/cache-gc-test.sh.
# Needs GH_TOKEN, GITHUB_REPOSITORY and, to delete, `actions: write`.
set -euo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
cutoff=$(date -u -d '2 days ago' +%Y-%m-%dT%H:%M:%SZ)
week=$(date -u -d '7 days ago' +%Y-%m-%dT%H:%M:%SZ)

# One line per entry before anything is deleted: GitHub publishes no eviction log, and this workflow runs after
# every develop push, so these lines (in the run's summary) are the sample of what was there and when it was last read.
echo "inventory (key, ref, MB, created, last used):"
gh api --paginate "repos/$repo/actions/caches?per_page=100" --jq '.actions_caches[]' |
    jq -r '"  \(.key)  \(.ref)  \(.size_in_bytes / 1000000 | floor)  \(.created_at)  \(.last_accessed_at)"' || echo "  (the inventory failed; the clean-up below goes on)"

gh api --paginate "repos/$repo/actions/caches?per_page=100" --jq '.actions_caches[]' |
    jq -rs --arg cutoff "$cutoff" --arg week "$week" '
      def group: if (.key | test("^v0-rust-.*-[0-9a-f]{8}-[0-9a-f]{8}$"))
                   then .key | sub("-[0-9a-f]{8}-[0-9a-f]{8}$"; "")
                 elif (.key | startswith("msb-linux-")) then "msb-linux"
                 elif (.key | startswith("node-cache-")) then "node-cache"
                 elif (.key | startswith("playwright-")) then "playwright"
                 else null end;
      def trunk: .ref == "refs/heads/develop" or .ref == "refs/heads/main";
      ( [ .[] | select(trunk and (group != null)) | . + {g: group} ]
        | group_by([.ref, .g])
        | map(sort_by(.created_at) | reverse | .[1:][]) ) as $old
      | ( [ .[] | select(trunk and (group != null) and (.last_accessed_at < $week)) ] ) as $orphans
      | ( [ .[] | select((trunk | not) and (.last_accessed_at < $cutoff)) ] ) as $stale
      | ( [ .[] | select(.ref | startswith("refs/heads/gh-readonly-queue/")) ] ) as $queue
      | ($old + $orphans + $stale + $queue | unique_by(.id))[] | [.id, .ref, .key, .size_in_bytes] | @tsv' |
    while IFS=$'\t' read -r id ref key size; do
        echo "delete $id  $ref  $key  $((size / 1000000)) MB"
        [ "${DRY_RUN:-}" = "1" ] || gh api -X DELETE "repos/$repo/actions/caches/$id" --silent
    done

gh api "repos/$repo/actions/cache/usage" --jq '"cache total: \(.active_caches_size_in_bytes / 1000000 | floor) MB in \(.active_caches_count) entries (cap 10000 MB)"'
