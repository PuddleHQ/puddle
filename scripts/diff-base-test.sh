#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests scripts/diff-base.sh in a throwaway repository: a pull request merge ref whose base branch moved
# on (another change added a file) is measured from the base's current tip, so the file the PR did not
# touch is not in `git diff <base>`; other runs keep DIFF_BASE; a missing or unknown DIFF_BASE falls back
# to the merge base with origin/develop.
set -eu
# A git hook runs this with GIT_DIR and friends set; they would point the throwaway repository at the real one.
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q -b develop .
git config user.email t@example.invalid && git config user.name t
mkdir scripts && cp "$here/diff-base.sh" scripts/
fail() { echo "diff-base-test: FAIL: $1" >&2; exit 1; }
commit() { echo "$2" >"$1" && git add "$1" && git commit -qm "$1"; }

commit a 1
old=$(git rev-parse HEAD)
git checkout -q -b pr
commit pr-file 1
pr=$(git rev-parse HEAD)
git checkout -q develop
commit other-file 1 # lands after the PR's base commit, like another PR's file
tip=$(git rev-parse HEAD)
git checkout -q --detach "$tip"
git merge -q --no-ff -m merge-ref "$pr"
merge=$(git rev-parse HEAD)
git update-ref refs/remotes/origin/develop "$tip"

run() { env -u DIFF_BASE -u GITHUB_EVENT_NAME "$@" scripts/diff-base.sh 2>/dev/null; }
# The chosen base is named once on stderr, so a log shows it; stdout stays the bare commit.
says() { want=$1 && shift && out=$(env -u DIFF_BASE -u GITHUB_EVENT_NAME "$@" scripts/diff-base.sh 2>&1 >/dev/null) &&
    [ "$out" = "$want" ] || fail "stderr: want '$want', got '$out'"; }

got=$(run DIFF_BASE="$old" GITHUB_EVENT_NAME=pull_request)
[ "$got" = "$tip" ] || fail "pull request run: want the base tip $tip, got $got"
git diff --name-only "$got" | grep -qx pr-file || fail "the PR's own file must stay in the diff"
git diff --name-only "$got" | grep -qx other-file && fail "a file the PR did not touch is in the diff"
git diff --name-only "$old" | grep -qx other-file || fail "fixture: the stale base must show the other file"

says "diff base: $tip (HEAD^1)" DIFF_BASE="$old" GITHUB_EVENT_NAME=pull_request
says "diff base: $old (DIFF_BASE)" DIFF_BASE="$old" GITHUB_EVENT_NAME=push

got=$(run DIFF_BASE="$old" GITHUB_EVENT_NAME=push)
[ "$got" = "$old" ] || fail "push run must keep DIFF_BASE, got $got"
got=$(run DIFF_BASE="$old")
[ "$got" = "$old" ] || fail "no event must keep DIFF_BASE, got $got"

# A pull request run on a plain commit (no merge ref) keeps DIFF_BASE.
git checkout -q --detach "$pr"
got=$(run DIFF_BASE="$old" GITHUB_EVENT_NAME=pull_request)
[ "$got" = "$old" ] || fail "non-merge HEAD must keep DIFF_BASE, got $got"

# No DIFF_BASE, or one this clone lacks: the merge base with origin/develop.
git checkout -q --detach "$merge"
got=$(run)
[ "$got" = "$tip" ] || fail "no DIFF_BASE: want merge base $tip, got $got"
got=$(run DIFF_BASE=0000000000000000000000000000000000000001)
[ "$got" = "$tip" ] || fail "unknown DIFF_BASE: want merge base $tip, got $got"
says "diff base: $tip (merge-base origin/develop)"
git update-ref -d refs/remotes/origin/develop
[ -z "$(run)" ] || fail "nothing to measure from must print nothing"
[ -z "$(env -u DIFF_BASE -u GITHUB_EVENT_NAME scripts/diff-base.sh 2>&1)" ] || fail "nothing to measure from must say nothing"
echo "diff-base-test: ok"
