#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The nextest filterset of the VM tests a run executes, for the `vm` profile (vm-linux.yml, vm-windows.yml).
# Real-machine tests (vm_machine_*) run in ci/windows-e2e.ps1, and the real-tool radar (vm_radar_*) in full-test.yml (the login-tool ones, vm_radar_login_tools_*, also nightly in live-contracts.yml; the lab Forgejo one, vm_radar_forgejo_*, in lab-forgejo.yml),
# not in the VM workflows. The soak (vm_soak_*, a 15-minute idle SSH session) and the performance bars (vm_perf_*, wall-clock
# bars too noisy on a shared runner to gate a merge) run on the nightly and on a dispatch only.
# The desktop VS Code tests (vm_vscode_*) are in no partition: one nightly job of vm-windows.yml runs them (`ci/vm-filter.sh vscode`).
# One place, because the build job lists the tests with it and the test jobs run them with it: both must agree.
# usage: ci/vm-filter.sh [vscode]   (reads GITHUB_EVENT_NAME; `vscode` prints the filter of the desktop VS Code job instead)
set -euo pipefail

if [ "${1:-}" = vscode ]; then
    echo 'test(/(^|::)vm_vscode_/)'
    exit 0
fi

case "${GITHUB_EVENT_NAME:-}" in
schedule | workflow_dispatch)
    echo 'not (test(/(^|::)vm_machine_/) | test(/(^|::)vm_radar_/) | test(/(^|::)vm_vscode_/))'
    ;;
*)
    echo 'not (test(/(^|::)vm_machine_/) | test(/(^|::)vm_radar_/) | test(/(^|::)vm_soak_/) | test(/(^|::)vm_perf_/) | test(/(^|::)vm_vscode_/))'
    ;;
esac
