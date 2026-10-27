#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test: the coverage gates run plain `cargo` even when CARGO names a wrapper (mbx). A shared build cache
# restores instrumented objects compiled in another checkout, so the wrapper's coverage report counts that
# checkout's files and fails the ratchet from the second checkout on. Runs the real scripts/check.sh in a scratch
# copy with stub `cargo`, `node` and `npm` on PATH, a stub coverage-run.sh and a wrapper that logs its calls.
# Cases: with the shim first on PATH the gates still pick the real Cargo; `coverage` with CARGO=<wrapper> calls the wrapper never and `cargo` for the tests and the three reports;
# `fmt` with CARGO=<wrapper> still calls the wrapper (the rule is for coverage only); `coverage-ratchet` follows
# the same rule. Not seen: a real mbx or a real report (see docs/STANDARDS.md, "Shared build cache").
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/coverage-cargo-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "coverage-cargo-test: $*" >&2
    exit 1
}
# shellcheck disable=SC2046 # the list is words by design
unset $(git rev-parse --local-env-vars)

ws=$tmp/ws
mkdir -p "$ws/scripts" "$ws/ui/scripts" "$tmp/bin" "$tmp/target"
cp "$here/scripts/check.sh" "$here/scripts/gate-sched.sh" "$ws/scripts/"
echo '{}' >"$ws/scripts/coverage-baseline.json"
: >"$ws/scripts/diff-coverage-exclusions.txt"
# The coverage tests step: logs the cargo it was given (check.sh's first argument), like the real one does.
cat >"$ws/scripts/coverage-run.sh" <<'STUB'
#!/bin/sh
echo "run $1" >>"$CALLS"
STUB
# One stub for every program: logs "<name> <first two arguments>".
for name in cargo wrapper node npm; do
    cat >"$tmp/bin/$name" <<'STUB'
#!/bin/sh
echo "$(basename "$0") $1 $2" >>"$CALLS"
STUB
    chmod +x "$tmp/bin/$name"
done
chmod +x "$ws/scripts/coverage-run.sh"
git -C "$ws" init -q -b develop
git -C "$ws" add -A
git -C "$ws" -c user.email=t@t -c user.name=t commit -q -m base
git -C "$ws" update-ref refs/remotes/origin/develop HEAD

run() {
    : >"$tmp/calls"
    (cd "$ws" && env -u CI CALLS="$tmp/calls" PATH="$tmp/bin:$PATH" CARGO="$tmp/bin/wrapper" \
        CARGO_TARGET_DIR="$tmp/target" PUDDLE_CHECK_SERIAL=1 CHECK_NO_SKIP=1 scripts/check.sh "$@") >"$tmp/out" 2>&1 ||
        fail "check.sh $* failed: $(cat "$tmp/out")"
}

for gate in coverage coverage-ratchet; do
    run "$gate"
    ! grep -q '^wrapper ' "$tmp/calls" || fail "$gate called the wrapper: $(cat "$tmp/calls")"
    grep -qx "run $tmp/bin/cargo" "$tmp/calls" || fail "$gate did not run the tests with plain cargo: $(cat "$tmp/calls")"
    [ "$(grep -c '^cargo llvm-cov' "$tmp/calls")" = 3 ] ||
        fail "$gate did not make the three reports with plain cargo: $(cat "$tmp/calls")"
done

# With mbx's Cargo shim first on PATH (a `mbx-target` file marks its directory) and mise's wrapper directory
# after it, the coverage gates skip both and run the real Cargo.
mkdir -p "$tmp/shim" "$tmp/mise/command-wrappers/bin"
cp "$tmp/bin/cargo" "$tmp/shim/cargo"
cp "$tmp/bin/cargo" "$tmp/mise/command-wrappers/bin/cargo"
echo mbx >"$tmp/shim/mbx-target"
for gate in coverage coverage-ratchet; do
    : >"$tmp/calls"
    (cd "$ws" && env -u CI CALLS="$tmp/calls" PATH="$tmp/shim:$tmp/mise/command-wrappers/bin:$tmp/bin:$PATH" \
        CARGO_TARGET_DIR="$tmp/target" PUDDLE_CHECK_SERIAL=1 CHECK_NO_SKIP=1 scripts/check.sh "$gate") >"$tmp/out" 2>&1 ||
        fail "check.sh $gate with the shim failed: $(cat "$tmp/out")"
    grep -qx "run $tmp/bin/cargo" "$tmp/calls" || fail "$gate used a shim for the tests: $(cat "$tmp/calls")"
done

# fmt is no coverage gate: the wrapper still runs it.
run fmt
grep -q '^wrapper fmt' "$tmp/calls" || fail "fmt no longer uses the wrapper: $(cat "$tmp/calls")"
echo "coverage-cargo-test: ok"
