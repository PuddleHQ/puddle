#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests scripts/cap-test-memory.sh: the command's status and arguments pass through, and a command
# that allocates without end fails at the limit (here 64 MiB) instead of growing.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cap="$here/cap-test-memory.sh"
fail() { echo "cap-test-memory-test: FAIL: $1" >&2; exit 1; }

out=$("$cap" echo "a b" c) || fail "a plain command must run"
[ "$out" = "a b c" ] || fail "arguments must pass through unchanged: $out"
rc=0
"$cap" sh -c 'exit 7' || rc=$?
[ "$rc" = 7 ] || fail "the command's status must pass through, got $rc"

# `s = s s` doubles a string until the allocation fails; without the limit it would take every byte.
rc=0
PUDDLE_TEST_MEMORY_KB=65536 "$cap" awk 'BEGIN { s = "x"; for (;;) s = s s }' >/dev/null 2>&1 || rc=$?
[ "$rc" != 0 ] || fail "a runaway allocation must fail at the limit"
echo "cap-test-memory-test: ok"
