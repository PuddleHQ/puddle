#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The daily quality report (.github/workflows/quality.yml). Usage: ci/quality-issue.sh
# Reads three sources, compares each with what the open tracking issue recorded the day before, and keeps that one
# issue in step: it opens it when none is open and rewrites its body otherwise. The body lists, per source, how many
# are open now, which are new since the last report and which are gone. A comment is added only on a day something is
# new, so the issue notifies on a rise and stays quiet otherwise. The issue is never closed: it is the standing
# report, and the run stays green (the gate on pull requests, `scripts/check.sh quality`, is what fails).
#   code scanning   open alerts (CodeQL and any other uploaded tool) on the default branch
#   Dependabot      open alerts
#   missing reason  sites `scripts/quality-gate.py --missing` lists: no SAFETY comment, reason, owner on a TODO
# A source that cannot be read (the default `GITHUB_TOKEN` has no Dependabot alerts permission; set the repository
# secret QUALITY_ALERTS_TOKEN to a token that has it) is reported as unavailable and keeps its previous list, so the
# next day's delta is not a flood. When the scanner fails without naming a site, or the issue cannot be read or
# written, the script exits 1 and touches nothing.
#
# Environment: GH_TOKEN, QUALITY_ALERTS_TOKEN (optional, used for the two alert reads only), GITHUB_REPOSITORY, GITHUB_SERVER_URL and GITHUB_RUN_ID (for the link);
# QUALITY_CHECK and QUALITY_GH replace the scanner command and `gh` in the self-test (ci/quality-issue-test.sh).
set -uo pipefail

gh=${QUALITY_GH:-gh}
check=${QUALITY_CHECK:-python3 -I scripts/quality-gate.py --missing .}
repo=${GITHUB_REPOSITORY:-unknown/unknown}
title="Code quality: open alerts and sites without a reason"
run_url="${GITHUB_SERVER_URL:-https://github.com}/$repo/actions/runs/${GITHUB_RUN_ID:-0}"
today=$(date -u +%F)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/quality-issue.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# Each source leaves $tmp/<name>.tsv (id, label, link) and $tmp/<name>.state: "ok" or "unavailable: <why>".
fetch_alerts() { # <name> <api path> <jq row filter>
    local name=$1 path=$2 filter=$3
    if env GH_TOKEN="${QUALITY_ALERTS_TOKEN:-${GH_TOKEN:-}}" "$gh" api --paginate "repos/$repo/$path?state=open&per_page=100" >"$tmp/$name.json" 2>"$tmp/$name.err" &&
        jq -s 'add // []' "$tmp/$name.json" >"$tmp/$name.all" 2>>"$tmp/$name.err" &&
        jq -r "$filter" "$tmp/$name.all" >"$tmp/$name.tsv" 2>>"$tmp/$name.err"; then
        echo ok >"$tmp/$name.state"
    else
        : >"$tmp/$name.tsv"
        echo "unavailable: $(head -n 1 "$tmp/$name.err" | cut -c1-200)" >"$tmp/$name.state"
    fi
}
fetch_alerts code code-scanning/alerts '.[] | [(.number|tostring), (.rule.id + " in " + (.most_recent_instance.location.path // "?")), .html_url] | @tsv'
fetch_alerts dep dependabot/alerts '.[] | [(.number|tostring), (.security_vulnerability.package.name + " (" + .security_advisory.severity + "): " + (.security_advisory.summary | gsub("[|\n]"; " "))), .html_url] | @tsv'

# shellcheck disable=SC2086 # $check is a command with arguments by design
$check >"$tmp/missing.out" 2>"$tmp/missing.err"
status=$?
if [ "$status" -ne 0 ] && [ ! -s "$tmp/missing.out" ]; then
    echo "quality-issue: the scanner exited $status without naming a site; no issue touched" >&2
    cat "$tmp/missing.err" >&2
    exit 1
fi
# Sites move when a line is added above them, so a site's identity is its file and its text, not its line.
awk -F'\t' '{ f=$1; sub(/:[0-9]+$/, "", f); print f " " $3 "\t" $1 ": " $3 }' "$tmp/missing.out" |
    sort -u >"$tmp/missing.tsv"
echo ok >"$tmp/missing.state"

find_issue() {
    "$gh" issue list --state open --search "in:title \"$title\"" --json number,title,body --limit 20 |
        jq -c --arg t "$title" 'map(select(.title == $t)) | first // empty'
}
issue=$(find_issue) || {
    echo "quality-issue: cannot list issues" >&2
    exit 1
}
number=$(jq -r '.number // empty' <<<"$issue")
# The previous report's lists ride in the body as one hidden comment: {"code":[ids],"dep":[ids],"missing":[ids]}.
prev=$(jq -r '.body // ""' <<<"$issue" | sed -n 's/^<!-- quality-state: \(.*\) -->$/\1/p' | head -n 1)
jq -e . <<<"$prev" >/dev/null 2>&1 || prev='{}'

ids() { cut -f1 "$tmp/$1.tsv" | jq -R . | jq -sc .; }
now_code=$(ids code)
now_dep=$(ids dep)
now_missing=$(ids missing)
# An unreadable source keeps what the last report knew.
for s in code dep missing; do
    case $(cat "$tmp/$s.state") in
    ok) ;;
    *) eval "now_$s=\$(jq -c --arg s $s '.[\$s] // []' <<<\"\$prev\")" ;;
    esac
done
state=$(jq -nc --argjson c "$now_code" --argjson d "$now_dep" --argjson m "$now_missing" --arg day "$today" \
    '{day: $day, code: $c, dep: $d, missing: $m}')

new_total=0
section() { # <key> <heading>
    local key=$1 heading=$2 st
    st=$(cat "$tmp/$key.state")
    local now_var=now_$key
    local now=${!now_var}
    local old
    old=$(jq -c --arg k "$key" '.[$k] // null' <<<"$prev")
    echo "### $heading"
    echo
    if [ "$st" != ok ]; then
        echo "Could not read this source today (${st#unavailable: }); the count is from the last report and no list is shown."
        echo
    fi
    local count
    count=$(jq 'length' <<<"$now")
    local added gone listed=
    if [ "$old" = null ]; then
        added=$now
        echo "Open: **$count** (first report; no earlier list to compare with)."
        listed="Open now (first 40):"
    else
        added=$(jq -c --argjson o "$old" 'map(select(. as $x | $o | index($x) | not))' <<<"$now")
        gone=$(jq -c --argjson n "$now" 'map(select(. as $x | $n | index($x) | not))' <<<"$old")
        echo "Open: **$count** ($(jq 'length' <<<"$added") new, $(jq 'length' <<<"$gone") gone since the last report on $(jq -r '.day // "an unknown day"' <<<"$prev"))."
        new_total=$((new_total + $(jq 'length' <<<"$added")))
        listed="New:"
    fi
    if [ "$(jq 'length' <<<"$added")" -gt 0 ]; then
        echo
        echo "$listed"
        echo
        while IFS=$'\t' read -r id label link; do
            jq -e --arg i "$id" 'index($i)' <<<"$added" >/dev/null || continue
            echo "- ${link:+[}$label${link:+]($link)}"
        done <"$tmp/$key.tsv" | head -n 40
    fi
    echo
}
{
    echo "Daily report ($today, $run_url) on \`develop\`. Counts are what is open now; the lists say what changed since the last report."
    echo
    section code "Code scanning alerts"
    section dep "Dependabot alerts"
    section missing "Sites without a written reason"
    echo "A site without a reason also fails \`scripts/check.sh quality\` on every pull request, so a rise there means a change got in some other way (a direct push, or the gate was skipped). Alerts are triaged by the daily reviewer; a rise in code scanning or Dependabot is only reported here."
    echo "The workflow \`quality.yml\` rewrites this body each day and never closes the issue."
    echo
    echo "<!-- quality-state: $state -->"
} >"$tmp/body.md"

if [ -n "$number" ]; then
    "$gh" issue edit "$number" --body-file "$tmp/body.md" || exit 1
    if [ "$new_total" -gt 0 ]; then
        "$gh" issue comment "$number" --body "$new_total new since the last report: see the issue body. Run: $run_url" || exit 1
    fi
    echo "quality-issue: updated #$number ($new_total new)"
else
    "$gh" issue create --title "$title" --body-file "$tmp/body.md" || exit 1
    echo "quality-issue: opened a new issue"
fi
