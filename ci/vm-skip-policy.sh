#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Called by the VM workflows when a runner has no hypervisor (no /dev/kvm, no WHP). On a pull
# request that fails the job: a required check must not pass without running its tests. Other
# events (the nightly, a dispatch) only warn, and the workflow skips the tests as before.
# Usage: ci/vm-skip-policy.sh <job> <reason>. Reads GITHUB_EVENT_NAME (unset counts as not a PR).
set -euo pipefail

job=${1:?usage: vm-skip-policy.sh <job> <reason>}
reason=${2:?usage: vm-skip-policy.sh <job> <reason>}

if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ]; then
    echo "::error title=$job cannot run::$reason; a required VM check must not pass without running VM tests"
    exit 1
fi
echo "::warning title=$job skipped::$reason; VM tests did not run (infrastructure, not a test result)"
