#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Splits the VM tests over the partition jobs by measured test time instead of nextest's hash of the name.
# `--partition hash:i/n` has no timing mode, and one hash share held 42 tests (859 s of tests) where another held
# 31 (563 s), so the slowest partition set the length of the whole run. Here every test gets its measured seconds
# from a timing file (ci/vm-test-timings-<os>.tsv, refreshed by ci/vm-timings-refresh.sh), and the tests go, longest
# first, to the partition with the least time so far. Every partition job runs the same list through the same
# deterministic assignment, so the jobs agree without talking to each other. The tests and their order within a
# partition stay; only which partition runs a test changes.
#
# usage: ci/vm-partition.sh args <index> <count> <list> <timings> <filterset>
#        ci/vm-partition.sh check <count> <list> <timings>
#        ci/vm-partition.sh table <count> <list> <timings>
#   args   prints, one per line, the nextest arguments that select partition <index> (1-based) of <count> out of the
#          tests <filterset> matches: `-E` and a filterset naming this partition's tests, or, when the timing data
#          cannot be used, `--partition` `hash:<index>/<count>` `-E` <filterset> and a warning that says why.
#   check  exits 0 when the filtersets of all partitions, read back as text, name every listed test exactly once.
#   table  prints the expected seconds and test count per partition.
# <list>: `nextest list --message-format oneline` output, "<binary id> <test name>" per line (made by the build job).
# <timings>: tab-separated "<binary id> <test name> <seconds>"; lines starting with # are comments.
#
# Fallback to the hash split, each with a `::warning` and a line in $GITHUB_STEP_SUMMARY: no list, no or empty timing
# file, fewer than half the listed tests in it (stale: the tests moved on), a name the filterset cannot carry, or a
# failed self-check. A test the file does not list (a new one) is not stale data: it gets the median time of the
# listed ones and a `::notice` names it, so a refresh is visible before it matters.
# A merge-group run fails instead of falling back (an unread warning hid a fallback for five groups, 2026-11-06);
# set VM_PARTITION_STRICT=1 to get the same elsewhere, VM_PARTITION_STRICT=0 to turn it off in a merge group.
# ANSI colour codes in the list (a `cargo nextest list` run with CARGO_TERM_COLOR=always) are stripped before use
# and a `::notice` names the cause; the workflows also list with `--color never`.
set -uo pipefail
export LC_ALL=C

mode=${1:-}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

note() { # <level> <title> <text>: one workflow annotation and one summary line
    echo "::$1 title=vm partition::$2" >&2
    echo "- vm partition: $2" >>"$summary"
}

# assign <count> <list> <timings>: "<bin> <TAB> <binary id> <TAB> <test> <TAB> <seconds>" per test on stdout, and
# "#..." lines for what a reader should know ("#unknown", "#coverage"); exit 3 with "#reason ..." when the data is unusable.
assign() {
    awk -v count="$1" -F'\t' '
    function fail(why) { print "#reason " why; bad = 1; exit 3 }
    FILENAME == ARGV[1] {
        sub(/\r$/, "")
        if ($0 ~ /^#/ || NF < 3 || $3 + 0 <= 0) next
        w[$1 SUBSEP $2] = $3 + 0
        next
    }
    {
        sub(/\r$/, "")
        n = split($0, f, /[ \t]+/)
        if (n < 2) next
        key = f[1] SUBSEP f[2]
        if (key in seen) next
        seen[key] = 1
        if (f[1] !~ /^[A-Za-z0-9_:\/.-]+$/ || f[2] !~ /^[A-Za-z0-9_:]+$/) fail("a test name the filterset cannot carry: " f[1] " " f[2])
        total++; bin_of[total] = f[1]; name_of[total] = f[2]
    }
    END {
        if (bad) exit 3
        if (total == 0) fail("the test list is empty")
        known = 0
        for (i = 1; i <= total; i++) if ((bin_of[i] SUBSEP name_of[i]) in w) { kw[++known] = w[bin_of[i] SUBSEP name_of[i]] }
        if (known == 0) fail("the timing file lists none of the " total " tests")
        if (known * 2 < total) fail("the timing file lists " known " of " total " tests, under half (stale)")
        # median of the known times, for the tests that have none
        for (i = 2; i <= known; i++) { v = kw[i]; j = i - 1; while (j >= 1 && kw[j] > v) { kw[j + 1] = kw[j]; j-- } kw[j + 1] = v }
        med = (known % 2) ? kw[(known + 1) / 2] : (kw[known / 2] + kw[known / 2 + 1]) / 2
        unk = ""
        for (i = 1; i <= total; i++) {
            k = bin_of[i] SUBSEP name_of[i]
            if (k in w) wt[i] = w[k]; else { wt[i] = med; unk = unk " " bin_of[i] "::" name_of[i]; nunk++ }
            id[i] = i
        }
        if (nunk) print "#unknown " nunk " tests have no timing and count as " med " s each:" unk
        # longest first, ties by binary then name, so every job derives the same order
        for (i = 2; i <= total; i++) {
            v = id[i]; j = i - 1
            while (j >= 1 && (wt[id[j]] < wt[v] || (wt[id[j]] == wt[v] && (bin_of[id[j]] " " name_of[id[j]] > bin_of[v] " " name_of[v])))) { id[j + 1] = id[j]; j-- }
            id[j + 1] = v
        }
        for (b = 1; b <= count; b++) load[b] = 0
        for (i = 1; i <= total; i++) {
            t = id[i]; best = 1
            for (b = 2; b <= count; b++) if (load[b] < load[best]) best = b
            load[best] += wt[t]
            printf "%d\t%s\t%s\t%s\n", best, bin_of[t], name_of[t], wt[t]
        }
    }' "$3" "$2"
}

# partition_expr <bin> <assignment lines>: the filterset of one partition, tests grouped by binary.
partition_expr() {
    awk -F'\t' -v want="$1" '
    $1 == want {
        if (!($2 in grp)) { order[++nb] = $2; grp[$2] = "" }
        grp[$2] = grp[$2] (grp[$2] == "" ? "" : " | ") "test(=" $3 ")"
    }
    END {
        if (nb == 0) { print "none()"; exit }
        out = ""
        for (i = 1; i <= nb; i++) out = out (i > 1 ? " | " : "") "(binary_id(=" order[i] ") & (" grp[order[i]] "))"
        print out
    }'
}

# verify <count> <list> <assignment>: every listed test is named by exactly one partition expression, read back from its text.
verify() {
    local count=$1 list=$2 asg=$3 b
    for b in $(seq 1 "$count"); do
        printf '%s\n' "$asg" | partition_expr "$b" | awk -v b="$b" '{
            s = $0
            while (match(s, /binary_id\(=[^)]*\) & \(/)) {
                bin = substr(s, RSTART + 11, RLENGTH - 16); s = substr(s, RSTART + RLENGTH)
                end = index(s, "))"); body = substr(s, 1, end - 1); s = substr(s, end + 2)
                m = split(body, parts, / \| /)
                for (i = 1; i <= m; i++) { t = parts[i]; sub(/^test\(=/, "", t); sub(/\)$/, "", t); print bin " " t }
            }
        }'
    done | LC_ALL=C sort >"$TMPV.got"
    awk 'NF >= 2 && !seen[$1 SUBSEP $2]++ { print $1 " " $2 }' "$list" | LC_ALL=C sort >"$TMPV.want"
    cmp -s "$TMPV.got" "$TMPV.want"
}

strict() { # true when a fallback must fail the job
    case "${VM_PARTITION_STRICT:-}" in 1) return 0 ;; 0) return 1 ;; esac
    [ "${GITHUB_EVENT_NAME:-}" = merge_group ]
}

fallback() { # <why> <index> <count> <filterset>
    if strict; then
        note error "the timing split cannot be used and this run may not fall back to the hash split: $1"
        exit 1
    fi
    note warning "falling back to the hash split (hash:$2/$3): $1"
    printf '%s\n' --partition "hash:$2/$3" -E "$4"
}

TMPV=$(mktemp "${TMPDIR:-/tmp}/vm-partition.XXXXXX")
trap 'rm -f "$TMPV" "$TMPV.got" "$TMPV.want" "$TMPV.list"' EXIT

# clean_list <list>: sets $list to a copy without ANSI escape sequences, and says so when it had any.
clean_list() {
    esc=$(printf '\033')
    if grep -q "$esc" "$1" 2>/dev/null; then
        sed "s/$esc\\[[0-9;]*[A-Za-z]//g" "$1" >"$TMPV.list"
        list=$TMPV.list
        note notice "the test list had ANSI colour codes (list it with --color never); they were stripped"
    fi
}

case "$mode" in
args)
    index=${2:?index} count=${3:?count} list=${4:?list} timings=${5:?timings} filter=${6:?filterset}
    case "$index$count" in *[!0-9]*) echo "vm-partition: index and count must be numbers" >&2; exit 2 ;; esac
    if [ "$index" -lt 1 ] || [ "$index" -gt "$count" ]; then echo "vm-partition: index $index is not in 1..$count" >&2; exit 2; fi
    if [ ! -s "$list" ]; then fallback "no test list ($list)" "$index" "$count" "$filter"; exit 0; fi
    if [ ! -s "$timings" ]; then fallback "no timing file ($timings), refresh it with ci/vm-timings-refresh.sh" "$index" "$count" "$filter"; exit 0; fi
    clean_list "$list"
    asg=$(assign "$count" "$list" "$timings"); rc=$?
    if [ "$rc" -ne 0 ]; then
        fallback "$(printf '%s\n' "$asg" | sed -n 's/^#reason //p' | head -1)" "$index" "$count" "$filter"
        exit 0
    fi
    if ! verify "$count" "$list" "$(printf '%s\n' "$asg" | grep -v '^#')"; then
        fallback "the self-check found a test in no partition or in two" "$index" "$count" "$filter"
        exit 0
    fi
    printf '%s\n' "$asg" | sed -n 's/^#unknown /::notice title=vm partition::/p' >&2
    printf '%s\n' "$asg" | sed -n 's/^#unknown /- vm partition: /p' >>"$summary"
    data=$(printf '%s\n' "$asg" | grep -v '^#')
    {
        echo "- vm partition $index of $count by test time: $(printf '%s\n' "$data" | awk -F'\t' -v b="$index" '$1 == b { n++; s += $4 } END { printf "%d tests, %.0f s expected", n, s }')"
    } >>"$summary"
    printf '%s\n' -E "($filter) & ($(printf '%s\n' "$data" | partition_expr "$index"))"
    ;;
check | table)
    count=${2:?count} list=${3:?list} timings=${4:?timings}
    [ -s "$list" ] && clean_list "$list"
    asg=$(assign "$count" "$list" "$timings") || { printf '%s\n' "$asg" | sed -n 's/^#reason //p' >&2; exit 1; }
    data=$(printf '%s\n' "$asg" | grep -v '^#')
    if [ "$mode" = check ]; then
        verify "$count" "$list" "$data" || { echo "vm-partition: check failed: a test is in no partition or in two" >&2; exit 1; }
        echo "vm-partition: ok ($(printf '%s\n' "$data" | wc -l | tr -d ' ') tests in $count partitions, each once)"
    else
        printf '%s\n' "$data" | awk -F'\t' '{ n[$1]++; s[$1] += $4 } END { for (b in n) printf "%s\t%d\t%.0f\n", b, n[b], s[b] }' | sort -n
    fi
    ;;
*)
    echo "usage: ci/vm-partition.sh args|check|table ..." >&2
    exit 2
    ;;
esac
