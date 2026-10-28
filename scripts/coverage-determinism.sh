#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Shows that two coverage runs of the crates in scripts/full-coverage-crates.txt execute the same lines.
# Their 100 % gate fails the build when one line stops running, so a line that a random property-test case
# reaches only now and then makes a pull request fail by chance. scripts/coverage-run.sh fixes the proptest
# seed under coverage; this script is the proof: it runs the crates' tests under coverage twice (the second time
# from clean counters) and compares the set of executed lines of each crate's src/ files. Any line in one run
# and not the other fails, and the output names it.
# Slow (two instrumented test runs, no UI build), so no gate set runs it: `scripts/check.sh coverage-determinism`
# by hand after touching coverage-run.sh, a property test or a coverage-gated crate's tests.
# usage: coverage-determinism.sh <cargo> | --self-test       env: COVERAGE_DETERMINISM_RUNS (default 2)
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)

# executed <lcov file> <crates list>: one `file:line` per executed line (count above 0) of the listed crates' src/
# files, sorted. An lcov record is `SF:<path>` then `DA:<line>,<count>` lines.
executed() {
    awk -F'[:,]' -v crates="$2" '
        BEGIN { n = split(crates, c, " ") }
        /^SF:/ {
            f = substr($0, 4); keep = 0
            for (i = 1; i <= n; i++) if (index(f, "/" c[i] "/src/") > 0) keep = 1
        }
        /^DA:/ && keep && $3 > 0 { print f ":" $2 }
    ' "$1" | sort -u
}

# crate_dirs: the repository-relative crate folders of the list, comments and blanks dropped, one line each.
crate_dirs() { sed -e 's/#.*//' -e 's|/*[[:space:]]*$||' -e 's/^[[:space:]]*//' "$here/scripts/full-coverage-crates.txt" | grep -v '^$'; }

if [ "${1:-}" = "--self-test" ]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/coverage-determinism-test.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    printf 'SF:/r/crates/a/src/x.rs\nDA:1,3\nDA:2,0\nSF:/r/crates/a/tests/t.rs\nDA:1,1\nSF:/r/crates/b/src/y.rs\nDA:7,1\nend_of_record\n' >"$tmp/one"
    printf 'SF:/r/crates/a/src/x.rs\nDA:1,9\nDA:2,1\nSF:/r/crates/b/src/y.rs\nDA:7,1\nend_of_record\n' >"$tmp/two"
    [ "$(executed "$tmp/one" "crates/a")" = "/r/crates/a/src/x.rs:1" ] || { echo "self-test: wrong set for crate a" >&2; exit 1; }
    [ "$(executed "$tmp/one" "crates/a crates/b" | wc -l)" = 2 ] || { echo "self-test: two crates should give two lines" >&2; exit 1; }
    executed "$tmp/one" "crates/a crates/b" >"$tmp/s1"
    executed "$tmp/two" "crates/a crates/b" >"$tmp/s2"
    diff "$tmp/s1" "$tmp/s2" >/dev/null && { echo "self-test: a line that runs in one report only was not seen" >&2; exit 1; }
    # Hit counts alone (3 against 9) must not count as a difference.
    executed "$tmp/two" "crates/a" | grep -q 'x.rs:1$' || { echo "self-test: lost a covered line" >&2; exit 1; }
    echo "coverage-determinism: self-test ok"
    exit 0
fi

[ "$#" -ge 1 ] || { echo "usage: coverage-determinism.sh <cargo> | --self-test" >&2; exit 2; }
cargo=$1
runs=${COVERAGE_DETERMINISM_RUNS:-2}
target=${CARGO_TARGET_DIR:-target}
dirs=$(crate_dirs | tr '\n' ' ')
pkgs=""
for d in $dirs; do pkgs="$pkgs -p $(basename "$d")"; done

n=0
while [ "$n" -lt "$runs" ]; do
    n=$((n + 1))
    "$cargo" llvm-cov clean --workspace
    # shellcheck disable=SC2086 # $pkgs is a list of -p flags
    "$here/scripts/coverage-run.sh" "$cargo" $pkgs --all-features --locked --profile "${NEXTEST_PROFILE:-default}"
    "$cargo" llvm-cov report --lcov --output-path "$target/coverage-determinism-$n.lcov"
    executed "$target/coverage-determinism-$n.lcov" "$dirs" >"$target/coverage-determinism-$n.lines"
    echo "coverage-determinism: run $n executed $(wc -l <"$target/coverage-determinism-$n.lines" | tr -d ' ') lines"
done

n=1
status=0
while [ "$n" -lt "$runs" ]; do
    n=$((n + 1))
    if ! diff "$target/coverage-determinism-1.lines" "$target/coverage-determinism-$n.lines" >"$target/coverage-determinism.diff"; then
        echo "coverage-determinism: run 1 and run $n executed different lines ('<' only in run 1, '>' only in run $n):" >&2
        cat "$target/coverage-determinism.diff" >&2
        status=1
    fi
done
[ "$status" = 0 ] && echo "coverage-determinism: $runs runs executed the same lines"
exit "$status"
