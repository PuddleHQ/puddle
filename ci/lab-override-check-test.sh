#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/lab-override-check.py, offline: PyPI answers come from fixture files. Cases it must flag (exit 1),
# pass (exit 0), fail open on, and refuse (exit 2).
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/lab-override-check-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "lab-override-check-test: $*" >&2
    exit 1
}

printf '# why\ntornado==6.6\n' >"$tmp/overrides.txt"
fixture() { # <name> <requires_dist json array>
    printf '{"info":{"version":"13.0.0","requires_dist":%s}}' "$2" >"$tmp/$1.json"
}
check() { # <label> <expected exit> <expected output substring> <args...>
    local label=$1 want=$2 text=$3 got=0
    shift 3
    python3 -I "$here/lab-override-check.py" "$@" >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "$label: exit $got, wanted $want: $(cat "$tmp/out")"
    grep -qF -- "$text" "$tmp/out" || fail "$label: output lacks '$text': $(cat "$tmp/out")"
}

fixture capped '["aioquic (>=1.2.0)", "tornado<=6.5.10,>=6.5.0", "pytest ; extra == \"dev\""]'
check "cap below the override: still needed" 0 "is needed" --json "$tmp/capped.json" "$tmp/overrides.txt"
fixture raised '["tornado (<=6.6,>=6.5.0)"]'
check "cap includes the override: can go" 1 "can go" --json "$tmp/raised.json" "$tmp/overrides.txt"
fixture open '["tornado>=6.5.0"]'
check "open upper end: can go" 1 "can go" --json "$tmp/open.json" "$tmp/overrides.txt"
fixture below '["tornado>=6.5.0,<6.6"]'
check "exclusive cap at the override: still needed" 0 "is needed" --json "$tmp/below.json" "$tmp/overrides.txt"
fixture major '["tornado~=6.5.0"]'
check "compatible-release cap: still needed" 0 "is needed" --json "$tmp/major.json" "$tmp/overrides.txt"
fixture dropped '["aioquic (>=1.2.0)"]'
check "bound dropped: can go" 1 "no bound" --json "$tmp/dropped.json" "$tmp/overrides.txt"
fixture extra '["tornado<6.0 ; extra == \"web\"", "tornado<=6.5.10"]'
check "an extra's bound is ignored" 0 "is needed" --json "$tmp/extra.json" "$tmp/overrides.txt"
printf '{"info":{"version":"13.0.0","requires_dist":null}}' >"$tmp/null.json"
check "no requires_dist at all: can go" 1 "can go" --json "$tmp/null.json" "$tmp/overrides.txt"

# Fails open: an unreadable or malformed answer is a message and exit 0, never a red run.
check "answer missing" 0 "could not read" --json "$tmp/none.json" "$tmp/overrides.txt"
printf '<html>502 Bad Gateway</html>' >"$tmp/html.json"
check "answer not JSON" 0 "could not read" --json "$tmp/html.json" "$tmp/overrides.txt"
printf '{"message":"rate limited"}' >"$tmp/shape.json"
check "answer of the wrong shape" 0 "could not read" --json "$tmp/shape.json" "$tmp/overrides.txt"

# Refuses: a bound it cannot evaluate must not pass as "still needed", and a bad overrides file is a usage error.
fixture odd '["tornado===6.5.10"]'
check "operator it cannot evaluate" 2 "cannot evaluate" --json "$tmp/odd.json" "$tmp/overrides.txt"
printf 'tornado>=6.6\n' >"$tmp/badpin.txt"
check "override that is not a pin" 2 "cannot read override" --json "$tmp/capped.json" "$tmp/badpin.txt"
check "overrides file missing" 2 "overrides.txt" --json "$tmp/capped.json" "$tmp/absent-overrides.txt"

echo "lab-override-check-test: ok"
