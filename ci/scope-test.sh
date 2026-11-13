#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/scope.sh: replays the merged pull requests in ci/scope-prs.txt, checks that every
# always-run path forces a full run, plants paths the rules do not know to show the fail-open, drives
# the whole script with a fake `gh` (a failed, stale, empty or huge file list, a merge queue diff,
# other events, the light pull request mode on and off), runs the result job's verdict and the
# full-steps guard on every combination that matters, and checks the four workflows are wired to
# match (python3 with PyYAML; skipped without it). SCOPE_TEST_METADATA=1 also checks the crate sets
# against `cargo metadata` (needs cargo and python3). Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
scope="$here/scope.sh"
fixture="$here/scope-prs.txt"
fail=0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

bad() { echo "FAIL: $*" >&2; fail=1; }

# classify <paths...> -> "class ci_rust win_rust ui vm"
classify() {
    printf '%s\n' "$@" | "$scope" classify | sort | awk -F= '{v[$1]=$2} END {print v["class"], v["ci_rust"], v["win_rust"], v["ui"], v["vm"]}'
}
want() { # want <label> <expected> <paths...>
    local label=$1 exp=$2 got
    shift 2
    got=$(classify "$@")
    [ "$got" = "$exp" ] || bad "$label: got '$got', want '$exp'"
}
FULL="full true true true true"
# Every always-run path of the list is a workflow the VM jobs do not use (the one way a full run skips the VM jobs).
vm_unrelated_only() {
    local p
    for p in "$@"; do
        if grep -Eq "$always_re" <<<"$p" && ! grep -Eq "$data_re" <<<"$p"; then
            grep -Eq '^\.github/workflows/[^/]+\.ya?ml$' <<<"$p" && ! grep -Eq 'workflows/vm-(linux|windows)\.yml$' <<<"$p" || return 1
        fi
    done
}

# 1. Replay of the merged pull requests: class and outputs as recorded, and an independent check
# that any pull request with an always-run path got a full run.
always_re='^(Cargo\.lock|Cargo\.toml|rust-toolchain\.toml|\.cargo/|\.config/|\.githooks/|\.github/workflows/|ci/|scripts/|crates/puddle-vm-tests/|crates/puddle-api/openapi/|ui/package-lock\.json|ui/src/lib/api/schema\.d\.ts|.*/Cargo\.toml$)'
data_re='^scripts/(diff-coverage-exclusions\.txt|diff-coverage-exclusions/[^/]+\.txt|coverage-baseline\.json|standalone-allow\.txt)$'
replayed=0
declare -A seen_class=()
pr="" exp="" paths=()
flush() {
    [ -n "$pr" ] || return 0
    local got always=0 p
    got=$(classify "${paths[@]}")
    [ "$got" = "$exp" ] || bad "pr $pr: got '$got', want '$exp'"
    for p in "${paths[@]}"; do
        if grep -Eq "$always_re" <<<"$p" && ! grep -Eq "$data_re" <<<"$p"; then always=1; fi
    done
    if [ "$always" = 1 ] && [ "$got" != "$FULL" ] && ! { [ "$got" = "full true true true false" ] && vm_unrelated_only "${paths[@]}"; }; then
        bad "pr $pr has an always-run path but got '$got'"
    fi
    seen_class[${got%% *}]=1
    replayed=$((replayed + 1))
}
while IFS= read -r line; do
    case $line in
    '#'* | '') ;;
    @*)
        flush
        pr=${line%% *}
        pr=${pr#@}
        exp=${line#* }
        paths=()
        ;;
    *) paths+=("$line") ;;
    esac
done <"$fixture"
flush
[ "$replayed" -ge 58 ] || bad "fixture replayed only $replayed pull requests (want at least 58)"
for c in full docs ui rust mixed; do
    [ -n "${seen_class[$c]:-}" ] || bad "the fixture has no pull request of class $c"
done

# 2. Every always-run path alone, and added to a docs-only and to a UI-only change, runs everything.
for p in Cargo.lock Cargo.toml rust-toolchain.toml .cargo/config.toml .config/nextest.toml .githooks/pre-push \
    .github/workflows/vm-linux.yml .github/workflows/vm-windows.yml ci/scope.sh ci/msb-runtime.sha256 scripts/check.sh scripts/gate-sched.sh \
    crates/puddle-vm-tests/src/lib.rs crates/puddle-vm-tests/tests/x.rs crates/puddle-vm-tests/tests/vm_x.rs crates/puddle-api/openapi/openapi.json \
    crates/puddle-agent/Cargo.toml ui/package-lock.json ui/src/lib/api/schema.d.ts ui/THIRD-PARTY-NOTICES.txt \
    crates/puddle-store/tests/data/repo-urls.json .github/actions/x/action.yml ui/scripts/diff-coverage.ts ui/scripts/coverage-ratchet.ts deny.toml about.toml clippy.toml rustfmt.toml _typos.toml \
    .gitleaks.toml .gitattributes; do
    want "always-run $p alone" "$FULL" "$p"
    want "always-run $p with docs" "$FULL" docs/STANDARDS.md "$p"
    want "always-run $p with ui" "$FULL" ui/src/lib/x.ts "$p"
done

# 3. Planted misses: a path no rule names, an empty list and a huge one must fall open.
want "unknown top-level file" "$FULL" docs/STANDARDS.md Makefile
want "unknown directory" "$FULL" ui/src/lib/x.ts tools/new/run.sh
want "unknown crate-less crates path" "$FULL" crates/README
want "docs plus one VM-test crate file" "$FULL" docs/STANDARDS.md README.md crates/puddle-vm-tests/src/lib.rs
want "docs plus one new script" "$FULL" docs/adr/0001.md scripts/new-gate.sh
want "empty list" "$FULL"
mapfile -t many < <(seq 1 300 | sed 's|^|docs/page-|')
want "300 docs files" "$FULL" "${many[@]}"
mapfile -t many < <(seq 1 299 | sed 's|^|docs/page-|')
want "299 docs files" "docs false false false false" "${many[@]}"

# 4. The classes, by example.
want "docs only" "docs false false false false" docs/STANDARDS.md README.md .github/dependabot.yml
want "ui only" "ui false false true false" ui/src/lib/a.ts ui/e2e/b.spec.ts docs/x.md
want "ui and gate data" "mixed true false true false" ui/src/a.ts scripts/diff-coverage-exclusions.txt
want "ui and a crate's exclusions" "mixed true false true false" ui/src/a.ts scripts/diff-coverage-exclusions/puddle-api.txt
want "gate data only" "rust true false true false" scripts/coverage-baseline.json
want "ui fixtures" "mixed true true true false" ui/e2e/fixtures/workspaces.json
want "vm crate" "rust true true true true" crates/puddle-compute-msb/src/lib.rs
want "host-side crate in the UI closure" "rust true true true true" crates/puddle-lifecycle/src/lib.rs
want "crate outside the VM set, in the UI closure" "rust true true true false" crates/puddle-app/src/lib.rs
want "crate in neither" "rust true true false false" crates/xtask/src/main.rs
want "crate with a README only" "rust true true true true" crates/puddle-host/README.md
want "rename out of an always-run path" "$FULL" ui/src/a.ts ci/old.sh

# 4a. Diffs the VM jobs skip although the paths are not docs: other workflows and non-VM test files.
# Each case also has its other direction: a path that can reach a VM test keeps `vm` true, and any
# mix with such a path runs the VM jobs.
want "another workflow only" "full true true true false" .github/workflows/ci.yml
want "several other workflows" "full true true true false" .github/workflows/windows.yml .github/workflows/cache-gc.yml docs/x.md
want "vm-linux workflow" "$FULL" .github/workflows/vm-linux.yml
want "vm-windows workflow" "$FULL" .github/workflows/vm-windows.yml
want "other workflow with the vm-linux workflow" "$FULL" .github/workflows/ci.yml .github/workflows/vm-linux.yml
want "other workflow with a shared action" "$FULL" .github/workflows/ci.yml .github/actions/cancel-group/action.yml
want "other workflow with a ci script" "$FULL" .github/workflows/ci.yml ci/vm-partition.sh
want "other workflow with a scripts file" "$FULL" .github/workflows/ci.yml scripts/check.sh
want "other workflow with an unknown path" "$FULL" .github/workflows/ci.yml mystery/file.txt
want "other workflow with a vm crate" "full true true true true" .github/workflows/ci.yml crates/puddle-host/src/lib.rs
want "a nested path under workflows" "$FULL" .github/workflows/sub/x.yml
want "a non-VM test file" "rust true true true false" crates/puddle-host/tests/fake_host.rs
want "a non-VM test file with a source file" "rust true true true true" crates/puddle-host/tests/fake_host.rs crates/puddle-host/src/lib.rs
want "a vm_ test file" "rust true true true true" crates/puddle-host/tests/vm_git.rs
want "a shared test module file" "rust true true true true" crates/puddle-host/tests/support.rs
want "a shared test module folder" "rust true true true true" crates/puddle-compute-msb/tests/support/mod.rs
want "a data file under tests" "rust true true true true" crates/puddle-host/tests/data/x.json
want "a test file nested in a folder" "rust true true true true" crates/puddle-host/tests/integration/x.rs
want "a non-VM test file of the VM test crate" "$FULL" crates/puddle-vm-tests/tests/x.rs
want "a test file in a crate outside the VM set" "rust true true true false" crates/puddle-app/tests/x.rs

# 4b. The nightly tier: which jobs a merge queue run defers (PUDDLE_TIER=on) and which paths pull a
# job back into the queue. defers <event> <PUDDLE_TIER> <paths...> -> "vm ui clippy webkit", each true
# (deferred) or false (runs in the queue).
defers() {
    local event=$1 tier=$2 out
    shift 2
    out=$(printf '%s\n' "$@" | env -u PUDDLE_TIER "PUDDLE_TIER=$tier" GITHUB_EVENT_NAME="$event" "$scope" classify | sort |
        awk -F= '{v[$1]=$2} END {print v["defer_vm_windows"], v["defer_win_ui"], v["defer_win_clippy"], v["defer_linux_webkit"]}')
    echo "$out"
}
tier_want() { # tier_want <label> <expected> <paths...>  (merge_group, PUDDLE_TIER=on)
    local label=$1 exp=$2 got
    shift 2
    got=$(defers merge_group on "$@")
    [ "$got" = "$exp" ] || bad "tier: $label: got '$got', want '$exp'"
}
tier_want "docs only: all four deferred" "true true true true" docs/guide.md
tier_want "a UI-only change" "true true true true" ui/src/a.ts
tier_want "a plain crate" "true true true true" crates/puddle-store/src/lib.rs
tier_want "the msb adapter pulls the VM job back" "false true true true" crates/puddle-compute-msb/src/lib.rs
tier_want "the msb adapter's tests" "false true true true" crates/puddle-compute-msb/tests/contract.rs
tier_want "the desktop shell pulls the UI job back" "true false true true" crates/puddle-app/src/main.rs
tier_want "the shell's capabilities" "true false true true" crates/puddle-app/capabilities/default.json
tier_want "a Windows source file" "false true false true" crates/puddle-certs/src/platform/windows.rs
tier_want "a Windows directory" "false true false true" crates/puddle-lifecycle/src/windows/job.rs
tier_want "a Windows snapshot" "false true false true" crates/puddle-doctor/tests/snapshots/windows_edr.snap
tier_want "a Windows doc, any case" "false true false true" docs/Windows-notes.md
tier_want "a rename out of a Windows path" "false true false true" crates/puddle-store/src/lib.rs crates/puddle-fs/src/private/windows.rs
# The WebKit half of the Linux UI tests: markup, styles and the browser test setup keep it in the queue;
# plain TypeScript, the e2e specs and Rust do not (the engines share them, and webkit.yml runs it nightly).
tier_want "a component pulls WebKit back" "true true true false" ui/src/lib/components/RequestRow.svelte
tier_want "a stylesheet pulls WebKit back" "true true true false" ui/src/lib/theme/base.css
tier_want "the document shell pulls WebKit back" "true true true false" ui/src/app.html
tier_want "a static file pulls WebKit back" "true true true false" ui/static/favicon.svg
tier_want "the ui manifest pulls WebKit back" "true true true false" ui/package.json
tier_want "the Playwright config pulls WebKit back" "true true true false" ui/playwright.config.ts
tier_want "the Vite config pulls WebKit back" "true true true false" ui/vite.config.ts
tier_want "the browser warm-up pulls WebKit back" "true true true false" ui/e2e/warm-up.ts
tier_want "a shared e2e helper pulls WebKit back" "true true true false" ui/e2e/support.ts
tier_want "the e2e fixture setup pulls WebKit back" "true true true false" ui/e2e/fixture.ts
tier_want "a rename out of a component" "true true true false" ui/src/a.ts ui/src/lib/old.svelte
tier_want "an e2e spec leaves WebKit to the nightly" "true true true true" ui/e2e/rules.spec.ts
tier_want "a store module leaves WebKit to the nightly" "true true true true" ui/src/lib/stores/theme.ts
tier_want "a fixture leaves WebKit to the nightly" "true true true true" ui/e2e/fixtures/rules.json
tier_want "a component beside a Rust change" "true true true false" crates/puddle-store/src/lib.rs ui/src/routes/inbox/+page.svelte
tier_want "two escalations add up" "false false false true" crates/puddle-compute-msb/src/lib.rs crates/puddle-app/src/lib.rs crates/puddle-ipc/src/windows/mod.rs
# Always-run paths (the msb pin among them, the shared VM test crate, the workflows) pull every moved job back.
for p in Cargo.lock Cargo.toml ci/msb-runtime.sha256 ci/msb-tag.sh ci/fetch-msb.sh crates/puddle-vm-tests/src/lib.rs \
    .github/workflows/vm-windows.yml .github/workflows/windows.yml .github/workflows/ci.yml ci/scope.sh scripts/check.sh; do
    tier_want "always-run path $p" "false false false false" docs/guide.md "$p"
done
mapfile -t tier_many < <(seq 1 300 | sed 's|^|docs/page-|')
tier_want "300 files: everything runs" "false false false false" "${tier_many[@]}"
tier_want "an unknown path" "false false false false" mystery/file.txt
[ "$(printf '' | env GITHUB_EVENT_NAME=merge_group PUDDLE_TIER=on "$scope" classify | grep -c '^defer_.*=false$')" = 4 ] ||
    bad "tier: an empty file list must run everything"
# Without the variable exactly `on`, or off a merge queue run, nothing is deferred.
for tier in "" off ON On true 1 yes "on "; do
    [ "$(defers merge_group "$tier" docs/guide.md)" = "false false false false" ] || bad "tier: PUDDLE_TIER='$tier' must not defer"
done
for ev in pull_request push schedule workflow_dispatch ""; do
    [ "$(defers "$ev" on docs/guide.md)" = "false false false false" ] || bad "tier: event '$ev' must not defer"
done

# 5. The whole script with a fake `gh`.
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# fake gh: `gh api repos/R/pulls/N` -> head sha; `gh api --paginate repos/R/pulls/N/files...` -> $FAKE_FILES;
# `gh api repos/R/compare/B...H` -> a compare reply with status $FAKE_STATUS and the files in $FAKE_FILES
case "$*" in
*/files*) [ "${FAKE_MODE:-}" = error ] && exit 1; cat "$FAKE_FILES" ;;
*/compare/*)
    [ "${FAKE_MODE:-}" = error ] && exit 1
    [ "$*" = "api repos/o/r/compare/base1...head1" ] || exit 1
    jq -Rn --arg st "${FAKE_STATUS:-ahead}" '{status: $st, files: [inputs | select(. != "") | {filename: .}]}' <"$FAKE_FILES"
    ;;
*) [ "${FAKE_MODE:-}" = headerror ] && exit 1; echo "${FAKE_HEAD:-abc123}" ;;
esac
GH
chmod +x "$tmp/bin/gh"
printf 'ui/src/a.ts\nui/src/b.ts\n' >"$tmp/ui.txt"
: >"$tmp/empty.txt"

# run_main <event> <base> <FAKE_MODE> <files> [PR_HEAD_SHA [VAR=value...]] -> key=value lines
# (MAIN_ENV, an array, adds more VAR=value pairs for every call: the light mode and merge queue cases)
MAIN_ENV=()
run_main() {
    local event=$1 base=$2 mode=$3 files=$4 head=${5:-abc123}
    env -u PUDDLE_QUEUE -u PUDDLE_TIER PATH="$tmp/bin:$PATH" GITHUB_EVENT_NAME="$event" PR_BASE_REF="$base" PR_NUMBER=7 GITHUB_REPOSITORY=o/r \
        PR_HEAD_SHA="$head" FAKE_MODE="$mode" FAKE_FILES="$files" FAKE_HEAD=abc123 GITHUB_OUTPUT="$tmp/out" \
        "${MAIN_ENV[@]}" "$scope" 2>/dev/null
    local rc=$?
    echo "rc=$rc"
}
main_case() { # main_case <label> <want class> <args of run_main>
    local label=$1 exp=$2 out
    shift 2
    : >"$tmp/out"
    out=$(run_main "$@")
    grep -q "^class=$exp$" <<<"$out" || bad "main: $label: want class $exp, got: $(tr '\n' ' ' <<<"$out")"
    grep -q '^rc=0$' <<<"$out" || bad "main: $label: exit status not 0"
    grep -q "^class=$exp$" "$tmp/out" || bad "main: $label: GITHUB_OUTPUT lacks class=$exp"
    [ "$(grep -c . "$tmp/out")" = 10 ] || bad "main: $label: GITHUB_OUTPUT has not exactly the ten outputs"
    grep -q "^light=${WANT_LIGHT:-false}$" "$tmp/out" || bad "main: $label: want light=${WANT_LIGHT:-false}, got: $(tr '\n' ' ' <"$tmp/out")"
}
printf '.github/workflows/ci.yml\ncrates/puddle-host/tests/fake_host.rs\n' >"$tmp/other-workflow.txt"
want_out() { grep -q "^$1\$" "$tmp/out" || bad "main: $2: want $1, got: $(tr '\n' ' ' <"$tmp/out")"; }
main_case "ui-only pull request to develop" ui pull_request develop ok "$tmp/ui.txt"
main_case "push" full push develop ok "$tmp/ui.txt"
for ev in push schedule workflow_dispatch; do
    : >"$tmp/out"
    MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1)
    run_main "$ev" develop ok "$tmp/other-workflow.txt" >/dev/null
    grep -q '^vm=true$' "$tmp/out" || bad "main: a $ev run of a diff no VM test can see must still run the VM jobs in full"
done
MAIN_ENV=()
main_case "schedule" full schedule develop ok "$tmp/ui.txt"
main_case "workflow_dispatch" full workflow_dispatch develop ok "$tmp/ui.txt"
main_case "pull request to main" full pull_request main ok "$tmp/ui.txt"
main_case "file list fails" full pull_request develop error "$tmp/ui.txt"
main_case "head lookup fails" full pull_request develop headerror "$tmp/ui.txt"
main_case "stale head" full pull_request develop ok "$tmp/ui.txt" deadbeef
main_case "empty list" full pull_request develop ok "$tmp/empty.txt"
printf 'ui/src/a.ts\nci/old.sh\n' >"$tmp/rename.txt"
main_case "rename from an always-run path" full pull_request develop ok "$tmp/rename.txt"
out=$(env -u GITHUB_EVENT_NAME PATH="$tmp/bin:$PATH" "$scope" 2>/dev/null | grep -c '^class=full$')
[ "$out" = 1 ] || bad "main: no event at all must run everything"
# A broken classifier (here: no gh and no PATH tools worth the name) still prints a full run.
out=$(env PATH="$tmp/nothing" GITHUB_EVENT_NAME=pull_request PR_BASE_REF=develop PR_NUMBER=7 GITHUB_REPOSITORY=o/r \
    "$(command -v bash)" "$scope" 2>/dev/null | grep -c '^class=full$')
[ "$out" = 1 ] || bad "main: missing gh must run everything"

# Merge queue runs: scoped by the compare diff between the queue's base and the tested commit;
# anything doubtful runs everything. Never the light mode, whatever PUDDLE_QUEUE says.
MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1 PUDDLE_QUEUE=on)
main_case "merge group, ui-only diff" ui merge_group "" ok "$tmp/ui.txt"
main_case "merge group, another workflow and a non-VM test file" full merge_group "" ok "$tmp/other-workflow.txt"
want_out vm=false "merge group: a diff no VM test can see skips the VM jobs"
main_case "merge group, always-run path" full merge_group "" ok "$tmp/rename.txt"
main_case "merge group, compare fails" full merge_group "" error "$tmp/ui.txt"
main_case "merge group, empty diff" full merge_group "" ok "$tmp/empty.txt"
MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1 FAKE_STATUS=diverged)
main_case "merge group, head not ahead of base" full merge_group "" ok "$tmp/ui.txt"
MAIN_ENV=(MERGE_GROUP_BASE_SHA=other MERGE_GROUP_HEAD_SHA=head1)
main_case "merge group, another base" full merge_group "" ok "$tmp/ui.txt"
MAIN_ENV=(MERGE_GROUP_HEAD_SHA=head1)
main_case "merge group, base sha missing" full merge_group "" ok "$tmp/ui.txt"
mapfile -t many < <(seq 1 300 | sed 's|^|docs/page-|')
printf '%s\n' "${many[@]}" >"$tmp/many.txt"
MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1)
main_case "merge group, 300 files (the compare API's cap)" full merge_group "" ok "$tmp/many.txt"

# The light mode: only PUDDLE_QUEUE=on exactly, only on a pull request to develop. The scoped
# outputs stay as the files say; a doubtful file list runs everything without the light mode.
MAIN_ENV=(PUDDLE_QUEUE=on)
WANT_LIGHT=true main_case "light: ui-only pull request to develop" ui pull_request develop ok "$tmp/ui.txt"
WANT_LIGHT=true main_case "light: pull request to develop with an always-run path" full pull_request develop ok "$tmp/rename.txt"
main_case "light: pull request to main" full pull_request main ok "$tmp/ui.txt"
main_case "light: push" full push develop ok "$tmp/ui.txt"
main_case "light: schedule" full schedule develop ok "$tmp/ui.txt"
main_case "light: workflow_dispatch" full workflow_dispatch develop ok "$tmp/ui.txt"
main_case "light: file list fails" full pull_request develop error "$tmp/ui.txt"
main_case "light: stale head" full pull_request develop ok "$tmp/ui.txt" deadbeef
for v in ON On true 1 yes "on " off ""; do
    MAIN_ENV=("PUDDLE_QUEUE=$v")
    main_case "light: PUDDLE_QUEUE='$v' is not on" ui pull_request develop ok "$tmp/ui.txt"
done
MAIN_ENV=()
main_case "light: PUDDLE_QUEUE unset" ui pull_request develop ok "$tmp/ui.txt"

# The Windows tests run on a pull request that changes Rust, in the light mode too (`win_rust` stays
# true there; the windows workflow no longer skips its tests job on `light`). A docs-only pull
# request keeps `win_rust=false`, so the job is skipped and the result job accepts that. The queue
# run of the same files is never light.
printf 'crates/puddle-fs/src/lib.rs\n' >"$tmp/rust.txt"
printf 'docs/STANDARDS.md\nREADME.md\n' >"$tmp/docs.txt"
printf 'ui/e2e/fixtures/data.json\n' >"$tmp/fixture.txt"
MAIN_ENV=(PUDDLE_QUEUE=on)
WANT_LIGHT=true main_case "light: rust-only pull request" rust pull_request develop ok "$tmp/rust.txt"
want_out win_rust=true "light: a rust-only pull request keeps the Windows tests"
want_out ci_rust=true "light: a rust-only pull request runs the Linux Rust gates"
WANT_LIGHT=true main_case "light: fixture-only pull request" mixed pull_request develop ok "$tmp/fixture.txt"
want_out win_rust=true "light: a fixture-only pull request keeps the Windows tests"
WANT_LIGHT=true main_case "light: docs-only pull request" docs pull_request develop ok "$tmp/docs.txt"
want_out win_rust=false "light: a docs-only pull request has no Windows tests"
WANT_LIGHT=true main_case "light: ui-only pull request" ui pull_request develop ok "$tmp/ui.txt"
want_out win_rust=false "light: a ui-only pull request has no Windows tests"
WANT_LIGHT=true main_case "light: always-run path" full pull_request develop ok "$tmp/rename.txt"
want_out win_rust=true "light: an always-run path keeps the Windows tests"
MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1 PUDDLE_QUEUE=on)
main_case "queue: rust-only merge group" rust merge_group "" ok "$tmp/rust.txt"
want_out win_rust=true "queue: a rust-only merge group runs the Windows tests"
MAIN_ENV=()

# The nightly tier through the whole script: only a merge queue run with PUDDLE_TIER=on, and the
# outputs of the group's diff; any doubt prints the full set with nothing deferred.
printf 'crates/puddle-app/src/lib.rs\n' >"$tmp/app.txt"
printf 'ui/src/lib/components/RequestRow.svelte\n' >"$tmp/component.txt"
MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1 PUDDLE_TIER=on)
main_case "tier: merge group, ui-only diff" ui merge_group "" ok "$tmp/ui.txt"
want_out defer_vm_windows=true "tier: ui-only diff defers the VM job"
want_out defer_win_ui=true "tier: ui-only diff defers the UI job"
want_out defer_win_clippy=true "tier: ui-only diff defers clippy"
want_out defer_linux_webkit=true "tier: a logic-only UI diff leaves WebKit to the nightly"
main_case "tier: merge group, desktop shell diff" rust merge_group "" ok "$tmp/app.txt"
want_out defer_win_ui=false "tier: a puddle-app diff runs the UI job"
want_out defer_vm_windows=true "tier: a puddle-app diff still defers the VM job"
main_case "tier: merge group, a component" ui merge_group "" ok "$tmp/component.txt"
want_out defer_linux_webkit=false "tier: a component keeps WebKit in the queue"
want_out defer_vm_windows=true "tier: a component still defers the VM job"
main_case "tier: merge group, always-run path" full merge_group "" ok "$tmp/rename.txt"
want_out defer_vm_windows=false "tier: an always-run path runs the VM job"
want_out defer_linux_webkit=false "tier: an always-run path keeps WebKit in the queue"
main_case "tier: merge group, compare fails" full merge_group "" error "$tmp/ui.txt"
want_out defer_win_ui=false "tier: a failed compare runs everything"
want_out defer_linux_webkit=false "tier: a failed compare keeps WebKit in the queue"
main_case "tier: pull request to develop" ui pull_request develop ok "$tmp/ui.txt"
want_out defer_vm_windows=false "tier: a pull request defers nothing"
want_out defer_linux_webkit=false "tier: a pull request keeps WebKit"
main_case "tier: push" full push develop ok "$tmp/ui.txt"
want_out defer_vm_windows=false "tier: a push defers nothing"
want_out defer_linux_webkit=false "tier: a push keeps WebKit"
MAIN_ENV=(MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1)
main_case "tier unset: merge group, ui-only diff" ui merge_group "" ok "$tmp/ui.txt"
want_out defer_vm_windows=false "tier unset: the queue runs the VM job as today"
want_out defer_linux_webkit=false "tier unset: the queue runs WebKit as today"
MAIN_ENV=()

# 6. The result job's verdict. needs <scope outputs> <job=result>... -> NEEDS json
needs() {
    local sc=$1 j
    shift
    local o="{\"scope\":{\"result\":\"success\",\"outputs\":$sc}"
    for j in "$@"; do o="$o,\"${j%%=*}\":{\"result\":\"${j#*=}\"}"; done
    echo "$o}"
}
# VERDICT_ENV (an array of VAR=value) sets the light mode or a tested push for the next calls.
VERDICT_ENV=()
verdict() { # verdict <event> <want 0|1> <label> <scope json> <specs...> -- <job=result>...
    local event=$1 exp=$2 label=$3 sc=$4 specs=() rc=0
    shift 4
    while [ "$1" != -- ]; do specs+=("$1"); shift; done
    shift
    env -u PUDDLE_QUEUE -u PUDDLE_QUEUE_SKIP_PUSH -u PUDDLE_TIER -u PR_BASE_REF -u GITHUB_REF -u GITHUB_STEP_SUMMARY \
        NEEDS="$(needs "$sc" "$@")" \
        GITHUB_EVENT_NAME="$event" "${VERDICT_ENV[@]}" "$scope" verdict "${specs[@]}" >/dev/null 2>&1 || rc=$?
    [ "$rc" = "$exp" ] || bad "verdict: $label: exit $rc, want $exp"
}
VM='{"vm":"true"}'
CI_FULL='{"ci_rust":"true","ui":"true"}'
VMF='{"vm":"false"}'
verdict pull_request 0 "vm ran and passed" "$VM" build=vm tests=vm -- build=success tests=success
verdict pull_request 0 "vm skipped by scope" "$VMF" build=vm tests=vm -- build=skipped tests=skipped
verdict pull_request 1 "vm skipped but scope said run" "$VM" build=vm tests=vm -- build=skipped tests=skipped
verdict pull_request 1 "vm skipped, scope outputs lost" '{}' build=vm tests=vm -- build=skipped tests=skipped
verdict pull_request 1 "tests skipped after a failed build" "$VM" build=vm tests=vm -- build=failure tests=skipped
verdict pull_request 1 "tests skipped, build passed (infrastructure)" "$VM" build=vm tests=vm -- build=success tests=skipped
verdict pull_request 1 "a failed test job" "$VMF" build=vm tests=vm -- build=skipped tests=failure
verdict pull_request 1 "a cancelled build" "$VM" build=vm tests=vm -- build=cancelled tests=skipped
verdict pull_request 1 "a job missing from needs" "$VM" build=vm tests=vm -- build=success
verdict schedule 0 "nightly skips tests on a green commit" "$VM" --nightly-skips build=vm tests=vm -- build=success tests=skipped
verdict pull_request 1 "a failed agent job" "$VM" build=vm agent=vm tests=vm -- build=success agent=failure tests=skipped
verdict pull_request 0 "agent, build and tests passed" "$VM" build=vm agent=vm tests=vm -- build=success agent=success tests=success
verdict schedule 0 "nightly skips everything" "$VM" --nightly-skips build=vm tests=vm -- build=skipped tests=skipped
verdict schedule 1 "nightly failure" "$VM" --nightly-skips build=vm tests=vm -- build=success tests=failure
verdict merge_group 1 "a merge queue run never skips as a nightly" "$VM" --nightly-skips build=vm tests=vm -- build=skipped tests=skipped
verdict workflow_dispatch 1 "a dispatch never skips as a nightly" "$VM" --nightly-skips build=vm tests=vm -- build=success tests=skipped
verdict push 1 "a skipped job off a pull request without the flag" "$CI_FULL" rust=ci_rust -- rust=skipped
verdict push 0 "ci push, all passed" "$CI_FULL" rust=ci_rust ui=ui -- rust=success ui=success
CI_UI='{"ci_rust":"false","ui":"true"}'
CI_DOCS='{"ci_rust":"false","ui":"false"}'
ci_specs=(rust=ci_rust gates=ci_rust 'light=!ci_rust' ui=ui 'ui-webkit=ui,queue,defer:defer_linux_webkit')
verdict pull_request 0 "ci full run" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=success
verdict pull_request 0 "ci ui-only: light and ui" "$CI_UI" "${ci_specs[@]}" -- rust=skipped gates=skipped light=success ui=success ui-webkit=success
verdict pull_request 0 "ci docs-only: light only" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped gates=skipped light=success ui=skipped ui-webkit=skipped
verdict pull_request 1 "ci docs-only: light failed" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped gates=skipped light=failure ui=skipped ui-webkit=skipped
verdict pull_request 1 "ci docs-only: light skipped too" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped gates=skipped light=skipped ui=skipped ui-webkit=skipped
verdict pull_request 1 "ci full: rust skipped, light ran" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped gates=skipped light=success ui=success ui-webkit=success
verdict pull_request 1 "ci full: rust failed" "$CI_FULL" "${ci_specs[@]}" -- rust=failure gates=failure light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci ui job skipped though needed" "$CI_UI" "${ci_specs[@]}" -- rust=skipped gates=skipped light=success ui=skipped ui-webkit=skipped
verdict pull_request 0 "ci scope failed: everything ran" '{}' "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci scope failed: rust skipped" '{}' "${ci_specs[@]}" -- rust=skipped gates=skipped light=success ui=success ui-webkit=success
verdict pull_request 1 "ci full: gates failed, tests passed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=failure light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci full: gates skipped though needed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=skipped light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci full: webkit failed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=failure
verdict pull_request 1 "ci full: webkit skipped without the light mode" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=skipped
# The nightly tier skips the WebKit job on a merge queue run when scope says so, and nowhere else.
CI_WK='{"ci_rust":"true","ui":"true","defer_linux_webkit":"true"}'
CI_NOWK='{"ci_rust":"true","ui":"true","defer_linux_webkit":"false"}'
VERDICT_ENV=(PUDDLE_TIER=on)
verdict merge_group 0 "tier: webkit deferred" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=skipped
verdict merge_group 1 "tier: webkit kept in the queue but skipped" "$CI_NOWK" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=skipped
verdict merge_group 1 "tier: chromium skipped, only webkit may defer" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=skipped ui-webkit=skipped
verdict pull_request 1 "tier: a pull request never defers webkit" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=skipped
VERDICT_ENV=()
verdict merge_group 1 "tier unset: webkit deferred" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=skipped
# Windows: the release build is not scoped (its own event rule skips it on a pull request to develop).
WIN_UI='{"win_rust":"false","ui":"true"}'
verdict pull_request 0 "windows ui-only, release skipped" "$WIN_UI" clippy=win_rust test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui ui-unit=ui -- \
    clippy=skipped test-build=skipped tests=skipped docs=skipped ui-e2e=success ui-unit=success release=skipped
verdict pull_request 1 "windows ui-only, ui failed" "$WIN_UI" clippy=win_rust test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui ui-unit=ui -- \
    clippy=skipped test-build=skipped tests=skipped docs=skipped ui-e2e=failure ui-unit=failure release=skipped
verdict pull_request 1 "windows release failed" "$WIN_UI" clippy=win_rust test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui ui-unit=ui -- \
    clippy=skipped test-build=skipped tests=skipped docs=skipped ui-e2e=success ui-unit=success release=failure
# The Windows tests are split in a build, two partitions and a doc job: each failure counts, and a partition
# skipped after a passing build is an infrastructure failure, not a scoped skip.
WIN_ALL_T='{"win_rust":"true","ui":"true"}'
WIN_SPLIT=(clippy=win_rust test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui ui-unit=ui)
verdict pull_request 0 "windows split: everything passed" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=success docs=success ui-e2e=success ui-unit=success release=skipped
verdict pull_request 1 "windows split: a partition failed" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=failure docs=success ui-e2e=success ui-unit=success release=skipped
verdict pull_request 1 "windows split: partitions skipped after a passing build" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=skipped docs=success ui-e2e=success ui-unit=success release=skipped
verdict pull_request 1 "windows split: the build failed, partitions skipped" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=failure tests=skipped docs=success ui-e2e=success ui-unit=success release=skipped
verdict pull_request 1 "windows split: the doc job failed" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=success docs=failure ui-e2e=success ui-unit=success release=skipped
verdict pull_request 1 "windows split: the Edge job failed" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=success docs=success ui-e2e=failure ui-unit=success release=skipped
verdict pull_request 1 "windows split: the UI unit job failed" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=success docs=success ui-e2e=success ui-unit=failure release=skipped
verdict pull_request 1 "windows split: the doc job is missing from needs" "$WIN_ALL_T" "${WIN_SPLIT[@]}" -- \
    clippy=success test-build=success tests=success ui-e2e=success ui-unit=success release=skipped
# Every context x scope state x job x result, per result job. A context is an event with the
# variables that change the verdict: the light mode (PUDDLE_QUEUE, the pull request's base) and a
# push that a merge queue run tested (PUDDLE_QUEUE_SKIP_PUSH, the ref, the queue-tested job). On a
# pull request to develop and a merge queue run scope's outputs are each true, false or lost; on
# every other event they are all true. The expectation is written out here, apart from the script:
# a job passes when it succeeded, or was skipped with its output false (the light twin: with its
# output not false), or skipped with the nightly flag on a schedule, with the queue mark in the
# light mode, or with the push mark on a tested push. Each case changes one job's result, the rest
# succeed.
#   name:event:base:PUDDLE_QUEUE:PUDDLE_QUEUE_SKIP_PUSH:ref:queue-tested result/skip output
CONTEXTS=(
    push:push::on::refs/heads/develop:skipped/
    push-tested:push::on:on:refs/heads/develop:success/true
    push-tested-off:push::::refs/heads/develop:success/true
    push-tested-no:push:::on:refs/heads/develop:success/false
    push-tested-main:push:::on:refs/heads/main:success/true
    schedule:schedule::on:on:refs/heads/develop:skipped/
    dispatch:workflow_dispatch::on:on:refs/heads/develop:success/true
    pr:pull_request:develop:::refs/pull/7/merge:skipped/
    pr-light:pull_request:develop:on::refs/pull/7/merge:skipped/
    pr-main-on:pull_request:main:on::refs/pull/7/merge:skipped/
    queue:merge_group::on:on:refs/heads/gh-readonly-queue/develop/pr-7-abc:skipped/
)
matrix() { # matrix <label> <nightly flag or ""> <outputs: key,key> <job=key[,flags]>...
    local label=$1 flag=$2 keys=$3 spec job key neg flags res st out sc want rc k i n=0 ctx
    local name event base q sp ref qt scoped light tested
    shift 3
    local -a ks states
    IFS=, read -r -a ks <<<"$keys"
    for ctx in "${CONTEXTS[@]}"; do
        IFS=: read -r name event base q sp ref qt <<<"$ctx"
        scoped=0 light=0 tested=0
        { [ "$event" = merge_group ] || { [ "$event" = pull_request ] && [ "$base" = develop ]; }; } && scoped=1
        [ "$event" = pull_request ] && [ "$base" = develop ] && [ "$q" = on ] && light=1
        [ "$event" = push ] && [ "$sp" = on ] && [ "$ref" = refs/heads/develop ] && [ "$qt" = success/true ] && tested=1
        # Scope states: one assignment of true/false/lost per output key (3^n when scoped).
        states=("")
        if [ "$scoped" = 1 ]; then
            for k in "${ks[@]}"; do
                local -a nx=()
                for st in "${states[@]}"; do for out in true false lost; do nx+=("$st$k=$out;"); done; done
                states=("${nx[@]}")
            done
        else
            for k in "${ks[@]}"; do states[0]+="$k=true;"; done
        fi
        for st in "${states[@]}"; do
            sc="{"
            for i in ${st//;/ }; do
                [ "${i#*=}" = lost ] || sc="$sc\"${i%%=*}\":\"${i#*=}\","
            done
            sc="${sc%,}}"
            for spec in "$@"; do
                job=${spec%%=*} key=${spec#*=} neg=0 flags=","
                case $key in *,*) flags=",${key#*,}," key=${key%%,*} ;; esac
                case $key in '!'*) neg=1 key=${key#!} ;; esac
                out=lost
                for i in ${st//;/ }; do [ "${i%%=*}" = "$key" ] && out=${i#*=}; done
                for res in success skipped failure cancelled; do
                    case $res in
                    success) want=0 ;;
                    skipped)
                        want=1
                        [ "$neg" = 0 ] && [ "$out" = false ] && want=0
                        [ "$neg" = 1 ] && [ "$out" != false ] && want=0
                        [ -n "$flag" ] && [ "$event" = schedule ] && want=0
                        [ "$light" = 1 ] && [[ $flags == *,queue,* ]] && want=0
                        [ "$tested" = 1 ] && [[ $flags == *,pushskip,* ]] && want=0
                        ;;
                    *) want=1 ;;
                    esac
                    local results=() other
                    for other in "$@"; do
                        [ "${other%%=*}" = "$job" ] && results+=("$job=$res") || results+=("${other%%=*}=success")
                    done
                    results+=("queue-tested=${qt%%/*}")
                    rc=0
                    env -u PUDDLE_QUEUE -u PUDDLE_QUEUE_SKIP_PUSH -u PUDDLE_TIER \
                        NEEDS="$(needs "$sc" "${results[@]}" | jq -c --arg s "${qt#*/}" '.["queue-tested"].outputs = {skip: $s}')" \
                        GITHUB_EVENT_NAME="$event" PR_BASE_REF="$base" GITHUB_REF="$ref" \
                        ${q:+PUDDLE_QUEUE=$q} ${sp:+PUDDLE_QUEUE_SKIP_PUSH=$sp} \
                        "$scope" verdict ${flag:+"$flag"} "$@" >/dev/null 2>&1 || rc=$?
                    [ "$rc" = "$want" ] || bad "matrix $label: $name, scope $sc, $job $res: exit $rc, want $want"
                    n=$((n + 1))
                done
            done
        done
    done
    MATRIX_CASES=$((${MATRIX_CASES:-0} + n))
}
# The specs are the result jobs' own (the wiring check below compares them with the workflows).
# The three run side by side (about 2000 cases); each prints its case count.
matrix_bg() { (fail=0; matrix "$@"; echo "matrix $1: $MATRIX_CASES cases" >&2; exit "$fail") & mpids+=($!); }
mpids=()
matrix_bg ci "" ci_rust,ui rust=ci_rust,pushskip gates=ci_rust,pushskip msvc=ci_rust,pushskip 'light=!ci_rust' ui=ui,pushskip ui-webkit=ui,queue,pushskip,defer:defer_linux_webkit
matrix_bg windows "" win_rust,ui clippy=win_rust,queue,defer:defer_win_clippy test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui,queue,defer:defer_win_ui ui-unit=ui,queue,defer:defer_win_ui
matrix_bg vm-linux --nightly-skips vm build=vm,queue agent=vm,queue tests=vm,queue
matrix_bg vm-windows --nightly-skips vm build=vm,queue,defer:defer_vm_windows agent=vm,queue,defer:defer_vm_windows tests=vm,queue,defer:defer_vm_windows
for p in "${mpids[@]}"; do wait "$p" || fail=1; done
verdict push 0 "ci push: light skipped because the Rust gates ran" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=success
verdict schedule 0 "ci nightly: light skipped" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=success
verdict workflow_dispatch 0 "ci dispatch: light skipped" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success light=skipped ui=success ui-webkit=success
verdict push 1 "ci push: light ran, rust skipped" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped gates=skipped light=success ui=success ui-webkit=success

# The light mode: a queue-only job may skip on a pull request to develop with PUDDLE_QUEUE=on, and
# nowhere else. The verdict works the mode out from the event, not from the scope job's output.
VM_Q=(--nightly-skips 'build=vm,queue' 'agent=vm,queue' 'tests=vm,queue')
WIN_Q=('clippy=win_rust,queue' 'test-build=win_rust' 'tests=win_rust' 'docs=win_rust' 'ui-e2e=ui,queue' 'ui-unit=ui,queue')
WIN_ALL='{"win_rust":"true","ui":"true","light":"true"}'
VERDICT_ENV=(PUDDLE_QUEUE=on PR_BASE_REF=develop)
verdict pull_request 0 "light: vm skipped" "$VM" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped
verdict pull_request 0 "light: windows clippy and ui skipped, tests ran, release skipped" "$WIN_ALL" "${WIN_Q[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=skipped
verdict pull_request 1 "light: windows tests skipped though a rust path changed" "$WIN_ALL" "${WIN_Q[@]}" -- clippy=skipped test-build=skipped tests=skipped docs=skipped ui-e2e=skipped ui-unit=skipped release=skipped
verdict pull_request 1 "light: windows tests failed" "$WIN_ALL" "${WIN_Q[@]}" -- clippy=skipped test-build=failure tests=failure docs=failure ui-e2e=skipped ui-unit=skipped release=skipped
verdict pull_request 1 "light: windows tests cancelled" "$WIN_ALL" "${WIN_Q[@]}" -- clippy=skipped test-build=cancelled tests=cancelled docs=cancelled ui-e2e=skipped ui-unit=skipped release=skipped
WIN_DOCS='{"win_rust":"false","ui":"false","light":"true"}'
verdict pull_request 0 "light: docs-only, every windows job skipped by scope" "$WIN_DOCS" "${WIN_Q[@]}" -- clippy=skipped test-build=skipped tests=skipped docs=skipped ui-e2e=skipped ui-unit=skipped release=skipped
verdict pull_request 1 "light: docs-only, a windows job failed" "$WIN_DOCS" "${WIN_Q[@]}" -- clippy=skipped test-build=failure tests=failure docs=failure ui-e2e=skipped ui-unit=skipped release=skipped
verdict pull_request 0 "light: vm ran anyway" "$VM" "${VM_Q[@]}" -- build=success agent=success tests=success
verdict pull_request 1 "light: vm build failed" "$VM" "${VM_Q[@]}" -- build=failure agent=failure tests=skipped
verdict pull_request 1 "light: a cancelled windows job" "$WIN_ALL" "${WIN_Q[@]}" -- clippy=cancelled test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=skipped
verdict pull_request 1 "light: a skip without the queue mark" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped gates=skipped light=skipped ui=success ui-webkit=success
verdict merge_group 1 "light env on a merge queue run" "$VM" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped
verdict push 1 "light env on a push" "$VM" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped
VERDICT_ENV=(PUDDLE_QUEUE=on PR_BASE_REF=main)
verdict pull_request 1 "light: pull request to main" "$WIN_ALL" "${WIN_Q[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=success
VERDICT_ENV=(PUDDLE_QUEUE=ON PR_BASE_REF=develop)
verdict pull_request 1 "light: PUDDLE_QUEUE=ON is not on" "$VM" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped
VERDICT_ENV=(PR_BASE_REF=develop)
verdict pull_request 1 "light: PUDDLE_QUEUE unset" "$VM" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped
verdict merge_group 1 "merge queue: a wrongly skipped vm job" "$VM" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped
verdict merge_group 0 "merge queue: vm scoped out by the group's diff" "$VMF" "${VM_Q[@]}" -- build=skipped agent=skipped tests=skipped

# The nightly tier: the result job accepts the skip of a moved job on a merge queue run only with
# PUDDLE_TIER=on and scope's defer output true for that job; the summary says so.
VMT=(--nightly-skips 'build=vm,queue,defer:defer_vm_windows' 'agent=vm,queue,defer:defer_vm_windows' 'tests=vm,queue,defer:defer_vm_windows')
WINT=('clippy=win_rust,queue,defer:defer_win_clippy' 'test-build=win_rust' 'tests=win_rust' 'docs=win_rust' 'ui-e2e=ui,queue,defer:defer_win_ui' 'ui-unit=ui,queue,defer:defer_win_ui')
VM_D='{"vm":"true","defer_vm_windows":"true"}'
VM_E='{"vm":"true","defer_vm_windows":"false"}'
WIN_D='{"win_rust":"true","ui":"true","defer_win_clippy":"true","defer_win_ui":"true"}'
VERDICT_ENV=(PUDDLE_TIER=on)
verdict merge_group 0 "tier: vm deferred, both jobs skipped" "$VM_D" "${VMT[@]}" -- build=skipped agent=skipped tests=skipped
verdict merge_group 0 "tier: vm deferred but it ran anyway" "$VM_D" "${VMT[@]}" -- build=success agent=success tests=success
verdict merge_group 1 "tier: vm escalated and skipped" "$VM_E" "${VMT[@]}" -- build=skipped agent=skipped tests=skipped
verdict merge_group 0 "tier: vm escalated and ran" "$VM_E" "${VMT[@]}" -- build=success agent=success tests=success
verdict merge_group 1 "tier: vm escalated, a test failed" "$VM_E" "${VMT[@]}" -- build=success agent=success tests=failure
verdict merge_group 1 "tier: vm deferred output lost" "$VM" "${VMT[@]}" -- build=skipped agent=skipped tests=skipped
verdict merge_group 1 "tier: vm deferred, build cancelled" "$VM_D" "${VMT[@]}" -- build=cancelled agent=cancelled tests=skipped
verdict merge_group 1 "tier: vm deferred, build failed" "$VM_D" "${VMT[@]}" -- build=failure agent=failure tests=skipped
verdict merge_group 0 "tier: windows ui and clippy deferred, tests ran" "$WIN_D" "${WINT[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=skipped
verdict merge_group 1 "tier: windows tests are never deferred" "$WIN_D" "${WINT[@]}" -- clippy=skipped test-build=skipped tests=skipped docs=skipped ui-e2e=skipped ui-unit=skipped release=skipped
verdict merge_group 1 "tier: ui deferred, clippy escalated and skipped" '{"win_rust":"true","ui":"true","defer_win_clippy":"false","defer_win_ui":"true"}' "${WINT[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=skipped
verdict merge_group 1 "tier: a deferred ui job failed" "$WIN_D" "${WINT[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=failure ui-unit=failure release=skipped
for ev in pull_request push schedule workflow_dispatch; do
    verdict "$ev" 1 "tier: $ev never accepts the deferred skip (ui)" "$WIN_D" "${WINT[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=skipped
done
verdict pull_request 1 "tier: a pull request to develop never defers (vm)" "$VM_D" "${VMT[@]}" -- build=skipped agent=skipped tests=skipped
for v in "" off ON On true 1 yes "on "; do
    VERDICT_ENV=("PUDDLE_TIER=$v")
    verdict merge_group 1 "tier: PUDDLE_TIER='$v' is not on (vm)" "$VM_D" "${VMT[@]}" -- build=skipped agent=skipped tests=skipped
    verdict merge_group 1 "tier: PUDDLE_TIER='$v' is not on (windows)" "$WIN_D" "${WINT[@]}" -- clippy=skipped test-build=success tests=success docs=success ui-e2e=skipped ui-unit=skipped release=skipped
done
# The job summary of a skipped required check says it ran in the nightly or release tier.
VERDICT_ENV=(PUDDLE_TIER=on)
: >"$tmp/summary"
env -u PUDDLE_QUEUE -u PR_BASE_REF NEEDS="$(needs "$VM_D" build=skipped agent=skipped tests=skipped)" GITHUB_EVENT_NAME=merge_group PUDDLE_TIER=on \
    GITHUB_STEP_SUMMARY="$tmp/summary" "$scope" verdict "${VMT[@]}" >/dev/null 2>&1 || bad "tier: summary case did not pass"
grep -q 'nightly and release tier' "$tmp/summary" || bad "tier: the summary of a deferred skip must name the nightly and release tier"
grep -q 'not a test result' "$tmp/summary" || bad "tier: the summary of a deferred skip must say it is not a test result"
: >"$tmp/summary"
env -u PUDDLE_QUEUE -u PR_BASE_REF NEEDS="$(needs "$VM_E" build=success agent=success tests=success)" GITHUB_EVENT_NAME=merge_group PUDDLE_TIER=on \
    GITHUB_STEP_SUMMARY="$tmp/summary" "$scope" verdict "${VMT[@]}" >/dev/null 2>&1 || bad "tier: summary case (ran) did not pass"
[ ! -s "$tmp/summary" ] || bad "tier: a job that ran leaves no deferral note in the summary"
VERDICT_ENV=()

# A develop push run whose commit a green merge queue run tested: the heavy ci jobs may skip when
# PUDDLE_QUEUE_SKIP_PUSH=on and the queue-tested job succeeded and said so; anything else runs them.
CI_P=('rust=ci_rust,pushskip' 'gates=ci_rust,pushskip' 'msvc=ci_rust,pushskip' 'light=!ci_rust' 'ui=ui,pushskip' 'ui-webkit=ui,queue,pushskip,defer:defer_linux_webkit')
TESTED='queue-tested=success'
needs_tested() { needs "$@" | jq -c '.["queue-tested"].outputs = {skip: "'"${SKIP_OUT-true}"'"}'; }
push_case() { # push_case <want> <label> <job=result>...
    local exp=$1 label=$2 rc=0
    shift 2
    env -u PUDDLE_QUEUE -u PUDDLE_QUEUE_SKIP_PUSH -u PUDDLE_TIER NEEDS="$(needs_tested "$CI_FULL" "$@")" GITHUB_EVENT_NAME="${PUSH_EVENT:-push}" \
        GITHUB_REF="${PUSH_REF:-refs/heads/develop}" "${VERDICT_ENV[@]}" "$scope" verdict "${CI_P[@]}" >/dev/null 2>&1 || rc=$?
    [ "$rc" = "$exp" ] || bad "verdict: tested push: $label: exit $rc, want $exp"
}
VERDICT_ENV=(PUDDLE_QUEUE_SKIP_PUSH=on)
push_case 0 "all heavy jobs skipped" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
push_case 0 "heavy jobs ran anyway" rust=success gates=success msvc=success light=skipped ui=success ui-webkit=success "$TESTED"
push_case 1 "rust failed" rust=failure gates=failure msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
push_case 1 "lookup failed" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped queue-tested=failure
push_case 1 "lookup skipped" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped queue-tested=skipped
SKIP_OUT=false push_case 1 "lookup said run" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
SKIP_OUT="" push_case 1 "lookup output lost" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
PUSH_REF=refs/heads/main push_case 1 "a push to main" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
PUSH_EVENT=workflow_dispatch push_case 1 "a dispatch" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
PUSH_EVENT=merge_group push_case 1 "a merge queue run" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
VERDICT_ENV=()
push_case 1 "PUDDLE_QUEUE_SKIP_PUSH unset" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
VERDICT_ENV=(PUDDLE_QUEUE_SKIP_PUSH=yes)
push_case 1 "PUDDLE_QUEUE_SKIP_PUSH=yes is not on" rust=skipped gates=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
VERDICT_ENV=()

# full-steps: off the light mode every guarded step must have succeeded; in the light mode anything goes.
steps_case() { # steps_case <want> <label> <args>...
    local exp=$1 label=$2 rc=0
    shift 2
    env -u PUDDLE_QUEUE -u PR_BASE_REF "${STEPS_ENV[@]}" "$scope" full-steps "$@" >/dev/null 2>&1 || rc=$?
    [ "$rc" = "$exp" ] || bad "full-steps: $label: exit $rc, want $exp"
}
for ev in push schedule workflow_dispatch merge_group pull_request; do
    STEPS_ENV=("GITHUB_EVENT_NAME=$ev" PR_BASE_REF=develop)
    steps_case 0 "$ev: all ran" git-env=success notices=success
    steps_case 1 "$ev: one skipped" git-env=skipped notices=success
    steps_case 1 "$ev: one failed" git-env=success notices=failure
    steps_case 1 "$ev: one lost" git-env= notices=success
    STEPS_ENV=("GITHUB_EVENT_NAME=$ev" PR_BASE_REF=develop PUDDLE_QUEUE=on)
    if [ "$ev" = pull_request ]; then
        steps_case 0 "light: skipped" git-env=skipped notices=skipped
    else
        steps_case 1 "$ev with PUDDLE_QUEUE=on: one skipped" git-env=skipped notices=success
    fi
done
STEPS_ENV=(GITHUB_EVENT_NAME=pull_request PR_BASE_REF=main PUDDLE_QUEUE=on)
steps_case 1 "pull request to main with PUDDLE_QUEUE=on: one skipped" git-env=skipped
[ "$(NEEDS='' GITHUB_EVENT_NAME=pull_request "$scope" verdict x=y >/dev/null 2>&1; echo $?)" = 1 ] || bad "verdict: empty NEEDS must fail"

# 7. The workflows are wired to match their result job's verdict (needs PyYAML).
if python3 -c 'import yaml' 2>/dev/null; then
    python3 -I - "$here/../.github/workflows" <<'PY' || fail=1
import re, sys, yaml
d = sys.argv[1]
results = {  # workflow: (result job id, required name, verdict specs as the matrix above tests them)
    "ci": ("linux", "linux (all gates)", "rust=ci_rust,pushskip gates=ci_rust,pushskip msvc=ci_rust,pushskip light=!ci_rust ui=ui,pushskip ui-webkit=ui,queue,pushskip,defer:defer_linux_webkit"),
    "windows": ("windows", "windows-msvc (build, clippy, tests)", "clippy=win_rust,queue,defer:defer_win_clippy test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui,queue,defer:defer_win_ui ui-unit=ui,queue,defer:defer_win_ui"),
    "vm-linux": ("vm-linux", "vm-linux (KVM, msb microVMs)", "--nightly-skips build=vm,queue agent=vm,queue tests=vm,queue"),
    "vm-windows": ("vm-windows", "vm-windows (WHP, msb microVMs)", "--nightly-skips build=vm,queue,defer:defer_vm_windows agent=vm,queue,defer:defer_vm_windows tests=vm,queue,defer:defer_vm_windows"),
}
LIGHT = "needs.scope.outputs.light != 'true'"
PUSHSKIP = "needs.queue-tested.outputs.skip != 'true'"
bad = []
def flat(x):
    return str(x).replace(" ", "")
for wf, (rid, rname, want_specs) in results.items():
    doc = yaml.safe_load(open(f"{d}/{wf}.yml"))
    on = doc.get("on", doc.get(True)) or {}
    # Every workflow that reports a required check runs on both events, or the check stays pending.
    if "pull_request" not in on:
        bad.append(f"{wf}: no pull_request trigger")
    # The nightly tier runs on no push trigger; a release PR to main must still reach the VM and Windows jobs.
    if wf != "ci" and "main" not in (on.get("pull_request") or {}).get("branches", []):
        bad.append(f"{wf}: pull_request must include branch main (the release PR runs the nightly-tier jobs)")
    if wf != "ci" and "push" in on:
        bad.append(f"{wf}: no push trigger (the nightly tier runs nightly and on a PR to main only)")
    mg = on.get("merge_group") if isinstance(on, dict) else None
    if not mg or mg.get("types") != ["checks_requested"]:
        bad.append(f"{wf}: needs `merge_group: types: [checks_requested]`")
    jobs = doc["jobs"]
    res = jobs[rid]
    if res.get("name") != rname:
        bad.append(f"{wf}: result job name {res.get('name')!r}, want {rname!r}")
    # ci.yml's nightly (schedule) runs only its advisories jobs, so its result job skips on that event.
    want_if = "${{always()&&github.event_name!='schedule'}}" if wf == "ci" else "${{always()}}"
    if flat(res.get("if", "")) != want_if:
        bad.append(f"{wf}: result job must run with always()")
    needs = res["needs"]
    sc = jobs.get("scope")
    if not sc or "ci/scope.sh" not in str(sc["steps"]):
        bad.append(f"{wf}: no scope job running ci/scope.sh")
        continue
    if "light" not in sc.get("outputs", {}):
        bad.append(f"{wf}: scope job has no output light")
    scope_env = [s for s in sc["steps"] if s.get("run") == "ci/scope.sh"][0].get("env", {})
    for k, v in (("PUDDLE_QUEUE", "${{ vars.PUDDLE_QUEUE }}"),
                 ("MERGE_GROUP_BASE_SHA", "${{ github.event.merge_group.base_sha }}"),
                 ("MERGE_GROUP_HEAD_SHA", "${{ github.event.merge_group.head_sha }}"),
                 ("PR_BASE_REF", "${{ github.event.pull_request.base.ref }}")):
        if scope_env.get(k) != v:
            bad.append(f"{wf}: scope step env {k}={scope_env.get(k)!r}, want {v!r}")
    vstep = [s for s in res["steps"] if "run" in s][-1]
    venv = vstep.get("env", {})
    tiered = "defer:" in want_specs
    for k, v in (("PUDDLE_QUEUE", "${{ vars.PUDDLE_QUEUE }}"), ("PR_BASE_REF", "${{ github.event.pull_request.base.ref }}")):
        if venv.get(k) != v:
            bad.append(f"{wf}: verdict step env {k}={venv.get(k)!r}, want {v!r}")
    if tiered and (venv.get("PUDDLE_TIER") != "${{ vars.PUDDLE_TIER }}" or scope_env.get("PUDDLE_TIER") != "${{ vars.PUDDLE_TIER }}"):
        bad.append(f"{wf}: the scope and verdict steps need PUDDLE_TIER from vars")
    if "pushskip" in want_specs and venv.get("PUDDLE_QUEUE_SKIP_PUSH") != "${{ vars.PUDDLE_QUEUE_SKIP_PUSH }}":
        bad.append(f"{wf}: verdict step needs PUDDLE_QUEUE_SKIP_PUSH from vars")
    m = re.search(r"ci/scope\.sh verdict (.*)", vstep["run"])
    got_specs = " ".join(t.replace("'", "") for t in m.group(1).split()) if m else ""
    if got_specs != want_specs:
        bad.append(f"{wf}: verdict specs {got_specs!r}, want {want_specs!r} (change the matrix with them)")
    specs = {}
    for tok in (t for t in got_specs.split() if not t.startswith("--")):
        j, k = tok.split("=", 1)
        specs[j] = k
    for j, k in specs.items():
        if j not in needs:
            bad.append(f"{wf}: verdict names {j} which the result job does not need")
        key, *flags = k.split(",")
        key = key.lstrip("!")
        jn = jobs[j].get("needs") or []
        jn = [jn] if isinstance(jn, str) else jn
        # A job behind a scoped job of the same spec (a test partition after its build) is skipped
        # by that job's skip; the others carry the condition themselves.
        behind = any(specs.get(x) == k for x in jn)
        cond = str(jobs[j].get("if", ""))
        want = f"needs.scope.outputs.{key} " + ("== 'false'" if k.startswith("!") else "!= 'false'")
        if not behind and (want not in cond or "!cancelled()" not in cond):
            bad.append(f"{wf}: job {j} if={cond!r}, want !cancelled() and {want!r}")
        if not behind and "scope" not in jn:
            bad.append(f"{wf}: job {j} does not need scope")
        # A queue-only job skips exactly in the light mode; a pushskip job exactly when queue-tested says so.
        if not behind and ("queue" in flags) != (LIGHT in cond):
            bad.append(f"{wf}: job {j}: the `queue` mark and {LIGHT!r} in its if must go together")
        if not behind and ("pushskip" in flags) != (PUSHSKIP in cond and "queue-tested" in jn):
            bad.append(f"{wf}: job {j}: the `pushskip` mark and {PUSHSKIP!r} (needing queue-tested) must go together")
        if key not in sc.get("outputs", {}):
            bad.append(f"{wf}: scope job has no output {key}")
        # A job moved to the nightly tier skips exactly when scope says `defer_...` is true (and only then).
        dkeys = [f[6:] for f in flags if f.startswith("defer:")]
        if not behind and bool(dkeys) != ("outputs.defer_" in cond):
            bad.append(f"{wf}: job {j}: the `defer:` mark and a defer_ output in its if must go together")
        for dk in dkeys:
            if not behind and f"needs.scope.outputs.{dk} != 'true'" not in cond:
                bad.append(f"{wf}: job {j} if={cond!r} lacks needs.scope.outputs.{dk} != 'true'")
            if dk not in sc.get("outputs", {}):
                bad.append(f"{wf}: scope job has no output {dk}")
    for j in needs:
        if j == "scope" or j in specs:
            continue
        # Unscoped jobs: the release build, the queue-tested lookup and the advisories checks, each with
        # its own event rule (advisories: not on a pull request to develop or a merge queue run, so a new
        # RustSec or npm entry fails no such run).
        cond = flat(jobs[j].get("if", ""))
        if wf == "ci" and j in ("advisories", "advisories-npm"):
            if ("github.event.pull_request.base.ref=='main'" not in cond or "github.event_name!='pull_request'" not in cond
                    or "github.event_name!='merge_group'&&" not in cond):
                bad.append(f"ci: job {j} if={cond!r} must skip exactly on a merge queue run and a pull request not to main")
        elif wf == "ci" and j == "queue-tested":
            for part in ("github.event_name=='push'", "github.ref=='refs/heads/develop'", "vars.PUDDLE_QUEUE_SKIP_PUSH=='on'"):
                if part not in cond:
                    bad.append(f"ci: job queue-tested if={cond!r} lacks {part}")
        elif not (wf == "windows" and j == "release"):
            bad.append(f"{wf}: needed job {j} is neither scoped nor the release build")
    if wf == "windows" and "merge_group" in flat(jobs["release"].get("if", "")):
        bad.append("windows: the release build has no merge queue twin and must not run there")
    # Every other job of the workflow is reached through the result job (directly or via needs).
    for j, spec in jobs.items():
        if j in (rid, "scope") or j in needs:
            continue
        n = spec.get("needs")
        n = [n] if isinstance(n, str) else (n or [])
        if not any(x in needs for x in n):
            bad.append(f"{wf}: job {j} is not covered by the result job")
    # Steps the light mode skips inside a running job: the verdict cannot see them, so the job's
    # full-steps guard names each one (by its id) and reads the event's own variables. A job's own
    # environment never narrows in the light mode: a skipped job (`ui-webkit`) is what the verdict sees.
    for j, spec in jobs.items():
        steps = spec.get("steps") or []
        light_steps = [s for s in steps if "outputs.light" in str(s.get("if", ""))]
        if "outputs.light" in str(spec.get("env", "")):
            bad.append(f"{wf}: job {j} narrows its environment in the light mode, which no guard can see (skip the job instead)")
        if not light_steps:
            continue
        guards = [s for s in steps if "ci/scope.sh full-steps" in str(s.get("run", ""))]
        if len(guards) != 1:
            bad.append(f"{wf}: job {j} skips steps in the light mode but has {len(guards)} full-steps guards")
            continue
        g = guards[0]
        tail = [x for x in steps if x.get("uses") != "./.github/actions/cancel-group"]
        if tail[-1] is not g:
            bad.append(f"{wf}: job {j}: the full-steps guard must be the last step (before the cancel-group step)")
        if flat(g.get("if", "")) != "${{!cancelled()}}":
            bad.append(f"{wf}: job {j}: the full-steps guard must run with !cancelled()")
        genv = g.get("env", {})
        if genv.get("PUDDLE_QUEUE") != "${{ vars.PUDDLE_QUEUE }}" or genv.get("PR_BASE_REF") != "${{ github.event.pull_request.base.ref }}":
            bad.append(f"{wf}: job {j}: the full-steps guard needs PUDDLE_QUEUE and PR_BASE_REF")
        for s in light_steps:
            sid = s.get("id")
            if not sid or f"{sid}=${{{{ steps.{sid}.outcome }}}}" not in g["run"]:
                bad.append(f"{wf}: job {j}: step {s.get('name')!r} skips in the light mode but the guard does not check it")
    # The Linux browsers: the `ui` job runs the end-to-end tests on Chromium, `ui-webkit` on WebKit, and nothing else
    # narrows them, so a pull request to main runs both (webkit.yml runs WebKit nightly for the skips above).
    if wf == "ci":
        for j, want in (("ui", "chromium"), ("ui-webkit", "webkit")):
            got = (jobs[j].get("env") or {}).get("PUDDLE_E2E_PROJECTS")
            if got != want:
                bad.append(f"ci: job {j} runs PUDDLE_E2E_PROJECTS={got!r}, want {want!r}")
            if not any("scripts/check.sh ui-e2e" in str(s.get("run", "")) for s in jobs[j]["steps"]):
                bad.append(f"ci: job {j} does not run the ui-e2e gate")
        # The two browser entries have their own keys (a shared one would give the WebKit job a Chromium-only hit),
        # webkit.yml restores the WebKit one, and the queue-tested lookup covers both.
        text = open(f"{d}/ci.yml").read()
        if text.count("key: playwright-webkit-${{ runner.os }}-${{ hashFiles('ui/package-lock.json') }}") < 3:
            bad.append("ci: the ui-webkit job and queue-tested must use the key playwright-webkit-... for the WebKit browsers")
        if "key: playwright-webkit-${{ runner.os }}" not in open(f"{d}/webkit.yml").read():
            bad.append("webkit: must restore the key playwright-webkit-... the ui-webkit job saves")
# The nightly gates of the VM jobs, Windows UI and clippy count only nightly and manual runs: a merge
# queue run on develop's commit must not skip the soak tests or the nightly UI run.
for wf in ("windows", "vm-linux", "vm-windows"):
    gates = re.findall(r"run: ci/should-run\.sh \S+(.*)", open(f"{d}/{wf}.yml").read())
    if not gates:
        bad.append(f"{wf}: no should-run.sh gate found")
    for g in gates:
        if g.strip() != "--events schedule,workflow_dispatch":
            bad.append(f"{wf}: nightly gate {g.strip()!r} must be exactly '--events schedule,workflow_dispatch'")

# The Windows tests: the partition jobs run the archive the build job shares, the matrix and the `hash:` split
# have one size, the build archives the workspace the way the partitions run it, and every job that opens
# a browser or the shell stops the storage service.
wj = yaml.safe_load(open(f"{d}/windows.yml"))["jobs"]
wtext2 = open(f"{d}/windows.yml").read()
if wj["tests"].get("needs") != "test-build":
    bad.append("windows: the partition job must need exactly test-build")
parts = wj["tests"]["strategy"]["matrix"]["partition"]
if len(parts) != 4 or parts != list(range(1, len(parts) + 1)):
    bad.append(f"windows: partitions {parts}, want 1..4 (the job names say 'of 4')")
if "of 4)" not in wj["tests"]["name"] or "of 4)" not in str([s.get("name") for s in wj["tests"]["steps"]]):
    bad.append("windows: the partition job and step names must say 'of 4' (the matrix size)")
if 'hash:${PARTITION}/${PARTITIONS}' not in wtext2 or "PARTITIONS: ${{ strategy.job-total }}" not in wtext2:
    bad.append("windows: the partitions must split by nextest `--partition hash:` over strategy.job-total")
up = [s for s in wj["test-build"]["steps"] if s.get("uses", "").startswith("actions/upload-artifact")]
dn = [s for s in wj["tests"]["steps"] if s.get("uses", "").startswith("actions/download-artifact")]
if len(up) != 1 or len(dn) != 1 or up[0]["with"]["name"] != dn[0]["with"]["name"]:
    bad.append("windows: the build job's upload and the partition job's download must name the same artifact")
if "cargo nextest archive --workspace --all-features --locked --profile ci" not in wtext2:
    bad.append("windows: the build job must archive the workspace with --all-features --locked --profile ci")
if 'export RUSTUP_TOOLCHAIN' not in wtext2 or "rust-toolchain.toml" not in str(wj["tests"]["steps"]):
    bad.append("windows: the partitions must export RUSTUP_TOOLCHAIN from rust-toolchain.toml (dev_override runs cargo)")
if "nextest run --profile ci" not in wtext2:
    bad.append("windows: the partitions must run the ci profile")
for j in ("ui-e2e", "ui-unit"):
    if not any("Stop-Service StorSvc" in str(s.get("run", "")) for s in wj[j]["steps"]):
        bad.append(f"windows: job {j} must stop the storage service")
if not any((s.get("env") or {}).get("TEMP") == "${{ runner.temp }}" and "ui-e2e" in str(s.get("run", "")) for s in wj["ui-e2e"]["steps"]):
    bad.append("windows: the Edge end-to-end step must keep TEMP on the runner's temp disk")
for j in ("docs", "ui-e2e", "ui-unit"):
    rc = [s for s in wj[j]["steps"] if str(s.get("uses", "")).startswith("Swatinem/rust-cache")]
    if len(rc) != 1 or rc[0]["with"].get("shared-key") != "windows" or rc[0]["with"].get("save-if") is not False:
        bad.append(f"windows: job {j} must restore the `windows` Rust cache without saving it")
sv = [s for s in wj["test-build"]["steps"] if str(s.get("uses", "")).startswith("Swatinem/rust-cache")]
if len(sv) != 1 or sv[0]["with"].get("shared-key") != "windows":
    bad.append("windows: the build job saves the `windows` Rust cache")

# The WebKit nightly (webkit.yml) is what the queue's Chromium-only run leans on: it must exist, run on a
# schedule, never be narrowed, never save the shared browser cache (a WebKit-only entry would starve the
# Chromium job that restores it) and be watched by the red-signal workflow.
wk = yaml.safe_load(open(f"{d}/webkit.yml"))
won = wk.get("on", wk.get(True)) or {}
if sorted(won) != ["schedule", "workflow_dispatch"]:
    bad.append(f"webkit: triggers {sorted(won)}, want exactly schedule and workflow_dispatch (a pull request to main runs both browsers in ci.yml)")
wtext = open(f"{d}/webkit.yml").read()
if "run: ci/should-run.sh webkit.yml\n" not in wtext:
    bad.append("webkit: the nightly gate must be `ci/should-run.sh webkit.yml`")
if "actions/cache/save" in wtext:
    bad.append("webkit: must not save the Playwright cache (ci.yml's `ui-webkit` job saves that key)")
for j, spec in wk["jobs"].items():
    if (spec.get("env") or {}).get("PUDDLE_E2E_PROJECTS") != "webkit":
        bad.append(f"webkit: job {j} must run PUDDLE_E2E_PROJECTS=webkit")
    if not any("scripts/check.sh ui-e2e" in str(x.get("run", "")) for x in spec.get("steps", [])):
        bad.append(f"webkit: job {j} does not run scripts/check.sh ui-e2e")
ns = yaml.safe_load(open(f"{d}/nightly-signal.yml"))
if "webkit" not in ((ns.get("on", ns.get(True)) or {}).get("workflow_run") or {}).get("workflows", []):
    bad.append("nightly-signal: webkit is not watched")
# Fail fast in the merge queue (ci/cancel-group.sh): every job that does work ends with the cancel-group step,
# guarded by failure() on a merge_group event and best effort, and has `actions: write`; the result jobs, the
# queue-tested lookup and the advisories checks (which skip on a merge queue run) do not.
for wf, (rid, rname, want_specs) in results.items():
    wjobs = yaml.safe_load(open(f"{d}/{wf}.yml"))["jobs"]
    for j, spec in wjobs.items():
        steps = spec.get("steps") or []
        last = steps[-1] if steps else {}
        has = last.get("uses") == "./.github/actions/cancel-group"
        exempt = j == rid or (wf == "ci" and j in ("queue-tested", "advisories", "advisories-npm"))
        if exempt:
            if any(x.get("uses") == "./.github/actions/cancel-group" for x in steps):
                bad.append(f"{wf}: job {j} must not cancel the group")
            continue
        if not has:
            bad.append(f"{wf}: job {j} does not end with the cancel-group step")
            continue
        if flat(last.get("if", "")) != "${{failure()&&github.event_name=='merge_group'}}" or last.get("continue-on-error") is not True:
            bad.append(f"{wf}: job {j}: the cancel-group step needs `failure() && merge_group` and continue-on-error")
        if (spec.get("permissions") or {}).get("actions") != "write":
            bad.append(f"{wf}: job {j}: the cancel-group step needs `actions: write` on the job")
# The VM workflows fail on a missing hypervisor on a pull request and a merge queue run.
if "ci/vm-skip-policy.sh" not in open(f"{d}/vm-linux.yml").read():
    bad.append("vm-linux: the KVM preflight does not use ci/vm-skip-policy.sh")
if "$env:GITHUB_EVENT_NAME -eq 'merge_group'" not in open(f"{d}/vm-windows.yml").read():
    bad.append("vm-windows: the WHP preflight does not fail on a merge queue run")
for b in bad:
    print("FAIL:", b, file=sys.stderr)
sys.exit(1 if bad else 0)
PY
else
    echo "note: PyYAML missing, workflow wiring not checked" >&2
fi

# 7b. What `vm_unrelated` (ci/scope.sh) assumes about the real tree, so a later change cannot make it wrong silently:
# the VM workflows call no other workflow, and no top-level non-VM test file of a crate holds a `vm_`
# test or is a module of a `vm_*` test file (`mod <name>;` or `#[path]`).
root="$here/.."
if grep -Ehn "uses: *\./\.github/workflows/|workflow_call|workflow_run" "$root/.github/workflows/vm-linux.yml" "$root/.github/workflows/vm-windows.yml" >&2; then
    bad "a VM workflow calls another workflow: vm_unrelated in ci/scope.sh would skip them for it"
fi
for f in "$root"/crates/*/tests/*.rs; do
    [ -e "$f" ] || continue
    case ${f##*/} in vm_*.rs | support.rs | common.rs | mod.rs) continue ;; esac
    [ "${f#"$root"/crates/puddle-vm-tests/}" != "$f" ] && continue
    stem=${f##*/}
    stem=${stem%.rs}
    if grep -Eq 'fn vm_[a-z0-9_]*' "$f"; then bad "$f holds a vm_ test, but vm_unrelated treats it as a non-VM file"; fi
    for v in "${f%/*}"/vm_*.rs; do
        [ -e "$v" ] || continue
        if grep -Eq "^[[:space:]]*(pub +)?mod +$stem *;|#\[path *= *\"$stem\.rs\"" "$v"; then bad "$v includes $f, but vm_unrelated treats it as a non-VM file"; fi
    done
done

# 8. The crate sets against the real graph (opt in: needs cargo; CI's Rust job turns it on).
if [ "${SCOPE_TEST_METADATA:-0}" = 1 ]; then
    meta="$tmp/meta.json"
    if (cd "$here/.." && cargo metadata --no-deps --offline --format-version 1 >"$meta" 2>"$tmp/meta.err"); then
        python3 -I - "$meta" "$scope" <<'PY' || fail=1
import json, re, sys
meta = json.load(open(sys.argv[1]))
src = open(sys.argv[2]).read()
pk = {p["name"]: p for p in meta["packages"]}
def closure(roots):
    seen, st = set(roots), list(roots)
    while st:
        for d in pk[st.pop()]["dependencies"]:
            if d["name"] in pk and d["name"] not in seen:
                seen.add(d["name"]); st.append(d["name"])
    return seen
def listed(var):
    return set(re.search(var + r'=" ([^"]*) "', src).group(1).split())
vm_roots = ["puddle-vm-tests", "puddle-compute-msb", "puddle-boot", "puddle-workspace", "puddle-lifecycle", "puddle-host", "puddle-agent"]
env = re.search(r"VM_CRATES: (.*)", open(sys.argv[2].replace("scope.sh", "../.github/workflows/vm-linux.yml")).read()).group(1)
want_vm = set(re.findall(r"-p (\S+)", env)) | {"puddle-agent"}
bad = []
if want_vm != set(vm_roots):
    bad.append(f"the VM crates in vm-linux.yml are {sorted(want_vm)}; scope-test.sh roots are {sorted(vm_roots)}")
for var, roots in (("NO_VM", vm_roots), ("NO_UI", ["puddle-e2e", "puddle-app"])):
    want = set(pk) - closure(roots)
    if listed(var) != want:
        bad.append(f"{var} is {sorted(listed(var))}; cargo metadata says {sorted(want)}")
dirs = {p["manifest_path"].split("/crates/")[-1].split("/")[0] for p in meta["packages"] if "/crates/" in p["manifest_path"]}
if dirs != set(pk):
    bad.append(f"crate directories {sorted(dirs)} differ from package names")
for b in bad:
    print("FAIL:", b, file=sys.stderr)
sys.exit(1 if bad else 0)
PY
    else
        bad "cargo metadata failed: $(cat "$tmp/meta.err")"
    fi
fi

if [ "$fail" = 0 ]; then
    echo "ok"
else
    exit 1
fi
