#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests the sweep cache of scripts/mutants.sh with a fake cargo (no mutation run): a passing run is
# recorded and skipped next time, a changed crate or a failing run is not skipped, --uncached ignores
# the cache, a dirty tree never uses it, and a cache hit that now fails reports a MISMATCH. Then the
# list's two forms: a crate with a file list is swept with one `-f` per file (and `diff` looks at those
# files only), a crate without one whole, a file unit has its own cache entry, a malformed list is refused.
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
tree) echo "$3 v0.0.0" ;;
mutants)
    if [ "$2" = --version ]; then echo "cargo-mutants 0.0.0"; exit 0; fi
    echo "$*" >>"$FAKE_RUNS"
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

# The list's two forms: puddle-a whole, puddle-b by file.
mkdir -p crates/puddle-b/src
echo x >crates/puddle-b/src/h2.rs && echo y >crates/puddle-b/src/http.rs && echo z >crates/puddle-b/src/other.rs
cat >scripts/mutants-crates.txt <<'L'
# a comment
puddle-a              # the whole crate
puddle-b src/h2.rs    # two files of a big crate
puddle-b src/http.rs
L
git add scripts crates && git commit -qm lists
[ "$(scripts/mutants.sh --list | tr '\n' ' ')" = "puddle-a puddle-b " ] || fail "--list must name each crate once"
[ "$(scripts/mutants.sh --units | tr '\n' ',')" = "puddle-a,puddle-b src/h2.rs,puddle-b src/http.rs," ] ||
    fail "--units must print the whole crate and each file"

: >"$FAKE_RUNS"
cache="$tmp/cache2"
sweep --cache "$cache" >/dev/null
[ "$(runs)" = 3 ] || fail "a sweep runs the whole crate and each file once, not $(runs)"
[ "$(grep -c -e '-p puddle-a ' "$FAKE_RUNS")" = 1 ] || fail "puddle-a must run once"
grep -e '-p puddle-a ' "$FAKE_RUNS" | grep -q -e ' -f ' && fail "a whole crate must not get -f"
[ "$(grep -c -e '-p puddle-b .* -f crates/puddle-b/src/h2.rs --output' "$FAKE_RUNS")" = 1 ] || fail "h2.rs must run alone"
[ "$(grep -c -e '-p puddle-b .* -f crates/puddle-b/src/http.rs --output' "$FAKE_RUNS")" = 1 ] || fail "http.rs must run alone"
grep -q 'other.rs' "$FAKE_RUNS" && fail "an unlisted file must not be swept"
out=$(sweep --cache "$cache")
[ "$(runs)" = 3 ] || fail "every unit must be skipped when unchanged"
[ "$(echo "$out" | grep -c skipped)" = 3 ] || fail "each skipped unit must be announced"

# --crate on a listed-by-file crate is one run with every listed file; --file is one file, with its own entry.
sweep --cache "$cache" --crate puddle-b >/dev/null
[ "$(runs)" = 4 ] || fail "--crate on a file-listed crate must run"
tail -n 1 "$FAKE_RUNS" | grep -q -e '-f crates/puddle-b/src/h2.rs -f crates/puddle-b/src/http.rs' ||
    fail "--crate must pass every listed file"
sweep --cache "$cache" --crate puddle-b --file src/h2.rs >/dev/null
[ "$(runs)" = 4 ] || fail "--file must reuse the cache entry of that file"
sweep --crate puddle-b --file src/new.rs >/dev/null
tail -n 1 "$FAKE_RUNS" | grep -q -e '-f crates/puddle-b/src/new.rs --output' || fail "--file may name a file that is not listed"
sweep --file src/h2.rs >/dev/null && fail "--file without --crate must be refused"

# Any change in the crate runs its files again, and a list entry added later does not hit an old entry.
echo w >crates/puddle-b/src/other.rs && git commit -qam other
: >"$FAKE_RUNS"
sweep --cache "$cache" >/dev/null
[ "$(runs)" = 2 ] || fail "a change in puddle-b must re-run its two files (and not puddle-a), ran $(runs)"
echo 'puddle-b src/other.rs' >>scripts/mutants-crates.txt && git commit -qam third
: >"$FAKE_RUNS"
sweep --cache "$cache" >/dev/null
[ "$(runs)" = 1 ] || fail "only the new file must run, ran $(runs)"

# diff: a change in an unlisted file is nothing to mutate; a listed file's change is mutated, alone.
echo v >crates/puddle-b/src/unlisted.rs && git add crates && git commit -qm unlisted
: >"$FAKE_RUNS"
scripts/mutants.sh diff HEAD~1 >/dev/null
[ "$(runs)" = 0 ] || fail "an unlisted file's change must not start cargo-mutants"
echo u >crates/puddle-b/src/h2.rs && git commit -qam h2
scripts/mutants.sh diff HEAD~2 >/dev/null
[ "$(runs)" = 1 ] || fail "a listed file's change must run once"
grep -q 'h2.rs' "$MUTANTS_OUT/change.diff" || fail "the diff must hold the listed file"
grep -q 'unlisted.rs' "$MUTANTS_OUT/change.diff" && fail "the diff must hold listed files only"

# A malformed list is refused.
printf 'puddle-a\npuddle-a src/x.rs\n' >scripts/mutants-crates.txt
scripts/mutants.sh --list >/dev/null 2>&1 && fail "a crate listed whole and by file must be refused"
printf 'puddle-a src/x.rs extra\n' >scripts/mutants-crates.txt
scripts/mutants.sh --list >/dev/null 2>&1 && fail "a line of three words must be refused"
echo "mutants-test: ok"
