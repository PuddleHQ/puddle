#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/tools-cache-heal.sh with a fake `gh`: no dispatch while the entry exists or a recent nightly or
# dispatch run on develop exists (a pull request or merge queue run, or an old run, does not count); one dispatch
# otherwise; none under DRY_RUN=1. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/tools-cache-heal-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "tools-cache-heal-test: FAIL: $*" >&2; fail=1; }

recent=$(date -u -d '2 hours ago' +%Y-%m-%dT%H:%M:%SZ)
old=$(date -u -d '9 hours ago' +%Y-%m-%dT%H:%M:%SZ)

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
case "$*" in
*"-X POST"*dispatches*) echo "$*" >>"$FAKE_DISPATCHED" ;;
*"actions/caches?key=vm-guest-env-tools-&ref=refs/heads/develop"*) echo "$FAKE_CACHE_COUNT" ;;
*"workflows/vm-linux.yml/runs?branch=develop"*) printf '{"workflow_runs": %s}\n' "$FAKE_RUNS" ;;
*) exit 1 ;;
esac
GH
chmod +x "$tmp/bin/gh"

# <name> <cache count> <runs json> <want dispatches> [env...]
case_() {
    local name=$1 count=$2 runs=$3 want=$4
    shift 4
    : >"$tmp/dispatched"
    env PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r FAKE_CACHE_COUNT="$count" FAKE_RUNS="$runs" \
        FAKE_DISPATCHED="$tmp/dispatched" "$@" "$here/tools-cache-heal.sh" >"$tmp/out" 2>&1 || bad "$name: exited non-zero: $(cat "$tmp/out")"
    local got
    got=$(grep -c . "$tmp/dispatched")
    [ "$got" = "$want" ] || bad "$name: $got dispatches, want $want: $(cat "$tmp/out")"
}

case_ "entry present" 1 '[]' 0
case_ "entry present, no run" 3 "[{\"event\":\"schedule\",\"created_at\":\"$old\"}]" 0
case_ "missing, recent nightly" 0 "[{\"event\":\"schedule\",\"created_at\":\"$recent\"}]" 0
case_ "missing, recent dispatch" 0 "[{\"event\":\"workflow_dispatch\",\"created_at\":\"$recent\"}]" 0
case_ "missing, only queue and pull request runs" 0 "[{\"event\":\"merge_group\",\"created_at\":\"$recent\"},{\"event\":\"pull_request\",\"created_at\":\"$recent\"}]" 1
case_ "missing, old nightly only" 0 "[{\"event\":\"schedule\",\"created_at\":\"$old\"}]" 1
case_ "missing, no runs" 0 '[]' 1
grep -q 'dispatches -f ref=develop' "$tmp/dispatched" || bad "the dispatch does not name ref=develop: $(cat "$tmp/dispatched")"
case_ "dry run" 0 '[]' 0 DRY_RUN=1
grep -q 'would dispatch' "$tmp/out" || bad "DRY_RUN=1 does not say what it would do: $(cat "$tmp/out")"
# A shorter window lets the 2-hour-old run count as old.
case_ "missing, window 1 h" 0 "[{\"event\":\"schedule\",\"created_at\":\"$recent\"}]" 1 HEAL_HOURS=1

if [ "$fail" = 0 ]; then
    echo "tools-cache-heal-test: ok"
else
    exit 1
fi
