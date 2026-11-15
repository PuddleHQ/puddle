#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Runner disk evidence that survives a dead runner. A hosted runner that fills its disk loses its job log, so
# the only record is what was written to the job summary and the annotations before the runner died.
#
#   disk-watch.sh snap <label>   free space and inodes (df only, no directory walk), appended to the job summary
#                                now (a summary is uploaded when its step ends). The size of the dirs a job fills
#                                (the slow part, 20 to 55 s a call) is added only when the disk is under the warning
#                                level, when a guard of this job saw it under the warning level, or when
#                                DISK_WATCH_SIZES=1 (a workflow passes it after a failure). Under the warning
#                                level it also lists the biggest top-level dirs and files held open after deletion.
#   disk-watch.sh sizes <label>  the same snapshot with the size walk, whatever the free space
#   disk-watch.sh guard <label>  loop for the duration of one heavy step (run it with `&`, stop it with `kill`):
#                                logs free space every interval to $RUNNER_TEMP/disk-watch-<label>.log; under the
#                                warning level takes one deep snapshot; under the floor takes another, annotates
#                                `::error`, and stops the process tree it was started for: every descendant of
#                                the shell that started it (the step), except the guard itself. Nothing outside that
#                                tree is touched, so a shared host's other jobs survive. $DISK_WATCH_KILL (an ERE
#                                matched against each process's command line) narrows the tree to the matching
#                                processes. The step then ends with a readable cause and the artifact upload still runs.
#
# Evidence only: both modes exit 0 whatever they find (the guard's kill is the one action, and only under the
# floor). Knobs: DISK_WATCH_WARN_GB (30), DISK_WATCH_FLOOR_GB (5), DISK_WATCH_INTERVAL (2 s),
# DISK_WATCH_ROOT (/), DISK_WATCH_SIZES (1: always walk the sizes), DISK_WATCH_FREE_FILE (test only: a file
# holding the free GB instead of df).
set -uo pipefail

warn_gb=${DISK_WATCH_WARN_GB:-30}
floor_gb=${DISK_WATCH_FLOOR_GB:-5}
interval=${DISK_WATCH_INTERVAL:-2}
root=${DISK_WATCH_ROOT:-/}
temp=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
sudo_cmd=()
if [ "$(id -u)" != 0 ] && command -v sudo >/dev/null 2>&1; then sudo_cmd=(sudo -n); fi

free_gb() {
    if [ -n "${DISK_WATCH_FREE_FILE:-}" ]; then
        cat "$DISK_WATCH_FREE_FILE"
    else
        df -Pk "$root" | awk 'NR == 2 { print int($4 / 1048576) }'
    fi
}

say() { # <text>: stdout and the job summary
    printf '%s\n' "$1"
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then printf '%s\n' "$1" >>"$GITHUB_STEP_SUMMARY"; fi
}

# The dirs a job of this repository fills: the runner's temp (archive, extracted tests, msb homes, code-server),
# the toolchains and caches, the workspace and its target, and the places a crash or a runaway log lands.
paths() {
    local w=${GITHUB_WORKSPACE:-$PWD}
    printf '%s\n' "$temp" "$temp/bundle" "$temp/vmx" "$temp/msb" "$temp/pvm" "$temp/code-server" \
        "$HOME/.rustup" "$HOME/.cargo" "$HOME/.cache" "$HOME/.npm" "$w" "$w/target" "$w/ui/node_modules" \
        /var/crash /var/lib/systemd/coredump /var/lib/docker /var/log /var/cache/apt /tmp \
        /opt/hostedtoolcache /usr/local/lib/android /usr/share/dotnet /opt/ghc /swapfile
    printf '%s\n' "$(dirname "$temp")/_diag" "$(dirname "$(dirname "$temp")")/_diag"
    if [ -n "${DISK_WATCH_PATHS:-}" ]; then printf '%s\n' "$DISK_WATCH_PATHS" | tr ':' '\n'; fi
}

# du that falls back to sudo, for root-only dirs (/var/crash, docker); a user without sudo gets what it can read.
du_run() { # <du args...>
    local out
    out=$(timeout 15 du "$@" 2>/dev/null)
    if [ -z "$out" ] && [ "${#sudo_cmd[@]}" -gt 0 ]; then out=$(timeout 15 "${sudo_cmd[@]}" du "$@" 2>/dev/null); fi
    printf '%s\n' "$out"
}

sizes() { # MiB and path of each existing dir over 50 MiB, biggest first
    paths | sort -u | while IFS= read -r p; do
        [ -e "$p" ] || continue
        mib=$(du_run -xsm "$p" | cut -f1)
        [ -n "$mib" ] && [ "$mib" -ge 50 ] && printf '%8s MiB  %s\n' "$mib" "$p"
    done | sort -rn
}

deep() { # what the targeted list can miss: the top level of the disk, and space held by deleted files
    echo "-- top level of $root (MiB)"
    du_run -xm -d1 "$root" | sort -rn | head -12
    if command -v lsof >/dev/null 2>&1; then
        echo "-- files deleted but still open (lsof +L1)"
        timeout 10 "${sudo_cmd[@]}" lsof -nP +L1 2>/dev/null | head -10
    fi
    echo "-- biggest processes (RSS KiB)"
    ps -eo rss=,pid=,comm= --sort=-rss | head -5
}

# The guard leaves this file when it saw the disk under the warning level, so a later snapshot of the same job
# walks the sizes even if the free space has recovered (a cache or archive deleted since).
low_marker() { printf '%s\n' "$temp/disk-watch-low"; }

snap() { # <label> [deep|sizes]
    local label=$1 free walk=0
    free=$(free_gb)
    if [ "${2:-}" = deep ] || [ "${2:-}" = sizes ] || [ "${DISK_WATCH_SIZES:-}" = 1 ] || [ "$free" -lt "$warn_gb" ] ||
        [ -e "$(low_marker)" ]; then walk=1; fi
    local body
    body=$(
        echo "### disk: $label"
        echo '```'
        date -u +%FT%TZ
        df -h "$root" | tail -n 2
        df -i "$root" | tail -n 2
        if [ "$walk" = 1 ]; then sizes; fi
        if [ "${2:-}" = deep ] || [ "$free" -lt "$warn_gb" ]; then deep; fi
        echo '```'
    )
    say "$body"
    if [ "$free" -lt "$warn_gb" ]; then
        echo "::warning title=runner disk low::$label: ${free} GB free (warning level ${warn_gb} GB); sizes in the job summary"
    fi
}

descendants() { # <pid>: its children, grandchildren and so on, one pid per line
    ps -eo pid=,ppid= | awk -v root="$1" '
        { kids[$2] = kids[$2] " " $1 }
        function walk(p,   n, a, i) {
            n = split(kids[p], a, " ")
            for (i = 1; i <= n; i++) { print a[i]; walk(a[i]) }
        }
        END { walk(root) }'
}

# Signal the step's processes: the descendants of the shell that started the guard, minus the guard and its own
# children (its sleep). With a pattern, only the ones whose command line matches it.
stop_step() { # <signal> <step pid>
    local pid args own
    own=" $$ $(descendants $$ | tr '\n' ' ')"
    for pid in $(descendants "$2"); do
        case "$own" in *" $pid "*) continue ;; esac
        if [ -n "${DISK_WATCH_KILL:-}" ]; then
            args=$(ps -o args= -p "$pid" 2>/dev/null) || continue
            grep -Eq -- "$DISK_WATCH_KILL" <<<"$args" || continue
        fi
        kill "-$1" "$pid" 2>/dev/null
    done
}

guard() { # <label>
    local label=$1 log="$temp/disk-watch-$1.log" free min t0=$SECONDS warned=0 step=$PPID
    mkdir -p "$temp"
    : >"$log"
    free=$(free_gb)
    min=$free
    finish() {
        say "disk guard $label: free ${free} GB now, lowest ${min} GB, $((SECONDS - t0)) s watched (samples: $log)"
        exit 0
    }
    trap finish TERM INT
    while :; do
        free=$(free_gb)
        [ "$free" -lt "$min" ] && min=$free
        echo "$((SECONDS - t0)) s free=${free}GB" >>"$log"
        if [ "$free" -lt "$floor_gb" ]; then
            echo "::error title=runner disk full::$label: ${free} GB free (floor ${floor_gb} GB) after $((SECONDS - t0)) s; stopping the step's processes so the job ends readable. Sizes in the job summary"
            stop_step TERM "$step"
            sleep 10 &
            wait $!
            stop_step KILL "$step"
            : >"$(low_marker)"
            # After the kill: nothing writes any more, and a slow du or lsof can no longer cost the step its disk.
            snap "$label: under the floor" deep >/dev/null
            finish
        elif [ "$free" -lt "$warn_gb" ] && [ "$warned" = 0 ]; then
            warned=1
            : >"$(low_marker)"
            # In the background, so the floor check is not held up by a slow du.
            snap "$label: under the warning level" deep >/dev/null &
            echo "::warning title=runner disk low::$label: ${free} GB free after $((SECONDS - t0)) s (warning level ${warn_gb} GB); sizes in the job summary"
        fi
        sleep "$interval" &
        wait $!
    done
}

case "${1:-}" in
snap) snap "${2:-snapshot}" ;;
sizes) snap "${2:-snapshot}" sizes ;;
guard) guard "${2:-guard}" ;;
*) echo "usage: disk-watch.sh snap <label> | sizes <label> | guard <label>" >&2 ;;
esac
exit 0
