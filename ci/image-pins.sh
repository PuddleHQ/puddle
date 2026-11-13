#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The default workspace image, the maintenance image and the image the VM tests boot and the one the fake runtime knows are pinned by digest, not by the moving tag
# (`mcr.microsoft.com/devcontainers/base:debian@sha256:...`), so a registry change reaches users only through a
# reviewed pull request. The constants live in Rust; this script keeps them equal and current.
#
#   ci/image-pins.sh check     every pin line holds `<name>:<tag>@sha256:<64 hex>` and all of them are the same reference
#   ci/image-pins.sh refresh   ask the registry for the digest the tag points at now; when it moved, rewrite every pin
#                              line, push the branch ci/refresh-image-pins as one commit and open (or update) one pull
#                              request to develop with auto-merge armed (squash). Never pushes to develop.
#   ci/image-pins.sh --self-test   ci/image-pins-test.sh
#
# DRY_RUN=1 makes `refresh` report and change nothing. The registry must answer an anonymous HEAD request on
# /v2/<repo>/manifests/<tag> (MCR does); CURL and GH override the binaries (the test's fakes). `refresh` needs a clean
# checkout of develop, GH_TOKEN and GITHUB_REPOSITORY, and, to push and open, `contents: write` and `pull-requests: write`
# on a token that starts the required checks (see .github/workflows/refresh-image-pins.yml).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# One pin line per file, found by its shape; a file with none or two fails the check.
files=(crates/puddle-api/src/workspaces.rs crates/puddle-workspace/src/workspaces.rs crates/puddle-vm-tests/src/lib.rs crates/puddle-compute/src/fake/mod.rs)
curl_bin=${CURL:-curl}
gh_bin=${GH:-gh}
branch=ci/refresh-image-pins
title="build: refresh the pinned default image digest"
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
say() { echo "$*" | tee -a "$summary"; }

# "<name>:<tag>" then an optional "@sha256:<hex>", inside a Rust string that ends the statement.
pin_re='"([a-z0-9.-]+(:[0-9]+)?(/[a-z0-9._-]+)+:[A-Za-z0-9._-]+)(@sha256:[0-9a-f]{64})?";[[:space:]]*$'

# pin_of <file>: prints the reference in the file's one pin line, with its digest if it has one.
pin_of() {
	local hits
	hits=$(grep -E "$pin_re" "$root/$1" | sed -E "s#.*$pin_re#\1\4#") || true
	if [ "$(grep -c . <<<"$hits")" != 1 ]; then
		echo "image-pins: $1 must hold exactly one pin line, found: ${hits:-none}" >&2
		return 1
	fi
	echo "$hits"
}

check() {
	local f pin first="" bad=0
	for f in "${files[@]}"; do
		pin=$(pin_of "$f") || { bad=1; continue; }
		if ! [[ $pin =~ ^[^@]+:[^@:/]+@sha256:[0-9a-f]{64}$ ]]; then
			echo "image-pins: $f pins '$pin', which has no sha256 digest (run ci/image-pins.sh refresh, or the refresh workflow)" >&2
			bad=1
		fi
		if [ -z "$first" ]; then
			first=$pin
		elif [ "$pin" != "$first" ]; then
			echo "image-pins: $f pins '$pin' but the first file pins '$first'; every pin must be the same reference" >&2
			bad=1
		fi
	done
	[ "$bad" = 0 ] && echo "image pins ok: $first"
	return "$bad"
}

# digest_of <name:tag>: the digest of the multi-arch index the tag points at, from the registry.
digest_of() {
	local ref=$1 host path tag head code digest
	host=${ref%%/*}
	path=${ref#*/}
	tag=${path##*:}
	path=${path%:*}
	head=$("$curl_bin" -sS -I --max-time 30 --retry 2 \
		-H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json' \
		"https://$host/v2/$path/manifests/$tag") || { echo "image-pins: the registry request for $ref failed" >&2; return 1; }
	code=$(head -n 1 <<<"$head" | tr -d '\r' | awk '{print $2}')
	if [ "$code" != 200 ]; then
		echo "image-pins: the registry answered ${code:-nothing} for $ref" >&2
		return 1
	fi
	digest=$(tr -d '\r' <<<"$head" | awk 'tolower($1) == "docker-content-digest:" {print $2}')
	if ! [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]]; then
		echo "image-pins: no usable Docker-Content-Digest for $ref (got '${digest:-none}')" >&2
		return 1
	fi
	echo "$digest"
}

# rewrite <new reference>: every pin line gets it.
rewrite() {
	local f
	for f in "${files[@]}"; do
		sed -i -E "s|$pin_re|\"$1\";|" "$root/$f"
	done
}

refresh() {
	local first ref old new digest pr url what f
	first=$(pin_of "${files[0]}")
	ref=${first%%@*}
	old=${first#"$ref"}
	old=${old#@}
	digest=$(digest_of "$ref")
	new=$ref@$digest
	for f in "${files[@]}"; do pin_of "$f" >/dev/null; done
	if [ "$first" = "$new" ] && check >/dev/null 2>&1; then
		say "Image pins: $ref is still ${digest:0:19} on the registry; nothing to refresh."
		pr=$("$gh_bin" pr list -R "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}" --head "$branch" --state open --json number --jq '.[0].number // empty')
		if [ -n "$pr" ] && [ "${DRY_RUN:-}" != 1 ]; then
			"$gh_bin" pr close -R "$GITHUB_REPOSITORY" "$pr" --delete-branch --comment "develop already pins the digest the registry serves; nothing left to refresh." >/dev/null
			say "Closed the refresh pull request #$pr, which develop has caught up with."
		fi
		return 0
	fi
	what="${old:+${old:0:19} to }${digest:0:19}"
	if [ "${DRY_RUN:-}" = 1 ]; then
		say "Image pins: would pin $ref at $what on $branch."
		return 0
	fi
	rewrite "$new"
	check >/dev/null
	cd "$root"
	pr=$("$gh_bin" pr list -R "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}" --head "$branch" --state open --json number --jq '.[0].number // empty')
	git config user.name "github-actions[bot]"
	git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
	local lease=()
	if git fetch -q origin "$branch" 2>/dev/null; then
		if [ -n "$pr" ] && git diff --quiet FETCH_HEAD -- "${files[@]}"; then
			say "Image pins: pull request #$pr already pins $ref at $what."
			return 0
		fi
		lease=("--force-with-lease=$branch:$(git rev-parse FETCH_HEAD)")
	fi
	git checkout -q -B "$branch"
	git add -- "${files[@]}"
	git commit -q -m "$title" -m "The registry now serves $ref at $digest ($what). Review what changed upstream, then merge; the VM tests pull this image."
	git push -q "${lease[@]}" origin "$branch"
	if [ -z "$pr" ]; then
		if ! url=$("$gh_bin" pr create -R "$GITHUB_REPOSITORY" --base develop --head "$branch" --title "$title" \
			--body "The registry's \`${ref##*/}\` moved to \`$digest\`. The default and maintenance images are pinned by digest, so this pull request is the only way a new image reaches users. The checks pull the image in the VM tests."); then
			echo "::warning::could not open the pull request (is \"Allow GitHub Actions to create and approve pull requests\" off?)"
			say "Image pins: pinned $ref at $what on branch \`$branch\`, but the pull request was not opened. Open it by hand: \`gh pr create -R $GITHUB_REPOSITORY --base develop --head $branch --fill\`."
			return 0
		fi
		pr=${url##*/}
	fi
	"$gh_bin" pr merge -R "$GITHUB_REPOSITORY" --auto --squash "$pr" || echo "::warning::could not arm auto-merge on #$pr"
	say "Image pins: pull request #$pr pins $ref at $what."
}

case ${1:-} in
check) check ;;
refresh) refresh ;;
--self-test) exec "$root/ci/image-pins-test.sh" ;;
*) echo "usage: image-pins.sh check | refresh | --self-test" >&2; exit 2 ;;
esac
