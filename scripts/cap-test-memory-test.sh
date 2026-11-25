#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests scripts/cap-test-memory.sh: the command's status and arguments pass through, and a command
# that allocates without end fails at the limit (here 64 MiB) instead of growing.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cap="$here/cap-test-memory.sh"
. "$(dirname "$0")/../scripts/test-fail.sh"

out=$("$cap" echo "a b" c) || fail "a plain command must run"
[ "$out" = "a b c" ] || fail "arguments must pass through unchanged: $out"
rc=0
"$cap" sh -c 'exit 7' || rc=$?
[ "$rc" = 7 ] || fail "the command's status must pass through, got $rc"

# `s = s s` doubles a string until the allocation fails; without the limit it would take every byte.
rc=0
PUDDLE_TEST_MEMORY_KB=65536 "$cap" awk 'BEGIN { s = "x"; for (;;) s = s s }' >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "a runaway allocation must fail at the limit"
# The limit is soft: a child may lift it for itself (the real tools the login tests start need that).
out=$(PUDDLE_TEST_MEMORY_KB=65536 "$cap" sh -c 'ulimit -S -d unlimited && ulimit -S -d') || fail "a child must be able to lift the soft limit"
[ "$out" = unlimited ] || fail "the lifted limit must read unlimited: $out"
# The default leaves room for the threads' stacks: 300 parked blocking threads (hostile_audit_backlog)
# reach about 450 MiB and aborted CI shards at 512 MiB.
out=$(env -u PUDDLE_TEST_MEMORY_KB "$cap" sh -c 'ulimit -S -d') || fail "the default limit must be readable"
[ "$out" -ge 1048576 ] || fail "the default limit must be at least 1 GiB for thread-heavy tests: $out"
# The tests' scratch directory: PUDDLE_TEST_TMPDIR replaces TMPDIR for the command, and a missing one is ignored.
out=$(TMPDIR=/elsewhere PUDDLE_TEST_TMPDIR="$here" "$cap" printenv TMPDIR) || fail "a command with PUDDLE_TEST_TMPDIR must run"
[ "$out" = "$here" ] || fail "PUDDLE_TEST_TMPDIR must become TMPDIR: $out"
out=$(TMPDIR=/elsewhere PUDDLE_TEST_TMPDIR="$here/gone" "$cap" printenv TMPDIR) || fail "a missing PUDDLE_TEST_TMPDIR must not fail the command"
[ "$out" = /elsewhere ] || fail "a missing PUDDLE_TEST_TMPDIR must leave TMPDIR alone: $out"
out=$(TMPDIR=/elsewhere "$cap" printenv TMPDIR) || fail "a command with no PUDDLE_TEST_TMPDIR must run"
[ "$out" = /elsewhere ] || fail "no PUDDLE_TEST_TMPDIR must leave TMPDIR alone: $out"
echo "cap-test-memory-test: ok"
