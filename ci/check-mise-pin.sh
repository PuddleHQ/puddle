#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks ci/mise.sha256 against what mise publishes: for every pinned line, the release's
# SHASUMS256.txt on mise's own site and on GitHub must list the same SHA-256 for the asset. Exits 1
# naming each line that differs or is missing; a failed download is retried (ci/fetch-retry.sh).
# Run nightly by the live-contracts workflow, never on a pull request: it needs the network.
# MISE_PIN_FILE, MISE_SITE_URL and MISE_GITHUB_URL replace the pin file and the two release bases
# (tests); FETCH_ATTEMPTS and FETCH_RETRY_BASE_S set the retry window.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
pins=${MISE_PIN_FILE:-$here/mise.sha256}
site=${MISE_SITE_URL:-https://mise.jdx.dev}
github=${MISE_GITHUB_URL:-https://github.com/jdx/mise/releases/download}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

failed=0
checked=0
while read -r version asset want _; do
    case $version in '' | '#'*) continue ;; esac
    [ -n "${asset:-}" ] && [ -n "${want:-}" ] || { echo "check-mise-pin: bad line for $version in $pins" >&2; failed=1; continue; }
    for source in "mise.jdx.dev:$site" "github.com:$github"; do
        name=${source%%:*}
        base=${source#*:}
        sums="$work/$name-$version"
        [ -s "$sums" ] || download check-mise-pin "$base/v$version/SHASUMS256.txt" "$sums" 120
        got=$(awk -v a="$asset" '$2 == a || $2 == "./" a { print $1; exit }' "$sums")
        if [ -z "$got" ]; then
            echo "check-mise-pin: $name does not list $asset for v$version" >&2
            failed=1
        elif [ "$got" != "$want" ]; then
            echo "check-mise-pin: $name lists $got for $asset, the pin has $want" >&2
            failed=1
        else
            checked=$((checked + 1))
        fi
    done
done <"$pins"
[ "$checked" -gt 0 ] || { echo "check-mise-pin: nothing was checked ($pins has no pinned line)" >&2; exit 1; }
[ "$failed" -eq 0 ] || exit 1
echo "check-mise-pin: $checked checks match"
