#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of wait_for_load in scripts/check.sh (the pause before vitest on a loaded host): it returns at once
# under the core count, waits and then goes on at the cap, and is skipped in CI or with CHECK_LOAD_WAIT=0.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed -n '/^wait_for_load() {/,/^}/p' "$here/check.sh" >"$tmp/fn.sh"
[ -s "$tmp/fn.sh" ] || { echo "check-load-wait-test: wait_for_load not found in check.sh" >&2; exit 1; }
fail() { echo "check-load-wait-test: $*" >&2; exit 1; }
run() { # run <loadavg line> [env...]: prints stderr, returns the function's status
    echo "$1" >"$tmp/loadavg"
    shift
    env -u CI "$@" CHECK_LOADAVG_FILE="$tmp/loadavg" CHECK_LOAD_CORES=4 CHECK_LOAD_POLL=1 sh -c '. "$1"; wait_for_load' _ "$tmp/fn.sh" 2>&1
}
out=$(run "1.00 1 1 1/1 1" CHECK_LOAD_WAIT=5)
[ -z "$out" ] || fail "an idle host must not wait or print, got: $out"
out=$(run "40.00 1 1 1/1 1" CHECK_LOAD_WAIT=2)
case $out in *"waiting for it to drop"*"still not under the core count after 2 s; running vitest anyway"*) ;; *) fail "a busy host must wait, then go on at the cap, got: $out" ;; esac
out=$(run "40.00 1 1 1/1 1" CHECK_LOAD_WAIT=0)
[ -z "$out" ] || fail "CHECK_LOAD_WAIT=0 must skip, got: $out"
out=$(run "40.00 1 1 1/1 1" CHECK_LOAD_WAIT=2 CI=true)
[ -z "$out" ] || fail "CI must skip, got: $out"
echo "40.00 1 1 1/1 1" >"$tmp/loadavg"
(sleep 2; echo "1.00 1 1 1/1 1" >"$tmp/loadavg") &
out=$(env -u CI CHECK_LOAD_WAIT=30 CHECK_LOADAVG_FILE="$tmp/loadavg" CHECK_LOAD_CORES=4 CHECK_LOAD_POLL=1 sh -c '. "$1"; wait_for_load' _ "$tmp/fn.sh" 2>&1)
case $out in *"waiting for it to drop"*) ;; *) fail "expected the waiting line, got: $out" ;; esac
case $out in *"anyway"*) fail "load dropped, must not run on at the cap: $out" ;; esac
echo "check-load-wait-test: ok"
