#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
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
11	refs/heads/develop	playwright-Linux-aaaa	$now	$now
12	refs/heads/develop	playwright-webkit-Linux-bbbb	$older	$now
13	refs/heads/develop	forgejo-Linux-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa	$older	$now
14	refs/heads/develop	forgejo-Linux-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb	$now	$now
15	refs/heads/develop	gateway-Linux-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc	$older	$now
16	refs/heads/develop	codeql-overlay-base-database-1-c801913f1ee29663-javascript-2.27.4-aaaa-1-1	$older	$older
17	refs/heads/develop	codeql-overlay-base-database-1-c801913f1ee29663-javascript-2.27.5-bbbb-2-1	$now	$now
18	refs/heads/develop	codeql-overlay-base-database-1-5faf507a7f104e7c-java-2.27.5-cccc-3-1	$older	$now
19	refs/heads/develop	v0-rust-windows-clippy-0123abcd-4567ef01	$now	$now
EOF
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
*"-X DELETE"*) echo "${4##*/}" >>"$FAKE_DELETED" ;;
*actions/caches*)
    while IFS=$'\t' read -r id ref key created used size; do
        jq -cn --argjson id "$id" --arg ref "$ref" --arg key "$key" --arg c "$created" --arg u "$used" --arg s "$size" \
            '{id: $id, ref: $ref, key: $key, created_at: $c, last_accessed_at: $u, size_in_bytes: (if $s == "" then 1000000 else ($s | tonumber) end)}'
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
# 11, 12: the Chromium and the WebKit browser entries are two groups, so the older one stays.
# 13: a pinned download superseded by a new pin (14) goes; 15, the only Gateway entry, stays.
# 16: an older CodeQL overlay database of a language (17, the newest, stays; 18 is the only Java one).
# 19: a windows-clippy entry, which nothing saves any more.
[ "$got" = "1 2 4 5 9 13 16 19 " ] || bad "deleted '$got', want '1 2 4 5 9 13 16 19 '"

: >"$tmp/deleted"
PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_CACHES="$tmp/caches.tsv" FAKE_DELETED="$tmp/deleted" DRY_RUN=1 \
    "$here/cache-gc.sh" >"$tmp/out" 2>&1 || bad "DRY_RUN=1 failed"
[ ! -s "$tmp/deleted" ] || bad "DRY_RUN=1 deleted entries"
grep -q 'gh-readonly-queue/develop/pr-7-abc' "$tmp/out" || bad "DRY_RUN=1 does not list the queue entry"
# The inventory lists every entry, deleted or kept, with ref, size and times.
grep -q '^  cargo-xwin-0.23.1-x86_64  refs/heads/develop  1  ' "$tmp/out" || bad "the inventory lacks the develop cargo-xwin entry"
[ "$(grep -c '^  ' "$tmp/out")" = 19 ] || bad "the inventory should list all 19 entries: $(cat "$tmp/out")"

# The budget: over BUDGET_MB the cheapest-to-rebuild trunk entries go first (mutants, windows, gates, linux); small
# ones (node, playwright) never. The sixth column of the fake list is the size in bytes.
cat >"$tmp/big.tsv" <<EOF
21	refs/heads/develop	v0-rust-linux-0123abcd-4567ef01	$now	$now	2000000000
22	refs/heads/develop	v0-rust-gates-0123abcd-4567ef01	$now	$now	3000000000
23	refs/heads/develop	v0-rust-mutants-0123abcd-4567ef01	$now	$now	3000000000
24	refs/heads/develop	v0-rust-windows-0123abcd-4567ef01	$now	$now	3000000000
25	refs/heads/develop	node-cache-Linux-x64-npm-9	$now	$now	50000000
EOF
budget_run() { # budget -> the deleted ids, sorted
    : >"$tmp/deleted"
    PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_CACHES="$tmp/big.tsv" FAKE_DELETED="$tmp/deleted" BUDGET_MB="$1" \
        "$here/cache-gc.sh" >"$tmp/out" 2>&1 || bad "budget run failed: $(cat "$tmp/out")"
    sort -n "$tmp/deleted" | tr '\n' ' '
}
got=$(budget_run 9000); [ "$got" = "23 " ] || bad "budget 9000: deleted '$got', want '23 '"
got=$(budget_run 4000); [ "$got" = "22 23 24 " ] || bad "budget 4000: deleted '$got', want '22 23 24 '"
got=$(budget_run 20000); [ -z "$got" ] || bad "budget 20000: deleted '$got', want nothing"

if [ "$fail" = 0 ]; then
    echo "cache-gc-test: ok"
else
    exit 1
fi
