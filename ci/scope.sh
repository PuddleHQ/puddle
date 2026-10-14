#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Path scoping for pull request checks. A `scope` job in each workflow runs `ci/scope.sh` and
# the heavy jobs read its outputs, so a pull request only pays for the jobs its files can affect.
#
#   ci/scope.sh                 work out the outputs for this run and append them to $GITHUB_OUTPUT
#   ci/scope.sh classify        the same from a file list on stdin (one path per line), for tests
#   ci/scope.sh verdict <spec>... the result job's check (below)
#
# Outputs (each `true` or `false`; a job is skipped only on an explicit `false`):
#   ci_rust   the full Rust and repository gates of ci.yml (false: only the light lint job)
#   win_rust  windows clippy and tests
#   ui        the UI gates on both systems
#   vm        the VM build and test jobs of vm-linux and vm-windows
#   class     full | docs | ui | rust | mixed (a label for the log and the tests)
#
# Fails open: everything runs unless the changed files are known and every one of them is
# understood. Any other event than a pull request to develop, a failed or stale file list, 300 or
# more files, an empty list, a path no rule names, and any error in this script all give
# `true` for every output. The job reading the outputs also treats a missing output as `true`.
#
# Classes of path (first rule wins; the list is below, in `path_class`):
#   always   everything runs: manifests and lockfile, toolchain, lint and tool config, workflows and
#            actions, ci/, scripts/ (except the three data files), the two UI scripts the Rust
#            diff-coverage and ratchet gates run, the shared VM test crate, the API contract and the
#            TypeScript types made from it, the notices inputs, a repo-url data file the UI tests read
#   gate     the coverage baseline, diff-coverage exclusions and standalone allowlist: read by the
#            Linux Rust gates and the UI gate only
#   fixture  ui/e2e/fixtures/: UI test data that the Rust fixture server includes
#   ui       the rest of ui/
#   crate    crates/<name>/ (a changed Cargo.toml is `always`, so the crate graph below is current)
#   docs     docs/, root Markdown, LICENSE, .gitignore, .github/ outside workflows/
#
# verdict: `ci/scope.sh verdict [--nightly-skips] <job>=<output> [<job>=!<output>]...` reads NEEDS (the result job's
# `toJSON(needs)`, which must include `scope`). A job may be skipped only when its output is
# `false` (`!output`: only when it is not `false`, for the light twin of a full job, which is
# skipped whenever the Rust gates run, so also on every push, nightly and dispatch, where scope
# prints every output true); a failed, cancelled or otherwise skipped job fails the verdict, so an
# infrastructure failure can never pass as a scoped skip. Off a pull request --nightly-skips also
# accepts a skip (the VM workflows, whose test jobs skip when the nightly's commit already has a
# green run). Jobs not named are held to that rule too, and only the Windows
# release build, which has its own event rule, may be skipped.
set -uo pipefail

# Crates that no VM test depends on (cargo metadata, all dependency kinds; checked by
# `SCOPE_TEST_METADATA=1 ci/scope-test.sh`).
NO_VM=" puddle puddle-app puddle-e2e xtask "
# Crates outside the closure of puddle-e2e and puddle-app, which the UI jobs build.
NO_UI=" puddle puddle-host puddle-lifecycle puddle-ssh puddle-workspace xtask "
MAX_FILES=300

path_class() {
    local c
    case $1 in
    Cargo.lock | Cargo.toml | */Cargo.toml | rust-toolchain.toml | .cargo/* | .config/* | .githooks/* | \
        .gitattributes | _typos.toml | .gitleaks.toml | deny.toml | about.toml | about.hbs | clippy.toml | \
        rustfmt.toml | ci/* | .github/workflows/* | .github/actions/*)
        echo always ;;
    scripts/diff-coverage-exclusions.txt | scripts/coverage-baseline.json | scripts/standalone-allow.txt)
        echo gate ;;
    scripts/* | ui/scripts/diff-coverage.ts | ui/scripts/coverage-ratchet.ts | crates/puddle-vm-tests/* | crates/puddle-api/openapi/* | crates/puddle-store/tests/data/repo-urls.json | \
        ui/src/lib/api/schema.d.ts | ui/package-lock.json | ui/THIRD-PARTY-NOTICES.txt)
        echo always ;;
    ui/e2e/fixtures/*) echo fixture ;;
    ui/*) echo ui ;;
    crates/*/*) c=${1#crates/}; echo "crate:${c%%/*}" ;;
    docs/* | .github/* | LICENSE | .gitignore) echo docs ;;
    */*) echo unknown ;;
    *.md) echo docs ;;
    *) echo unknown ;;
    esac
}

full_outputs() {
    printf 'ci_rust=true\nwin_rust=true\nui=true\nvm=true\nclass=full\n'
}

# stdin: paths. stdout: the outputs.
classify() {
    local always=0 gate=0 fixture=0 ui=0 crates=0 vm=0 e2e=0 n=0 p c name
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        n=$((n + 1))
        c=$(path_class "$p")
        case $c in
        always | unknown) always=1 ;;
        gate) gate=1 ;;
        fixture) fixture=1 ;;
        ui) ui=1 ;;
        docs) ;;
        crate:*)
            name=${c#crate:}
            crates=1
            case "$NO_VM" in *" $name "*) ;; *) vm=1 ;; esac
            case "$NO_UI" in *" $name "*) ;; *) e2e=1 ;; esac
            ;;
        esac
    done
    if [ "$n" -eq 0 ] || [ "$n" -ge "$MAX_FILES" ] || [ "$always" = 1 ]; then
        full_outputs
        return
    fi
    local ci_rust=false win_rust=false uib=false vmb=false class
    { [ "$crates" = 1 ] || [ "$fixture" = 1 ] || [ "$gate" = 1 ]; } && ci_rust=true
    { [ "$crates" = 1 ] || [ "$fixture" = 1 ]; } && win_rust=true
    { [ "$ui" = 1 ] || [ "$fixture" = 1 ] || [ "$gate" = 1 ] || [ "$e2e" = 1 ]; } && uib=true
    [ "$vm" = 1 ] && vmb=true
    if [ "$crates" = 0 ] && [ "$fixture" = 0 ] && [ "$gate" = 0 ] && [ "$ui" = 0 ]; then
        class=docs
    elif [ "$crates" = 0 ] && [ "$fixture" = 0 ] && [ "$gate" = 0 ]; then
        class=ui
    elif [ "$ui" = 0 ] && [ "$fixture" = 0 ]; then
        class=rust
    else
        class=mixed
    fi
    printf 'ci_rust=%s\nwin_rust=%s\nui=%s\nvm=%s\nclass=%s\n' "$ci_rust" "$win_rust" "$uib" "$vmb" "$class"
}

# The changed paths of this pull request (renames give both names), from the GitHub API. Prints
# nothing and returns 1 when the list cannot be trusted.
changed_files() {
    local pr=${PR_NUMBER:-} repo=${GITHUB_REPOSITORY:-} head json
    [ -n "$pr" ] && [ -n "$repo" ] || return 1
    # The list is the pull request's current diff: refuse one that is about another head.
    if [ -n "${PR_HEAD_SHA:-}" ]; then
        head=$(gh api "repos/$repo/pulls/$pr" --jq .head.sha) || return 1
        [ "$head" = "$PR_HEAD_SHA" ] || return 1
    fi
    json=$(gh api --paginate "repos/$repo/pulls/$pr/files?per_page=100" \
        --jq '.[] | .filename, (.previous_filename // empty)') || return 1
    printf '%s\n' "$json"
}

scope() {
    local reason="" files out
    if [ "${GITHUB_EVENT_NAME:-}" != pull_request ]; then
        reason="event ${GITHUB_EVENT_NAME:-unknown} always runs everything"
    elif [ "${PR_BASE_REF:-}" != develop ]; then
        reason="pull request to ${PR_BASE_REF:-an unknown branch}, not develop: everything runs"
    elif ! files=$(changed_files); then
        reason="the changed files could not be read or do not match the head: everything runs"
    fi
    if [ -n "$reason" ]; then
        echo "scope: $reason" >&2
        full_outputs
        return
    fi
    echo "scope: $(printf '%s\n' "$files" | grep -c .) changed paths" >&2
    if ! out=$(printf '%s\n' "$files" | classify); then
        echo "scope: classify failed, everything runs" >&2
        full_outputs
        return
    fi
    printf '%s\n' "$out"
}

verdict() {
    local nightly=0
    [ "${1:-}" = --nightly-skips ] && { nightly=1; shift; }
    local needs=${NEEDS:-} bad=0 spec job key neg res want out event=${GITHUB_EVENT_NAME:-} names mapped=" "
    [ -n "$needs" ] || { echo "verdict: NEEDS is empty" >&2; return 1; }
    names=$(jq -r 'keys[]' <<<"$needs") || return 1
    for spec in "$@"; do
        job=${spec%%=*} key=${spec#*=} neg=0
        case $key in '!'*) neg=1 key=${key#!} ;; esac
        mapped="$mapped$job "
        res=$(jq -r --arg j "$job" '.[$j].result // "missing"' <<<"$needs")
        out=$(jq -r --arg k "$key" '.scope.outputs[$k] // ""' <<<"$needs")
        case $res in
        success) want=ok ;;
        skipped)
            # Off a pull request scope prints every output true: a scoped job runs, and only the
            # light twin (`!output`) is skipped, so one rule covers every event.
            if [ "$neg" = 0 ] && [ "$out" = false ]; then
                want=ok
            elif [ "$neg" = 1 ] && [ "$out" != false ]; then
                want=ok
            elif [ "$nightly" = 1 ] && [ "$event" != pull_request ]; then
                want=ok
            else
                want=BAD
            fi
            ;;
        *) want=BAD ;;
        esac
        echo "$job: $res (scope $key=${out:-unset}) $want"
        [ "$want" = ok ] || bad=1
    done
    local j
    for j in $names; do
        [ "$j" = scope ] && continue
        case "$mapped" in *" $j "*) continue ;; esac
        res=$(jq -r --arg j "$j" '.[$j].result' <<<"$needs")
        # An unmapped job (the release build) skips by its own event rule, never by scope.
        if [ "$res" = success ] || [ "$res" = skipped ]; then
            echo "$j: $res (not scoped) ok"
        else
            echo "$j: $res BAD"
            bad=1
        fi
    done
    return "$bad"
}

main() {
    local out
    case ${1:-} in
    classify) classify ;;
    verdict) shift; verdict "$@" ;;
    "")
        out=$(scope) || out=$(full_outputs)
        # Anything but the five keys (or a lost one) means something broke: run everything.
        out=$(grep -E '^(ci_rust|win_rust|ui|vm|class)=' <<<"$out")
        [ "$(grep -c . <<<"$out")" = 5 ] || out=$(full_outputs)
        echo "$out"
        [ -z "${GITHUB_OUTPUT:-}" ] || echo "$out" >>"$GITHUB_OUTPUT"
        [ -z "${GITHUB_STEP_SUMMARY:-}" ] || {
            echo '### Path scope'
            echo '```'
            echo "$out"
            echo '```'
        } >>"$GITHUB_STEP_SUMMARY"
        ;;
    *) echo "usage: scope.sh [classify | verdict <job>=<output>...]" >&2; return 2 ;;
    esac
}

main "$@"
