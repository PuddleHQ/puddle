#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The nextest filterset of the VM tests a run executes, for the `vm` profile (vm-linux.yml, vm-windows.yml).
# Real-machine tests (vm_machine_*) run in ci/windows-e2e.ps1, and the real-tool radar (vm_radar_*) in full-test.yml (the login-tool ones, vm_radar_login_tools_*, also nightly in live-contracts.yml; the lab Forgejo one, vm_radar_forgejo_*, in lab-forgejo.yml),
# not in the VM workflows. The soak (vm_soak_*, a 15-minute idle SSH session) runs on the nightly and on a dispatch only.
# One place, because the build job lists the tests with it and the test jobs run them with it: both must agree.
# usage: ci/vm-filter.sh   (reads GITHUB_EVENT_NAME)
set -euo pipefail

case "${GITHUB_EVENT_NAME:-}" in
schedule | workflow_dispatch)
    echo 'not (test(/(^|::)vm_machine_/) | test(/(^|::)vm_radar_/))'
    ;;
*)
    echo 'not (test(/(^|::)vm_machine_/) | test(/(^|::)vm_radar_/) | test(/(^|::)vm_soak_/))'
    ;;
esac
