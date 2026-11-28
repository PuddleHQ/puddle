#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of scripts/hooks-gate.sh: it runs every command of a list, reports each failure with its output, and exits 1 on any failure.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/hgt.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
# shellcheck disable=SC1091 # the helper sits in scripts/
. "$(dirname "$0")/../scripts/test-fail.sh"

printf '# a comment\n\necho one\nsleep 1 && echo two\n' >"$tmp/ok.list"
out=$(HOOKS_GATE_JOBS=2 "$here/scripts/hooks-gate.sh" "$tmp/ok.list" 2>&1) || fail "a list of passing commands failed: $out"
case $out in *"2 commands ok (2 at a time)"*) ;; *) fail "no ok line: $out" ;; esac

printf 'echo fine\necho the-reason >&2; exit 3\nfalse\necho also-fine\n' >"$tmp/bad.list"
rc=0
out=$(HOOKS_GATE_JOBS=3 "$here/scripts/hooks-gate.sh" "$tmp/bad.list" 2>&1) || rc=$?
[ "$rc" = 1 ] || fail "a failing list exited $rc, not 1: $out"
case $out in *"FAILED: echo the-reason >&2; exit 3 (status 3)"*) ;; *) fail "the failed command is not named: $out" ;; esac
case $out in *the-reason*"FAILED: false (status 1)"* | *"FAILED: false (status 1)"*the-reason*) ;; *) fail "the output of the failed command or the second failure is missing: $out" ;; esac
case $out in *"2 of 4 commands failed"*) ;; *) fail "no count of failures: $out" ;; esac

# the commands run in parallel: with four jobs, each of four commands announces itself and then waits until all four have, so the
# list only passes when they ran side by side. The wait bounds only the failure case (a serial runner); load cannot fail a pass.
rendezvous_list() { # <marker dir> <polls> <list file>
    mkdir "$1"
    : >"$3"
    for i in 1 2 3 4; do
        # shellcheck disable=SC2016 # the command text keeps its own $ for the sh that runs it
        printf 'touch "%s/%s"; n=0; while [ "$(ls "%s" | wc -l)" -lt 4 ]; do n=$((n + 1)); [ "$n" -le %s ] || exit 9; sleep 0.1; done\n' "$1" "$i" "$1" "$2" >>"$3"
    done
}
rendezvous_list "$tmp/rv-par" 600 "$tmp/par.list"
HOOKS_GATE_JOBS=4 "$here/scripts/hooks-gate.sh" "$tmp/par.list" >/dev/null 2>&1 || fail "four commands that wait for each other did not pass with four jobs: they did not run side by side"

# with one job the same list cannot pass (the first command gives up waiting), so the check above does prove parallelism
rendezvous_list "$tmp/rv-ser" 3 "$tmp/ser.list"
rc=0
HOOKS_GATE_JOBS=1 "$here/scripts/hooks-gate.sh" "$tmp/ser.list" >/dev/null 2>&1 || rc=$?
[ "$rc" = 1 ] || fail "the rendezvous list exited $rc with one job, not 1: the parallel check proves nothing"

# a command runs from the repository root
printf 'test -f scripts/hooks-gate.sh\n' >"$tmp/cwd.list"
"$here/scripts/hooks-gate.sh" "$tmp/cwd.list" >/dev/null 2>&1 || fail "a command did not run from the repository root"

: >"$tmp/empty.list"
rc=0
"$here/scripts/hooks-gate.sh" "$tmp/empty.list" >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "an empty list exited $rc, not 2"
rc=0
HOOKS_GATE_JOBS=0 "$here/scripts/hooks-gate.sh" "$tmp/ok.list" >/dev/null 2>&1 || rc=$?
[ "$rc" = 2 ] || fail "HOOKS_GATE_JOBS=0 exited $rc, not 2"
echo "hooks-gate-test: ok"
