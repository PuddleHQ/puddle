#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Runs the Rust tests with coverage (`cargo llvm-cov nextest --no-report <args>`) and sets aside the profile files
# that cannot be read, so the report step that follows merges readable files only (scripts/check.sh runs
# `cargo llvm-cov report` after this script). The instrumented runtime writes one `.profraw` file per process
# when it exits; a process stopped while it writes leaves a short or zeroed file, and `llvm-profdata merge`
# fails on one such file although every test passed. cargo-llvm-cov merges in several batches, so the merge's own
# `--failure-mode=all` does not help when one batch holds only unreadable files. Checking each file first does.
# A dropped file is renamed to `<name>.profraw.dropped` (kept as evidence, no longer merged) and named in a
# warning line (a GitHub annotation in CI), so a loss is never silent. When no readable file is left the report
# fails, and so does the ratchet and diff-coverage gate when the counters really went missing.
# usage: coverage-run.sh <cargo> <nextest args...>      Exit status: the test run's, else 0.
set -u

[ "$#" -gt 1 ] || {
    echo "usage: coverage-run.sh <cargo> <nextest args...>" >&2
    exit 2
}
cargo=$1
shift

# Property tests draw random cases, so which arm a run reaches would change from run to run, and a 100 % gate
# (scripts/full-coverage-crates.txt) would fail by chance on an arm only a proptest reaches (it did, on a pull
# request that touched none of that code). A fixed seed under coverage makes the line set the same every run;
# ordinary test runs keep their random seeds and keep exploring. Set PROPTEST_RNG_SEED to try another seed.
# Proptest reads the variable in its default config, which every `proptest!` block here extends.
PROPTEST_RNG_SEED=${PROPTEST_RNG_SEED:-20261028}
export PROPTEST_RNG_SEED

"$cargo" llvm-cov nextest --no-report "$@"
rc=$?
[ "$rc" = 0 ] || exit "$rc"

# llvm-profdata of the same LLVM as the instrumented code first: another version rejects its profile format.
profdata=${LLVM_PROFDATA:-}
[ -n "$profdata" ] || profdata=$(find "$(rustc --print sysroot)/lib/rustlib" -name llvm-profdata -type f 2>/dev/null | head -n 1)
[ -n "$profdata" ] || profdata=$(command -v llvm-profdata || true)
[ -n "$profdata" ] || {
    echo "coverage-run: llvm-profdata not found (rustup component add llvm-tools-preview)" >&2
    exit 2
}

# The profile files are in the target directory cargo-llvm-cov reports, or in its `llvm-cov-target` subdirectory
# (where `nextest` builds); both are searched, one level deep.
dir=$("$cargo" llvm-cov show-env --sh | sed -n "s/^export CARGO_LLVM_COV_TARGET_DIR=//p" | tr -d "'\"")
[ -d "$dir" ] || {
    echo "coverage-run: the profile directory '$dir' does not exist" >&2
    exit 2
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/coverage-run.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT

# `llvm-profdata show` reads a whole file and fails on a bad one (`error: <file>: <reason>`); eight at a time,
# each failure one tab-separated line (short enough to be written whole).
export profdata
# shellcheck disable=SC2016 # the script text is for the inner shell
find "$dir" "$dir/llvm-cov-target" -maxdepth 1 -name '*.profraw' -type f -print0 2>/dev/null | xargs -0 -r -P 8 -n 32 sh -c '
    for f; do
        err=$("$profdata" show "$f" 2>&1 >/dev/null) && continue
        reason=${err#error: "$f": }
        printf "%s\t%s\n" "$f" "$(printf "%s" "$reason" | tr "\n\t" "  " | cut -c1-200)"
    done' sh >"$tmp/bad" 2>/dev/null

sort -u "$tmp/bad" | while IFS="$(printf '\t')" read -r file reason; do
    [ -n "$file" ] || continue
    mv -f "$file" "$file.dropped" 2>/dev/null || rm -f "$file"
    # The shape of llvm-profdata's own merge warning, which the host lanes' evidence block reads.
    msg="$file: $reason; coverage profile dropped, its counters are missing from the report"
    echo "warning: $msg" >&2
    [ -z "${GITHUB_ACTIONS:-}" ] || echo "::warning title=coverage profile dropped::$msg"
done
exit 0
