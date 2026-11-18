#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Checks the slice of GitHub's REST description that the readers of puddle-repos are tested against
# (crates/puddle-repos/tests/data/github-rest-slice.json):
#   ci/github-rest/check.sh            the pinned file (ci/github-rest/pin.txt) has its pinned SHA-256 and the
#                                      checked-in slice is exactly what slice.py cuts from it
#   ci/github-rest/check.sh --latest   the same for the description on GitHub's main branch today, for the
#                                      operations only: exits 1 when GitHub changed what it says about them,
#                                      and prints the pin line that would move to it
# RAW_BASE_URL replaces https://raw.githubusercontent.com/github/rest-api-description (tests); the latest
# commit comes from RAW_LATEST_REF (default main). FETCH_ATTEMPTS and FETCH_RETRY_BASE_S: ci/fetch-retry.sh.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/../fetch-retry.sh"
slice=${SLICE_FILE:-$here/../../crates/puddle-repos/tests/data/github-rest-slice.json}
base=${RAW_BASE_URL:-https://raw.githubusercontent.com/github/rest-api-description}
python=python3
command -v python3 >/dev/null 2>&1 || python=python

read -r commit file want <<PIN
$(grep -v '^#' "$here/pin.txt" | head -n 1)
PIN
[ -n "$want" ] || { echo "github-rest: no pin in ci/github-rest/pin.txt" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

case "${1:-}" in
    "")
        download github-rest "$base/$commit/$file" "$work/full.json" 600
        got=$(sha256sum <"$work/full.json" | cut -d' ' -f1)
        if [ "$got" != "$want" ]; then
            echo "github-rest: checksum mismatch for $file at $commit: got $got, pinned $want" >&2
            exit 1
        fi
        "$python" -I "$here/slice.py" check "$work/full.json" "$slice"
        echo "github-rest: the slice is what $commit gives"
        ;;
    --latest)
        ref=${RAW_LATEST_REF:-main}
        download github-rest "$base/$ref/$file" "$work/full.json" 600
        if "$python" -I "$here/slice.py" check "$work/full.json" "$slice"; then
            echo "github-rest: the operations are described the same on $ref as at $commit"
        else
            echo "github-rest: GitHub changed its description of the operations the readers use; read the new slice, fix the readers or their tests, then move the pin:" >&2
            echo "$ref $file $(sha256sum <"$work/full.json" | cut -d' ' -f1)  (replace the ref by the commit of $ref)" >&2
            exit 1
        fi
        ;;
    *) echo "usage: $0 [--latest]" >&2; exit 2 ;;
esac
