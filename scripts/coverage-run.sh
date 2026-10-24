#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Runs the coverage command given as arguments (scripts/check.sh: `cargo llvm-cov nextest ...`) so that one
# unreadable profile file does not fail the merge. The instrumented runtime writes one `.profraw` file per
# process when it exits; a process stopped while it writes leaves a short file, and `llvm-profdata merge`
# fails the whole merge on one such file although every test passed. `--failure-mode=all` (through
# LLVM_PROFDATA_FLAGS, which cargo-llvm-cov hands to the merge) makes the merge fail only when no file is
# readable. The dropped file is named in a warning line (a GitHub annotation in CI), so a loss is never
# silent; the ratchet and the diff-coverage gate still fail when the counters really went missing.
# A caller that sets the flag itself (a lane that reports dropped files its own way) gets no second warning.
# Exit status: the command's.
set -u

[ "$#" -gt 0 ] || {
    echo "usage: coverage-run.sh <command> [args...]" >&2
    exit 2
}

report=1
case "${LLVM_PROFDATA_FLAGS:-}" in
*--failure-mode=*) report=0 ;;
*) LLVM_PROFDATA_FLAGS="${LLVM_PROFDATA_FLAGS:+$LLVM_PROFDATA_FLAGS }--failure-mode=all" ;;
esac
export LLVM_PROFDATA_FLAGS

tmp=$(mktemp -d "${TMPDIR:-/tmp}/coverage-run.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT

# The status goes through a file: a pipe into tee would hide it in a POSIX shell.
(
    "$@"
    echo $? >"$tmp/rc"
) 2>&1 | tee "$tmp/out"
rc=$(cat "$tmp/rc" 2>/dev/null) || rc=1

if [ "$report" = 1 ]; then
    # llvm-profdata: `warning: <path>.profraw: <reason>`
    grep -E '^warning: .*\.profraw: ' "$tmp/out" | while IFS= read -r line; do
        rest=${line#warning: }
        file=${rest%%.profraw: *}.profraw
        reason=${rest#*.profraw: }
        msg="coverage profile dropped: $file ($reason); its counters are missing from the report"
        echo "warning: $msg" >&2
        [ -z "${GITHUB_ACTIONS:-}" ] || echo "::warning title=coverage profile dropped::$msg"
    done
fi
exit "$rc"
