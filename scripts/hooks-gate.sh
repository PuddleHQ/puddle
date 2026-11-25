#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The `hooks` gate of scripts/check.sh: every command of scripts/hooks-gate.list, HOOKS_GATE_JOBS at a time (default: the CPU count,
# 2 to 8). The commands were one `&&` chain (about 133 s of script self-tests, and 3 times that, because
# scripts/check-hooks-tmpdir-test.sh reruns the gate under two long TMPDIRs); they share no state (each builds its scratch below
# TMPDIR), so they run beside each other. A failing command prints its own output after the others finished, and the gate then
# names every failed command. Exit 0 only when every command passed.
#
# usage: scripts/hooks-gate.sh [list-file]      (self-test of the runner: scripts/hooks-gate-test.sh)
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)

if [ "${1:-}" = --one ]; then
    # --one <outdir> <index> <command>: one command, its output to <outdir>/<index>.log, its status to <outdir>/<index>.rc
    out=$2
    idx=$3
    cmd=$4
    rc=0
    (cd "$here" && sh -c "$cmd") >"$out/$idx.log" 2>&1 || rc=$?
    echo "$rc" >"$out/$idx.rc"
    printf '%s\n' "$cmd" >"$out/$idx.cmd"
    exit 0
fi

list=${1:-$here/scripts/hooks-gate.list}
jobs=${HOOKS_GATE_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}
case $jobs in '' | *[!0-9]* | 0) echo "hooks-gate: HOOKS_GATE_JOBS must be a whole number of at least 1 (got '$jobs')" >&2; exit 2 ;; esac
[ -n "${HOOKS_GATE_JOBS:-}" ] || { [ "$jobs" -ge 2 ] || jobs=2; [ "$jobs" -le 8 ] || jobs=8; }
[ -f "$list" ] || { echo "hooks-gate: no list $list" >&2; exit 2; }

out=$(mktemp -d "${CHECK_TMPDIR:-${TMPDIR:-/tmp}}/hg.XXXXXX")
trap 'rm -rf "$out"' EXIT

# one NUL-separated record per command: "<index> <command>" would need splitting, so the index goes in as its own argument pair
n=0
: >"$out/queue"
while IFS= read -r line || [ -n "$line" ]; do
    case $line in '' | '#'*) continue ;; esac
    n=$((n + 1))
    printf '%s\0%s\0' "$n" "$line" >>"$out/queue"
done <"$list"
[ "$n" -gt 0 ] || { echo "hooks-gate: the list $list has no command" >&2; exit 2; }

xargs -0 -n 2 -P "$jobs" sh "$0" --one "$out" <"$out/queue" || {
    echo "hooks-gate: the runner itself failed" >&2
    exit 1
}

failed=0
for rcfile in "$out"/*.rc; do
    idx=$(basename "$rcfile" .rc)
    [ "$(cat "$rcfile")" = 0 ] && continue
    failed=$((failed + 1))
    echo "hooks-gate: FAILED: $(cat "$out/$idx.cmd") (status $(cat "$rcfile"))" >&2
    tail -n 40 "$out/$idx.log" >&2
done
ran=$(find "$out" -name '*.rc' | wc -l | tr -d ' ')
[ "$ran" = "$n" ] || {
    echo "hooks-gate: only $ran of $n commands reported a status" >&2
    exit 1
}
if [ "$failed" -gt 0 ]; then
    echo "hooks-gate: $failed of $n commands failed" >&2
    exit 1
fi
echo "hooks-gate: $n commands ok ($jobs at a time)"
