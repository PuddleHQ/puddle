#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/cache-gc.sh with a fake `gh` that serves a fixed cache list and records deletions:
# a merge queue entry goes whatever its age, a fresh pull request entry and the newest develop entry
# of a group stay, a superseded develop entry and a stale pull request entry go, and the cargo-xwin
# entry on develop stays. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/cache-gc-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "cache-gc-test: FAIL: $*" >&2; fail=1; }

now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
old=$(date -u -d '3 days ago' +%Y-%m-%dT%H:%M:%SZ)
older=$(date -u -d '4 days ago' +%Y-%m-%dT%H:%M:%SZ)
# id ref key created last-used
cat >"$tmp/caches.tsv" <<EOF
1	refs/heads/gh-readonly-queue/develop/pr-7-abc	node-cache-Linux-x64-npm-1	$now	$now
2	refs/heads/gh-readonly-queue/develop/pr-8-def	cargo-xwin-0.23.1-x86_64	$now	$now
3	refs/pull/7/merge	node-cache-Linux-x64-npm-2	$now	$now
4	refs/pull/6/merge	node-cache-Linux-x64-npm-3	$old	$old
5	refs/heads/develop	v0-rust-linux-0123abcd-4567ef01	$older	$now
6	refs/heads/develop	v0-rust-linux-89abcdef-01234567	$now	$now
7	refs/heads/develop	cargo-xwin-0.23.1-x86_64	$older	$older
8	refs/heads/gh-readonly-queue-lookalike	node-cache-Linux-x64-npm-4	$now	$now
9	refs/heads/develop	notices-ok-1111111111111111111111111111111111111111111111111111111111111111	$older	$now
10	refs/heads/develop	notices-ok-2222222222222222222222222222222222222222222222222222222222222222	$now	$now
EOF
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
*"-X DELETE"*) echo "${4##*/}" >>"$FAKE_DELETED" ;;
*actions/caches*)
    while IFS=$'\t' read -r id ref key created used; do
        jq -cn --argjson id "$id" --arg ref "$ref" --arg key "$key" --arg c "$created" --arg u "$used" \
            '{id: $id, ref: $ref, key: $key, created_at: $c, last_accessed_at: $u, size_in_bytes: 1000000}'
    done <"$FAKE_CACHES"
    ;;
*cache/usage*) echo "cache total: 0 MB" ;;
*) exit 1 ;;
esac
GH
chmod +x "$tmp/bin/gh"

: >"$tmp/deleted"
if ! PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_CACHES="$tmp/caches.tsv" FAKE_DELETED="$tmp/deleted" \
    "$here/cache-gc.sh" >"$tmp/out" 2>&1; then
    bad "cache-gc.sh failed: $(cat "$tmp/out")"
fi
got=$(sort -n "$tmp/deleted" | tr '\n' ' ')
# 1, 2: queue refs, fresh; 4: a stale pull request entry; 5: a superseded develop rust entry.
# 8: a fresh entry on a branch whose name only starts like the queue prefix stays.
# 9: a superseded notices stamp on develop (10, the newest, stays).
[ "$got" = "1 2 4 5 9 " ] || bad "deleted '$got', want '1 2 4 5 9 '"

: >"$tmp/deleted"
PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_CACHES="$tmp/caches.tsv" FAKE_DELETED="$tmp/deleted" DRY_RUN=1 \
    "$here/cache-gc.sh" >"$tmp/out" 2>&1 || bad "DRY_RUN=1 failed"
[ ! -s "$tmp/deleted" ] || bad "DRY_RUN=1 deleted entries"
grep -q 'gh-readonly-queue/develop/pr-7-abc' "$tmp/out" || bad "DRY_RUN=1 does not list the queue entry"
# The inventory lists every entry, deleted or kept, with ref, size and times.
grep -q '^  cargo-xwin-0.23.1-x86_64  refs/heads/develop  1  ' "$tmp/out" || bad "the inventory lacks the develop cargo-xwin entry"
[ "$(grep -c '^  ' "$tmp/out")" = 10 ] || bad "the inventory should list all 10 entries: $(cat "$tmp/out")"

if [ "$fail" = 0 ]; then
    echo "cache-gc-test: ok"
else
    exit 1
fi
