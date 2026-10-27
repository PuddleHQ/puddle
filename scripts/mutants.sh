#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Mutation testing with cargo-mutants (docs/STANDARDS.md, "Mutation testing"). Usage:
#   scripts/mutants.sh diff [<base>]       the mutants on lines the change adds or changes, in the
#                                          listed crates only (the per-landing gate; `check.sh mutants`)
#   scripts/mutants.sh sweep [options]     every mutant of the listed crates (the nightly run)
#       --shard K/N      run shard K of N (cargo-mutants splits the mutant list; K counts from 0)
#       --crate NAME     only this crate (it need not be in the list: how a new crate is triaged)
#       --cache DIR      skip a crate whose inputs hashed the same as in a run that passed (below)
#       --uncached       ignore the cache and run everything (the weekly run that proves the cache)
#   scripts/mutants.sh --list              prints the listed crates
#   scripts/mutants.sh --key CRATE         prints the crate's cache key (clean tree only)
# The crates are in scripts/mutants-crates.txt. Settings are in .cargo/mutants.toml. Results go to
# $MUTANTS_OUT (default <target dir>/mutants-out); a survivor or a timeout fails the run (exit 2 or 3
# from cargo-mutants, passed on), and the missed mutants are printed as `file:line: description`.
#
# Cache (a sweep only): a crate's result is reused only when every input that can change it is
# byte-identical to the last passing run: the committed trees of the crate and of every workspace crate
# it depends on (normal, build and dev, transitively), Cargo.lock, rust-toolchain.toml,
# .cargo/mutants.toml, .config/nextest.toml, this script and the cargo-mutants version. The key is
# conservative on purpose (a change to a dependency re-runs every crate above it) and a dirty tree
# never uses it. Only a passing run is recorded, so a cache never hides a survivor. `--uncached` runs
# everything and, when a crate has a cache entry, says so if the two outcomes differ (the weekly run).
set -eu

cd "$(dirname "$0")/.."
cargo="${CARGO:-cargo}"
unset CARGO
target="${CARGO_TARGET_DIR:-target}"
out="${MUTANTS_OUT:-$target/mutants-out}"
# cargo-mutants gives each of its parallel jobs its own copy of the tree and, unless told otherwise, that
# copy's own target directory. A shared CARGO_TARGET_DIR made them race: cargo names a workspace member's
# artifacts from its path inside the workspace, so every copy writes the same files, and one job tested
# another job's mutant (a different set of mutants survived on each of two runs). So it is not passed on.
unset CARGO_TARGET_DIR
list=scripts/mutants-crates.txt
# Mutants tested at the same time (each copies the tree and builds it): MUTANTS_JOBS, default 2.
jobs="${MUTANTS_JOBS:-2}"

crates() { sed 's/#.*//' "$list" | awk 'NF { print $1 }'; }

# The workspace crates a crate depends on, itself included, one per line.
closure() {
    "$cargo" tree -p "$1" -e normal,build,dev --prefix none --offline 2>/dev/null |
        awk '$1 ~ /^puddle/ { print $1 }' | sort -u
}

# The cache key of a crate: a hash of the committed trees of its closure and the shared inputs.
crate_key() {
    {
        for c in $(closure "$1"); do
            printf '%s %s\n' "$c" "$(git rev-parse "HEAD:crates/$c")"
        done
        for f in Cargo.lock rust-toolchain.toml .cargo/mutants.toml .config/nextest.toml scripts/mutants.sh; do
            printf '%s %s\n' "$f" "$(git rev-parse "HEAD:$f")"
        done
        "$cargo" mutants --version
    } | sha256sum | cut -d' ' -f1
}

report() {
    # Prints the survivors of the run in $1 as `file:line: what`.
    for f in missed.txt timeout.txt; do
        [ -s "$1/$f" ] || continue
        echo "--- $f ($1)" >&2
        cat "$1/$f" >&2
    done
}

mode=${1:-}
[ "$#" -eq 0 ] || shift
case $mode in
--list)
    crates
    ;;
--key)
    crate_key "$1"
    ;;
diff)
    base=${1:-}
    [ -n "$base" ] || base=$(git merge-base HEAD origin/develop)
    mkdir -p "$out"
    patch="$out/change.diff"
    : >"$patch"
    status=0
    args=
    for c in $(crates); do
        args="$args -p $c"
        git diff "$base" -- "crates/$c/" >>"$patch"
    done
    if ! grep -q '^@@' "$patch"; then
        echo "mutants: no change in the listed crates ($(crates | tr '\n' ' ')), nothing to mutate"
        exit 0
    fi
    # shellcheck disable=SC2086 # -p flags are words by design
    "$cargo" mutants $args --in-diff "$patch" -j "$jobs" --output "$out/diff" || status=$?
    report "$out/diff/mutants.out"
    exit "$status"
    ;;
sweep)
    shard='' only='' cache='' uncached=0
    while [ "$#" -gt 0 ]; do
        case $1 in
        --shard) shard=$2 && shift 2 ;;
        --crate) only=$2 && shift 2 ;;
        --cache) cache=$2 && shift 2 ;;
        --uncached) uncached=1 && shift ;;
        *) echo "mutants: unknown option $1" >&2 && exit 2 ;;
        esac
    done
    [ -n "$only" ] && todo=$only || todo=$(crates)
    status=0
    for c in $todo; do
        key=
        if [ -n "$cache" ] && [ -z "$(git status --porcelain --untracked-files=no)" ]; then
            key=$(crate_key "$c")
            if [ "$uncached" = 0 ] && [ -e "$cache/$c-$key.ok" ]; then
                echo "mutants: $c unchanged since a passing run ($key), skipped"
                continue
            fi
        fi
        shard_args=
        [ -n "$shard" ] && shard_args="--shard $shard"
        mkdir -p "$out/$c"
        rc=0
        # shellcheck disable=SC2086
        "$cargo" mutants -p "$c" -j "$jobs" $shard_args --output "$out/$c" || rc=$?
        report "$out/$c/mutants.out"
        if [ "$rc" -ne 0 ]; then
            status=$rc
            [ -n "$key" ] && [ -e "$cache/$c-$key.ok" ] &&
                echo "mutants: MISMATCH: $c passed with this cache key before and fails now (key $key)" >&2
        elif [ -n "$key" ] && [ -z "$shard" ]; then
            mkdir -p "$cache"
            : >"$cache/$c-$key.ok"
        fi
    done
    exit "$status"
    ;;
*)
    echo "usage: scripts/mutants.sh diff [<base>] | sweep [--shard K/N] [--crate C] [--cache DIR] [--uncached] | --list" >&2
    exit 2
    ;;
esac
