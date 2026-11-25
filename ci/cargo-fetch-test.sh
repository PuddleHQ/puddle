#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/cargo-fetch.sh with a fake `cargo` and `sleep` on PATH (no network, no waiting): a fetch
# that works at once, one that fails with the submodule SSL error twice and then works, and one
# that never works (the cause is named, as a CI annotation too, and the tries are bounded).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
. "$(dirname "$0")/../scripts/test-fail.sh"

mkdir -p "$tmp/bin"
# The fake cargo fails its first $FAKE_CARGO_FAILS calls, and records its arguments and every call.
cat >"$tmp/bin/cargo" <<'STUB'
#!/bin/sh
n=$(($(cat "$FAKE_DIR/calls" 2>/dev/null || echo 0) + 1))
echo "$n" >"$FAKE_DIR/calls"
echo "$*" >>"$FAKE_DIR/args"
if [ "$n" -le "$FAKE_CARGO_FAILS" ]; then
    echo "error: failed to fetch submodule \`skills\` from https://github.com/superradcompany/skills.git" >&2
    echo "  SSL error: unknown error; class=Ssl (16)" >&2
    exit 101
fi
echo "fetched"
STUB
cat >"$tmp/bin/sleep" <<'STUB'
#!/bin/sh
echo "$1" >>"$FAKE_DIR/sleeps"
STUB
chmod +x "$tmp/bin/cargo" "$tmp/bin/sleep"
export FAKE_DIR="$tmp"

# run <name> <cargo failures> [env assignments...]: leaves rc- and err- files, calls and sleeps.
run() {
    name=$1 fails=$2
    shift 2
    : >"$tmp/args"
    printf '' >"$tmp/sleeps"
    echo 0 >"$tmp/calls"
    rc=0
    env FAKE_CARGO_FAILS="$fails" "$@" PATH="$tmp/bin:$PATH" "$here/cargo-fetch.sh" >"$tmp/out-$name" 2>"$tmp/err-$name" || rc=$?
    echo "$rc" >"$tmp/rc-$name"
}
rc_of() { cat "$tmp/rc-$1"; }
calls() { cat "$tmp/calls"; }
sleeps() { tr '\n' ' ' <"$tmp/sleeps"; }

run clean 0
[ "$(rc_of clean)" = 0 ] || fail "a working fetch failed"
[ "$(calls)" = 1 ] || fail "a working fetch ran $(calls) times"
[ "$(cat "$tmp/args")" = "fetch --locked" ] || fail "cargo ran with '$(cat "$tmp/args")'"

run flaky 2
[ "$(rc_of flaky)" = 0 ] || fail "two SSL failures were not retried to success"
[ "$(calls)" = 3 ] || fail "expected 3 tries, got $(calls)"
[ "$(sleeps)" = "5 10 " ] || fail "pauses were '$(sleeps)', expected '5 10 '"
grep -q "ok on try 3 of 5" "$tmp/out-flaky" || fail "the recovery is not reported"

run never 99 CARGO_FETCH_ATTEMPTS=4
[ "$(rc_of never)" = 1 ] || fail "a fetch that never works exited $(rc_of never)"
[ "$(calls)" = 4 ] || fail "a failing fetch ran $(calls) times, expected 4"
grep -q "::error title=cargo fetch failed::4 tries failed" "$tmp/err-never" || fail "no annotation"
grep -q "class=Ssl" "$tmp/err-never" || fail "the cause is not shown"

echo "cargo-fetch-test: ok"
