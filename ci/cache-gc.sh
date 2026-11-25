#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Keeps the repo's Actions caches under GitHub's 10 GB cap, so the 137 MB cargo-xwin entry (a miss
# costs a 14-minute MSVC CRT download) is never evicted. Deletes, newest kept:
#   - on develop/main, all but the newest entry of each rust-cache / msb build / node / notices-stamp / pinned-download
#     group (forgejo-, gateway-, vscode-, gitleaks-<os>[-<arch>]-<pin hash>) (an old
#     Cargo.lock hash leaves a 1-3 GB entry behind that nothing restores any more);
#   - on develop/main, an entry of those groups unused for a week (a group whose key was renamed
#     leaves its last entry behind, and the newest-of-group rule keeps it for ever);
#   - on any other ref (task branches, pull requests), entries not used for two days;
#   - on a merge queue ref (refs/heads/gh-readonly-queue/...), every entry at once: only a rerun of
#     that one queue entry could read it, and the steps that save on a miss on any ref (npm,
#     cargo-xwin, the msb build) would otherwise add hundreds of MB per landing.
# Never touches the cargo-xwin entry on develop (so entries of older cargo-xwin versions stay, about 137 MB each). DRY_RUN=1 lists instead of deleting.
# Self-test: ci/cache-gc-test.sh. BUDGET_MB (default 9500: the wanted set of about 10.5 GB does not fit under 9000 without dropping a big entry that the next run saves again) is the size the entries left must fit.
# Needs GH_TOKEN, GITHUB_REPOSITORY and, to delete, `actions: write`.
set -euo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
gone=""
cutoff=$(date -u -d '2 days ago' +%Y-%m-%dT%H:%M:%SZ)
week=$(date -u -d '7 days ago' +%Y-%m-%dT%H:%M:%SZ)

# One line per entry before anything is deleted: GitHub publishes no eviction log, and this workflow runs after
# every develop push, so these lines (in the run's summary) are the sample of what was there and when it was last read.
echo "inventory (key, ref, MB, created, last used):"
gh api --paginate "repos/$repo/actions/caches?per_page=100" --jq '.actions_caches[]' |
    jq -r '"  \(.key)  \(.ref)  \(.size_in_bytes / 1000000 | floor)  \(.created_at)  \(.last_accessed_at)"' || echo "  (the inventory failed; the clean-up below goes on)"

while IFS=$'\t' read -r id ref key size; do
        echo "delete $id  $ref  $key  $((size / 1000000)) MB"
        gone="$gone$id "
        [ "${DRY_RUN:-}" = "1" ] || gh api -X DELETE "repos/$repo/actions/caches/$id" --silent
    done < <(gh api --paginate "repos/$repo/actions/caches?per_page=100" --jq '.actions_caches[]' |
    jq -rs --arg cutoff "$cutoff" --arg week "$week" '
      def group: if (.key | test("^v0-rust-.*-[0-9a-f]{8}-[0-9a-f]{8}$"))
                   then .key | sub("-[0-9a-f]{8}-[0-9a-f]{8}$"; "")
                 elif (.key | startswith("msb-linux-")) then "msb-linux"
                 elif (.key | startswith("node-cache-")) then "node-cache"
                 elif (.key | startswith("playwright-webkit-")) then "playwright-webkit"
                 elif (.key | startswith("playwright-")) then "playwright"
                 elif (.key | startswith("notices-ok-")) then "notices-ok"
                 elif (.key | startswith("vm-guest-env-tools-")) then "vm-guest-env-tools"
                 elif (.key | test("^(forgejo|gateway|vscode|gitleaks)-.*-[0-9a-f]{64}$")) then .key | sub("-[0-9a-f]{64}$"; "")
                 else null end;
      def trunk: .ref == "refs/heads/develop" or .ref == "refs/heads/main";
      ( [ .[] | select(trunk and (group != null)) | . + {g: group} ]
        | group_by([.ref, .g])
        | map(sort_by(.created_at) | reverse | .[1:][]) ) as $old
      | ( [ .[] | select(trunk and (group != null) and (.last_accessed_at < $week)) ] ) as $orphans
      | ( [ .[] | select((trunk | not) and (.last_accessed_at < $cutoff)) ] ) as $stale
      | ( [ .[] | select(.ref | startswith("refs/heads/gh-readonly-queue/")) ] ) as $queue
      | ($old + $orphans + $stale + $queue | unique_by(.id))[] | [.id, .ref, .key, .size_in_bytes] | @tsv')

# Budget: GitHub stops saving ANY new entry (a fresh unique key included) once the repo holds more than 10 GB, whatever
# the 20 GB storage-limit setting says, and reports it as "Unable to reserve cache ... another job may be creating this
# cache" (2026-11-24: usage 10.75 GB, a probe could not save; after deleting a 3.1 GB entry the same probe saved
# both a fresh key and the forgejo key). So what is left after the deletions above must fit BUDGET_MB. Over it, drop
# trunk entries that are cheapest to rebuild first (clippy, mutants, fuzz, windows, gates, linux; oldest use first
# within a kind). Never the small ones whose loss costs a download.
budget_mb=${BUDGET_MB:-9500}
gh api --paginate "repos/$repo/actions/caches?per_page=100" --jq '.actions_caches[]' |
    jq -rs --arg gone "$gone" --argjson budget "$budget_mb" '
      def rank: if (.key | test("^v0-rust-.*clippy")) then 1
                elif (.key | test("^v0-rust-mutants")) then 2
                elif (.key | test("^v0-rust-fuzz")) then 3
                elif (.key | test("^v0-rust-windows")) then 4
                elif (.key | test("^v0-rust-gates")) then 5
                elif (.key | test("^v0-rust-linux")) then 6
                else null end;
      ($gone | split(" ") | map(select(. != "") | tonumber)) as $g
      | [ .[] | select(.id as $i | $g | index($i) | not) ] as $left
      | ($left | map(.size_in_bytes) | add // 0) as $total
      | ($budget * 1000000) as $cap
      | if $total <= $cap then empty else
          ( [ $left[] | select((.ref == "refs/heads/develop" or .ref == "refs/heads/main") and (rank != null)) ]
            | sort_by([rank, .last_accessed_at])
            | reduce .[] as $e ({left: $total, out: []};
                if .left > $cap then {left: (.left - $e.size_in_bytes), out: (.out + [$e])} else . end)
            | .out[] | [.id, .ref, .key, .size_in_bytes] | @tsv )
        end' |
    while IFS=$'\t' read -r id ref key size; do
        echo "delete $id  $ref  $key  $((size / 1000000)) MB  (over the ${budget_mb} MB budget)"
        [ "${DRY_RUN:-}" = "1" ] || gh api -X DELETE "repos/$repo/actions/caches/$id" --silent
    done

gh api "repos/$repo/actions/cache/usage" --jq '"cache total: \(.active_caches_size_in_bytes / 1000000 | floor) MB in \(.active_caches_count) entries (saves stop above 10000 MB)"'
