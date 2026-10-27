#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/cache-drop.sh with a stub `gh` that answers the cache list from a fixture and records
# every write. Each case states what must be deleted, and most state that nothing may be: a run that is
# not on develop, a run that saved nothing (an exact hit, a failed save), an entry this run did not
# restore, one that is gone, another group, another ref, a replacement a concurrent run saved. Not seen: the real GitHub API (its query filters
# are ignored by the stub, so the script's own filtering is what is tested) and rust-cache itself.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/cache-drop-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "cache-drop-test: FAIL: $*" >&2
    exit 1
}

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'STUB'
#!/bin/sh
# gh api [--paginate] <path> [-X DELETE] [--silent]
case " $* " in
*" -X DELETE "*)
    echo "$*" >>"$STUB_WRITES"
    [ "${STUB_DELETE_FAILS:-}" != 1 ] || exit 1
    ;;
*)
    echo "$*" >>"$STUB_READS"
    [ "${STUB_LIST_FAILS:-}" != 1 ] || exit 1
    cat "$STUB_LIST"
    ;;
esac
STUB
chmod +x "$tmp/bin/gh"

P=v0-rust-linux-
OLD=${P}Linux-x64-a34eac98-6e1d5580
NEW=${P}Linux-x64-a34eac98-c213da47
entry() { # id key ref created accessed
    printf '{"id":%s,"key":"%s","ref":"%s","created_at":"%s","last_accessed_at":"%s","size_in_bytes":6815000000}' "$@"
}
list() { # entries...: a one-page API answer
    local IFS=,
    printf '{"total_count":%s,"actions_caches":[%s]}\n' "$#" "$*" >"$tmp/list.json"
}
D=refs/heads/develop

# run <ref> <mode> [env...]: runs the script, leaves its output in $tmp/out
run() {
    local ref=$1 mode=$2
    shift 2
    : >"$tmp/writes" && : >"$tmp/reads"
    env PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r GITHUB_REF="$ref" CACHE_DROP_STATE="$tmp/state.json" \
        STUB_WRITES="$tmp/writes" STUB_READS="$tmp/reads" STUB_LIST="$tmp/list.json" "$@" \
        "$here/cache-drop.sh" "$mode" "$P" >"$tmp/out" 2>&1 || fail "$mode exited non-zero: $(cat "$tmp/out")"
}
deleted() { # <n> <label>: how many deletes were made
    [ "$(wc -l <"$tmp/writes")" = "$1" ] || fail "$2: want $1 delete(s), got: $(cat "$tmp/writes") / $(cat "$tmp/out")"
}
said() { grep -q -- "$1" "$tmp/out" || fail "$2: output lacks '$1': $(cat "$tmp/out")"; }

# The develop restore finds OLD (newest of the group) among other groups, another ref and a newer
# pull request entry of the same group.
list \
    "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-24T08:00:00Z)" \
    "$(entry 2 "${P}Linux-x64-a34eac98-00000000" $D 2026-10-20T08:00:00Z 2026-10-20T08:00:00Z)" \
    "$(entry 3 v0-rust-windows-Windows_NT-x64-5db6e08f-c213da47 $D 2026-10-25T08:00:00Z 2026-10-25T08:00:00Z)" \
    "$(entry 4 "$NEW" refs/pull/9/merge 2026-10-25T09:00:00Z 2026-10-25T09:00:00Z)" \
    "$(entry 5 cargo-xwin-0.23.1-x86_64 $D 2026-10-24T07:00:00Z 2026-10-24T07:00:00Z)"

# 1. snapshot picks the newest develop entry of the group only.
run $D snapshot
[ "$(jq -r .entry.key "$tmp/state.json")" = "$OLD" ] || fail "snapshot remembered $(cat "$tmp/state.json")"
deleted 0 "snapshot"

# 2. restored (its access moved) and the replacement saved: exactly the remembered entry goes, by key and ref.
SAVED="$(entry 6 "$NEW" $D 2026-10-26T10:20:00.123456Z 2026-10-26T10:20:00.123456Z)"
list \
    "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00.5Z)" \
    "$SAVED" \
    "$(entry 3 v0-rust-windows-Windows_NT-x64-5db6e08f-c213da47 $D 2026-10-25T08:00:00Z 2026-10-25T08:00:00Z)" \
    "$(entry 5 cargo-xwin-0.23.1-x86_64 $D 2026-10-24T07:00:00Z 2026-10-26T10:00:00Z)"
snap_at() { jq -c --arg at "$1" '.at = $at' "$tmp/state.json" >"$tmp/state2.json" && mv "$tmp/state2.json" "$tmp/state.json"; }
snap_at 2026-10-26T10:00:00Z
run $D drop
deleted 1 "restored entry"
grep -q "caches?key=$OLD&ref=refs%2Fheads%2Fdevelop" "$tmp/writes" || fail "the delete is not by exact key and ref: $(cat "$tmp/writes")"
said "deleted $OLD" "restored entry"
said "replaced by $NEW" "restored entry"

# 3b. a newer entry of a concurrent run counts as the replacement; only the remembered one goes.
list \
    "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00Z)" \
    "$(entry 7 "${P}Linux-x64-ffffffff-c213da47" $D 2026-10-26T10:05:00Z 2026-10-26T10:05:00Z)"
snap_at 2026-10-26T10:00:00Z
run $D drop
deleted 1 "concurrent save"
grep -q "key=$OLD&" "$tmp/writes" || fail "concurrent save: wrong entry deleted: $(cat "$tmp/writes")"

# 3c. nothing saved since the snapshot (exact hit, failed or cancelled save): the only entry stays.
list "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00Z)"
run $D drop
deleted 0 "no replacement"
said "nothing was saved" "no replacement"
# An entry from before the snapshot (older than the restore) is no replacement either.
list \
    "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00Z)" \
    "$(entry 8 "${P}Linux-x64-a34eac98-11111111" $D 2026-10-25T08:00:00Z 2026-10-25T08:00:00Z)"
run $D drop
deleted 0 "older sibling"

# 4. every guard keeps the entry.
for r in refs/pull/9/merge refs/heads/gh-readonly-queue/develop/pr-9-abc refs/heads/main refs/heads/task/x ""; do
    run "$r" drop
    deleted 0 "ref '$r'"
    [ ! -s "$tmp/reads" ] || fail "ref '$r': the script listed caches"
    said "not a develop run" "ref '$r'"
done
# The workflow's own condition is the first guard; the script's is the second: a snapshot off develop is empty.
run refs/pull/9/merge snapshot
[ ! -s "$tmp/state.json" ] || fail "a pull request run remembered an entry"
run refs/pull/9/merge drop
deleted 0 "pull request after an empty snapshot"

list "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-24T08:00:00Z)"
run $D snapshot
run $D drop
deleted 0 "not restored (access unchanged)"
said "not restored" "not restored"

list "$(entry 6 "$NEW" $D 2026-10-26T10:05:00Z 2026-10-26T10:05:00Z)"
run $D drop
deleted 0 "already gone"
said "already gone" "already gone"

list "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00Z)"
: >"$tmp/state.json"
run $D drop
deleted 0 "nothing remembered"
said "no entry was remembered" "nothing remembered"

# An empty cache: the snapshot remembers nothing, the drop keeps quiet.
list "$(entry 3 v0-rust-windows-Windows_NT-x64-5db6e08f-c213da47 $D 2026-10-25T08:00:00Z 2026-10-25T08:00:00Z)"
run $D snapshot
[ ! -s "$tmp/state.json" ] || fail "snapshot remembered an entry of another group"
run $D drop
deleted 0 "empty group"

# 5. DRY_RUN prints, deletes nothing.
list "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-24T08:00:00Z)"
run $D snapshot
snap_at 2026-10-26T10:00:00Z
list "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00Z)" "$SAVED"
run $D drop DRY_RUN=1
deleted 0 "dry run"
said "would delete $OLD" "dry run"

# 6. a failing API never fails the job and never deletes.
run $D drop STUB_LIST_FAILS=1
deleted 0 "list fails"
said "::warning::" "list fails"
run $D drop STUB_DELETE_FAILS=1
said "::warning::" "delete fails"
run $D snapshot STUB_LIST_FAILS=1
[ ! -s "$tmp/state.json" ] || fail "a failed snapshot left an entry"

# 7. a wrong group in the state file is refused (the state is a file a step wrote, not a trusted value).
printf '{"entry":%s,"at":"2026-10-26T10:00:00Z"}\n' "$(entry 1 cargo-xwin-0.23.1-x86_64 $D 2026-10-24T07:00:00Z 2026-10-24T07:00:00Z)" >"$tmp/state.json"
list "$(entry 1 cargo-xwin-0.23.1-x86_64 $D 2026-10-24T07:00:00Z 2026-10-26T10:00:00Z)" "$SAVED"
run $D drop
deleted 0 "other group in the state"
said "not in the group" "other group in the state"

# 7b. the drop's outcome lands in the job summary; the snapshot writes nothing there.
list "$(entry 1 "$OLD" $D 2026-10-24T08:00:00Z 2026-10-26T10:00:00Z)"
: >"$tmp/summary"
run $D snapshot GITHUB_STEP_SUMMARY="$tmp/summary"
[ ! -s "$tmp/summary" ] || fail "the snapshot wrote to the summary: $(cat "$tmp/summary")"
run $D drop GITHUB_STEP_SUMMARY="$tmp/summary"
grep -q '^- cache-drop: kept: ' "$tmp/summary" || fail "the drop outcome is not in the summary: $(cat "$tmp/summary")"
run refs/pull/9/merge drop GITHUB_STEP_SUMMARY="$tmp/summary"
[ "$(grep -c '^- cache-drop: ' "$tmp/summary")" = 2 ] || fail "a pull request drop left no summary line: $(cat "$tmp/summary")"

# 8. the action's two entry points reach the script the way GitHub runs them (node, INPUT_PREFIX, the workspace).
if command -v node >/dev/null 2>&1; then
    act=$here/../.github/actions/cache-drop
    for step in main post; do
        out=$(env PATH="$tmp/bin:$PATH" GITHUB_WORKSPACE="$here/.." INPUT_PREFIX=$P GITHUB_REPOSITORY=o/r \
            GITHUB_REF=refs/pull/9/merge CACHE_DROP_STATE="$tmp/state.json" node "$act/$step.js" 2>&1) ||
            fail "$step.js exited non-zero: $out"
        grep -q "not a develop run" <<<"$out" || fail "$step.js did not run the script: $out"
    done
    out=$(GITHUB_WORKSPACE="$tmp/nowhere" INPUT_PREFIX=$P node "$act/post.js" 2>&1) || fail "a missing script failed the job: $out"
    grep -q "::warning::cache-drop" <<<"$out" || fail "a missing script gave no warning: $out"
else
    echo "cache-drop-test: node missing, the action wrapper is not tested" >&2
fi

echo "cache-drop-test: ok"
