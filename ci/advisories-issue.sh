#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The nightly advisory check (ci.yml, job `advisories`, on its schedule): runs `cargo deny check advisories`
# and keeps one tracking issue in step with the result. A failing check opens the issue, or rewrites its body
# when it is already open, and exits 1 so the run is red; a clean check closes the open issue with a comment.
# A run that fails without naming an advisory (no database, no network, a broken manifest) exits 1 and
# touches no issue: an outage must not close the issue of a real advisory.
#
# Environment: GH_TOKEN (issues: write), GITHUB_REPOSITORY, GITHUB_SERVER_URL and GITHUB_RUN_ID for the link;
# ADVISORIES_DENY and ADVISORIES_GH replace `cargo deny` and `gh` in the self-test (ci/advisories-issue-test.sh).
set -uo pipefail

deny=${ADVISORIES_DENY:-cargo deny}
gh=${ADVISORIES_GH:-gh}
title="Dependency advisories: cargo deny check advisories fails"
run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-unknown}/actions/runs/${GITHUB_RUN_ID:-0}"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/advisories-issue.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# shellcheck disable=SC2086 # $deny is a command with arguments by design
$deny --locked --format json check advisories 2>"$tmp/report.json" >/dev/null
status=$?

# One row per advisory that fails the check: id, crate, version, kind, title, the tool's fix hint. Ignored
# advisories are notes, not errors, and are not listed.
if ! jq -r 'select(.type == "diagnostic" and (.fields.severity == "error" or .fields.severity == "warning"))
        | [.fields.advisory.id // .fields.code, .fields.advisory.package // "?", .fields.graphs[0].Krate.version // "?",
           .fields.code, (.fields.advisory.title // .fields.message | gsub("[|\n]"; " ")),
           ((.fields.notes // []) | map(select(startswith("Solution:"))) | first // "")]
        | @tsv' "$tmp/report.json" >"$tmp/rows.tsv" 2>"$tmp/jq.err"; then
    echo "advisories-issue: cannot read the cargo-deny report: $(cat "$tmp/jq.err")" >&2
    cat "$tmp/report.json" >&2
    exit 1
fi

find_issue() {
    "$gh" issue list --state open --search "in:title \"$title\"" --json number,title --limit 20 |
        jq -r --arg t "$title" 'map(select(.title == $t)) | first | .number // empty'
}

if [ ! -s "$tmp/rows.tsv" ]; then
    if [ "$status" -ne 0 ]; then
        echo "advisories-issue: cargo deny exited $status without an advisory; no issue touched" >&2
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

{
    echo "\`cargo deny check advisories\` fails on the default branch. The nightly run ($run_url) lists:"
    echo
    echo "| Advisory | Crate | Kind | Title | Fix |"
    echo "|---|---|---|---|---|"
    while IFS=$'\t' read -r id crate version kind adv_title solution; do
        echo "| [$id](https://rustsec.org/advisories/$id) | \`$crate $version\` | $kind | $adv_title | ${solution:-none published} |"
    done <"$tmp/rows.tsv"
    echo
    echo "To clear it: bump the crate (\`cargo update -p <crate>\`), or, when no fix exists or the code path is not reachable,"
    echo "add the advisory to \`ignore\` in \`deny.toml\` with a reason and a review date."
    echo "Pull requests to \`develop\` do not run this check; pull requests to \`main\`, pushes to \`develop\` and the nightly do."
    echo "The nightly (job \`advisories\` in \`ci.yml\`) rewrites this issue on every failing run and closes it on the first clean one."
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
