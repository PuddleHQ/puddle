#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests the sweep cache of scripts/mutants.sh with a fake cargo (no mutation run): a passing run is
# recorded and skipped next time, a changed crate or a failing run is not skipped, --uncached ignores
# the cache, a dirty tree never uses it, and a cache hit that now fails reports a MISMATCH.
set -eu
# A git hook runs this with GIT_DIR and friends set; they would point the throwaway repository's git
# commands at the real one.
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q .
git config user.email t@example.invalid && git config user.name t
mkdir -p scripts crates/puddle-a .cargo .config
cp "$here/mutants.sh" scripts/mutants.sh
echo puddle-a >scripts/mutants-crates.txt
echo a >crates/puddle-a/lib.rs
echo l >Cargo.lock && echo t >rust-toolchain.toml && echo m >.cargo/mutants.toml && echo n >.config/nextest.toml
git add -A && git commit -qm base

cat >fake-cargo <<'F'
#!/bin/sh
case $1 in
tree) echo "puddle-a v0.0.0" ;;
mutants)
    if [ "$2" = --version ]; then echo "cargo-mutants 0.0.0"; exit 0; fi
    echo run >>"$FAKE_RUNS"
    exit "${FAKE_RC:-0}"
    ;;
esac
F
chmod +x fake-cargo
export CARGO="$tmp/fake-cargo" FAKE_RUNS="$tmp/runs" MUTANTS_OUT="$tmp/out"
: >"$FAKE_RUNS"
sweep() { scripts/mutants.sh sweep --cache "$tmp/cache" "$@" 2>&1; }
runs() { wc -l <"$FAKE_RUNS" | tr -d ' '; }
fail() { echo "mutants-test: FAIL: $1" >&2; exit 1; }

sweep >/dev/null
[ "$(runs)" = 1 ] || fail "first sweep must run"
out=$(sweep)
[ "$(runs)" = 1 ] || fail "unchanged crate must be skipped"
echo "$out" | grep -q 'skipped' || fail "skip must be announced"
sweep --uncached >/dev/null
[ "$(runs)" = 2 ] || fail "--uncached must run"

echo b >crates/puddle-a/lib.rs
git commit -qam change
FAKE_RC=2 sweep >/dev/null && fail "failing run must fail the sweep"
[ "$(runs)" = 3 ] || fail "changed crate must run"
FAKE_RC=2 sweep >/dev/null || true
[ "$(runs)" = 4 ] || fail "a failing run must not be recorded as passing"
sweep >/dev/null
sweep >/dev/null
[ "$(runs)" = 5 ] || fail "a passing run must be recorded once"

echo c >>crates/puddle-a/lib.rs
sweep >/dev/null
[ "$(runs)" = 6 ] || fail "a dirty tree must never use the cache"
git commit -qam again
sweep >/dev/null
out=$(FAKE_RC=2 sweep --uncached) && fail "a failing uncached run must fail"
echo "$out" | grep -q MISMATCH || fail "a cache hit that now fails must say MISMATCH"
echo "mutants-test: ok"
