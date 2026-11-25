#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of scripts/hooks-gate.sh: it runs every command of a list, reports each failure with its output, and exits 1 on any failure.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/hgt.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
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

# the commands run in parallel: four one-second sleeps take well under four seconds with four jobs
printf 'sleep 1\nsleep 1\nsleep 1\nsleep 1\n' >"$tmp/par.list"
start=$(date +%s)
HOOKS_GATE_JOBS=4 "$here/scripts/hooks-gate.sh" "$tmp/par.list" >/dev/null 2>&1 || fail "the parallel list failed"
[ $(($(date +%s) - start)) -lt 4 ] || fail "four one-second commands with four jobs took 4 s or more: they did not run side by side"

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
