#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Writes a timing file for ci/vm-partition.sh from nextest junit.xml files: per test the median of its passing seconds.
# A failed or skipped test has no self-closing <testcase/> line and is left out, so a bad run adds no wrong time.
#
# usage: ci/vm-timings-refresh.sh <out.tsv> <junit.xml>...
#        ci/vm-timings-refresh.sh --runs <linux|windows> <out.tsv> <repo> <run-id>...
#   --runs downloads the `vm-<os>-results-*` artifacts of the given vm-linux / vm-windows runs with `gh` (they keep
#   14 days) and reads every junit.xml in them. Use runs on a full group: a tier-skipped run has no artifacts.
#   Refresh when the partition notice names new tests, or when the slowest partition drifts from its siblings (the
#   job summary prints the expected seconds per partition beside the actual).
set -euo pipefail
export LC_ALL=C

if [ "${1:-}" = --runs ]; then
    os=${2:?os}; out=${3:?out}; repo=${4:?repo}; shift 4
    [ "$#" -gt 0 ] || { echo "vm-timings-refresh: give at least one run id" >&2; exit 2; }
    work=$(mktemp -d "${TMPDIR:-/tmp}/vm-timings.XXXXXX")
    trap 'rm -rf "$work"' EXIT
    for run in "$@"; do
        gh run download "$run" -R "$repo" -D "$work/$run" -p "vm-$os-results-*"
    done
    mapfile -t files < <(find "$work" -name junit.xml | sort)
    label="runs $*"
    exec "$0" --label "$label" "$out" "${files[@]}"
fi

label=""
if [ "${1:-}" = --label ]; then label=$2; shift 2; fi
out=${1:?out}; shift
[ "$#" -gt 0 ] || { echo "vm-timings-refresh: no junit.xml given" >&2; exit 2; }
for f in "$@"; do [ -s "$f" ] || { echo "vm-timings-refresh: $f is missing or empty" >&2; exit 2; }; done

body=$(awk '
    # <testcase name="n" classname="binary id" ... time="1.234"/>
    /<testcase / && /\/>[[:space:]]*$/ {
        name = $0; sub(/.*[ ]name="/, "", name); sub(/".*/, "", name)
        cls = $0; sub(/.*[ ]classname="/, "", cls); sub(/".*/, "", cls)
        t = $0; sub(/.*[ ]time="/, "", t); sub(/".*/, "", t)
        if (name == "" || cls == "" || t + 0 <= 0 || name ~ /[&<>]/ || cls ~ /[&<>]/) next
        key = cls "\t" name
        if (!(key in n)) order[++keys] = key
        v[key, ++n[key]] = t + 0
    }
    END {
        for (i = 1; i <= keys; i++) {
            k = order[i]; m = n[k]
            for (a = 2; a <= m; a++) { x = v[k, a]; b = a - 1; while (b >= 1 && v[k, b] > x) { v[k, b + 1] = v[k, b]; b-- } v[k, b + 1] = x }
            med = (m % 2) ? v[k, (m + 1) / 2] : (v[k, m / 2] + v[k, m / 2 + 1]) / 2
            printf "%s\t%.3f\n", k, med
        }
    }' "$@" | sort)
[ -n "$body" ] || { echo "vm-timings-refresh: no passing test in the given junit files" >&2; exit 1; }
{
    echo "# VM test seconds for ci/vm-partition.sh: <binary id> <test> <median seconds>, from ${label:-$# junit.xml files}."
    echo "# Written by ci/vm-timings-refresh.sh; a missing or stale file makes the partition jobs fall back to the hash split."
    printf '%s\n' "$body"
} >"$out"
echo "vm-timings-refresh: $(printf '%s\n' "$body" | wc -l | tr -d ' ') tests from $# files into $out"
