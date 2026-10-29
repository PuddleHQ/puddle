#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The nightly coverage-baseline workflow: raises scripts/coverage-baseline.json and ui/coverage-baseline.json from the coverage
# CI itself measured on develop, in ONE pull request, `build: raise the coverage baseline`, that goes through the merge queue
# like any other (required checks, no bypass). Pull requests do not carry a raise of their own; this is the only way the floors
# move up.
#   1. Run from a checkout of develop. The Rust measurement is the `coverage` gate's own: the `ci` workflow uploads
#      `coverage-summary.json` in its `linux-test-results` artifact, and the newest green run on develop's tip is used, or on
#      an older commit when every file changed since is documentation (ci/scope.sh classifies the file list). Never a
#      measurement of another commit: a raise above what the tip reaches would fail the next pull request. The UI's
#      measurement (`coverage-summary.json` in the `linux-ui-test-results` artifact of the `ui` job) is looked up the same
#      way, and on an older commit when the files changed since touch nothing the `ui` job runs for (ci/scope.sh says
#      ui=false). A baseline with no measurement tonight is left as it is; the other one is still raised.
#   2. The ratchets (ui/scripts/coverage-ratchet.ts with the floors from scripts/check.sh, ui/scripts/ui-coverage-ratchet.ts)
#      rewrite their baseline: per field the higher of the committed value and the measured one, rounded down to 0.1. A
#      measurement below a baseline fails the job and raises nothing; neither ratchet ever writes a lower number.
#   3. Nothing to raise: exit 0, and an open raise pull request that develop's baselines have caught up with is closed.
#      An open one that already carries exactly these files: nothing to do. Otherwise the branch ci/raise-coverage-baseline is
#      rebuilt on the checkout as one commit and pushed with a lease, and the pull request is opened (or left open) with
#      auto-merge armed (squash: develop takes no other merge method).
# The token decides whether the pull request's checks run: a pull request opened or updated with the default GITHUB_TOKEN does
# not trigger them (GitHub's rule), so the workflow passes the installation token of a GitHub App (see the
# workflow). When GitHub refuses to open the pull request (the setting "Allow GitHub Actions to create and approve pull
# requests" is off), the branch stays pushed, a warning says how to open it by hand, and the exit is 0.
# DRY_RUN=1 reports what would happen and changes nothing remote. Needs GH_TOKEN, GITHUB_REPOSITORY, `actions: read` and,
# to push and open, `contents: write` and `pull-requests: write`. GH overrides the gh binary. Self-test: ci/raise-coverage-baseline-test.sh.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
gh=${GH:-gh}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
bl=scripts/coverage-baseline.json
ubl=ui/coverage-baseline.json
branch=ci/raise-coverage-baseline
title="build: raise the coverage baseline"
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
work=$(mktemp -d "${TMPDIR:-/tmp}/raise-coverage-baseline.XXXXXX")
trap 'rm -rf "$work"' EXIT
say() { echo "$*" | tee -a "$summary"; }
fields() { jq -r 'to_entries | map("\(.key) \(.value)") | join(", ")'; }
classified() { git diff --name-only "$1" HEAD | "$here/scope.sh" classify; }
docs_only() { # docs_only <older commit>: every file changed from it to HEAD is documentation
	local out
	out=$(classified "$1")
	grep -qx 'ci_rust=false' <<<"$out" && grep -qx 'ui=false' <<<"$out"
}
ui_untouched() { # ui_untouched <older commit>: no file changed from it to HEAD is one the `ui` job runs for
	classified "$1" | grep -qx 'ui=false'
}
# find_summary <predicate> <artifact>: the newest green ci run on the tip, or on an older first-parent commit while the predicate
# holds for it, whose artifact holds a coverage-summary.json. Sets found (the commit) and found_file; both empty when none does.
find_summary() {
	local same=$1 art=$2 c id
	local cands=("$tip")
	while read -r c; do
		"$same" "$c" || break
		cands+=("$c")
	done < <(git rev-list --first-parent --skip=1 --max-count=15 HEAD)
	found=''
	found_file=''
	for c in "${cands[@]}"; do
		while read -r id; do
			[ -n "$id" ] || continue
			rm -rf "$work/dl"
			if "$gh" run download -R "$repo" "$id" -n "$art" -D "$work/dl" >/dev/null 2>&1; then
				found_file=$(find "$work/dl" -name coverage-summary.json -print -quit)
				[ -z "$found_file" ] || { found=$c; return 0; }
			fi
		done < <("$gh" run list -R "$repo" --workflow ci.yml --status success --commit "$c" --json databaseId --jq '.[].databaseId')
	done
}

# 1. The measurements. The UI's summary is copied out of the download folder, which the next lookup clears.
tip=$(git rev-parse HEAD)
find_summary docs_only linux-test-results
measured=$found
file=$found_file
if [ -n "$file" ]; then cp "$file" "$work/rust-summary.json" && file=$work/rust-summary.json; fi
find_summary ui_untouched linux-ui-test-results
ui_measured=$found
ui_file=$found_file
if [ -z "$measured" ] && [ -z "$ui_measured" ]; then
	say "Coverage baseline: no green ci run with a coverage summary on develop's tip ${tip:0:10} (or on an older commit with documentation-only changes since); nothing raised tonight."
	exit 0
fi

# 2. The ratchets.
tracked=()
what=''
if [ -n "$measured" ]; then
	cov_lines=$(sed -n 's/^COV_LINES=//p' scripts/check.sh)
	cov_regions=$(sed -n 's/^COV_REGIONS=//p' scripts/check.sh)
	[ -n "$cov_lines" ] && [ -n "$cov_regions" ] || { echo "cannot read the floors from scripts/check.sh" >&2; exit 2; }
	old=$(git show "HEAD:$bl" | fields)
	node "$here/../ui/scripts/coverage-ratchet.ts" --summary "$file" --baseline "$bl" \
		--floor-lines "$cov_lines" --floor-regions "$cov_regions" --write
	tracked+=("$bl")
	if ! git diff --quiet -- "$bl"; then what="rust: $old -> $(fields <"$bl")"; fi
else
	say "Coverage baseline: no Rust measurement tonight; $bl is left as it is."
fi
if [ -n "$ui_measured" ]; then
	uold=$(git show "HEAD:$ubl" | fields)
	node "$here/../ui/scripts/ui-coverage-ratchet.ts" --summary "$ui_file" --baseline "$ubl" --write
	tracked+=("$ubl")
	if ! git diff --quiet -- "$ubl"; then what="${what:+$what; }ui: $uold -> $(fields <"$ubl")"; fi
else
	say "Coverage baseline: no UI measurement tonight; $ubl is left as it is."
fi
at=${measured:-$ui_measured}
pr=$("$gh" pr list -R "$repo" --head "$branch" --base develop --state open --json number --jq '.[0].number // empty')

# 3. Nothing, the same, or a new raise.
if git diff --quiet -- "${tracked[@]}"; then
	say "Coverage baseline: nothing to raise (measured on ${at:0:10})."
	if [ -n "$pr" ] && [ "${DRY_RUN:-}" != 1 ]; then
		"$gh" pr close -R "$repo" "$pr" --delete-branch --comment "develop's baselines are already at or above what the nightly measured; nothing left to raise." >/dev/null
		say "Closed the raise pull request #$pr, which develop's baselines have caught up with."
	fi
	exit 0
fi
# What a branch (or the tree) holds of the tracked files, one line per file, to tell whether the open pull request is current.
snapshot() { # snapshot [<ref>]: the files in the tree, or at <ref>
	local f
	for f in "$bl" "$ubl"; do
		if [ -n "${1:-}" ]; then git show "$1:$f" 2>/dev/null; else cat "$f"; fi | fields 2>/dev/null || echo "-"
	done
}
if [ -n "$pr" ] && [ "$(snapshot "origin/$branch")" = "$(snapshot)" ]; then
	say "Coverage baseline: pull request #$pr already raises it ($what)."
	exit 0
fi
if [ "${DRY_RUN:-}" = 1 ]; then
	say "Coverage baseline: would raise it ($what; measured on ${at:0:10}) in '$title' on $branch."
	exit 0
fi

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
lease=()
if git fetch -q origin "$branch" 2>/dev/null; then lease=("--force-with-lease=$branch:$(git rev-parse FETCH_HEAD)"); fi
git checkout -q -B "$branch"
git add -- "${tracked[@]}"
git commit -q -m "$title" -m "Coverage went up: $what. Measured by the ci workflow on develop at ${at:0:10}."
git push -q "${lease[@]}" origin "$branch"

if [ -z "$pr" ]; then
	body="The nightly run found the coverage of develop at ${at:0:10}, as the CI coverage gates measured it, above the baselines: $what. This is the only pull request that moves the baselines."
	if ! url=$("$gh" pr create -R "$repo" --base develop --head "$branch" --title "$title" --body "$body"); then
		echo "::warning::could not open the pull request (is \"Allow GitHub Actions to create and approve pull requests\" off?)"
		say "Coverage baseline: raised ($what) on branch \`$branch\`, but the pull request was not opened. Open it by hand: \`gh pr create -R $repo --base develop --head $branch --fill\`, then \`gh pr merge -R $repo --auto --squash $branch\`."
		exit 0
	fi
	pr=${url##*/}
fi
"$gh" pr merge -R "$repo" --auto --squash "$pr" || echo "::warning::could not arm auto-merge on #$pr"
say "Coverage baseline: pull request #$pr raises it ($what; measured on ${at:0:10})."
