#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Proves the flake-rule layout merges cleanly: one branch adds a rule, another adds a second rule, a third
# flips a counted rule to failing, and `git merge-tree` merges all three with no conflict and keeps every
# change. A rule list kept in one set or one table would conflict on exactly these edits.
# usage: scripts/flake-rules-test.sh
set -eu

# check.sh runs under hooks that export GIT_DIR and friends; a leaked one would point `git init` at the real repository.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_PREFIX GIT_OBJECT_DIRECTORY GIT_NAMESPACE

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

counted=$(cd "$root/scripts/flake-rules" && grep -l '^status: counted$' ./*.rule | head -n 1 | sed 's|^\./||')
[ -n "$counted" ] || { echo "flake-rules-test: no counted rule to flip" >&2; exit 1; }

git init -q "$tmp/r"
cd "$tmp/r"
git config user.email t@example.invalid
git config user.name t
mkdir -p scripts/flake-rules
cp "$root"/scripts/flake-rules/*.rule scripts/flake-rules/
git add -A
git commit -q -m base
git branch -q base

new_rule() {
    git checkout -q -b "$1" base
    printf 'status: counted\nfixture: x.bad.rs.txt\nas: crates/x/tests/t.rs\nfix: use the kit\n' >"scripts/flake-rules/$1.rule"
    git add -A
    git commit -q -m "add $1"
}
new_rule zz1
new_rule zz2
git checkout -q -b flip base
sed -i 's/^status: counted$/status: failing/' "scripts/flake-rules/$counted"
git add -A
git commit -q -m flip

tree=$(git merge-tree --write-tree zz1 zz2) || { echo "flake-rules-test: two added rules conflict" >&2; exit 1; }
both=$(git commit-tree "$tree" -p zz1 -p zz2 -m both)
tree=$(git merge-tree --write-tree "$both" flip) || { echo "flake-rules-test: an added rule and a flip conflict" >&2; exit 1; }
for want in zz1.rule zz2.rule; do
    git cat-file -e "$tree:scripts/flake-rules/$want" || { echo "flake-rules-test: merge lost $want" >&2; exit 1; }
done
git show "$tree:scripts/flake-rules/$counted" | grep -q '^status: failing$' || { echo "flake-rules-test: merge lost the flip of $counted" >&2; exit 1; }
echo "flake-rules-test: pass"
