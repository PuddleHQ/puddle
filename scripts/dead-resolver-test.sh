#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of scripts/dead-resolver.sh: inside it a lookup of a made-up name waits for the resolver's timeout
# (so a test that does it is slow, as in a real outage), the name is listed and the run fails even though the
# command passed; a command that asks nothing but `localhost` passes; the command's own exit status comes
# through; it runs as the caller, not as root, with the caller's environment; the stub nameserver is gone
# afterwards, and only the stub of its own run is looked at: a second copy of this test (another worker's commit
# hook) running at the same time does not fail it. Where the namespaces cannot be made (no passwordless sudo) it says so and passes, except in CI.
set -u
here=$(cd "$(dirname "$0")" && pwd)
. "$(dirname "$0")/../scripts/test-fail.sh"
sudo=sudo
[ "$(id -u)" != 0 ] || sudo=
if { [ -n "$sudo" ] && ! sudo -n true 2>/dev/null; } || ! command -v setpriv >/dev/null; then
    [ -z "${CI:-}" ] || fail "CI needs passwordless sudo and setpriv for the dead resolver"
    echo "dead-resolver-test: skipped, no passwordless sudo or no setpriv"
    exit 0
fi
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dead-resolver-test.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

# The stub is started with its run's log path (under that run's TMPDIR) as an argument, so a run's stubs are the
# processes whose command line names that folder; the name alone would match every other run on the machine.
stubs_in() { pgrep -f "dead-resolver-stub.py 127.77.[0-9.]* $1/" || true; }

# A second run, in its own TMPDIR, keeps its stub alive for the whole test, as a parallel copy of this test would.
mkdir "$tmp/other" "$tmp/own"
TMPDIR="$tmp/other" "$here/dead-resolver.sh" sleep 8 >/dev/null 2>&1 &
other=$!
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    [ -n "$(stubs_in "$tmp/other")" ] && break
    sleep 0.25
done
[ -n "$(stubs_in "$tmp/other")" ] || fail "the second run's stub did not start"
export TMPDIR="$tmp/own"

start=$(date +%s)
# shellcheck disable=SC2016 # the command's text is for the inner shell
DEAD_RESOLVER_PROBE=kept "$here/dead-resolver.sh" sh -c 'getent hosts no-such-name.example.org >/dev/null; echo "$(id -u) $DEAD_RESOLVER_PROBE"' >"$tmp/stdout" 2>"$tmp/stderr"
rc=$?
took=$(($(date +%s) - start))
[ "$rc" = 1 ] || fail "a name asked must fail a passing command with 1, got $rc"
grep -q 'no-such-name.example.org' "$tmp/stderr" || fail "the name asked is not listed: $(cat "$tmp/stderr")"
[ "$took" -ge 1 ] || fail "the lookup did not wait for the resolver's timeout ($took s)"
[ "$(cat "$tmp/stdout")" = "$(id -u) kept" ] || fail "the command did not run as the caller with its environment: $(cat "$tmp/stdout")"

"$here/dead-resolver.sh" sh -c 'getent hosts localhost >/dev/null' 2>"$tmp/stderr" || fail "localhost alone failed: $(cat "$tmp/stderr")"
[ ! -s "$tmp/stderr" ] || fail "localhost alone was reported: $(cat "$tmp/stderr")"
# shellcheck disable=SC2016 # the command's text is for the inner shell
"$here/dead-resolver.sh" sh -c 'getent hosts "$(hostname)" >/dev/null' 2>"$tmp/stderr" || fail "the machine's own name was not resolved: $(cat "$tmp/stderr")"
[ ! -s "$tmp/stderr" ] || fail "the machine's own name was reported: $(cat "$tmp/stderr")"
"$here/dead-resolver.sh" sh -c 'exit 7' 2>/dev/null
[ "$?" = 7 ] || fail "the command's exit status did not come through"

sleep 1
[ -z "$(stubs_in "$tmp/own")" ] || fail "a stub nameserver of this test is still running"
[ -n "$(stubs_in "$tmp/other")" ] || fail "the second run's stub is gone before its command ended, so the runs did not overlap"
wait "$other" || fail "the second run, started at the same time, failed"
sleep 1
[ -z "$(stubs_in "$tmp/other")" ] || fail "the second run left its stub nameserver running"
echo "dead-resolver-test: ok"
