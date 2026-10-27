#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/quality-issue.sh with a stub `gh` (answers `api` from files, `issue list` from a variable, records
# its write calls and keeps the body file) and a stub scanner. Cases: no open issue opens one with the first report;
# an open issue is rewritten and its recorded lists give the delta (new and gone, a comment only when something is new);
# an unreadable Dependabot source is reported as unavailable and keeps its last list (nothing counted as new or gone);
# a scanner that fails without naming a site, and a failing issue list, touch nothing and fail. Not seen: the real `gh`,
# the GitHub API and the real scanner output beyond the three TSV columns the script reads.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/quality-issue-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "quality-issue-test: $*" >&2
    exit 1
}

cat >"$tmp/gh" <<'STUB'
#!/bin/sh
case "$1 $2" in
"api --paginate")
    case "$3" in
    */code-scanning/*) cat "$CODE_JSON" ;;
    */dependabot/*) [ -z "${DEP_FAIL:-}" ] || { echo "HTTP 403: Resource not accessible by integration" >&2; exit 1; }; cat "$DEP_JSON" ;;
    esac ;;
"issue list") [ -z "${LIST_FAIL:-}" ] || exit 1; printf '%s\n' "${OPEN_ISSUES:-[]}" ;;
"issue create") echo "$*" >>"$GH_LOG"; cp "$6" "$tmp_body" ;;
"issue edit") echo "$*" >>"$GH_LOG"; cp "$5" "$tmp_body" ;;
*) echo "$*" >>"$GH_LOG" ;;
esac
STUB
cat >"$tmp/scan" <<'STUB'
#!/bin/sh
[ -z "${SCAN_OUT:-}" ] || cat "$SCAN_OUT"
exit "${SCAN_STATUS:-0}"
STUB
chmod +x "$tmp/gh" "$tmp/scan"

cat >"$tmp/code1.json" <<'JSON'
[{"number":1,"html_url":"https://x/1","rule":{"id":"rust/hard-coded-cryptographic-value"},"most_recent_instance":{"location":{"path":"crates/a/tests/t.rs"}}},
 {"number":2,"html_url":"https://x/2","rule":{"id":"rust/cleartext-logging"},"most_recent_instance":{"location":{"path":"crates/b/tests/u.rs"}}}]
JSON
cat >"$tmp/code2.json" <<'JSON'
[{"number":2,"html_url":"https://x/2","rule":{"id":"rust/cleartext-logging"},"most_recent_instance":{"location":{"path":"crates/b/tests/u.rs"}}},
 {"number":3,"html_url":"https://x/3","rule":{"id":"rust/access-invalid-pointer"},"most_recent_instance":{"location":{"path":"crates/c/src/w.rs"}}}]
JSON
cat >"$tmp/dep.json" <<'JSON'
[{"number":5,"html_url":"https://x/d5","security_vulnerability":{"package":{"name":"tornado"}},"security_advisory":{"severity":"high","summary":"DoS in tornado"}}]
JSON
# shellcheck disable=SC2016 # backticks are literal scanner text
printf 'crates/x/src/lib.rs:10\tprod\tunsafe-fn: no `// SAFETY:` | unsafe fn f()\n' >"$tmp/scan1.tsv"
# shellcheck disable=SC2016 # backticks are literal scanner text
printf 'crates/x/src/lib.rs:14\tprod\tunsafe-fn: no `// SAFETY:` | unsafe fn f()\n' >"$tmp/scan2.tsv"
: >"$tmp/none.tsv"

# run <name> <expected exit> <code json> <scan tsv> <scan status> <open issues json> [dep fail]
run() {
    local name=$1 want=$2 code=$3 scan=$4 sstatus=$5 open=$6 depfail=${7:-} got=0
    : >"$tmp/gh.log"
    rm -f "$tmp/body.md"
    CODE_JSON="$tmp/$code" DEP_JSON="$tmp/dep.json" DEP_FAIL=$depfail SCAN_OUT="$tmp/$scan" SCAN_STATUS=$sstatus \
        OPEN_ISSUES=$open GH_LOG="$tmp/gh.log" tmp_body="$tmp/body.md" LIST_FAIL=${LIST_FAIL:-} \
        QUALITY_CHECK="$tmp/scan" QUALITY_GH="$tmp/gh" GITHUB_REPOSITORY=o/r GITHUB_RUN_ID=42 \
        "$here/quality-issue.sh" >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "$name: exit $got, wanted $want: $(cat "$tmp/out")"
}
# An open issue whose body carries the given state, as the issue list prints it.
open_with() { jq -nc --arg b "$1" '[{number: 7, title: "Code quality: open alerts and sites without a reason", body: $b}, {number: 9, title: "other", body: ""}]'; }

run "first report" 0 code1.json scan1.tsv 1 '[]'
grep -q '^issue create --title Code quality: open alerts and sites without a reason --body-file ' "$tmp/gh.log" || fail "no issue opened: $(cat "$tmp/gh.log")"
[ "$(wc -l <"$tmp/gh.log")" = 1 ] || fail "more than one write: $(cat "$tmp/gh.log")"
grep -q 'Open: \*\*2\*\* (first report' "$tmp/body.md" || fail "code scanning count missing: $(cat "$tmp/body.md")"
grep -q 'tornado (high): DoS in tornado' "$tmp/body.md" || fail "dependabot row missing"
grep -q 'unsafe-fn: no' "$tmp/body.md" || fail "missing-reason site not listed"
grep -q '^<!-- quality-state: {"day":"[0-9-]*","code":\["1","2"\],"dep":\["5"\],"missing":\["crates/x/src/lib.rs unsafe-fn' "$tmp/body.md" || fail "state comment off: $(tail -n 2 "$tmp/body.md")"
body1=$(cat "$tmp/body.md")
run "delta" 0 code2.json scan2.tsv 1 "$(open_with "$body1")"
grep -q '^issue edit 7 --body-file ' "$tmp/gh.log" || fail "open issue not rewritten: $(cat "$tmp/gh.log")"
grep -q 'issue edit 9' "$tmp/gh.log" && fail "another issue was edited"
grep -q '^issue comment 7 ' "$tmp/gh.log" || fail "no comment on a day with a new alert: $(cat "$tmp/gh.log")"
grep -q 'Open: \*\*2\*\* (1 new, 1 gone' "$tmp/body.md" || fail "code scanning delta wrong: $(cat "$tmp/body.md")"
grep -q 'access-invalid-pointer in crates/c/src/w.rs' "$tmp/body.md" || fail "the new alert is not listed"
grep -q 'hard-coded-cryptographic-value' "$tmp/body.md" && fail "a gone alert is listed as new"
grep -q 'Open: \*\*1\*\* (0 new, 0 gone' "$tmp/body.md" || fail "a site that only moved lines counts as new or gone"

run "dependabot unreadable keeps its list" 0 code2.json scan2.tsv 1 "$(open_with "$body1")" 1
grep -q 'Could not read this source today (HTTP 403' "$tmp/body.md" || fail "outage not stated"
grep -q '"dep":\["5"\]' "$tmp/body.md" || fail "the unreadable source lost its list"

run "nothing new, no comment" 0 code1.json scan1.tsv 1 "$(open_with "$body1")"
grep -q 'issue comment' "$tmp/gh.log" && fail "commented on a day with nothing new"
grep -q '^issue edit 7 ' "$tmp/gh.log" || fail "body not refreshed"

run "clean, no issue" 0 code1.json none.tsv 0 '[]'
grep -q 'Open: \*\*0\*\* (first report' "$tmp/body.md" || fail "an empty missing list is not reported"

run "scanner crash touches nothing" 1 code1.json none.tsv 1 "$(open_with "$body1")"
[ ! -s "$tmp/gh.log" ] || fail "a scanner crash wrote: $(cat "$tmp/gh.log")"

LIST_FAIL=1 run "issue list failure touches nothing" 1 code1.json scan1.tsv 1 '[]'
[ ! -s "$tmp/gh.log" ] || fail "a failed issue list wrote: $(cat "$tmp/gh.log")"


echo "quality-issue-test: ok"
