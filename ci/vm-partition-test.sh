#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/vm-partition.sh, ci/vm-timings-refresh.sh and ci/vm-filter.sh on small fixtures: the split is balanced
# by time, deterministic and independent of the list's order, names every test exactly once, treats new tests as the
# median, and falls back to the hash split (with a warning) on a missing list, a missing or stale timing file and a
# name the filterset cannot carry; reads a list with ANSI colour codes; fails a merge-group run instead of falling back; the refresh takes the median of passing tests only. Prints ok only when every case passes.
set -uo pipefail

# a merge-group or strict environment must not change the cases below: each case sets what it needs
unset GITHUB_EVENT_NAME VM_PARTITION_STRICT GITHUB_STEP_SUMMARY
here=$(cd "$(dirname "$0")" && pwd)
part="$here/vm-partition.sh"
refresh="$here/vm-timings-refresh.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/vm-partition-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "vm-partition-test: FAIL: $*" >&2; fail=1; }
sum="$tmp/summary"

# 9 tests in 2 binaries: 100 80 70 60 50 40 30 20 10 s, total 460, so 3 partitions of about 153 s
{
    # <binary id> <test> <seconds>
    for row in 'crate-a::vm_one case_1 100' 'crate-a::vm_one case_2 80' 'crate-a::vm_one case_3 70' \
        'crate-b::vm_two case_4 60' 'crate-b::vm_two case_5 50' 'crate-b::vm_two case_6 40' \
        'crate-b::vm_two case_7 30' 'crate-b::vm_two case_8 20' 'crate-b::vm_two case_9 10'; do
        # shellcheck disable=SC2086 # the row is three words on purpose
        printf '%s\t%s\t%s\n' $row
    done
    echo '# a comment'
} >"$tmp/timings"
awk -F'\t' 'NF == 3 { print $1 " " $2 }' "$tmp/timings" >"$tmp/list"
FILTER='not test(/soak/)'

# 1. balanced: 3 partitions within the longest test of each other, here at 150 to 160 s
out=$("$part" table 3 "$tmp/list" "$tmp/timings") || bad "table failed"
max=$(printf '%s\n' "$out" | awk -F'\t' 'BEGIN { m = 0 } $3 > m { m = $3 } END { print m }')
min=$(printf '%s\n' "$out" | awk -F'\t' 'BEGIN { m = 1e9 } $3 < m { m = $3 } END { print m }')
[ "$(printf '%s\n' "$out" | wc -l | tr -d ' ')" = 3 ] || bad "a partition got no test: $out"
[ $((max - min)) -le 10 ] || bad "unbalanced: max $max min $min ($out)"

# 2. every test once, read back from the filtersets
"$part" check 3 "$tmp/list" "$tmp/timings" >/dev/null || bad "check failed on a good fixture"

# 3. deterministic, and the same for a shuffled list
for i in 1 2 3; do GITHUB_STEP_SUMMARY=$sum "$part" args "$i" 3 "$tmp/list" "$tmp/timings" "$FILTER" >"$tmp/a$i" 2>/dev/null; done
sort -r "$tmp/list" >"$tmp/list.rev"
for i in 1 2 3; do GITHUB_STEP_SUMMARY=$sum "$part" args "$i" 3 "$tmp/list.rev" "$tmp/timings" "$FILTER" >"$tmp/b$i" 2>/dev/null; cmp -s "$tmp/a$i" "$tmp/b$i" || bad "partition $i differs for a shuffled list"; done
[ "$(sed -n 1p "$tmp/a1")" = "-E" ] || bad "args do not start with -E: $(cat "$tmp/a1")"
grep -q "^(not test(/soak/)) & (" "$tmp/a1" || bad "the run's filterset is not part of the expression"
# the three expressions together name each test exactly once
all=$(cat "$tmp/a1" "$tmp/a2" "$tmp/a3" | grep -o 'test(=case_[0-9])' | sort | tr '\n' ' ')
[ "$all" = "test(=case_1) test(=case_2) test(=case_3) test(=case_4) test(=case_5) test(=case_6) test(=case_7) test(=case_8) test(=case_9) " ] || bad "tests named: $all"

# 4. a new test (not in the timing file) counts as the median and shows as a notice
echo 'crate-b::vm_two case_10' >>"$tmp/list"
: >"$sum"
"$part" args 1 3 "$tmp/list" "$tmp/timings" "$FILTER" >/dev/null 2>"$tmp/err" || bad "args failed with a new test"
grep -q '^::notice title=vm partition::1 tests have no timing and count as 50 s each: crate-b::vm_two::case_10' "$tmp/err" || bad "no notice for the new test: $(cat "$tmp/err")"
"$part" check 3 "$tmp/list" "$tmp/timings" >/dev/null || bad "check failed with a new test"
sed -i '$d' "$tmp/list"

# 5. fallbacks: the hash split, a warning on stderr and a line in the summary
expect_hash() { # <label> <list> <timings> <why>
    : >"$sum"
    GITHUB_STEP_SUMMARY=$sum "$part" args 2 3 "$2" "$3" "$FILTER" >"$tmp/fb" 2>"$tmp/err"
    [ "$(tr '\n' ' ' <"$tmp/fb")" = "--partition hash:2/3 -E $FILTER " ] || bad "$1: not the hash split: $(cat "$tmp/fb")"
    grep -q "^::warning title=vm partition::falling back to the hash split (hash:2/3): .*$4" "$tmp/err" || bad "$1: no warning naming '$4': $(cat "$tmp/err")"
    grep -q "falling back to the hash split" "$sum" || bad "$1: no summary line"
}
expect_hash "missing timing file" "$tmp/list" "$tmp/none.tsv" "no timing file"
: >"$tmp/empty"; expect_hash "empty timing file" "$tmp/list" "$tmp/empty" "no timing file"
expect_hash "missing list" "$tmp/none.list" "$tmp/timings" "no test list"
grep -v 'case_[1-5]	' "$tmp/timings" >"$tmp/stale"          # 4 of 9 tests are timed
expect_hash "stale timing file" "$tmp/list" "$tmp/stale" "under half"
printf 'crate-a::vm_one case_1\ncrate-a::vm_one bad)name\n' >"$tmp/oddlist"
expect_hash "unsafe name" "$tmp/oddlist" "$tmp/timings" "cannot carry"

# 5b. a list coloured by CARGO_TERM_COLOR=always (the 2026-11-06 fallback) is read like the plain one, with a notice naming the cause
esc=$(printf '\033')
awk -v e="$esc" '{ printf "%s[35m%s%s[0m %s[36m%s%s[0m\n", e, $1, e, e, $2, e }' "$tmp/list" >"$tmp/colour.list"
grep -q "$esc" "$tmp/colour.list" || bad "the coloured fixture has no escape code"
: >"$sum"
GITHUB_STEP_SUMMARY=$sum "$part" args 1 3 "$tmp/colour.list" "$tmp/timings" "$FILTER" >"$tmp/c1" 2>"$tmp/err" || bad "args failed on a coloured list"
cmp -s "$tmp/a1" "$tmp/c1" || bad "a coloured list gives another partition than the plain one: $(cat "$tmp/c1")"
grep -q '^::notice title=vm partition::the test list had ANSI colour codes' "$tmp/err" || bad "no notice naming the colour codes: $(cat "$tmp/err")"
grep -q 'falling back' "$tmp/err" && bad "a coloured list still falls back"
"$part" check 3 "$tmp/colour.list" "$tmp/timings" >/dev/null 2>&1 || bad "check failed on a coloured list"

# 5c. a merge-group run fails instead of falling back; so does VM_PARTITION_STRICT=1; VM_PARTITION_STRICT=0 turns it off
GITHUB_EVENT_NAME=merge_group GITHUB_STEP_SUMMARY=$sum "$part" args 2 3 "$tmp/list" "$tmp/none.tsv" "$FILTER" >"$tmp/fb" 2>"$tmp/err"
[ $? = 1 ] || bad "a merge-group fallback did not fail"
[ ! -s "$tmp/fb" ] || bad "a failed merge-group fallback still printed arguments: $(cat "$tmp/fb")"
grep -q '^::error title=vm partition::.*no timing file' "$tmp/err" || bad "no error naming the cause: $(cat "$tmp/err")"
VM_PARTITION_STRICT=1 "$part" args 2 3 "$tmp/none.list" "$tmp/timings" "$FILTER" >/dev/null 2>&1; [ $? = 1 ] || bad "VM_PARTITION_STRICT=1 did not fail on a fallback"
GITHUB_EVENT_NAME=merge_group VM_PARTITION_STRICT=0 "$part" args 2 3 "$tmp/list" "$tmp/none.tsv" "$FILTER" >/dev/null 2>&1 || bad "VM_PARTITION_STRICT=0 did not allow the fallback"
GITHUB_EVENT_NAME=merge_group "$part" args 1 3 "$tmp/list" "$tmp/timings" "$FILTER" >/dev/null 2>&1 || bad "a merge group failed with good data"

# 6. more partitions than tests: an empty one matches nothing
printf 'crate-a::vm_one case_1\n' >"$tmp/one"
grep -q 'none()' <<<"$("$part" args 2 2 "$tmp/one" "$tmp/timings" "$FILTER" 2>/dev/null)" || bad "an empty partition does not select nothing"
"$part" args 4 3 "$tmp/list" "$tmp/timings" "$FILTER" >/dev/null 2>&1; [ $? = 2 ] || bad "index over count is not a usage error"

# 7. the refresh: median of passing seconds, a failed test and an escaped name left out
mk() { # <file> <seconds of case_1> <seconds of case_2>
    cat >"$1" <<XML
<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run">
    <testsuite name="crate-a::vm_one" tests="3">
        <testcase name="case_1" classname="crate-a::vm_one" timestamp="x" time="$2"/>
        <testcase name="case_2" classname="crate-a::vm_one" timestamp="x" time="$3">
            <failure message="boom"/>
        </testcase>
        <testcase name="case&amp;3" classname="crate-a::vm_one" timestamp="x" time="9.000"/>
    </testsuite>
</testsuites>
XML
}
mk "$tmp/j1.xml" 10.000 99.000; mk "$tmp/j2.xml" 30.000 99.000; mk "$tmp/j3.xml" 20.000 99.000
"$refresh" "$tmp/out.tsv" "$tmp/j1.xml" "$tmp/j2.xml" "$tmp/j3.xml" >/dev/null || bad "refresh failed"
[ "$(grep -v '^#' "$tmp/out.tsv" | tr '\t' ' ')" = "crate-a::vm_one case_1 20.000" ] || bad "refresh wrote: $(grep -v '^#' "$tmp/out.tsv")"
"$refresh" "$tmp/out2.tsv" "$tmp/none.xml" >/dev/null 2>&1; [ $? = 2 ] || bad "refresh accepted a missing junit file"

# 8. the filterset: the soak and the performance bars run on the nightly and on a dispatch only
for ev in pull_request merge_group push ""; do grep -q vm_soak_ <<<"$(GITHUB_EVENT_NAME=$ev "$here/vm-filter.sh")" || bad "soak is not excluded on '$ev'"; done
for ev in schedule workflow_dispatch; do grep -q vm_soak_ <<<"$(GITHUB_EVENT_NAME=$ev "$here/vm-filter.sh")" && bad "soak is excluded on $ev"; done
# The desktop VS Code tests are in no partition on any event, and the job that runs them selects exactly them.
for ev in schedule workflow_dispatch pull_request merge_group push ""; do grep -q 'not (.*vm_vscode_' <<<"$(GITHUB_EVENT_NAME=$ev "$here/vm-filter.sh")" || bad "vscode tests are not excluded on '$ev'"; done
[ "$("$here/vm-filter.sh" vscode)" = 'test(/(^|::)vm_vscode_/)' ] || bad "the vscode job's filter is not just the vscode tests"
for ev in pull_request merge_group push ""; do grep -q vm_perf_ <<<"$(GITHUB_EVENT_NAME=$ev "$here/vm-filter.sh")" || bad "perf bars are not excluded on '$ev'"; done
for ev in schedule workflow_dispatch; do grep -q vm_perf_ <<<"$(GITHUB_EVENT_NAME=$ev "$here/vm-filter.sh")" && bad "perf bars are excluded on $ev"; done

# 9. the committed timing files are usable: every line parses and the file is not empty
for os in linux windows; do
    f="$here/vm-test-timings-$os.tsv"
    [ -s "$f" ] || { bad "$f is missing"; continue; }
    badlines=$(grep -v '^#' "$f" | awk -F'\t' 'NF != 3 || $3 + 0 <= 0' | wc -l | tr -d ' ')
    [ "$badlines" = 0 ] || bad "$f has $badlines unparsable lines"
done

# 10. the Linux build job archives only the `vm_*` integration test binaries (`--test 'vm_*'`, the bundle is 3 times the size
# of the Windows one otherwise). A VM test anywhere else would be left out of the archive without a word, so none may exist:
# a test function or module named `vm_*` goes in `crates/<crate>/tests/vm_<name>.rs` (or a `tests/vm_<name>/main.rs`)
root=$(cd "$here/.." && pwd)
grep -q "nextest archive .* --test 'vm_\*'" "$root/.github/workflows/vm-linux.yml" || bad "vm-linux.yml does not archive only --test 'vm_*'"
stray=$(cd "$root" && grep -rnE '^[[:space:]]*(pub(\([a-z]+\))? )?(async )?(fn|mod) vm_' crates --include='*.rs' | grep -Ev '^crates/[^/]+/tests/vm_' || true)
[ -z "$stray" ] || bad "a vm_* test outside a tests/vm_* file would not be in the Linux archive; move it into crates/<crate>/tests/vm_<name>.rs: $stray"

[ "$fail" = 0 ] && echo ok
exit "$fail"
