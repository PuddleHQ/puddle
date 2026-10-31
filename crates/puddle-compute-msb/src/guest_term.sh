#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Run in the guest just before a stop: sends SIGTERM to every process of the guest, once, and
# waits until they have exited or the grace period ($1 seconds) is over. The runtime's own shutdown
# signals only the commands it started itself and then kills everything else at once, so without
# this a detached daemon (an image's entrypoint such as dockerd, a database) gets no chance to
# shut down cleanly and can leave state behind that breaks it at the next boot.
#
# Left alone: PID 1 (the runtime's agent), this shell, its ancestors (the exec that runs it) and
# its own children, kernel threads (children of kthreadd, PID 2) and zombies. Each process gets
# one SIGTERM only: some count them (dockerd quits without cleanup at the third).
#
# Prints the processes still running when the grace ran out, on one line ("left: <pid> <name>,
# ..."). Exits 0 when none is left and 1 otherwise. POSIX sh: dash, busybox and bash images all
# run it.
grace=${1:-10}
self=$$

# A wall-clock deadline, not a count of ticks: one look at every process takes a fork per process,
# so a tick lasts well over its sleep in a busy guest.
end=$(($(date +%s) + grace))
if sleep 0.1 2>/dev/null; then tick=0.1; else tick=1; fi

# The stat fields after the command name: state, ppid. `cat`, not `read`: dash reads procfs a
# byte at a time and sees a truncated file.
fields() {
    stat=$(cat "/proc/$1/stat" 2>/dev/null) || return 1
    printf '%s\n' "${stat##*) }"
}

ancestors=" "
p=$PPID
while [ -n "$p" ] && [ "$p" -gt 1 ]; do
    ancestors="$ancestors$p "
    f=$(fields "$p") || break
    # shellcheck disable=SC2086 # word splitting picks the fields
    set -- $f
    p=${2:-}
done

# The processes this script may stop, one pid per line.
targets() {
    for d in /proc/[0-9]*; do
        p=${d#/proc/}
        case "$p" in 1 | 2 | "$self") continue ;; esac
        case "$ancestors" in *" $p "*) continue ;; esac
        f=$(fields "$p") || continue
        # shellcheck disable=SC2086 # word splitting picks the fields
        set -- $f
        [ "${1:-Z}" = Z ] && continue
        case "${2:-}" in 2 | "$self") continue ;; esac
        printf '%s\n' "$p"
    done
}

signalled=" "
while :; do
    left=$(targets)
    [ -z "$left" ] && exit 0
    for p in $left; do
        case "$signalled" in *" $p "*) continue ;; esac
        kill -s TERM -- "$p" 2>/dev/null
        signalled="$signalled$p "
    done
    [ "$(date +%s)" -ge "$end" ] && break
    sleep "$tick"
done
names=
for p in $left; do
    name=$(cat "/proc/$p/comm" 2>/dev/null) && names="${names:+$names, }$p $name"
done
printf 'left: %s\n' "$names"
exit 1
