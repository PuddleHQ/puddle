#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Prints the commit a change is measured from for the diff gates (diff-coverage, mutants on the changed
# lines, diff-coverage-fast): $DIFF_BASE if it resolves, else the merge base with origin/develop; nothing
# when neither exists.
#
# A pull request run checks out the merge ref (the base branch's tip merged with the PR head) but
# DIFF_BASE is the base commit the PR was opened or last synced against. When the base branch has moved
# since, `git diff $DIFF_BASE` also contains every line that landed in between, and a gate can fail on
# another change's code. On a pull request run (GITHUB_EVENT_NAME=pull_request) whose HEAD is a merge
# commit whose first parent descends from DIFF_BASE, that first parent is the base branch's current tip,
# so it is used instead: the diff is then exactly what the PR adds. Any other run keeps DIFF_BASE.
#
# stdout is only the commit (callers capture it). One line `diff base: <sha> (<source>)` goes to stderr so
# a log shows the choice; source is HEAD^1, DIFF_BASE or merge-base origin/develop.
set -eu
say() { echo "diff base: $1 ($2)" >&2; }
if [ -n "${DIFF_BASE:-}" ] && git cat-file -e "${DIFF_BASE}^{commit}" 2>/dev/null; then
    if [ "${GITHUB_EVENT_NAME:-}" = pull_request ] && git rev-parse --verify -q 'HEAD^2' >/dev/null &&
        git merge-base --is-ancestor "$DIFF_BASE" 'HEAD^1'; then
        b=$(git rev-parse 'HEAD^1')
        say "$b" HEAD^1
    else
        b=$DIFF_BASE
        say "$b" DIFF_BASE
    fi
    echo "$b"
elif git rev-parse --verify -q origin/develop >/dev/null; then
    b=$(git merge-base HEAD origin/develop)
    say "$b" 'merge-base origin/develop'
    echo "$b"
fi
