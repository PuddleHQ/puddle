#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks ci/vm-skip-policy.sh: a pull request and a merge queue run fail with ::error; schedule, workflow_dispatch and an
# unset GITHUB_EVENT_NAME warn with ::warning and exit 0. Prints ok only when every case passes.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
policy="$here/vm-skip-policy.sh"
fail=0

# run_case <event or "unset"> <want exit> <want marker>
run_case() {
    local event=$1 want_rc=$2 want_marker=$3 out rc=0
    if [ "$event" = unset ]; then
        out=$(env -u GITHUB_EVENT_NAME "$policy" vm-linux "test reason" 2>&1) || rc=$?
    else
        out=$(GITHUB_EVENT_NAME=$event "$policy" vm-linux "test reason" 2>&1) || rc=$?
    fi
    if [ "$rc" != "$want_rc" ] || ! grep -qF -- "$want_marker" <<<"$out"; then
        echo "FAIL: event=$event: exit $rc (want $want_rc), output: $out" >&2
        fail=1
    fi
}

run_case pull_request 1 '::error'
run_case merge_group 1 '::error'
run_case schedule 0 '::warning'
run_case workflow_dispatch 0 '::warning'
run_case unset 0 '::warning'

if [ "$fail" = 0 ]; then
    echo ok
else
    exit 1
fi
