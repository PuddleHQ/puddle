#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: ci/apt-retry.sh <command that runs apt>...   (e.g. sudo apt-get install -y libfoo)
# ci/apt-setup.sh picks a mirror before the install, but a probe of 2 MB cannot see every slow
# backend: a backend that passed the probe still served the real install at 40 kB/s, and one runner
# hung before the probe printed anything. So the install itself is bounded: a normal one takes 10 to
# 60 s, and one that is still running after $APT_FIRST_TRY_S (default 180) is stopped, Canonical's
# archive is put first, dpkg is repaired and the command runs again (apt resumes partial downloads).
# Only the stall path is bounded this tightly; the retry gets $APT_RETRY_S (default 300).
# `timeout` stops only its own child. `playwright install-deps` runs `sudo apt-get` in a session of
# its own, so after the timeout that apt-get lived on holding the dpkg lock and the retry failed in
# 2 s on "Could not get lock". So after a timeout every process holding an apt or dpkg lock is
# stopped (SIGTERM, then SIGKILL) and the locks are waited for before dpkg is repaired.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
first=${APT_FIRST_TRY_S:-180}
second=${APT_RETRY_S:-300}
# APT_LOCKS overrides the lock files (the test uses its own).
read -r -a locks <<<"${APT_LOCKS:-/var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/lib/apt/lists/lock /var/cache/apt/archives/lock}"
[ "$#" -gt 0 ] || { echo "usage: apt-retry.sh <command>..." >&2; exit 2; }

# lock_holders: pids with a lock file open, found in /proc (needs root for other users' processes).
lock_holders() {
    local l
    for l in "${locks[@]}"; do
        [ -e "$l" ] || continue
        { find /proc/[0-9]*/fd -maxdepth 1 -lname "$l" 2>/dev/null
            sudo -n find /proc/[0-9]*/fd -maxdepth 1 -lname "$l" 2>/dev/null; } | cut -d/ -f3
    done | sort -un
}
signal_holders() {
    local p
    for p in $(lock_holders); do
        echo "::warning::apt-retry: stopping ($1) pid $p ($(ps -o args= -p "$p" 2>/dev/null | cut -c1-100)) that still holds an apt lock"
        kill "-$1" "$p" 2>/dev/null || sudo -n kill "-$1" "$p" 2>/dev/null || true
    done
}
wait_free() {
    local _
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ -z "$(lock_holders)" ] && return 0
        sleep 1
    done
    return 1
}
release_locks() {
    [ -n "$(lock_holders)" ] || return 0
    signal_holders TERM
    wait_free && return 0
    signal_holders KILL
    wait_free && return 0
    echo "::error::apt-retry: pids $(lock_holders | tr '\n' ' ')still hold an apt lock after SIGKILL; the retry will probably fail"
}

status=0
timeout -k 10 "$first" "$@" || status=$?
[ "$status" -ne 0 ] || exit 0
release_locks
echo "::warning::apt-retry: '$*' ended with status $status after at most ${first}s; retrying on archive.ubuntu.com"
"$here/apt-setup.sh" --canonical ||
    echo "::warning::apt-retry: 'apt-setup.sh --canonical' failed; retrying with the current mirror list"
[ -n "${APT_RETRY_NO_REPAIR:-}" ] || sudo dpkg --configure -a ||
    echo "::warning::apt-retry: 'dpkg --configure -a' failed; retrying anyway"
status=0
timeout -k 10 "$second" "$@" || status=$?
[ "$status" -eq 0 ] || release_locks
[ "$status" -eq 0 ] || echo "::error::apt-retry: '$*' failed again (status $status) on archive.ubuntu.com after ${second}s"
exit "$status"
