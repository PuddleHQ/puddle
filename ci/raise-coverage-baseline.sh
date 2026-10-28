#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The nightly coverage-baseline workflow: raises scripts/coverage-baseline.json from the coverage CI itself measured on
# develop, in ONE pull request, `build: raise the coverage baseline`, that goes through the merge queue like any other
# (required checks, no bypass). Pull requests do not carry a raise of their own; this is the only way the floor moves up.
#   1. Run from a checkout of develop. The measurement is the `coverage` gate's own: the `ci` workflow uploads
#      `coverage-summary.json` in its `linux-test-results` artifact, and the newest green run on develop's tip is used, or on
#      an older commit when every file changed since is documentation (ci/scope.sh classifies the file list). Never a
#      measurement of another commit: a raise above what the tip reaches would fail the next pull request.
#   2. The ratchet (ui/scripts/coverage-ratchet.ts --write, the floors from scripts/check.sh) rewrites the baseline: per field
#      the higher of the committed value and the measured one, rounded down to 0.1.
#   3. Nothing to raise: exit 0, and an open raise pull request that develop's baseline has caught up with is closed.
#      An open one that already carries exactly this file: nothing to do. Otherwise the branch ci/raise-coverage-baseline is
#      rebuilt on the checkout as one commit and pushed with a lease, and the pull request is opened (or left open) with
#      auto-merge armed.
# The token decides whether the pull request's checks run: a pull request opened or updated with the default GITHUB_TOKEN does
# not trigger them (GitHub's rule), so the workflow uses the repository secret COVERAGE_BASELINE_TOKEN when it is set (see the
# workflow). When GitHub refuses to open the pull request (the setting "Allow GitHub Actions to create and approve pull
# requests" is off), the branch stays pushed, a warning says how to open it by hand, and the exit is 0.
# DRY_RUN=1 reports what would happen and changes nothing remote. Needs GH_TOKEN, GITHUB_REPOSITORY, `actions: read` and,
# to push and open, `contents: write` and `pull-requests: write`. GH overrides the gh binary. Self-test: ci/raise-coverage-baseline-test.sh.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
gh=${GH:-gh}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
bl=scripts/coverage-baseline.json
branch=ci/raise-coverage-baseline
title="build: raise the coverage baseline"
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
work=$(mktemp -d "${TMPDIR:-/tmp}/raise-coverage-baseline.XXXXXX")
trap 'rm -rf "$work"' EXIT
say() { echo "$*" | tee -a "$summary"; }
fields() { jq -r '"lines \(.lines), regions \(.regions)"'; }
docs_only() { # docs_only <older commit>: every file changed from it to HEAD is documentation
	local out
	out=$(git diff --name-only "$1" HEAD | "$here/scope.sh" classify)
	grep -qx 'ci_rust=false' <<<"$out" && grep -qx 'ui=false' <<<"$out"
}

# 1. The measurement.
tip=$(git rev-parse HEAD)
cands=("$tip")
while read -r c; do
	docs_only "$c" || break
	cands+=("$c")
done < <(git rev-list --first-parent --skip=1 --max-count=15 HEAD)
measured=''
file=''
for c in "${cands[@]}"; do
	while read -r id; do
		[ -n "$id" ] || continue
		rm -rf "$work/dl"
		if "$gh" run download -R "$repo" "$id" -n linux-test-results -D "$work/dl" >/dev/null 2>&1; then
			file=$(find "$work/dl" -name coverage-summary.json -print -quit)
			[ -z "$file" ] || { measured=$c; break 2; }
		fi
	done < <("$gh" run list -R "$repo" --workflow ci.yml --status success --commit "$c" --json databaseId --jq '.[].databaseId')
done
if [ -z "$measured" ]; then
	say "Coverage baseline: no green ci run with a coverage summary on develop's tip ${tip:0:10} (or on an older commit with documentation-only changes since); nothing raised tonight."
	exit 0
fi

# 2. The ratchet.
cov_lines=$(sed -n 's/^COV_LINES=//p' scripts/check.sh)
cov_regions=$(sed -n 's/^COV_REGIONS=//p' scripts/check.sh)
[ -n "$cov_lines" ] && [ -n "$cov_regions" ] || { echo "cannot read the floors from scripts/check.sh" >&2; exit 2; }
old=$(git show "HEAD:$bl" | fields)
node "$here/../ui/scripts/coverage-ratchet.ts" --summary "$file" --baseline "$bl" \
	--floor-lines "$cov_lines" --floor-regions "$cov_regions" --write
new=$(fields <"$bl")
pr=$("$gh" pr list -R "$repo" --head "$branch" --base develop --state open --json number --jq '.[0].number // empty')

# 3. Nothing, the same, or a new raise.
if git diff --quiet -- "$bl"; then
	say "Coverage baseline: nothing to raise ($old; measured on ${measured:0:10})."
	if [ -n "$pr" ] && [ "${DRY_RUN:-}" != 1 ]; then
		"$gh" pr close -R "$repo" "$pr" --delete-branch --comment "develop's baseline ($old) is already at or above what the nightly measured; nothing left to raise." >/dev/null
		say "Closed the raise pull request #$pr, which develop's baseline has caught up with."
	fi
	exit 0
fi
what="$old -> $new"
if [ -n "$pr" ] && [ "$(git show "origin/$branch:$bl" 2>/dev/null | fields 2>/dev/null)" = "$new" ]; then
	say "Coverage baseline: pull request #$pr already raises it ($what)."
	exit 0
fi
if [ "${DRY_RUN:-}" = 1 ]; then
	say "Coverage baseline: would raise it ($what; measured on ${measured:0:10}) in '$title' on $branch."
	exit 0
fi

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
lease=()
if git fetch -q origin "$branch" 2>/dev/null; then lease=("--force-with-lease=$branch:$(git rev-parse FETCH_HEAD)"); fi
git checkout -q -B "$branch"
git add -- "$bl"
git commit -q -m "$title" -m "Coverage went up: $what. Measured by the ci workflow on develop at ${measured:0:10}."
git push -q "${lease[@]}" origin "$branch"

if [ -z "$pr" ]; then
	body="The nightly run found the coverage of develop at ${measured:0:10}, as the CI coverage gate measured it, above the baseline: $what. This is the only pull request that moves the baseline."
	if ! url=$("$gh" pr create -R "$repo" --base develop --head "$branch" --title "$title" --body "$body"); then
		echo "::warning::could not open the pull request (is \"Allow GitHub Actions to create and approve pull requests\" off?)"
		say "Coverage baseline: raised to $new on branch \`$branch\`, but the pull request was not opened. Open it by hand: \`gh pr create -R $repo --base develop --head $branch --fill\`, then \`gh pr merge -R $repo --auto --rebase $branch\`."
		exit 0
	fi
	pr=${url##*/}
fi
"$gh" pr merge -R "$repo" --auto --rebase "$pr" || echo "::warning::could not arm auto-merge on #$pr"
say "Coverage baseline: pull request #$pr raises it ($what; measured on ${measured:0:10})."
