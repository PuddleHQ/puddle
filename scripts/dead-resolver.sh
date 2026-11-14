#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Runs a command (the test suite) with a DNS resolver that never answers, so a test that waits on a real
# resolver is caught here and not on the day a resolver is down (2026-11-14: a dead LAN resolver on the
# build host turned `squid_upstream` and `private_access` red in many pull requests).
#
#   scripts/dead-resolver.sh <command> [args...]
#
# The command runs in a private mount namespace whose /etc/resolv.conf names one nameserver on
# loopback (an address of 127.77.0.0/16 of its own per run) that logs every query and never replies (scripts/dead-resolver-stub.py; `timeout:1 attempts:1`, so
# one lookup costs a second), and whose /etc/nsswitch.conf asks files and DNS only (no systemd-resolved
# module that would bypass the file). It runs as the caller, never as root. When the command ends the script
# lists the names that were asked and fails if there were any: the test that asked is the one that waits a
# second or more, so rerun the crate with the same wrapper and look at nextest's SLOW lines. `localhost` and
# the other names in /etc/hosts never reach a resolver. Exit status: the command's; else 1 for a name asked.
#
# The namespaces need passwordless sudo (then `setpriv` drops back to the caller). An unprivileged user
# namespace is no substitute: in one, `/` belongs to nobody and the tests that look at who owns a folder fail.
# CI (CI set) fails when sudo does not work; elsewhere the command runs as it is, with a note, so a machine
# without it (macOS, a locked-down container) loses the check and not the run.
#
# Internal: `--inside <logfile> <envfile> <uid> <gid> <command...>` is the part that runs in the namespaces.
set -u

if [ "${1:-}" = "--inside" ]; then
    log=$2 envfile=$3 uid=$4 gid=$5
    shift 5
    # shellcheck disable=SC1090 # the caller's exported environment, saved by the outer half
    . "$envfile"
    dir=$(dirname "$log")
    printf "nameserver %s\noptions timeout:1 attempts:1\n" "$DEAD_RESOLVER_ADDR" >"$dir/resolv.conf"
    printf 'hosts: files dns\n' >"$dir/nsswitch.conf"
    mount --bind "$dir/resolv.conf" /etc/resolv.conf || exit 2
    mount --bind "$dir/nsswitch.conf" /etc/nsswitch.conf || exit 2
    here=$(cd "$(dirname "$0")" && pwd)
    python3 "$here/dead-resolver-stub.py" "$DEAD_RESOLVER_ADDR" "$log" >"$dir/stub.ready" &
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        [ -s "$dir/stub.ready" ] && break
        sleep 0.1
    done
    [ -s "$dir/stub.ready" ] || {
        echo "dead-resolver: the stub nameserver did not start" >&2
        exit 2
    }
    exec setpriv --reuid="$uid" --regid="$gid" --init-groups -- "$@"
fi

[ "$#" -gt 0 ] || {
    echo "usage: scripts/dead-resolver.sh <command> [args...]" >&2
    exit 2
}
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/dead-resolver.XXXXXX") || exit 2
trap 'rm -rf "$work"' EXIT
: >"$work/queries"
# A loopback address of its own per run (derived from the pid), so two runs at once do not share a port.
DEAD_RESOLVER_ADDR="127.77.$(($$ / 250 % 250)).$(($$ % 250 + 1))"
export DEAD_RESOLVER_ADDR
export -p >"$work/env"
# sudo starts the command with the limits of root's login, not the caller's: restore the ones tests read.
for opt in c n; do
    # shellcheck disable=SC3045 # dash and bash both take -H and -S
    printf 'ulimit -H%s %s; ulimit -S%s %s\n' "$opt" "$(ulimit -H"$opt")" "$opt" "$(ulimit -S"$opt")" >>"$work/env"
done
uid=$(id -u) gid=$(id -g)

unavailable() {
    if [ -n "${CI:-}" ]; then
        echo "dead-resolver: no namespace can be made here (needs root or passwordless sudo, unshare and setpriv)" >&2
        exit 2
    fi
    echo "note: no namespace can be made here; running without the dead resolver" >&2
    exec "$@"
}

sudo=sudo
[ "$uid" != 0 ] || sudo=
if { [ -z "$sudo" ] || sudo -n true 2>/dev/null; } && command -v setpriv >/dev/null && command -v unshare >/dev/null; then
    # shellcheck disable=SC2086 # $sudo is `sudo` or empty (already root)
    ${sudo:+sudo -n} unshare -m sh "$here/dead-resolver.sh" --inside "$work/queries" "$work/env" "$uid" "$gid" "$@"
    rc=$?
else
    unavailable "$@"
fi
# rc 2 from setup (not the command) is indistinguishable from a command's own 2; the stub's ready file tells.
if [ "$rc" = 2 ] && [ ! -s "$work/stub.ready" ]; then
    unavailable "$@"
fi

asked=$(grep -vx "localhost\|localhost\." "$work/queries" | sort | uniq -c | sort -rn)
if [ -n "$asked" ]; then
    {
        echo "dead-resolver: the run asked the resolver (count, name):"
        echo "$asked"
        echo "dead-resolver: a test waits on a real resolver. Inject a lookup (docs/STANDARDS.md, 'Tests and DNS') or use a name from /etc/hosts."
    } >&2
    [ "$rc" != 0 ] || rc=1
fi
exit "$rc"
