#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Refills the guest tools cache (vm-linux.yml, `vm-guest-env-tools-*`) after GitHub evicted it. The entry is saved only
# by a develop vm-linux run (the nightly or a dispatch), and while it exists every pull request and merge queue run
# restores it in 5 s (321 MB). But the repo's cache set is over the 10 GB cap whenever a Rust entry is replaced, and
# GitHub then purges, oldest first, with no log: the entry saved at 11:10 UTC on 2026-11-06 was gone by 12:50, and the
# queue runs after that missed until the next nightly. So: when no develop entry of that key family is left, and no
# nightly or dispatch vm-linux run on develop started in the last HEAL_HOURS (default 8; that run is the refill, or it
# failed and a retry waits), dispatch one. The test fills the cache from the real mirrors and the save step stores it.
# Called by cache-gc.yml after every develop and merge queue run. DRY_RUN=1 only says what it would do.
# Self-test: ci/tools-cache-heal-test.sh.
# Needs GH_TOKEN, GITHUB_REPOSITORY and, to dispatch, `actions: write`.
set -euo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
hours=${HEAL_HOURS:-8}
cutoff=$(date -u -d "$hours hours ago" +%Y-%m-%dT%H:%M:%SZ)

present=$(gh api "repos/$repo/actions/caches?key=vm-guest-env-tools-&ref=refs/heads/develop&per_page=1" --jq '.total_count')
if [ "$present" != 0 ]; then
    echo "tools cache: present on develop, nothing to do"
    exit 0
fi

recent=$(gh api "repos/$repo/actions/workflows/vm-linux.yml/runs?branch=develop&per_page=30" |
    jq --arg cutoff "$cutoff" '[.workflow_runs[] | select((.event == "schedule" or .event == "workflow_dispatch") and .created_at >= $cutoff)] | length')
if [ "$recent" != 0 ]; then
    echo "tools cache: missing, but $recent nightly or dispatch vm-linux run(s) on develop started within $hours h; waiting for it"
    exit 0
fi

if [ "${DRY_RUN:-}" = 1 ]; then
    echo "tools cache: missing; DRY_RUN=1, would dispatch vm-linux.yml on develop"
    exit 0
fi
echo "tools cache: missing and no recent develop run; dispatching vm-linux.yml on develop to refill it"
gh api -X POST "repos/$repo/actions/workflows/vm-linux.yml/dispatches" -f ref=develop --silent
