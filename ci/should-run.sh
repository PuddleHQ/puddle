#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Two gates on "this commit already has a green run". Needs GH_TOKEN, GITHUB_REPOSITORY, GITHUB_SHA
# and `actions: read`; writes to $GITHUB_OUTPUT. Self-test: ci/should-run-test.sh.
#
#   ci/should-run.sh <workflow-file> [--events <event>,...]
#     The nightly gate: sets `run=true|false`. Only scheduled runs can skip; they skip when this
#     workflow already has a green run on exactly this commit (any event, or only the listed ones:
#     the Windows release build and flake report count only nightly and manual runs, so a merge
#     queue run on develop's commit does not stop them).
#
#   ci/should-run.sh <workflow-file> --queue-tested
#     The develop push gate: sets `tested=true|false`. true only on a push to develop with
#     PUDDLE_QUEUE_SKIP_PUSH=on when this workflow has a green merge queue (`merge_group`) run on
#     exactly this commit (the queue fast-forwards develop to the commit it tested). An API error
#     or any doubt gives false, so the push run tests the commit again.
set -euo pipefail

workflow=${1:?usage: should-run.sh <workflow-file> [--events <event>,... | --queue-tested]}
shift
mode=nightly events=""
case ${1:-} in
"") ;;
--events)
    [ "$#" = 2 ] && [ -n "$2" ] || { echo "usage: --events needs one comma-separated list" >&2; exit 2; }
    events=$2
    ;;
--queue-tested) mode=queue; [ "$#" = 1 ] || { echo "usage: too many arguments" >&2; exit 2; } ;;
*) echo "usage: should-run.sh <workflow-file> [--events <event>,... | --queue-tested]" >&2; exit 2 ;;
esac

# green <event or ""> -> the number of green runs of the workflow on this commit (for that event)
green() {
    gh run list -R "$GITHUB_REPOSITORY" --workflow "$workflow" --status success \
        --commit "$GITHUB_SHA" ${1:+--event "$1"} --limit 1 --json databaseId --jq 'length'
}

if [ "$mode" = queue ]; then
    tested=false
    if [ "${PUDDLE_QUEUE_SKIP_PUSH:-}" != on ]; then
        echo "PUDDLE_QUEUE_SKIP_PUSH is not on: the push run tests this commit"
    elif [ "${GITHUB_EVENT_NAME:-}" != push ] || [ "${GITHUB_REF:-}" != refs/heads/develop ]; then
        echo "not a push to develop: the run tests this commit"
    elif ! n=$(green merge_group); then
        echo "could not list the merge queue runs: the push run tests this commit"
    elif [ "$n" = 1 ]; then
        echo "$workflow has a green merge queue run on $GITHUB_SHA"
        tested=true
    else
        echo "$workflow has no green merge queue run on $GITHUB_SHA: the push run tests it"
    fi
    echo "tested=$tested" >>"$GITHUB_OUTPUT"
    exit 0
fi

if [ "${GITHUB_EVENT_NAME:-}" != "schedule" ]; then
    echo "run=true" >>"$GITHUB_OUTPUT"
    exit 0
fi

found=0
if [ -z "$events" ]; then
    found=$(green "")
else
    IFS=, read -r -a list <<<"$events"
    for e in "${list[@]}"; do
        [ "$(green "$e")" = 0 ] || { found=1; break; }
    done
fi
if [ "$found" != "0" ]; then
    echo "$workflow already has a green ${events:+$events }run on $GITHUB_SHA; skipping"
    echo "run=false" >>"$GITHUB_OUTPUT"
else
    echo "run=true" >>"$GITHUB_OUTPUT"
fi
