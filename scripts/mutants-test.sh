#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests the sweep cache of scripts/mutants.sh with a fake cargo (no mutation run): a passing run is
# recorded and skipped next time, a changed crate or a failing run is not skipped, --uncached ignores
# the cache, a dirty tree never uses it, and a cache hit that now fails reports a MISMATCH. Then the
# list's two forms: a crate with a file list is swept with one `-f` per file (and `diff` looks at those
# files only), a crate without one whole, a file unit has its own cache entry, a malformed list is refused.
# The job matrix is a list of {crate, file, shard, name} objects and the workflow spreads it with `include:`.
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
# A small shared cargo home: the sweeps copy it, and the real one can be gigabytes.
mkdir -p "$tmp/shared/registry/index" "$tmp/shared/git"
echo seed >"$tmp/shared/registry/index/entry"
echo '[net]' >"$tmp/shared/config.toml"
export CARGO_HOME="$tmp/shared"
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

# The job matrix: a list of objects whose fields are strings, and the workflow spreads it with `include:` (a
# list of objects under `matrix.crate` made the first nightly fail with "A mapping was not expected").
printf 'puddle-a\npuddle-b src/h2.rs\n' >scripts/mutants-crates.txt
matrix=$(scripts/mutants.sh --matrix)
[ "$(echo "$matrix" | jq -c '[.[] | [.crate, .file, .name] | map(type == "string") | all] | all')" = true ] || fail "matrix fields must be strings: $matrix"
[ "$(echo "$matrix" | jq -r '.[1] | "\(.crate) \(.file) \(.name)"')" = "puddle-b src/h2.rs src-h2-rs" ] || fail "matrix entry wrong: $matrix"
[ "$(echo "$matrix" | jq -r '.[0].file')" = "" ] || fail "a whole-crate unit has an empty file: $matrix"
real=$("$here/mutants.sh" --matrix)
[ "$(echo "$real" | jq 'type == "array" and length > 0 and all(.[]; type == "object")')" = true ] || fail "the real list's matrix must be a list of objects: $real"
wf="$here/../.github/workflows/mutants.yml"
grep -Eq '^ +include: \$\{\{ fromJson\(needs\.crates\.outputs\.matrix\) \}\}$' "$wf" || fail "the sweep job must use matrix include: with the crate list"
grep -Eq '^ +crate: \$\{\{ fromJson' "$wf" && fail "the matrix must not be keyed by crate: its entries are objects"
grep -q 'scripts/mutants.sh --matrix' "$wf" || fail "the workflow must build the matrix with scripts/mutants.sh --matrix"

# `crate shards=N`: N units `crate @K/N`, one `--shard K/N` run each, with their own cache entries; the matrix
# carries the shard and a name safe for a file or artifact; another crate's lines are untouched.
printf 'puddle-a shards=3   # a big crate\nputdle-x\n' | sed 's/putdle/puddle/' >scripts/mutants-crates.txt
[ "$(scripts/mutants.sh --units | tr '\n' ',')" = "puddle-a @0/3,puddle-a @1/3,puddle-a @2/3,puddle-x," ] || fail "--units must print one unit per shard"
[ "$(scripts/mutants.sh --list | tr '\n' ' ')" = "puddle-a puddle-x " ] || fail "--list must name a sharded crate once"
matrix=$(scripts/mutants.sh --matrix)
[ "$(echo "$matrix" | jq -c '[.[] | [.crate, .file, .shard, .name] | map(type == "string") | all] | all')" = true ] || fail "matrix fields must be strings: $matrix"
[ "$(echo "$matrix" | jq -r '.[1] | "\(.crate)|\(.file)|\(.shard)|\(.name)"')" = "puddle-a||1/3|shard-1of3" ] || fail "shard matrix entry wrong: $matrix"
[ "$(echo "$matrix" | jq -r '.[3] | "\(.crate)|\(.file)|\(.shard)|\(.name)"')" = "puddle-x|||" ] || fail "unsharded matrix entry wrong: $matrix"
[ "$(echo "$matrix" | jq 'length')" = 4 ] || fail "matrix must have one entry per unit: $matrix"
mkdir -p crates/puddle-x && echo x >crates/puddle-x/lib.rs && git add scripts crates && git commit -qm sharded
: >"$FAKE_RUNS"
cache="$tmp/cache3"
sweep --cache "$cache" >/dev/null
[ "$(runs)" = 4 ] || fail "a sharded sweep runs each shard and the other crate once, ran $(runs)"
for k in 0 1 2; do
    [ "$(grep -c -e "-p puddle-a .*--shard $k/3 --output" "$FAKE_RUNS")" = 1 ] || fail "shard $k/3 must run once"
done
grep -e '-p puddle-x ' "$FAKE_RUNS" | grep -q -e '--shard' && fail "an unsharded crate must not get --shard"
sweep --cache "$cache" >/dev/null
[ "$(runs)" = 4 ] || fail "every passing shard must be skipped when unchanged"
# One failing shard fails the sweep and is not recorded; the passing ones stay skipped.
echo y >crates/puddle-a/lib.rs && git commit -qam change-a
: >"$FAKE_RUNS"
cat >fake-cargo-shard <<'F'
#!/bin/sh
case $1 in
tree) echo "$3 v0.0.0" ;;
mutants)
    [ "$2" = --version ] && { echo "cargo-mutants 0.0.0"; exit 0; }
    echo "$*" >>"$FAKE_RUNS"
    case $* in *"--shard 1/3"*) exit 2 ;; esac
    ;;
esac
F
chmod +x fake-cargo-shard
: >"$FAKE_RUNS"
CARGO="$tmp/fake-cargo-shard" sweep --cache "$cache" >/dev/null && fail "a failing shard must fail the sweep"
[ "$(runs)" = 3 ] || fail "all three shards of the changed crate run, ran $(runs)"
: >"$FAKE_RUNS"
CARGO="$tmp/fake-cargo-shard" sweep --cache "$cache" >/dev/null && fail "a failing shard must fail the sweep again"
[ "$(runs)" = 1 ] || fail "only the failing shard runs again, ran $(runs)"
# --shard by hand still works on a crate that is not sharded in the list.
printf 'puddle-x\n' >scripts/mutants-crates.txt
: >"$FAKE_RUNS"
sweep --crate puddle-x --shard 2/5 >/dev/null
tail -n 1 "$FAKE_RUNS" | grep -q -e '--shard 2/5 --output' || fail "--shard on the command line must pass through"
printf 'puddle-a shards=0\n' >scripts/mutants-crates.txt
scripts/mutants.sh --list >/dev/null 2>&1 && fail "shards=0 must be refused"
printf 'puddle-a shards=2\npuddle-a src/x.rs\n' >scripts/mutants-crates.txt
scripts/mutants.sh --list >/dev/null 2>&1 && fail "a crate with shards=N and a file line must be refused"

# A malformed list is refused.
printf 'puddle-a\npuddle-a src/x.rs\n' >scripts/mutants-crates.txt
scripts/mutants.sh --list >/dev/null 2>&1 && fail "a crate listed whole and by file must be refused"
printf 'puddle-a src/x.rs extra\n' >scripts/mutants-crates.txt
scripts/mutants.sh --list >/dev/null 2>&1 && fail "a line of three words must be refused"

# Cargo home. A fake cargo takes a lock in its CARGO_HOME's package cache, as cargo does, and holds it until the
# other sweep holds its own too: two sweeps on one home wait on each other (the fake gives up after 10 s and
# fails), two private homes both get through at once.
echo puddle-a >scripts/mutants-crates.txt
mkdir -p "$tmp/rv" "$tmp/rv-shared"
cat >fake-cargo-home <<'F'
#!/bin/sh
case $1 in
mutants)
    [ "$2" = --version ] && { echo "cargo-mutants 0.0.0"; exit 0; }
    [ -f "$CARGO_HOME/registry/index/entry" ] && [ -f "$CARGO_HOME/config.toml" ] || exit 7
    echo "$CARGO_HOME" >>"$FAKE_HOMES"
    n=0
    until mkdir "$CARGO_HOME/.package-cache.lock" 2>/dev/null; do
        n=$((n + 1)) && [ "$n" -lt 100 ] || exit 9
        sleep 0.1
    done
    : >"$FAKE_RV/$$"
    n=0
    while [ "$(find "$FAKE_RV" -type f | wc -l)" -lt 2 ]; do
        n=$((n + 1)) && [ "$n" -lt 100 ] || exit 8
        sleep 0.1
    done
    rmdir "$CARGO_HOME/.package-cache.lock"
    ;;
esac
F
chmod +x fake-cargo-home
export CARGO="$tmp/fake-cargo-home" FAKE_HOMES="$tmp/homes" FAKE_RV="$tmp/rv"
: >"$FAKE_HOMES"
before=$(find "$tmp/shared" | wc -l)
MUTANTS_OUT="$tmp/o1" scripts/mutants.sh sweep >/dev/null 2>&1 &
p1=$!
MUTANTS_OUT="$tmp/o2" scripts/mutants.sh sweep >/dev/null 2>&1 &
p2=$!
wait "$p1" || fail "the first of two concurrent sweeps must not wait on the other"
wait "$p2" || fail "the second of two concurrent sweeps must not wait on the other"
[ "$(sort -u "$FAKE_HOMES" | wc -l)" = 2 ] || fail "each sweep must have its own cargo home"
grep -qx "$tmp/shared" "$FAKE_HOMES" && fail "a sweep must not use the shared cargo home"
while read -r h; do [ ! -e "$h" ] || fail "a private cargo home must be removed when the sweep ends"; done <"$FAKE_HOMES"
[ "$(find "$tmp/shared" | wc -l)" = "$before" ] || fail "the shared cargo home must stay untouched"
# The control: the same two sweeps on the shared home do wait on each other.
export MUTANTS_SHARED_CARGO_HOME=1 FAKE_RV="$tmp/rv-shared"
MUTANTS_OUT="$tmp/o1" scripts/mutants.sh sweep >/dev/null 2>&1 &
p1=$!
MUTANTS_OUT="$tmp/o2" scripts/mutants.sh sweep >/dev/null 2>&1 &
p2=$!
rc=0
wait "$p1" || rc=1
wait "$p2" || rc=1
[ "$rc" = 1 ] || fail "MUTANTS_SHARED_CARGO_HOME=1 must keep the shared home (the control: its sweeps wait on one lock)"
# Temp dir: cargo-mutants runs with TMPDIR under the target dir (never the system temp dir), removed when the run
# ends, and the jobs are capped to what the free space holds.
unset MUTANTS_SHARED_CARGO_HOME
export CARGO_TARGET_DIR="$tmp/target"
cat >fake-cargo-tmp <<'F'
#!/bin/sh
case $1 in
mutants)
    [ "$2" = --version ] && { echo "cargo-mutants 0.0.0"; exit 0; }
    echo "TMPDIR=$TMPDIR CARGO_HOME=$CARGO_HOME $*" >>"$FAKE_TMPS"
    : >"$TMPDIR/copy"
    ;;
esac
F
chmod +x fake-cargo-tmp
export CARGO="$tmp/fake-cargo-tmp" FAKE_TMPS="$tmp/tmps"
: >"$FAKE_TMPS"
MUTANTS_JOBS=4 MUTANTS_FREE_GB=1000 scripts/mutants.sh sweep >/dev/null 2>&1
grep -q -e ' -j 4 ' "$FAKE_TMPS" || fail "with room for every job the cap must not lower them"
used=$(sed 's/^TMPDIR=\([^ ]*\) .*/\1/' "$FAKE_TMPS")
case $used in "$tmp"/target/mutants-tmp.*) ;; *) fail "TMPDIR must be a mutants-tmp dir under the target dir, was $used" ;; esac
[ ! -e "$used" ] || fail "the private TMPDIR must be removed when the run ends"
grep -q "CARGO_HOME=$used/" "$FAKE_TMPS" || fail "the private cargo home must sit inside the private TMPDIR"
: >"$FAKE_TMPS"
err=$(MUTANTS_JOBS=4 MUTANTS_FREE_GB=30 scripts/mutants.sh sweep 2>&1 >/dev/null)
grep -q -e ' -j 2 ' "$FAKE_TMPS" || fail "30 GB free must cap four jobs at two"
echo "$err" | grep -q 'running 2 job(s) instead of 4' || fail "a lowered job count must be said"
: >"$FAKE_TMPS"
MUTANTS_JOBS=4 MUTANTS_FREE_GB=1 scripts/mutants.sh sweep >/dev/null 2>&1
grep -q -e ' -j 1 ' "$FAKE_TMPS" || fail "never fewer than one job"
: >"$FAKE_TMPS"
MUTANTS_TMPDIR="$tmp/elsewhere" MUTANTS_FREE_GB=1000 scripts/mutants.sh sweep >/dev/null 2>&1
grep -q "^TMPDIR=$tmp/elsewhere/mutants-tmp\." "$FAKE_TMPS" || fail "MUTANTS_TMPDIR must pick the directory"
[ -z "$(ls "$tmp/elsewhere")" ] || fail "MUTANTS_TMPDIR must be left empty after the run"
: >"$FAKE_TMPS"
MUTANTS_SHARED_CARGO_HOME=1 MUTANTS_FREE_GB=1000 scripts/mutants.sh sweep >/dev/null 2>&1
grep -q "^TMPDIR=$tmp/target/mutants-tmp\." "$FAKE_TMPS" || fail "the private TMPDIR must not depend on the cargo home choice"
echo "mutants-test: ok"
