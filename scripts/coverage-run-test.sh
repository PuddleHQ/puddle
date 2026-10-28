#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of scripts/coverage-run.sh with the real compiler and the real `llvm-profdata`: a tiny instrumented
# program writes a good `.profraw` file, a copy cut to 100 bytes (and an empty file) plays the file of a process
# that stopped mid-write, and a stub "cargo llvm-cov" merges the files the way cargo-llvm-cov does, in batches of
# four, so a batch can hold only unreadable files (the case that failed a CI run although `--failure-mode=all`
# was set: that flag only helps while a batch holds one readable file). What it checks: with many good files and
# one, two or only-cut-batch unreadable files the tests step plus the report pass, and one warning per unreadable
# file names it and the reason (and one GitHub annotation each in CI); with no readable file the report still
# fails (so the ratchet never sees a report made of nothing); no warning without an unreadable file; a failed test
# run fails the script; check.sh runs the coverage tests through the script and reports afterwards.
# What it cannot see: cargo-llvm-cov itself (CI's coverage gate exercises it), or the instrumented runtime
# of a process killed at a real moment.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/coverage-run-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "coverage-run-test: $*" >&2
    exit 1
}
# A hook (git-env's victim variables) must not steer anything below.
# shellcheck disable=SC2046 # the list is words by design
unset $(git rev-parse --local-env-vars)

# The tool of rustc's own LLVM first: a system llvm-profdata of another version rejects its profile format.
profdata=$(find "$(rustc --print sysroot)/lib/rustlib" -name llvm-profdata -type f 2>/dev/null | head -n 1)
[ -n "$profdata" ] || profdata=$(command -v llvm-profdata || true)
[ -n "$profdata" ] || fail "llvm-profdata not found (rustup component add llvm-tools-preview)"

echo 'fn main() { println!("{}", std::env::args().count()); }' >"$tmp/prog.rs"
rustc -C instrument-coverage -o "$tmp/prog" "$tmp/prog.rs" || fail "rustc could not build the instrumented program"
mkdir "$tmp/good"
LLVM_PROFILE_FILE="$tmp/good/ok.profraw" "$tmp/prog" >/dev/null
[ -s "$tmp/good/ok.profraw" ] || fail "the instrumented program wrote no profile"

# The stub cargo: `llvm-cov nextest` puts $PROFILES into its target directory (and, without --no-report, merges
# at once, as the old one-step command did); `llvm-cov report` merges the directory in batches of four.
cat >"$tmp/cargo" <<STUB
#!/bin/sh
tgt="\$TARGET/llvm-cov-target"
case "\$2" in
show-env) echo "export CARGO_LLVM_COV_TARGET_DIR='\$TARGET'"; exit 0 ;;
nextest)
    echo "stub cargo: \$* (PROPTEST_RNG_SEED=\${PROPTEST_RNG_SEED:-unset})"
    mkdir -p "\$tgt" && cp "\$PROFILES"/*.profraw "\$tgt"/
    [ -z "\${FAIL_TESTS:-}" ] || exit 100
    case " \$* " in *" --no-report "*) exit 0 ;; esac ;;
esac
echo "stub cargo: \$* (LLVM_PROFDATA_FLAGS=\${LLVM_PROFDATA_FLAGS:-})"
set -- "\$tgt"/*.profraw
[ -e "\$1" ] || { echo "error: no profile can be merged" >&2; exit 1; }
n=0
while [ "\$#" -gt 0 ]; do
    n=\$((n + 1))
    batch=""
    for _ in 1 2 3 4; do
        [ "\$#" -gt 0 ] || break
        batch="\$batch \$1"
        shift
    done
    # shellcheck disable=SC2086 # the batch is up to four profile paths, split on purpose
    "$profdata" merge \${LLVM_PROFDATA_FLAGS:-} -o "$tmp/batch-\$n.profdata" \$batch || exit 1
done
STUB
chmod +x "$tmp/cargo"

# scenario <name> <good files> <cut files> [empty files]: a directory of profiles. The unreadable ones sort last,
# so with four per batch the last batch holds nothing else.
scenario() {
    d=$tmp/$1
    mkdir -p "$d"
    n=0
    while [ "$n" -lt "$2" ]; do
        n=$((n + 1))
        cp "$tmp/good/ok.profraw" "$d/a-ok-$n.profraw"
    done
    n=0
    while [ "$n" -lt "$3" ]; do
        n=$((n + 1))
        head -c 100 "$tmp/good/ok.profraw" >"$d/z-cut-$n.profraw"
    done
    n=0
    while [ "$n" -lt "${4:-0}" ]; do
        n=$((n + 1))
        : >"$d/z-empty-$n.profraw"
    done
}
# run <name> [env assignment]: the tests step, then the report, as check.sh does. GITHUB_ACTIONS is unset first (a CI
# runner sets it, which adds an annotation line per warning); the exit status goes to $tmp/<name>.rc, the output to
# $tmp/<name>.out.
run() {
    name=$1
    shift
    mkdir -p "$tmp/target-$name"
    set +e
    (
        # shellcheck disable=SC2016 # the script text is for the inner shell
        env -u LLVM_PROFDATA_FLAGS -u GITHUB_ACTIONS -u PROPTEST_RNG_SEED PROFILES="$tmp/$name" TARGET="$tmp/target-$name" "$@" sh -c '
            "$0/scripts/coverage-run.sh" "$1" --workspace && "$1" llvm-cov report --lcov' "$here" "$tmp/cargo"
    ) >"$tmp/$name.out" 2>&1
    echo $? >"$tmp/$name.rc"
    set -e
}
rc() { cat "$tmp/$1.rc"; }

scenario clean 5 0
run clean
[ "$(rc clean)" = 0 ] || fail "clean run failed: $(cat "$tmp/clean.out")"
grep -q -e '--no-report' "$tmp/clean.out" || fail "the tests step did not skip the report"
! grep -q 'coverage profile dropped' "$tmp/clean.out" || fail "warning without a cut file"

# Property tests run from a fixed seed under coverage (a random one makes a 100 % gate fail by chance), and the
# caller can pick another.
grep -q 'PROPTEST_RNG_SEED=[0-9][0-9]*)' "$tmp/clean.out" || fail "the tests step did not fix the proptest seed: $(cat "$tmp/clean.out")"
scenario seeded 1 0
run seeded PROPTEST_RNG_SEED=7
grep -q 'PROPTEST_RNG_SEED=7)' "$tmp/seeded.out" || fail "a seed given by the caller was replaced: $(cat "$tmp/seeded.out")"
scenario unseeded 1 0
run unseeded
[ "$(sed -n 's/.*PROPTEST_RNG_SEED=\([0-9]*\)).*/\1/p' "$tmp/clean.out" | head -n 1)" = "$(sed -n 's/.*PROPTEST_RNG_SEED=\([0-9]*\)).*/\1/p' "$tmp/unseeded.out" | head -n 1)" ] ||
    fail "the default seed changes between runs"

# The failing case of a CI run: nine good files, so the last batch of four holds the cut file alone.
scenario cut 8 1
run cut
[ "$(rc cut)" = 0 ] || fail "a batch of only a cut file failed the run: $(cat "$tmp/cut.out")"
grep -q "^warning: .*z-cut-1\.profraw: .*coverage profile dropped" "$tmp/cut.out" ||
    fail "no warning naming the cut file: $(cat "$tmp/cut.out")"
grep -q 'its counters are missing' "$tmp/cut.out" || fail "the warning does not say what is missing"
[ "$(grep -c '^warning: ' "$tmp/cut.out")" = 1 ] || fail "not exactly one warning for one cut file"
[ -f "$tmp/target-cut/llvm-cov-target/z-cut-1.profraw.dropped" ] || fail "the cut file was not kept as evidence"
[ ! -e "$tmp/target-cut/llvm-cov-target/z-cut-1.profraw" ] || fail "the cut file is still merged"

scenario annotated 3 1
run annotated GITHUB_ACTIONS=true
[ "$(rc annotated)" = 0 ] || fail "annotated run failed"
grep -q '^::warning title=coverage profile dropped::' "$tmp/annotated.out" || fail "no GitHub annotation in CI"
[ "$(grep -c '^::warning' "$tmp/annotated.out")" = 1 ] || fail "not exactly one annotation"

scenario two 8 2 1
run two
[ "$(rc two)" = 0 ] || fail "three unreadable files failed the run: $(cat "$tmp/two.out")"
[ "$(grep -c '^warning: ' "$tmp/two.out")" = 3 ] || fail "three unreadable files, not three warnings"
grep -q 'empty raw profile file' "$tmp/two.out" || fail "the reason of the empty file is not shown"

scenario none 0 2
run none
[ "$(rc none)" != 0 ] || fail "a run with no readable profile passed"
grep -q 'no profile can be merged' "$tmp/none.out" || fail "the merge error is not shown"
[ "$(grep -c '^warning: ' "$tmp/none.out")" = 2 ] || fail "the unreadable files are not named"

scenario failing 2 0
run failing FAIL_TESTS=1
[ "$(rc failing)" = 100 ] || fail "a failed test run did not fail the script: $(cat "$tmp/failing.out")"

scenario lane 4 1
run lane LLVM_PROFDATA_FLAGS=--failure-mode=all
[ "$(rc lane)" = 0 ] || fail "a run with the lane's flag failed"
[ "$(grep -c '^warning: ' "$tmp/lane.out")" = 1 ] || fail "not exactly one warning with the lane's flag"

# shellcheck disable=SC2016 # a literal pattern
grep -q 'scripts/coverage-run.sh "\$plain_cargo" --workspace' "$here/scripts/check.sh" ||
    fail "check.sh does not run the coverage tests through coverage-run.sh"
grep -q 'llvm-cov report --lcov' "$here/scripts/check.sh" || fail "check.sh does not report after the tests"
echo "coverage-run-test: ok"
