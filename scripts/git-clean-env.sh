#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Source this at the top of a shell test that runs git: `. "$(dirname "$0")/git-clean-env.sh"`.
# A git hook (pre-commit, pre-push) exports GIT_DIR, GIT_INDEX_FILE, GIT_WORK_TREE and more. Left
# set, a test's `git init` or `git clone --bare` in a temporary directory acts on the hook's
# repository instead, and in a linked worktree that flips the shared `core.bare` to true.
# `git rev-parse --local-env-vars` is git's own list, so a variable added by a later git is covered.
# shellcheck disable=SC2046 # the list is words by design
unset $(git rev-parse --local-env-vars)
