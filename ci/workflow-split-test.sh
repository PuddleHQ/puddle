#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of the split of two files that grew past 1000 lines: ci/msi-install-test.ps1 (its phases
# live in ci/msi-install-test/*.ps1, dot-sourced by the main script) and .github/workflows/ci.yml (its
# unit tier and its merge-queue lookup are reusable workflows). Fails when a file is over 1000 lines
# again, when a dot-sourced part or a called workflow is missing, when a PowerShell function is defined
# in two parts (the later definition would silently win), when a called workflow lacks `workflow_call`,
# when msi.yml no longer runs for a change to a part, or when the env: block of a called workflow
# differs from ci.yml's. It also holds scripts/quality-gate.py and ci/scope-test.sh (split into sibling
# files) and the Rust files of T-2502 (workspaces.rs, fake/mod.rs, contract/cases.rs, boot_sh, plan.rs,
# vm_lifecycle.rs) to the same 1000-line limit. Prints ok only when every check passes.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root" || exit 2
fail=0
bad() { echo "workflow-split-test: FAIL: $*" >&2; fail=1; }

main=ci/msi-install-test.ps1
parts=(ci/msi-install-test/*.ps1)

for f in "$main" .github/workflows/ci.yml "${parts[@]}" \
    scripts/quality-gate.py scripts/quality_gate_*.py ci/scope-test.sh ci/scope-test/*.sh; do
    [ -f "$f" ] || { bad "$f is missing"; continue; }
    n=$(wc -l <"$f")
    [ "$n" -le 1000 ] || bad "$f has $n lines (limit 1000): split it"
done

# The Rust files split for the same limit (T-2502): each file of the split folders, plus the files
# that stay beside them.
for f in crates/puddle-workspace/src/workspaces.rs crates/puddle-workspace/src/workspaces/*.rs \
    crates/puddle-compute/src/fake/*.rs crates/puddle-compute/src/contract/cases.rs \
    crates/puddle-compute/src/contract/cases/*.rs crates/puddle-boot/tests/boot_sh/*.rs \
    crates/puddle-boot/src/plan.rs crates/puddle-boot/src/plan/*.rs \
    crates/puddle-lifecycle/tests/vm_lifecycle.rs crates/puddle-lifecycle/tests/vm_lifecycle/*.rs; do
    [ -f "$f" ] || { bad "$f is missing"; continue; }
    n=$(wc -l <"$f")
    [ "$n" -le 1000 ] || bad "$f has $n lines (limit 1000): split it"
done

# Every part is dot-sourced by the main script, and every dot-sourced file exists.
for p in "${parts[@]}"; do
    [ -f "$p" ] || continue
    grep -qF ". \"\$PSScriptRoot/msi-install-test/$(basename "$p")\"" "$main" ||
        bad "$main does not dot-source $p"
done
# shellcheck disable=SC2016 # a literal $PSScriptRoot in the pattern
while IFS= read -r rel; do
    [ -f "ci/$rel" ] || bad "$main dot-sources ci/$rel, which does not exist"
done < <(sed -n 's/^\. "\$PSScriptRoot\/\(.*\)"$/\1/p' "$main")

# A part sits one folder deeper than the main script: a `$PSScriptRoot '..\\x'` path in it must still
# name an existing file from there.
for p in "${parts[@]}"; do
    [ -f "$p" ] || continue
    while IFS= read -r rel; do
        [ -e "$(dirname "$p")/${rel//\\//}" ] || bad "$p: \$PSScriptRoot path '$rel' does not exist from $(dirname "$p")"
    done < <(sed -n "s/.*PSScriptRoot '\\(\\.\\.[^']*\\)'.*/\\1/p" "$p")
done

# A function name defined in two files is a silent override.
dups=$(cat "$main" "${parts[@]}" 2>/dev/null | sed -n 's/^function \([A-Za-z0-9-]*\).*/\1/p' | sort | uniq -d)
[ -z "$dups" ] || bad "function defined more than once: $(echo "$dups" | tr '\n' ' ')"

# Every reusable workflow ci.yml calls exists and takes `workflow_call`.
while IFS= read -r wf; do
    [ -f "$wf" ] || { bad "ci.yml calls $wf, which does not exist"; continue; }
    grep -q '^  workflow_call:' "$wf" || bad "$wf has no workflow_call trigger"
done < <(sed -n 's|^    uses: \./\(\.github/workflows/.*\.yml\)$|\1|p' .github/workflows/ci.yml)
grep -q '^    uses: \./\.github/workflows/' .github/workflows/ci.yml || bad "ci.yml calls no reusable workflow"

grep -qF 'ci/msi-install-test/**' .github/workflows/msi.yml || bad "msi.yml paths do not list ci/msi-install-test/**"

# The top-level `env:` block of each called workflow is a copy of ci.yml's (a called workflow does not
# inherit it): a change to one that misses the others makes the jobs run with different settings.
env_block() { awk '/^env:$/ {p=1; print; next} p && /^[^ #]/ {exit} p && NF {print}' "$1"; }
want_env=$(env_block .github/workflows/ci.yml)
[ -n "$want_env" ] || bad "ci.yml has no top-level env: block"
for wf in ci-unit ci-unit-ui ci-queue-tested; do
    f=.github/workflows/$wf.yml
    [ -f "$f" ] || continue
    [ "$(env_block "$f")" = "$want_env" ] || bad "$f: its env: block differs from ci.yml's (copy it again)"
done

if [ "$fail" = 0 ]; then echo "workflow-split-test: ok"; else exit 1; fi
