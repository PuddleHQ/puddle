#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/image-pins.sh: a bare origin, a checkout of its develop with two files that hold a pin line, a fake
# `curl` (a registry that answers from $FAKE_REG) and a fake `gh` (a table of open pull requests, a log of calls). Cases:
# `check` passes on equal digest pins and fails on a tag without digest, on two different pins, on a missing pin line and on
# two pin lines in one file; `refresh` with an unchanged digest does nothing; a moved digest rewrites both files in one commit on
# ci/refresh-image-pins (never on develop), opens one pull request and arms auto-merge; the same digest again opens no second
# one; a newer digest updates the branch with a lease; a registry error, a non-200 answer and a digest that is not sha256 change
# nothing and fail; a dry run pushes nothing; a caught-up open pull request is closed. Prints ok only when every case passes.
set -uo pipefail
# A hook exports GIT_DIR and GIT_INDEX_FILE: without this every git call below would hit the real repository.
# shellcheck disable=SC2046
unset $(git rev-parse --local-env-vars)

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/image-pins-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "image-pins-test: FAIL: $*" >&2; fail=1; }
G() { git -c user.email=t@t -c user.name=t "$@"; }
ref=mcr.microsoft.com/devcontainers/base:debian
d1=sha256:$(printf '1%.0s' $(seq 64))
d2=sha256:$(printf '2%.0s' $(seq 64))
d3=sha256:$(printf '3%.0s' $(seq 64))
a=crates/puddle-api/src/workspaces.rs
b=crates/puddle-workspace/src/workspaces.rs
c=crates/puddle-vm-tests/src/lib.rs
d=crates/puddle-compute/src/fake/mod.rs
branch=ci/refresh-image-pins

mkdir "$tmp/bin"
cat >"$tmp/bin/curl" <<'CURL'
#!/usr/bin/env bash
# fake registry: $FAKE_REG holds "<status> <digest header value>"; "down" fails the request
echo "curl $*" >>"$FAKE_LOG"
grep -q '^down$' "$FAKE_REG" && exit 6
read -r status digest <"$FAKE_REG"
printf 'HTTP/2 %s\r\ncontent-type: application/vnd.oci.image.index.v1+json\r\n' "$status"
[ -z "$digest" ] || printf 'docker-content-digest: %s\r\n' "$digest"
CURL
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# fake gh: $FAKE_PR is the open refresh pull request's number (empty: none)
echo "$*" >>"$FAKE_LOG"
case "$1 $2" in
"pr list") echo "${FAKE_PR:-}" ;;
"pr create") echo "https://github.com/o/r/pull/41"; : >"$FAKE_CREATED" ;;
"pr merge" | "pr close") ;;
*) exit 1 ;;
esac
GH
chmod +x "$tmp/bin/curl" "$tmp/bin/gh"

# fresh <pin a> <pin b> [extra line for file a]: a new origin and checkout with the two files.
fresh() {
	rm -rf "$tmp/origin.git" "$tmp/w"
	git init -q --bare -b develop "$tmp/origin.git"
	git clone -q "$tmp/origin.git" "$tmp/w" 2>/dev/null
	mkdir -p "$tmp/w/ci" "$tmp/w/$(dirname "$a")" "$tmp/w/$(dirname "$b")" "$tmp/w/$(dirname "$c")" "$tmp/w/$(dirname "$d")"
	cp "$here/image-pins.sh" "$tmp/w/ci/"
	printf 'pub const DEFAULT_IMAGE: &str = "%s";\n%s\n' "$1" "${3:-}" >"$tmp/w/$a"
	printf 'impl W {\n    pub const DEFAULT_MAINTENANCE_IMAGE: &'"'"'static str =\n        "%s";\n}\n' "$2" >"$tmp/w/$b"
	printf '/// The image the VM tier boots.\npub const DEBIAN_DEVCONTAINER: &str = "%s";\n' "${4:-$1}" >"$tmp/w/$c"
	printf 'impl FakeRuntime {\n    pub const DEBIAN: &'"'"'static str = "%s";\n}\n' "${5:-$1}" >"$tmp/w/$d"
	(cd "$tmp/w" && G checkout -q -B develop && G add -A && G commit -q -m init && G push -q origin develop)
	: >"$tmp/log"
	rm -f "$tmp/created"
}
run() { # run <subcommand>: output in $tmp/out, status in $rc
	(cd "$tmp/w" && env PATH="$tmp/bin:$PATH" CURL=curl GH=gh GITHUB_REPOSITORY=o/r GITHUB_STEP_SUMMARY=/dev/null \
		FAKE_LOG="$tmp/log" FAKE_REG="$tmp/reg" FAKE_PR="${FAKE_PR:-}" FAKE_CREATED="$tmp/created" DRY_RUN="${DRY_RUN:-}" \
		ci/image-pins.sh "$1") >"$tmp/out" 2>&1
	rc=$?
}
reg() { printf '%s\n' "$*" >"$tmp/reg"; }
pins() { (cd "$tmp/w" && grep -ho 'mcr.microsoft[^"]*' "$a" "$b" "$c" "$d"); }
on_develop() { (cd "$tmp/w" && G checkout -q develop); }
remote_branches() { git -C "$tmp/origin.git" branch --list | tr -d ' *'; }
on_branch() { git -C "$tmp/origin.git" show "$branch:$1"; }

# check
fresh "$ref@$d1" "$ref@$d1"
run check
{ [ "$rc" = 0 ] && grep -q "image pins ok: $ref@$d1" "$tmp/out"; } || bad "check: equal digest pins must pass: $(cat "$tmp/out")"
fresh "$ref" "$ref@$d1"
run check
{ [ "$rc" != 0 ] && grep -q 'has no sha256 digest' "$tmp/out"; } || bad "check: a bare tag must fail"
fresh "$ref@$d1" "$ref@$d2"
run check
{ [ "$rc" != 0 ] && grep -q 'must be the same reference' "$tmp/out"; } || bad "check: two different digests must fail"
fresh "$ref@$d1" "other.example/x/y:tag@$d1"
run check
{ [ "$rc" != 0 ] && grep -q 'same reference' "$tmp/out"; } || bad "check: two different images must fail"
fresh "$ref@sha256:abc" "$ref@$d1"
run check
[ "$rc" != 0 ] || bad "check: a short digest must fail"
fresh "$ref@$d1" "$ref@$d1" "" "$ref@$d2"
run check
{ [ "$rc" != 0 ] && grep -q "$c pins" "$tmp/out"; } || bad "check: a differing third pin must fail"
fresh "$ref@$d1" "$ref@$d1" 'pub const OTHER: &str = "mcr.microsoft.com/x/y:z";'
run check
{ [ "$rc" != 0 ] && grep -q 'exactly one pin line' "$tmp/out"; } || bad "check: two pin lines in one file must fail"
fresh "$ref@$d1" "$ref@$d1"
printf 'fn nothing() {}\n' >"$tmp/w/$b"
run check
{ [ "$rc" != 0 ] && grep -q 'exactly one pin line' "$tmp/out"; } || bad "check: a file with no pin line must fail"

# refresh: nothing moved
fresh "$ref@$d1" "$ref@$d1"
reg "200 $d1"
run refresh
{ [ "$rc" = 0 ] && grep -q 'nothing to refresh' "$tmp/out" && [ ! -e "$tmp/created" ]; } ||
	bad "refresh: an unchanged digest must do nothing: $(cat "$tmp/out")"
grep -q "manifests/debian" "$tmp/log" || bad "refresh: the registry request must name the tag"
[ "$(remote_branches)" = develop ] || bad "refresh: nothing moved but a branch was pushed"

# refresh: a bare tag is pinned (the first pin)
fresh "$ref" "$ref"
reg "200 $d1"
run refresh
{ [ "$rc" = 0 ] && [ "$(pins | sort -u | wc -l)" = 1 ] && pins | head -n 1 | grep -q "$d1"; } ||
	bad "refresh: a bare tag must be pinned in both files: $(cat "$tmp/out")"

# refresh: a moved digest
fresh "$ref@$d1" "$ref@$d1"
before=$(git -C "$tmp/origin.git" rev-parse develop)
reg "200 $d2"
run refresh
[ "$rc" = 0 ] || bad "refresh: a moved digest failed: $(cat "$tmp/out")"
[ "$(git -C "$tmp/origin.git" rev-parse develop)" = "$before" ] || bad "refresh: develop must never be pushed to"
git -C "$tmp/origin.git" rev-parse -q --verify "refs/heads/$branch" >/dev/null || bad "refresh: the branch was not pushed"
[ "$(git -C "$tmp/origin.git" rev-list --count "develop..$branch")" = 1 ] || bad "refresh: the branch must carry exactly one commit"
changed=$(git -C "$tmp/origin.git" diff --name-only develop "$branch" | sort | tr '\n' ' ')
[ "$changed" = "$a $d $c $b " ] || bad "refresh: the commit must touch exactly the four pin files, got $changed"
[ "$(on_branch "$a" | grep -c "$ref@$d2")" = 1 ] || bad "refresh: file a lacks the new digest"
[ "$(on_branch "$b" | grep -c "$ref@$d2")" = 1 ] || bad "refresh: file b lacks the new digest"
[ "$(on_branch "$c" | grep -c "$ref@$d2")" = 1 ] || bad "refresh: file c lacks the new digest"
[ "$(on_branch "$d" | grep -c "$ref@$d2")" = 1 ] || bad "refresh: file d lacks the new digest"
[ "$(on_branch "$b" | grep -c 'pub const DEFAULT_MAINTENANCE_IMAGE')" = 1 ] || bad "refresh: the rewrite must keep the rest of the file"
[ -e "$tmp/created" ] || bad "refresh: no pull request was opened"
grep -q "^pr create .*--base develop --head $branch" "$tmp/log" || bad "refresh: the pull request must target develop from the branch"
grep -q '^pr merge .*--auto --squash 41' "$tmp/log" || bad "refresh: auto-merge was not armed with squash"
[ "$(grep -c '^pr create' "$tmp/log")" = 1 ] || bad "refresh: exactly one pull request"

# the same digest again with the pull request open (the workflow starts from develop each night, which still pins the old
# digest, while the branch already holds the new one): nothing is pushed and no second pull request opens
on_develop
: >"$tmp/log"
rm -f "$tmp/created"
branch_before=$(git -C "$tmp/origin.git" rev-parse "$branch")
FAKE_PR=41 run refresh
{ [ "$rc" = 0 ] && [ ! -e "$tmp/created" ] && grep -q 'already pins' "$tmp/out"; } ||
	bad "refresh: an open pull request must not be opened twice: $(cat "$tmp/out")"
[ "$(git -C "$tmp/origin.git" rev-parse "$branch")" = "$branch_before" ] || bad "refresh: an unchanged branch must not be pushed again"
# a newer digest updates the same branch
on_develop
reg "200 $d3"
FAKE_PR=41 run refresh
[ "$rc" = 0 ] || bad "refresh: a newer digest failed: $(cat "$tmp/out")"
[ "$(on_branch "$a" | grep -c "$ref@$d3")" = 1 ] || bad "refresh: the branch must carry the newest digest"
[ "$(git -C "$tmp/origin.git" rev-list --count "develop..$branch")" = 1 ] || bad "refresh: still one commit after an update"
[ ! -e "$tmp/created" ] || bad "refresh: a newer digest must not open a second pull request"

# nothing changes on registry trouble
for answer in "404 $d2" "500 " "200 " "200 md5:abc" "200 sha256:zz" down; do
	fresh "$ref@$d1" "$ref@$d1"
	reg "$answer"
	run refresh
	{ [ "$rc" != 0 ] && [ "$(pins | grep -c "$d1")" = 4 ] && [ ! -e "$tmp/created" ] && [ "$(remote_branches)" = develop ]; } ||
		bad "refresh: registry answer '$answer' must fail and change nothing: $(cat "$tmp/out")"
done

# dry run
fresh "$ref@$d1" "$ref@$d1"
reg "200 $d2"
DRY_RUN=1 run refresh
{ [ "$rc" = 0 ] && grep -q 'would pin' "$tmp/out" && [ "$(pins | grep -c "$d1")" = 4 ] && [ ! -e "$tmp/created" ] && [ "$(remote_branches)" = develop ]; } ||
	bad "refresh: a dry run must change nothing: $(cat "$tmp/out")"

# a caught-up pull request is closed, and a dry run leaves it
fresh "$ref@$d1" "$ref@$d1"
reg "200 $d1"
FAKE_PR=41 run refresh
{ [ "$rc" = 0 ] && grep -q '^pr close .*41' "$tmp/log"; } || bad "refresh: a caught-up pull request must be closed"
fresh "$ref@$d1" "$ref@$d1"
FAKE_PR=41 DRY_RUN=1 run refresh
! grep -q '^pr close' "$tmp/log" || bad "refresh: a dry run must not close a pull request"

# the real files hold a consistent pin
(cd "$here/.." && ci/image-pins.sh check >/dev/null) || bad "the repository's own pins fail the check"

[ "$fail" = 0 ] && echo "image-pins-test: ok"
exit "$fail"
