#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The result job's verdict, the check matrix and the push cases of ci/scope-test.sh.
# Sourced by ci/scope-test.sh at script scope: it shares the main script's variables, functions and `fail`.
# shellcheck shell=bash disable=SC2034,SC2154 # `fail`, `bad` and the variables are shared with ci/scope-test.sh, which sources this file
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
ci_specs=(rust=ci_rust gates=ci_rust hooks=ci_rust 'light=!ci_rust' ui=ui 'ui-webkit=ui,queue,defer:defer_linux_webkit')
verdict pull_request 0 "ci full run" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=success
verdict pull_request 0 "ci ui-only: light and ui" "$CI_UI" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=success ui=success ui-webkit=success
verdict pull_request 0 "ci docs-only: light only" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=success ui=skipped ui-webkit=skipped
verdict pull_request 1 "ci docs-only: light failed" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=failure ui=skipped ui-webkit=skipped
verdict pull_request 1 "ci docs-only: light skipped too" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=skipped ui=skipped ui-webkit=skipped
verdict pull_request 1 "ci full: rust skipped, light ran" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=success ui=success ui-webkit=success
verdict pull_request 1 "ci full: rust failed" "$CI_FULL" "${ci_specs[@]}" -- rust=failure gates=failure hooks=failure light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci ui job skipped though needed" "$CI_UI" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=success ui=skipped ui-webkit=skipped
verdict pull_request 0 "ci scope failed: everything ran" '{}' "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci scope failed: rust skipped" '{}' "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=success ui=success ui-webkit=success
verdict pull_request 1 "ci full: hooks failed, the rest passed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=failure light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci full: hooks skipped though needed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=skipped light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci full: gates failed, tests passed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=failure hooks=failure light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci full: gates skipped though needed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=skipped hooks=skipped light=skipped ui=success ui-webkit=success
verdict pull_request 1 "ci full: webkit failed" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=failure
verdict pull_request 1 "ci full: webkit skipped without the light mode" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=skipped
# The nightly tier skips the WebKit job on a merge queue run when scope says so, and nowhere else.
CI_WK='{"ci_rust":"true","ui":"true","defer_linux_webkit":"true"}'
CI_NOWK='{"ci_rust":"true","ui":"true","defer_linux_webkit":"false"}'
VERDICT_ENV=(PUDDLE_TIER=on)
verdict merge_group 0 "tier: webkit deferred" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=skipped
verdict merge_group 1 "tier: webkit kept in the queue but skipped" "$CI_NOWK" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=skipped
verdict merge_group 1 "tier: chromium skipped, only webkit may defer" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=skipped ui-webkit=skipped
verdict pull_request 1 "tier: a pull request never defers webkit" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=skipped
VERDICT_ENV=()
verdict merge_group 1 "tier unset: webkit deferred" "$CI_WK" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=skipped
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
matrix_bg ci "" ci_rust,msvc,ui rust=ci_rust,pushskip gates=ci_rust,pushskip hooks=ci_rust,pushskip msvc=msvc,pushskip 'light=!ci_rust' ui=ui,pushskip ui-webkit=ui,queue,pushskip,defer:defer_linux_webkit
# ci.yml's light CI unit tier, apart: its two outputs vary independently of the others (one matrix would be 81 states a context).
matrix_bg ci-unit "" unit,unit_ui unit=unit unit-ui=unit_ui
matrix_bg windows "" win_rust,ui clippy=win_rust,queue,defer:defer_win_clippy test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui,queue,defer:defer_win_ui ui-unit=ui,queue,defer:defer_win_ui
matrix_bg vm-linux --nightly-skips vm build=vm,queue agent=vm,queue tests=vm,queue
matrix_bg vm-windows --nightly-skips vm build=vm,queue,defer:defer_vm_windows agent=vm,queue,defer:defer_vm_windows tests=vm,queue,defer:defer_vm_windows
for p in "${mpids[@]}"; do wait "$p" || fail=1; done
verdict push 0 "ci push: light skipped because the Rust gates ran" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=success
verdict schedule 0 "ci nightly: light skipped" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=success
verdict workflow_dispatch 0 "ci dispatch: light skipped" "$CI_FULL" "${ci_specs[@]}" -- rust=success gates=success hooks=success light=skipped ui=success ui-webkit=success
verdict push 1 "ci push: light ran, rust skipped" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=success ui=success ui-webkit=success

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
verdict pull_request 1 "light: a skip without the queue mark" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped gates=skipped hooks=skipped light=skipped ui=success ui-webkit=success
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
CI_P=('rust=ci_rust,pushskip' 'gates=ci_rust,pushskip' 'hooks=ci_rust,pushskip' 'msvc=msvc,pushskip' 'light=!ci_rust' 'ui=ui,pushskip' 'ui-webkit=ui,queue,pushskip,defer:defer_linux_webkit')
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
push_case 0 "all heavy jobs skipped" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
push_case 0 "heavy jobs ran anyway" rust=success gates=success hooks=success msvc=success light=skipped ui=success ui-webkit=success "$TESTED"
push_case 1 "rust failed" rust=failure gates=failure hooks=failure msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
push_case 1 "lookup failed" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped queue-tested=failure
push_case 1 "lookup skipped" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped queue-tested=skipped
SKIP_OUT=false push_case 1 "lookup said run" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
SKIP_OUT="" push_case 1 "lookup output lost" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
PUSH_REF=refs/heads/main push_case 1 "a push to main" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
PUSH_EVENT=workflow_dispatch push_case 1 "a dispatch" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
PUSH_EVENT=merge_group push_case 1 "a merge queue run" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
VERDICT_ENV=()
push_case 1 "PUDDLE_QUEUE_SKIP_PUSH unset" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
VERDICT_ENV=(PUDDLE_QUEUE_SKIP_PUSH=yes)
push_case 1 "PUDDLE_QUEUE_SKIP_PUSH=yes is not on" rust=skipped gates=skipped hooks=skipped msvc=skipped light=skipped ui=skipped ui-webkit=skipped "$TESTED"
VERDICT_ENV=()
