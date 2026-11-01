#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Mutation testing with cargo-mutants (docs/STANDARDS.md, "Mutation testing"). Usage:
#   scripts/mutants.sh diff [<base>]       the mutants on lines the change adds or changes, in the
#                                          listed crates only (the per-landing gate; `check.sh mutants`)
#   scripts/mutants.sh sweep [options]     every mutant of the listed crates (the nightly run)
#       --shard K/N      run shard K of N (cargo-mutants splits the mutant list; K counts from 0)
#       --crate NAME     only this crate (it need not be in the list: how a new crate is triaged)
#       --file PATH      with --crate: only this file, a path under the crate's directory (src/h2.rs)
#       --cache DIR      skip a crate whose inputs hashed the same as in a run that passed (below)
#       --uncached       ignore the cache and run everything (the weekly run that proves the cache)
#   scripts/mutants.sh --list              prints the listed crates
#   scripts/mutants.sh --units             prints the sweep's units, `CRATE` or `CRATE FILE` per line
#   scripts/mutants.sh --matrix            prints the sweep's units as the workflow's job matrix: one-line JSON, a list of
#                                          {crate, file, name} objects (needs jq; the workflow uses it as `matrix.include`)
#   scripts/mutants.sh --key CRATE [FILE]  prints the cache key of the crate (or of one file unit) (clean tree only)
# The crates are in scripts/mutants-crates.txt: a `CRATE` line lists the whole crate, `CRATE FILE` lines
# (FILE under the crate's directory, one per line) list only those files, so a big crate joins file by file
# while the rest is still being swept. A crate has either form, not both. Settings are in .cargo/mutants.toml. Results go to
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

# The list's entries, comments and blank lines dropped: `CRATE` or `CRATE FILE` per line.
entries() { sed 's/#.*//' "$list" | awk 'NF { print $1, $2 }'; }
crates() { entries | awk '!seen[$1]++ { print $1 }'; }
# The listed files of a crate under the crate's directory, empty when the whole crate is listed.
files_of() { entries | awk -v c="$1" '$1 == c && NF > 1 { print $2 }'; }
# The sweep's units: a whole crate, or one file of a crate that lists files.
units() { entries | awk '{ print }' | sed 's/ $//'; }
# A path as one word for file and directory names: src/h2.rs -> src-h2-rs.
slug() { printf '%s\n' "$1" | tr '/.' '--'; }

# Refuses a list that is malformed: more than two words on a line, or a crate listed both whole and by file.
check_list() {
    sed 's/#.*//' "$list" | awk 'NF > 2 { bad = 1; print "mutants: " $0 ": a line is CRATE or CRATE FILE" > "/dev/stderr" }
        NF == 1 { whole[$1] = 1 } NF == 2 { part[$1] = 1 }
        END { for (c in whole) if (c in part) { bad = 1; print "mutants: " c " is listed whole and by file" > "/dev/stderr" }
              exit bad }'
}

# The workspace crates a crate depends on, itself included, one per line.
closure() {
    "$cargo" tree -p "$1" -e normal,build,dev --prefix none --offline 2>/dev/null |
        awk '$1 ~ /^puddle/ { print $1 }' | sort -u
}

# The cache key of a crate, or of one file unit of it ($2): a hash of the committed trees of its closure and the shared inputs.
crate_key() {
    {
        for c in $(closure "$1"); do
            printf '%s %s\n' "$c" "$(git rev-parse "HEAD:crates/$c")"
        done
        # The files the run covers (the unit's one file, else every listed file): a unit's key does not
        # change when another file joins the list.
        if [ -n "${2:-}" ]; then echo "$2"; else files_of "$1"; fi
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
check_list || exit 2
case $mode in
--list)
    crates
    ;;
--units)
    units
    ;;
--matrix)
    units | jq -R 'split(" ") | {crate: .[0], file: (.[1] // ""), name: ((.[1] // "") | gsub("[/.]"; "-"))}' | jq -sc .
    ;;
--key)
    crate_key "$1" "${2:-}"
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
        paths=
        for f in $(files_of "$c"); do paths="$paths crates/$c/$f"; done
        # shellcheck disable=SC2086 # paths are words by design (no spaces in the list)
        git diff "$base" -- ${paths:-crates/$c/} >>"$patch"
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
    shard='' only='' onlyfile='' cache='' uncached=0
    while [ "$#" -gt 0 ]; do
        case $1 in
        --shard) shard=$2 && shift 2 ;;
        --crate) only=$2 && shift 2 ;;
        --file) onlyfile=$2 && shift 2 ;;
        --cache) cache=$2 && shift 2 ;;
        --uncached) uncached=1 && shift ;;
        *) echo "mutants: unknown option $1" >&2 && exit 2 ;;
        esac
    done
    [ -z "$onlyfile" ] || [ -n "$only" ] || { echo "mutants: --file needs --crate" >&2 && exit 2; }
    # One unit per line, "CRATE" or "CRATE FILE". --crate alone is the crate as listed (all its listed
    # files in one run, or the whole crate when it lists none); with --file it is that one file.
    if [ -n "$onlyfile" ]; then
        todo="$only $onlyfile"
    elif [ -n "$only" ]; then
        todo=$only
    else
        todo=$(units)
    fi
    status=0
    while read -r c f; do
        # The files this run covers, and the names its output and cache entry carry.
        if [ -n "$f" ]; then
            files=$f name=$c-$(slug "$f") dir=$out/$c/$(slug "$f")
        else
            files=$(files_of "$c") name=$c dir=$out/$c
        fi
        key=
        if [ -n "$cache" ] && [ -z "$(git status --porcelain --untracked-files=no)" ]; then
            key=$(crate_key "$c" "$f")
            if [ "$uncached" = 0 ] && [ -e "$cache/$name-$key.ok" ]; then
                echo "mutants: $name unchanged since a passing run ($key), skipped"
                continue
            fi
        fi
        shard_args=
        [ -n "$shard" ] && shard_args="--shard $shard"
        file_args=
        for x in $files; do file_args="$file_args -f crates/$c/$x"; done
        mkdir -p "$dir"
        rc=0
        # shellcheck disable=SC2086 # shard_args and file_args are words by design
        "$cargo" mutants -p "$c" -j "$jobs" $shard_args $file_args --output "$dir" </dev/null || rc=$?
        report "$dir/mutants.out"
        if [ "$rc" -ne 0 ]; then
            status=$rc
            [ -n "$key" ] && [ -e "$cache/$name-$key.ok" ] &&
                echo "mutants: MISMATCH: $name passed with this cache key before and fails now (key $key)" >&2
        elif [ -n "$key" ] && [ -z "$shard" ]; then
            mkdir -p "$cache"
            : >"$cache/$name-$key.ok"
        fi
    done <<EOF
$todo
EOF
    exit "$status"
    ;;
*)
    echo "usage: scripts/mutants.sh diff [<base>] | sweep [--shard K/N] [--crate C [--file F]] [--cache DIR] [--uncached] | --list | --units | --matrix" >&2
    exit 2
    ;;
esac
