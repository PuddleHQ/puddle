#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Frees the Rust cache entry a develop run restored once rust-cache has saved its replacement, so the
# old and the new 6.7 GB entry sit together in the repo (10 GB cap, cache-gc.yml) for seconds, not
# until a later workflow run. The cache-drop action (.github/actions/cache-drop) runs it twice around
# rust-cache: `snapshot` in its main step (before the restore) and `drop` in its post step, which GitHub
# runs after rust-cache's own post step, the save.
#   cache-drop.sh snapshot <key-prefix>   remembers the newest develop entry of the group
#   cache-drop.sh drop <key-prefix>       deletes that entry, by its exact key and ref, if every guard holds
# The entry is deleted only after its replacement exists: deleting it earlier leaves pull requests with
# no entry to restore (a cold build takes about 35 minutes of a job limited to 30). Guards, each of which
# keeps the entry and says why:
#   - the run is not on refs/heads/develop (a pull request, a queue branch, a tag, a fork);
#   - nothing was remembered, or the entry is already gone;
#   - the run did not restore it (its last access did not move since the snapshot);
#   - no other entry of the group on develop was created since the snapshot (no save happened: an
#     exact hit, a failed or cancelled save), so the remembered entry is the only one left.
# It never fails the job: a failed call is a warning, and cache-gc.yml removes what is left.
# Needs GH_TOKEN, GITHUB_REPOSITORY, GITHUB_REF and, to delete, `actions: write`. DRY_RUN=1 prints the
# delete instead of making it. CACHE_DROP_STATE is the file that carries the snapshot between the steps.
set -uo pipefail

mode=${1:-}
prefix=${2:-}
case "$mode" in snapshot | drop) ;; *)
    echo "usage: cache-drop.sh snapshot|drop <key-prefix>" >&2
    exit 2
    ;;
esac
[ -n "$prefix" ] || {
    echo "usage: cache-drop.sh snapshot|drop <key-prefix>" >&2
    exit 2
}

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
ref=refs/heads/develop
state=${CACHE_DROP_STATE:-${RUNNER_TEMP:-/tmp}/cache-drop-snapshot.json}

say() { echo "cache-drop: $*"; }
warn() { echo "::warning::cache-drop: $*"; }

# Prints the develop entries of the group (key = prefix + anything ending in two 8-digit hashes, the
# shape rust-cache gives), one JSON object per line, timestamps cut to whole seconds (GitHub sends
# microseconds, the runner's `date` does not, and ".5Z" sorts below "Z").
entries() {
    gh api --paginate "repos/$repo/actions/caches?per_page=100&ref=$ref&key=$prefix" 2>/dev/null |
        jq -c --arg ref "$ref" --arg prefix "$prefix" '.actions_caches[]
          | select(.ref == $ref and (.key | startswith($prefix)) and (.key | test("-[0-9a-f]{8}-[0-9a-f]{8}$")))
          | .created_at |= sub("\\.[0-9]+"; "") | .last_accessed_at |= sub("\\.[0-9]+"; "")'
}

if [ "$mode" = snapshot ]; then
    [ "${GITHUB_REF:-}" = "$ref" ] || {
        say "kept: not a develop run (${GITHUB_REF:-unset})"
        : >"$state"
        exit 0
    }
    if ! list=$(entries); then
        warn "could not list the caches; nothing will be dropped"
        : >"$state"
        exit 0
    fi
    # The newest entry is what rust-cache's restore key picks.
    printf '%s\n' "$list" | jq -cs --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        'sort_by(.created_at) | last | select(. != null) | {entry: ., at: $at}' >"$state"
    if [ -s "$state" ]; then
        say "remembered $(jq -r '.entry | "\(.id) \(.key) accessed \(.last_accessed_at)"' "$state")"
    else
        say "no entry of $prefix* on $ref"
    fi
    exit 0
fi

# drop
[ "${GITHUB_REF:-}" = "$ref" ] || {
    say "kept: not a develop run (${GITHUB_REF:-unset})"
    exit 0
}
[ -s "$state" ] || {
    say "kept: no entry was remembered before the restore"
    exit 0
}
id=$(jq -r .entry.id "$state")
key=$(jq -r .entry.key "$state")
was=$(jq -r .entry.last_accessed_at "$state")
at=$(jq -r .at "$state")
case "$key" in "$prefix"*) ;; *)
    say "kept: $key is not in the group $prefix*"
    exit 0
    ;;
esac
if ! list=$(entries); then
    warn "could not list the caches; kept $key"
    exit 0
fi
now=$(printf '%s\n' "$list" | jq -c --argjson id "$id" --arg key "$key" 'select(.id == $id and .key == $key)')
[ -n "$now" ] || {
    say "kept: $key (id $id) is already gone"
    exit 0
}
is=$(jq -r .last_accessed_at <<<"$now")
[ "$is" \> "$was" ] || {
    say "kept: $key was not restored by this run (last access $is, was $was)"
    exit 0
}
# A replacement: another entry of the group on develop, created after the snapshot.
fresh=$(printf '%s\n' "$list" | jq -r --arg key "$key" --arg at "$at" 'select(.key != $key and .created_at > $at) | .key' | head -n 1)
[ -n "$fresh" ] || {
    say "kept: $key is the only entry, nothing was saved since the snapshot at $at"
    exit 0
}
size=$(jq -r '.size_in_bytes / 1000000 | floor' <<<"$now")
if [ "${DRY_RUN:-}" = 1 ]; then
    say "would delete $key on $ref ($size MB, id $id, restored $is; replaced by $fresh)"
    exit 0
fi
enc=$(jq -rn --arg k "$key" --arg r "$ref" '"key=\($k|@uri)&ref=\($r|@uri)"')
if gh api -X DELETE "repos/$repo/actions/caches?$enc" --silent; then
    say "deleted $key on $ref ($size MB, id $id, restored $is; replaced by $fresh)"
else
    warn "could not delete $key; cache-gc.yml removes it after the run"
fi
exit 0
