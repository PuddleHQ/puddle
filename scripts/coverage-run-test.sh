#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of scripts/coverage-run.sh with the real compiler and the real `llvm-profdata merge`: a tiny
# instrumented program writes a good `.profraw` file, a copy cut to 100 bytes plays the file of a process
# that stopped mid-write, and a stub "cargo llvm-cov" merges the files the way cargo-llvm-cov does (it passes
# $LLVM_PROFDATA_FLAGS to the merge). What it checks: with one good and one cut file the command passes, and
# a warning names the cut file and the reason; with only cut files it still fails (so the ratchet never sees
# a report made of nothing); no warning without a cut file; no second warning when the caller set the flag
# itself; check.sh runs the coverage tests through the script.
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

# The stub: `cargo llvm-cov nextest ...` merges $PROFILES like cargo-llvm-cov does.
cat >"$tmp/cargo" <<STUB
#!/bin/sh
echo "stub cargo: \$* (LLVM_PROFDATA_FLAGS=\${LLVM_PROFDATA_FLAGS:-})"
# shellcheck disable=SC2086
exec "$profdata" merge \${LLVM_PROFDATA_FLAGS:-} -o "$tmp/merged.profdata" "\$PROFILES"/*.profraw
STUB
chmod +x "$tmp/cargo"

# scenario <name> <good files: 0|1> <cut files>: a directory of profiles.
scenario() {
    d=$tmp/$1
    mkdir -p "$d"
    [ "$2" = 0 ] || cp "$tmp/good/ok.profraw" "$d/run-ok.profraw"
    n=0
    while [ "$n" -lt "$3" ]; do
        n=$((n + 1))
        head -c 100 "$tmp/good/ok.profraw" >"$d/run-cut-$n.profraw"
    done
}
# run <name> [env assignment]: the exit status goes to $tmp/<name>.rc, the output to $tmp/<name>.out.
run() {
    name=$1
    shift
    set +e
    env -u LLVM_PROFDATA_FLAGS PROFILES="$tmp/$name" "$@" "$here/scripts/coverage-run.sh" "$tmp/cargo" llvm-cov nextest \
        >"$tmp/$name.out" 2>&1
    echo $? >"$tmp/$name.rc"
    set -e
}
rc() { cat "$tmp/$1.rc"; }

scenario clean 1 0
run clean
[ "$(rc clean)" = 0 ] || fail "clean run failed: $(cat "$tmp/clean.out")"
grep -q 'failure-mode=all' "$tmp/clean.out" || fail "the flag was not set"
! grep -q 'coverage profile dropped' "$tmp/clean.out" || fail "warning without a cut file"

scenario cut 1 1
run cut
[ "$(rc cut)" = 0 ] || fail "a cut file failed the run: $(cat "$tmp/cut.out")"
grep -q "^warning: coverage profile dropped: .*run-cut-1\.profraw (.*)" "$tmp/cut.out" ||
    fail "no warning naming the cut file: $(cat "$tmp/cut.out")"
grep -q 'its counters are missing' "$tmp/cut.out" || fail "the warning does not say what is missing"
[ "$(grep -c 'coverage profile dropped' "$tmp/cut.out")" = 1 ] || fail "more than one warning for one cut file"

scenario annotated 1 1
run annotated GITHUB_ACTIONS=true
grep -q '^::warning title=coverage profile dropped::' "$tmp/annotated.out" || fail "no GitHub annotation in CI"

scenario two 1 2
run two
[ "$(grep -c '^warning: coverage profile dropped' "$tmp/two.out")" = 2 ] || fail "two cut files, not two warnings"

scenario none 0 2
run none
[ "$(rc none)" != 0 ] || fail "a run with no readable profile passed"
grep -q 'no profile can be merged' "$tmp/none.out" || fail "the merge error is not shown"

scenario preset 1 1
run preset LLVM_PROFDATA_FLAGS=--failure-mode=all
[ "$(rc preset)" = 0 ] || fail "preset flag run failed"
! grep -q 'coverage profile dropped' "$tmp/preset.out" || fail "second warning when the caller set the flag"

scenario appended 1 1
run appended LLVM_PROFDATA_FLAGS=--sparse
grep -q 'LLVM_PROFDATA_FLAGS=--sparse --failure-mode=all' "$tmp/appended.out" || fail "the caller's flags were not kept"

# shellcheck disable=SC2016 # a literal pattern
grep -q 'scripts/coverage-run.sh "\$cargo" llvm-cov nextest' "$here/scripts/check.sh" ||
    fail "check.sh does not run the coverage tests through coverage-run.sh"
echo "coverage-run-test: ok"
