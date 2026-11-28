#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
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
always_re='^(Cargo\.lock|Cargo\.toml|rust-toolchain\.toml|\.cargo/|\.config/|\.githooks/|\.github/workflows/|ci/|scripts/|crates/puddle-vm-tests/|crates/puddle-api/openapi/|ui/package-lock\.json|ui/src/lib/api/schema\.d\.ts|docs/serve\.md$|ui/src/lib/settings/model\.ts$|.*/Cargo\.toml$)'
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
# Every path of the fixture exists in this checkout: a moved or deleted file leaves the replay
# classifying a path the rules no longer see.
while IFS= read -r line; do
    case $line in
    '#'* | '' | @*) ;;
    *) [ -e "$here/../$line" ] || bad "fixture path does not exist: $line (point it at the file's new home, or swap in a live path of the same class)" ;;
    esac
done <"$fixture"
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

# 3a. Files a Rust test includes from outside crates/ (docs/serve.md and the settings model.ts used to be
# `docs` and `ui`, so a change to only one of them skipped the Rust test that reads it).
want "included docs page" "$FULL" docs/serve.md
want "included docs page with ui" "$FULL" ui/src/a.ts docs/serve.md
want "included settings model" "$FULL" ui/src/lib/settings/model.ts
want "included settings model with docs" "$FULL" docs/STANDARDS.md ui/src/lib/settings/model.ts
# Guard: every include_str!/include_bytes! path of a crate source that resolves outside crates/ must be
# a path the rules run the Rust gates for (always, gate, fixture), so a new include cannot slip past
# the list above. ui/src/lib/settings/cpus.ts is the one exception: puddle-e2e includes it and the UI
# jobs build puddle-e2e, so a `ui` class change already runs its test.
root=$(cd "$here/.." && pwd)
included=0
while IFS= read -r -d '' rs; do
    dir=$(dirname "$rs")
    while IFS= read -r rel; do
        [ -n "$rel" ] || continue
        abs=$(realpath -m "$dir/$rel")
        repo=${abs#"$root"/}
        case $repo in crates/*) continue ;; esac
        included=$((included + 1))
        case $repo in ui/src/lib/settings/cpus.ts) continue ;; esac
        got=$(classify "$repo")
        case $got in
        "$FULL" | "rust "* | "mixed "*) ;;
        *) bad "${rs#"$root"/} includes $repo, which scopes as '$got': add it to path_class in ci/scope.sh (always)" ;;
        esac
    done < <(grep -Pzo 'include_(str|bytes)!\(\s*"[^"]+"' "$rs" | tr '\0' '\n' | sed -n 's/^include_[a-z]*!(\s*"\(.*\)"$/\1/p')
done < <(find "$root/crates" -name '*.rs' -not -path '*/target/*' -print0)
[ "$included" -ge 5 ] || bad "include guard found only $included includes outside crates/ (the scan is broken)"

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
# Windows `cfg` content: classify takes the list of Rust files that hold it in SCOPE_CFG_WINDOWS (the
# merge queue run fills it from the files at the tested commit; section 5b drives that part).
cfgdefers() { # cfgdefers <list of cfg files, newline separated> <paths...>
    local list=$1
    shift
    printf '%s\n' "$@" | env SCOPE_CFG_WINDOWS="$list" PUDDLE_TIER=on GITHUB_EVENT_NAME=merge_group "$scope" classify | sort |
        awk -F= '{v[$1]=$2} END {print v["defer_vm_windows"], v["defer_win_ui"], v["defer_win_clippy"], v["defer_linux_webkit"]}'
}
[ "$(cfgdefers "crates/puddle-store/src/lib.rs" crates/puddle-store/src/lib.rs)" = "false true false true" ] ||
    bad "tier: a Rust file with Windows cfg content must pull the VM and clippy jobs back"
[ "$(cfgdefers "crates/puddle-store/src/lib.rs" crates/puddle-store/src/lib.rs crates/puddle-types/src/a.rs)" = "false true false true" ] ||
    bad "tier: one cfg file among others still escalates"
[ "$(cfgdefers "crates/puddle-store/src/lib.rs" crates/puddle-types/src/a.rs)" = "true true true true" ] ||
    bad "tier: a Rust file that is not on the cfg list must stay deferred"
[ "$(cfgdefers "crates/puddle-store/src/lib.rs" crates/puddle-store/src/lib.rs.bak)" = "true true true true" ] ||
    bad "tier: the cfg list matches whole paths, not prefixes"
[ "$(cfgdefers "" crates/puddle-store/src/lib.rs)" = "true true true true" ] || bad "tier: an empty cfg list defers"
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
*/contents/*)
    # `gh api -H Accept... repos/R/contents/PATH?ref=SHA` -> the file in $FAKE_CONTENTS, or an error when it is missing
    [ "${FAKE_MODE:-}" = contentserror ] && exit 1
    last=${*: -1}
    path=${last#repos/o/r/contents/}
    [ "${path%%\?ref=*}?ref=head1" = "$path" ] || exit 1
    path=${path%%\?ref=*}
    cat "$FAKE_CONTENTS/$path" || exit 1
    ;;
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
    env -u PUDDLE_QUEUE -u PUDDLE_TIER -u PUDDLE_LIGHT_CI PATH="$tmp/bin:$PATH" GITHUB_EVENT_NAME="$event" PR_BASE_REF="$base" PR_NUMBER=7 GITHUB_REPOSITORY=o/r \
        PR_HEAD_SHA="$head" FAKE_MODE="$mode" FAKE_FILES="$files" FAKE_HEAD=abc123 FAKE_CONTENTS="$tmp/contents" GITHUB_OUTPUT="$tmp/out" \
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
    [ "$(grep -c . "$tmp/out")" = 13 ] || bad "main: $label: GITHUB_OUTPUT has not exactly the thirteen outputs"
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

# 5b. Windows cfg content on a merge queue run in the nightly tier, read from the files at the tested commit.
mkdir -p "$tmp/contents/crates/a/src" "$tmp/contents/crates/b/src"
printf 'fn a() {}\n#[cfg(windows)]\nfn w() {}\n' >"$tmp/contents/crates/a/src/lib.rs"
printf 'fn b() {}\n' >"$tmp/contents/crates/b/src/lib.rs"
printf '#[cfg(any(unix,\n    target_os = "windows"))]\nfn m() {}\n' >"$tmp/contents/crates/a/src/multi.rs"
printf '#[cfg(not(windows))]\nfn n() {}\n' >"$tmp/contents/crates/a/src/not.rs"
printf 'let x = cfg!(target_os = "windows");\n' >"$tmp/contents/crates/a/src/macro.rs"
printf '// windows is mentioned here only in prose; cfg too\nfn p() {}\n' >"$tmp/contents/crates/a/src/prose.rs"
printf '#[cfg(target_os = "linux")]\nfn l() {}\n' >"$tmp/contents/crates/a/src/linux.rs"
tierfiles() { # tierfiles <FAKE_MODE> <paths...> -> "vm clippy" defer flags of the merge queue run
    local mode=$1 f="$tmp/tierfiles.txt"
    shift
    printf '%s\n' "$@" >"$f"
    : >"$tmp/out"
    env -u PUDDLE_QUEUE PATH="$tmp/bin:$PATH" GITHUB_EVENT_NAME=merge_group GITHUB_REPOSITORY=o/r PUDDLE_TIER=on \
        MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1 FAKE_MODE="$mode" FAKE_FILES="$f" FAKE_CONTENTS="$tmp/contents" \
        GITHUB_OUTPUT="$tmp/out" "$scope" >/dev/null 2>&1
    awk -F= '{v[$1]=$2} END {print v["defer_vm_windows"], v["defer_win_clippy"]}' "$tmp/out"
}
tf_want() {
    local label=$1 exp=$2 got
    shift 2
    got=$(tierfiles "$@")
    [ "$got" = "$exp" ] || bad "cfg windows: $label: got '$got', want '$exp'"
}
tf_want "a file with a cfg(windows) block" "false false" ok crates/a/src/lib.rs
tf_want "a multi-line cfg with target_os" "false false" ok crates/a/src/multi.rs
tf_want "a not(windows) cfg" "false false" ok crates/a/src/not.rs
tf_want "cfg! with target_os" "false false" ok crates/a/src/macro.rs
tf_want "a file with no cfg content stays deferred" "true true" ok crates/b/src/lib.rs
tf_want "windows in prose is not a cfg" "true true" ok crates/a/src/prose.rs
tf_want "a cfg for another os stays deferred" "true true" ok crates/a/src/linux.rs
tf_want "a cfg file beside a plain one" "false false" ok crates/b/src/lib.rs crates/a/src/lib.rs
tf_want "a file that cannot be read escalates" "false false" ok crates/b/src/gone.rs
tf_want "every read failing escalates" "false false" contentserror crates/b/src/lib.rs
tf_want "a non-Rust file is not read" "true true" contentserror docs/guide.md
mapfile -t rs_many < <(seq 1 61 | sed 's|^|crates/b/src/m|; s|$|.rs|')
tf_want "more Rust files than the read cap escalate" "false false" ok "${rs_many[@]}"
# Off the nightly tier nothing is read: a pull request run never calls the contents API.
: >"$tmp/out"
printf 'crates/b/src/lib.rs\n' >"$tmp/rs.txt"
out=$(env -u PUDDLE_TIER PATH="$tmp/bin:$PATH" GITHUB_EVENT_NAME=merge_group GITHUB_REPOSITORY=o/r MERGE_GROUP_BASE_SHA=base1 \
    MERGE_GROUP_HEAD_SHA=head1 FAKE_MODE=contentserror FAKE_FILES="$tmp/rs.txt" FAKE_CONTENTS="$tmp/contents" "$scope" 2>&1 >/dev/null)
grep -q 'Windows cfg' <<<"$out" && bad "cfg windows: reads happened without PUDDLE_TIER=on"
grep -q 'scope:' <<<"$out" || bad "cfg windows: the off-tier control run printed nothing, so it proves nothing"

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

# The light CI mode (PUDDLE_LIGHT_CI=on exactly): a pull request to develop and a merge queue run get
# every heavy output false and the unit tier the Rust and UI answers; every other event, a pull
# request to main and a doubtful file list run the full set with the unit tier off.
light_ci_want() { # light_ci_want <label> <want "ci_rust win_rust ui vm unit unit_ui"> <args of main_case>
    local label=$1 exp=$2 got
    shift 2
    main_case "light CI: $label" "$@"
    # The cross-checked Windows clippy (`msvc`): in the light CI mode it follows win_rust (a crate or fixture changed),
    # not the unit tier's ci_rust; everywhere else it follows ci_rust.
    local want_msvc=${MSVC_WANT:-}
    if [ -z "$want_msvc" ]; then
        case $exp in "false false false false"*) want_msvc=$(awk '{print $5}' <<<"$exp") ;; *) want_msvc=true ;; esac
    fi
    grep -qx "msvc=$want_msvc" "$tmp/out" || bad "main: light CI: $label: want msvc=$want_msvc, got $(grep '^msvc=' "$tmp/out")"
    got=$(awk -F= '{v[$1]=$2} END {print v["ci_rust"], v["win_rust"], v["ui"], v["vm"], v["unit"], v["unit_ui"]}' "$tmp/out")
    [ "$got" = "$exp" ] || bad "main: light CI: $label: got '$got', want '$exp'"
}
HEAVY_OFF="false false false false"
# A crate no UI or VM job builds: the Rust unit tier only.
printf 'crates/puddle-freshness/src/lib.rs\n' >"$tmp/rust-only.txt"
printf 'scripts/coverage-baseline.json\n' >"$tmp/gate-only.txt"
MAIN_ENV=(PUDDLE_LIGHT_CI=on)
light_ci_want "rust-only pull request" "$HEAVY_OFF true false" rust pull_request develop ok "$tmp/rust-only.txt"
MSVC_WANT=false light_ci_want "a gate-only pull request: the unit tier runs, the Windows clippy does not" "$HEAVY_OFF true true" rust pull_request develop ok "$tmp/gate-only.txt"
light_ci_want "ui-only pull request" "$HEAVY_OFF false true" ui pull_request develop ok "$tmp/ui.txt"
light_ci_want "docs-only pull request" "$HEAVY_OFF false false" docs pull_request develop ok "$tmp/docs.txt"
light_ci_want "always-run path" "$HEAVY_OFF true true" full pull_request develop ok "$tmp/rename.txt"
light_ci_want "fixture-only pull request" "$HEAVY_OFF true true" mixed pull_request develop ok "$tmp/fixture.txt"
light_ci_want "a crate the UI tests build" "$HEAVY_OFF true true" rust pull_request develop ok "$tmp/rust.txt"
light_ci_want "pull request to main" "true true true true false false" full pull_request main ok "$tmp/ui.txt"
light_ci_want "push" "true true true true false false" full push develop ok "$tmp/ui.txt"
light_ci_want "schedule" "true true true true false false" full schedule develop ok "$tmp/ui.txt"
light_ci_want "workflow_dispatch" "true true true true false false" full workflow_dispatch develop ok "$tmp/ui.txt"
light_ci_want "file list fails" "true true true true false false" full pull_request develop error "$tmp/ui.txt"
light_ci_want "stale head" "true true true true false false" full pull_request develop ok "$tmp/ui.txt" deadbeef
MAIN_ENV=(PUDDLE_LIGHT_CI=on PUDDLE_QUEUE=on)
WANT_LIGHT=true light_ci_want "with the light pull request mode too" "$HEAVY_OFF true false" rust pull_request develop ok "$tmp/rust-only.txt"
MAIN_ENV=(PUDDLE_LIGHT_CI=on MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1)
light_ci_want "merge group, rust-only diff" "$HEAVY_OFF true false" rust merge_group "" ok "$tmp/rust-only.txt"
light_ci_want "merge group, always-run path" "$HEAVY_OFF true true" full merge_group "" ok "$tmp/rename.txt"
light_ci_want "merge group, compare fails" "true true true true false false" full merge_group "" error "$tmp/ui.txt"
MAIN_ENV=(PUDDLE_LIGHT_CI=on PUDDLE_TIER=on MERGE_GROUP_BASE_SHA=base1 MERGE_GROUP_HEAD_SHA=head1)
light_ci_want "merge group in the nightly tier" "$HEAVY_OFF false true" ui merge_group "" ok "$tmp/ui.txt"
for v in ON On true 1 yes "on " off ""; do
    MAIN_ENV=("PUDDLE_LIGHT_CI=$v")
    light_ci_want "PUDDLE_LIGHT_CI='$v' is not on" "true true false false false false" rust pull_request develop ok "$tmp/rust-only.txt"
done
MAIN_ENV=()
light_ci_want "PUDDLE_LIGHT_CI unset" "true true false false false false" rust pull_request develop ok "$tmp/rust-only.txt"

# The nightly tier through the whole script: only a merge queue run with PUDDLE_TIER=on, and the
# outputs of the group's diff; any doubt prints the full set with nothing deferred.
printf 'crates/puddle-app/src/lib.rs\n' >"$tmp/app.txt"
mkdir -p "$tmp/contents/crates/puddle-app/src"
printf 'fn app() {}\n' >"$tmp/contents/crates/puddle-app/src/lib.rs"
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

# The result job's verdict, the matrix and the push cases.
# shellcheck source=ci/scope-test/verdict.sh
. "$here/scope-test/verdict.sh"
# The full-steps guard and the workflow wiring checks.
# shellcheck source=ci/scope-test/wiring.sh
. "$here/scope-test/wiring.sh"

if [ "$fail" = 0 ]; then
    echo "ok"
else
    exit 1
fi
