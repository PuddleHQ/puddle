#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Path scoping for pull request and merge queue checks. A `scope` job in each workflow runs
# `ci/scope.sh` and the heavy jobs read its outputs, so a change only pays for the jobs its files
# can affect.
#
#   ci/scope.sh                   work out the outputs for this run and append them to $GITHUB_OUTPUT
#   ci/scope.sh classify          the same from a file list on stdin (one path per line), for tests
#   ci/scope.sh verdict <spec>... the result job's check (below)
#   ci/scope.sh full-steps <step>=<outcome>...
#                                 the last step of a job whose steps the light mode skips (below)
#
# Outputs (each `true` or `false`; a job is skipped only on an explicit `false`, except the `defer_` outputs, which skip it only on an explicit `true`):
#   ci_rust   the full Rust and repository gates of ci.yml (false: only the light lint job)
#   win_rust  windows clippy and tests
#   ui        the UI gates on both systems
#   vm        the VM build and test jobs of vm-linux and vm-windows
#   defer_vm_windows, defer_win_ui, defer_win_clippy
#             the nightly tier (below): true only on a merge queue run with PUDDLE_TIER=on whose diff
#             did not escalate that job; the job then skips and the result job accepts the skip
#   defer_linux_webkit
#             the same for the WebKit half of the Linux UI end-to-end tests: the `ui` job runs on
#             Chromium alone and `full-steps` accepts that; nothing skips, so the verdict is untouched
#   light     the light pull request mode (below): the Windows and VM jobs skip, and so do the
#             Linux steps marked for it; true only in that mode, so a lost output runs everything
#   class     full | docs | ui | rust | mixed (a label for the log and the tests)
#
# Nightly tier: with the repository variable PUDDLE_TIER set to `on`, a merge queue run skips the
# jobs that run nightly and on a pull request to main instead (the WHP VM jobs of vm-windows, the
# Windows UI job and the native Windows clippy job, and the WebKit half of the Linux UI end-to-end
# tests, which webkit.yml runs nightly), unless the group's diff touches a path that
# can only break that job, which pulls it back into the queue (`escalates`, below). Unset, any
# other value, any other event and any doubt run everything, as before. The required check names
# do not change: the result job's summary says the job ran in the nightly or release tier.
#
# Fails open: everything runs unless the changed files are known and every one of them is
# understood. An event other than a pull request to develop or a merge queue run, a failed or
# stale file list, 300 or more files, an empty list, a path no rule names, and any error in this
# script all give `true` for every scoped output (and `light=false`). The job reading the outputs
# also treats a missing output as `true`.
#
# A merge queue run (`merge_group`) is scoped by the diff between the queue's base and the commit
# it tests (the GitHub compare API), which covers every pull request in the group.
#
# Light mode: with the repository variable PUDDLE_QUEUE set to `on`, a pull request to develop
# runs only the Linux jobs (without the slow `git-env` and `notices` gates, and with the UI
# end-to-end tests on Chromium only); the merge queue then runs the full set on the commit that
# lands. Any other value, an unset variable, any other event and a pull request to another branch
# run the full set. The verdict and `full-steps` work the mode out again from the event, never
# from the scope job's output, so a wrong `light=true` fails the result.
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
# verdict: `ci/scope.sh verdict [--nightly-skips] <job>=<output>[,queue][,pushskip][,defer:<output>] [<job>=!<output>]...`
# reads NEEDS (the result job's `toJSON(needs)`, which must include `scope`). A job may be skipped
# only when its output is `false` (`!output`: only when it is not `false`, for the light twin of a
# full job, which is skipped whenever the Rust gates run, so also on every push, nightly and
# dispatch, where scope prints every output true); a failed, cancelled or otherwise skipped job
# fails the verdict, so an infrastructure failure can never pass as a scoped skip. Four more
# skips are accepted, each only where its own condition holds:
#   ,queue           the job skips in light mode (a pull request to develop with PUDDLE_QUEUE=on)
#   ,pushskip        a develop push run whose commit a green merge queue run already tested
#                    (PUDDLE_QUEUE_SKIP_PUSH=on and the `queue-tested` job said so; ci/should-run.sh)
#   ,defer:<output>  a merge queue run with PUDDLE_TIER=on where scope's `<output>` is `true`: the job
#                    runs in the nightly and release tier (the summary says so, so nobody reads the
#                    green check as a test result)
#   --nightly-skips  a scheduled run (the VM workflows, whose test jobs skip when the nightly's
#                    commit already has a green run)
# Jobs not named are held to that rule too, and only the Windows release build, which has its own
# event rule, and the `queue-tested` lookup may be skipped.
set -uo pipefail

# Crates that no VM test depends on (cargo metadata, all dependency kinds; checked by
# `SCOPE_TEST_METADATA=1 ci/scope-test.sh`).
NO_VM=" puddle puddle-app puddle-test-kit xtask "
# Crates outside the closure of puddle-e2e and puddle-app, which the UI jobs build.
NO_UI=" puddle puddle-test-kit xtask "
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
    printf 'ci_rust=true\nwin_rust=true\nui=true\nvm=true\ndefer_vm_windows=false\ndefer_win_ui=false\ndefer_win_clippy=false\ndefer_linux_webkit=false\nlight=false\nclass=full\n'
}

# Nightly tier: the repository variable PUDDLE_TIER is exactly `on` and this is a merge queue run.
# Reads only the event's own variables, so the verdict can check the scope job's outputs.
tier_mode() {
    [ "${PUDDLE_TIER:-}" = on ] && [ "${GITHUB_EVENT_NAME:-}" = merge_group ]
}

# Paths that can break a job moved to the nightly tier and are not already `always` paths (the msb
# SDK pin lives in Cargo.toml, Cargo.lock and ci/msb-*, the shared VM test crate and the workflows
# are `always`). Prints the jobs it pulls back into the queue: vm, ui, clippy, webkit.
escalates() {
    local lower=${1,,}
    case $1 in
    crates/puddle-compute-msb/*) echo vm ;;
    crates/puddle-app/*) echo ui ;;
    esac
    case $lower in *windows*) echo vm; echo clippy ;; esac
    # What a second browser engine can see differently: markup and styles (a component, a stylesheet,
    # the document shell, static files) and the browser test setup. Plain TypeScript logic is not
    # listed: the engines share it, and the nightly WebKit run covers the rest.
    case $1 in
    ui/*.svelte | ui/*.css | ui/src/app.html | ui/static/* | ui/package.json | ui/playwright.config.ts | \
        ui/vite.config.ts | ui/e2e/warm-up.ts | ui/e2e/fixture.ts | ui/e2e/fixture-build.ts | ui/e2e/support.ts) echo webkit ;;
    esac
}

# Light mode: the repository variable PUDDLE_QUEUE is exactly `on` and this is a pull request to
# develop. Reads only the event's own variables, so the verdict can check the scope job's output.
light_mode() {
    [ "${PUDDLE_QUEUE:-}" = on ] && [ "${GITHUB_EVENT_NAME:-}" = pull_request ] && [ "${PR_BASE_REF:-}" = develop ]
}

# stdin: paths. stdout: the outputs.
classify() {
    local always=0 gate=0 fixture=0 ui=0 crates=0 vm=0 e2e=0 n=0 p c name esc=" " j
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        n=$((n + 1))
        c=$(path_class "$p")
        for j in $(escalates "$p"); do esc="$esc$j "; done
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
    local ci_rust=false win_rust=false uib=false vmb=false class d_vm=false d_ui=false d_clip=false d_wk=false
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
    if tier_mode; then
        case "$esc" in *" vm "*) ;; *) d_vm=true ;; esac
        case "$esc" in *" ui "*) ;; *) d_ui=true ;; esac
        case "$esc" in *" clippy "*) ;; *) d_clip=true ;; esac
        case "$esc" in *" webkit "*) ;; *) d_wk=true ;; esac
    fi
    printf 'ci_rust=%s\nwin_rust=%s\nui=%s\nvm=%s\ndefer_vm_windows=%s\ndefer_win_ui=%s\ndefer_win_clippy=%s\ndefer_linux_webkit=%s\nlight=false\nclass=%s\n' \
        "$ci_rust" "$win_rust" "$uib" "$vmb" "$d_vm" "$d_ui" "$d_clip" "$d_wk" "$class"
}

# The changed paths of this pull request (renames give both names), from the GitHub API. Prints
# nothing and returns 1 when the list cannot be trusted.
pr_files() {
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

# The changed paths of a merge queue run: the queue's base against the commit under test. The
# compare API lists at most 300 files, which the 300-file rule turns into a full run. Returns 1
# unless the tested commit is strictly ahead of the base.
group_files() {
    local base=${MERGE_GROUP_BASE_SHA:-} head=${MERGE_GROUP_HEAD_SHA:-} repo=${GITHUB_REPOSITORY:-} json
    [ -n "$base" ] && [ -n "$head" ] && [ -n "$repo" ] || return 1
    json=$(gh api "repos/$repo/compare/$base...$head") || return 1
    [ "$(jq -r .status <<<"$json")" = ahead ] || return 1
    jq -r '.files[] | .filename, (.previous_filename // empty)' <<<"$json"
}

scope() {
    local reason="" files out
    case ${GITHUB_EVENT_NAME:-} in
    pull_request)
        if [ "${PR_BASE_REF:-}" != develop ]; then
            reason="pull request to ${PR_BASE_REF:-an unknown branch}, not develop: everything runs"
        elif ! files=$(pr_files); then
            reason="the changed files could not be read or do not match the head: everything runs"
        fi
        ;;
    merge_group)
        if ! files=$(group_files); then
            reason="the merge group's changed files could not be read: everything runs"
        fi
        ;;
    *) reason="event ${GITHUB_EVENT_NAME:-unknown} always runs everything" ;;
    esac
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
    if light_mode; then
        echo "scope: light mode (PUDDLE_QUEUE=on, pull request to develop): the merge queue runs the full set" >&2
        out=${out/light=false/light=true}
    fi
    printf '%s\n' "$out"
}

verdict() {
    local nightly=0
    [ "${1:-}" = --nightly-skips ] && { nightly=1; shift; }
    local needs=${NEEDS:-} bad=0 spec job key neg res want out event=${GITHUB_EVENT_NAME:-} mapped=" "
    local flags why light=0 tested=0 tag k v skip="" tier=0 dkey deferred=""
    local -A result=() output=()
    [ -n "$needs" ] || { echo "verdict: NEEDS is empty" >&2; return 1; }
    # One pass over NEEDS: every job's result, the scope job's outputs and the queue-tested lookup's answer.
    while IFS=$'\t' read -r tag k v; do
        case $tag in
        r) result[$k]=$v ;;
        o) output[$k]=$v ;;
        t) skip=$v ;;
        esac
    done < <(jq -r '(to_entries[] | ["r", .key, (.value.result // "missing" | tostring)]),
        (.scope.outputs // {} | to_entries[] | ["o", .key, (.value | tostring)]),
        ["t", "skip", (.["queue-tested"].outputs.skip // "" | tostring)] | @tsv' <<<"$needs" 2>/dev/null) || true
    [ "${#result[@]}" -gt 0 ] || { echo "verdict: NEEDS is not a jobs object" >&2; return 1; }
    light_mode && light=1
    tier_mode && tier=1
    # A develop push run whose commit a green merge queue run tested (the queue-tested job's
    # output, from ci/should-run.sh --queue-tested and the cache lookups).
    if [ "${PUDDLE_QUEUE_SKIP_PUSH:-}" = on ] && [ "$event" = push ] && [ "${GITHUB_REF:-}" = refs/heads/develop ] &&
        [ "${result[queue-tested]:-}" = success ] && [ "$skip" = true ]; then
        tested=1
    fi
    for spec in "$@"; do
        job=${spec%%=*} key=${spec#*=} neg=0 flags="," why=""
        dkey=""
        case $key in *,*) flags=",${key#*,}," key=${key%%,*} ;; esac
        case $flags in *,defer:*) dkey=${flags#*,defer:}; dkey=${dkey%%,*} ;; esac
        case $key in '!'*) neg=1 key=${key#!} ;; esac
        mapped="$mapped$job "
        res=${result[$job]:-missing}
        out=${output[$key]:-}
        case $res in
        success) want=ok ;;
        skipped)
            # On a push, nightly or dispatch scope prints every output true: a scoped job runs, and
            # only the light twin (`!output`) is skipped, so one rule covers every event.
            if [ "$neg" = 0 ] && [ "$out" = false ]; then
                want=ok
            elif [ "$neg" = 1 ] && [ "$out" != false ]; then
                want=ok
            elif [ "$nightly" = 1 ] && [ "$event" = schedule ]; then
                want=ok
            elif [ "$light" = 1 ] && [[ $flags == *,queue,* ]]; then
                want=ok why=" (light mode: the merge queue runs it)"
            elif [ "$tested" = 1 ] && [[ $flags == *,pushskip,* ]]; then
                want=ok why=" (a green merge queue run tested this commit)"
            elif [ "$tier" = 1 ] && [ -n "$dkey" ] && [ "${output[$dkey]:-}" = true ]; then
                want=ok why=" (nightly tier: PUDDLE_TIER=on and the diff did not escalate it; it runs in the nightly and release tier)"
                deferred="$deferred $job"
            else
                want=BAD
            fi
            ;;
        *) want=BAD ;;
        esac
        echo "$job: $res (scope $key=${out:-unset}) $want$why"
        [ "$want" = ok ] || bad=1
    done
    if [ -n "$deferred" ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        {
            echo '### Not run here: nightly and release tier'
            echo "Skipped on this merge queue run:$deferred. They run nightly and on a pull request to main (PUDDLE_TIER=on), so this green check is not a test result for them."
        } >>"$GITHUB_STEP_SUMMARY"
    fi
    local j
    for j in "${!result[@]}"; do
        [ "$j" = scope ] && continue
        case "$mapped" in *" $j "*) continue ;; esac
        res=${result[$j]}
        # An unmapped job (the release build, the queue-tested lookup) skips by its own event rule,
        # never by scope.
        if [ "$res" = success ] || [ "$res" = skipped ]; then
            echo "$j: $res (not scoped) ok"
        else
            echo "$j: $res BAD"
            bad=1
        fi
    done
    return "$bad"
}

# full-steps <step>=<outcome>...: off light mode every named step must have succeeded, and the UI
# end-to-end tests must not be narrowed to some browsers (PUDDLE_E2E_PROJECTS empty). In light mode
# they may be skipped. The one other narrowing is the nightly tier's: a merge queue run with
# PUDDLE_TIER=on and DEFER_LINUX_WEBKIT=true (scope's output) may run on Chromium alone, and says so
# in the job summary. Catches a step that the light mode's condition skipped where it must not.
full_steps() {
    local spec bad=0 narrow=0
    tier_mode && [ "${DEFER_LINUX_WEBKIT:-}" = true ] && narrow=1
    if light_mode; then
        echo "full-steps: light mode, the merge queue runs these steps: $*"
        return 0
    fi
    for spec in "$@"; do
        if [ "${spec#*=}" = success ]; then
            echo "${spec%%=*}: success ok"
        else
            echo "${spec%%=*}: ${spec#*=} BAD (only the light pull request mode may skip it)"
            bad=1
        fi
    done
    if [ -z "${PUDDLE_E2E_PROJECTS:-}" ]; then
        :
    elif [ "$narrow" = 1 ] && [ "$PUDDLE_E2E_PROJECTS" = chromium ]; then
        echo "PUDDLE_E2E_PROJECTS=chromium: nightly tier (PUDDLE_TIER=on, no path that WebKit can see differently changed); webkit.yml runs WebKit nightly"
        [ -z "${GITHUB_STEP_SUMMARY:-}" ] || {
            echo '### WebKit not run here: nightly and release tier'
            echo 'The end-to-end tests ran on Chromium only on this merge queue run (PUDDLE_TIER=on, and no markup, style or test-setup path changed). WebKit runs nightly (webkit.yml) and on a pull request to main.'
        } >>"$GITHUB_STEP_SUMMARY"
    else
        echo "PUDDLE_E2E_PROJECTS=$PUDDLE_E2E_PROJECTS BAD (only the light pull request mode and the nightly tier's Chromium run may narrow the browsers)"
        bad=1
    fi
    return "$bad"
}

main() {
    local out
    case ${1:-} in
    classify) classify ;;
    verdict) shift; verdict "$@" ;;
    full-steps) shift; full_steps "$@" ;;
    "")
        out=$(scope) || out=$(full_outputs)
        # Anything but the ten keys (or a lost one) means something broke: run everything.
        out=$(grep -E '^(ci_rust|win_rust|ui|vm|defer_vm_windows|defer_win_ui|defer_win_clippy|defer_linux_webkit|light|class)=' <<<"$out")
        [ "$(grep -c . <<<"$out")" = 10 ] || out=$(full_outputs)
        echo "$out"
        [ -z "${GITHUB_OUTPUT:-}" ] || echo "$out" >>"$GITHUB_OUTPUT"
        [ -z "${GITHUB_STEP_SUMMARY:-}" ] || {
            echo '### Path scope'
            echo '```'
            echo "$out"
            echo '```'
        } >>"$GITHUB_STEP_SUMMARY"
        ;;
    *) echo "usage: scope.sh [classify | verdict <job>=<output>... | full-steps <step>=<outcome>...]" >&2; return 2 ;;
    esac
}

main "$@"
