#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The nightly advisory check (ci.yml, jobs `advisories` and `advisories-npm`, on their schedule). Usage:
#   ci/advisories-issue.sh rust    runs `cargo deny check advisories` (RustSec)
#   ci/advisories-issue.sh npm     runs `node scripts/audit.ts --json` in ui/ (npm audit, minus ui/audit-exceptions.json)
# and keeps one tracking issue per source in step with the result, each with its own title. A failing check
# opens the issue, or rewrites its body when it is already open, and exits 1 so the run is red; a clean check
# closes the open issue with a comment. A run that fails without naming an advisory (no database, no network,
# a broken manifest) exits 1 and touches no issue: an outage must not close the issue of a real advisory.
#
# Environment: GH_TOKEN (issues: write), GITHUB_REPOSITORY, GITHUB_SERVER_URL and GITHUB_RUN_ID for the link;
# ADVISORIES_CHECK and ADVISORIES_GH replace the check command and `gh` in the self-test
# (ci/advisories-issue-test.sh).
set -uo pipefail

source=${1:-}
gh=${ADVISORIES_GH:-gh}
case "$source" in
rust)
    check=${ADVISORIES_CHECK:-cargo deny --locked --format json check advisories}
    title="Dependency advisories: cargo deny check advisories fails"
    ;;
npm)
    check=${ADVISORIES_CHECK:-node ui/scripts/audit.ts --json}
    title="Dependency advisories: npm audit fails"
    ;;
*)
    echo "usage: ci/advisories-issue.sh rust|npm" >&2
    exit 2
    ;;
esac
run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-unknown}/actions/runs/${GITHUB_RUN_ID:-0}"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/advisories-issue.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# Both sources leave one row per failing advisory in $tmp/rows.tsv: id, package, version, kind, title, fix hint.
# Ignored advisories (cargo-deny notes) and excepted ones (npm) are not errors and are not listed.
# shellcheck disable=SC2086 # $check is a command with arguments by design
if [ "$source" = rust ]; then
    $check 2>"$tmp/report.json" >/dev/null
    status=$?
    parse='select(.type == "diagnostic" and (.fields.severity == "error" or .fields.severity == "warning"))
        | [.fields.advisory.id // .fields.code, .fields.advisory.package // "?", .fields.graphs[0].Krate.version // "?",
           .fields.code, (.fields.advisory.title // .fields.message | gsub("[|\n]"; " ")),
           ((.fields.notes // []) | map(select(startswith("Solution:"))) | first // "")]
        | @tsv'
else
    $check >"$tmp/report.json" 2>"$tmp/check.err"
    status=$?
    cat "$tmp/check.err" >&2
    # Exit 0 clean, 1 findings; any other status (2: npm could not answer) has no rows and is an outage.
    parse='.[] | [.id, .name, "", .severity, (.title | gsub("[|\n]"; " ")), "update the package, or add an exception to ui/audit-exceptions.json"] | @tsv'
    [ "$status" -le 1 ] || : >"$tmp/report.json"
fi
if ! jq -r "$parse" "$tmp/report.json" >"$tmp/rows.tsv" 2>"$tmp/jq.err"; then
    echo "advisories-issue: cannot read the $source report: $(cat "$tmp/jq.err")" >&2
    cat "$tmp/report.json" >&2
    exit 1
fi

find_issue() {
    "$gh" issue list --state open --search "in:title \"$title\"" --json number,title --limit 20 |
        jq -r --arg t "$title" 'map(select(.title == $t)) | first | .number // empty'
}

if [ ! -s "$tmp/rows.tsv" ]; then
    if [ "$status" -ne 0 ]; then
        echo "advisories-issue: the $source check exited $status without an advisory; no issue touched" >&2
        cat "$tmp/report.json" >&2
        exit 1
    fi
    number=$(find_issue) || exit 1
    if [ -n "$number" ]; then
        "$gh" issue close "$number" --comment "No advisory fails the check in $run_url; closing." || exit 1
        echo "advisories-issue: clean; closed #$number"
    else
        echo "advisories-issue: clean; no open issue"
    fi
    exit 0
fi

if [ "$source" = rust ]; then
    what="\`cargo deny check advisories\` fails on the default branch."
    link="https://rustsec.org/advisories"
    job=advisories
    clear="To clear it: bump the crate (\`cargo update -p <crate>\`), or, when no fix exists or the code path is not reachable,
add the advisory to \`ignore\` in \`deny.toml\` with a reason and a review date."
else
    what="\`npm audit\` (production dependencies, moderate and above) fails on the default branch."
    link="https://github.com/advisories"
    job=advisories-npm
    clear="To clear it: update the package (\`npm audit fix\` in \`ui/\`, or bump the direct dependency), or, when no fix exists or
the code path is not reachable, add the advisory to \`ui/audit-exceptions.json\` with a reason and a review date."
fi
{
    echo "$what The nightly run ($run_url) lists:"
    echo
    echo "| Advisory | Package | Kind | Title | Fix |"
    echo "|---|---|---|---|---|"
    while IFS=$'\t' read -r id crate version kind adv_title solution; do
        echo "| [$id]($link/$id) | \`${crate}${version:+ $version}\` | $kind | $adv_title | ${solution:-none published} |"
    done <"$tmp/rows.tsv"
    echo
    echo "$clear"
    echo "Pull requests to \`develop\` do not run this check; pull requests to \`main\`, pushes to \`develop\` and the nightly do."
    echo "The nightly (job \`$job\` in \`ci.yml\`) rewrites this issue on every failing run and closes it on the first clean one."
} >"$tmp/body.md"

number=$(find_issue) || exit 1
if [ -n "$number" ]; then
    "$gh" issue edit "$number" --body-file "$tmp/body.md" || exit 1
    echo "advisories-issue: updated #$number"
else
    "$gh" issue create --title "$title" --body-file "$tmp/body.md" || exit 1
    echo "advisories-issue: opened a new issue"
fi
echo "advisories-issue: $(wc -l <"$tmp/rows.tsv") advisory(ies) fail the check; failing the run" >&2
exit 1
