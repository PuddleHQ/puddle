#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# ci/deny-fresh.sh <cargo-deny check arguments...>: refreshes cargo-deny's advisory database and registry
# index, then runs `cargo deny <arguments>`.
#
# Why: the check judges against the copy of the advisory database and the registry index on disk. A crate
# yanked, or an advisory published, after the last fetch passes on a machine with an old copy and fails in the
# scheduled job, which fetches. Fetching first makes the local run see what the job sees.
#
# Before the fetch it says how old the database was (it prints a line when that is over a day, or when it
# cannot tell), so a pass that follows a failed fetch is visibly a pass on old data. A failed fetch (offline)
# warns and goes on: the check then judges on the copy it has, and says so.
#
# Environment: DENY (the cargo-deny command, default `cargo deny`), DENY_DB_DIR (the advisory database folder,
# default $CARGO_HOME/advisory-dbs), DENY_MAX_AGE_S (age that triggers the notice, default 86400),
# DENY_FETCH=0 (skip the fetch; for a run that must stay offline).
set -eu

deny=${DENY:-cargo deny}
db_dir=${DENY_DB_DIR:-${CARGO_HOME:-$HOME/.cargo}/advisory-dbs}
max_age=${DENY_MAX_AGE_S:-86400}

# Newest fetch time (seconds since the epoch) of any database under $db_dir: the mtime of a clone's FETCH_HEAD,
# which every fetch rewrites, even one that finds nothing new.
newest=
if [ -d "$db_dir" ]; then
    for f in "$db_dir"/*/.git/FETCH_HEAD "$db_dir"/*/FETCH_HEAD; do
        [ -f "$f" ] || continue
        t=$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null) || continue
        if [ -z "$newest" ] || [ "$t" -gt "$newest" ]; then newest=$t; fi
    done
fi
if [ -z "$newest" ]; then
    echo "deny-fresh: no advisory database fetched yet under $db_dir"
else
    age=$(($(date +%s) - newest))
    if [ "$age" -gt "$max_age" ]; then
        echo "deny-fresh: the advisory database was last fetched $((age / 3600)) hours ago (over $((max_age / 3600)) h)"
    fi
fi

if [ "${DENY_FETCH:-1}" = 0 ]; then
    echo "deny-fresh: DENY_FETCH=0, judging on the copy on disk"
else
    # $deny is a command with arguments, meant to split.
    # shellcheck disable=SC2086
    if $deny fetch all; then
        echo "deny-fresh: advisory database and registry index refreshed"
    else
        echo "deny-fresh: warning: the fetch failed; judging on the copy on disk, which may miss a yanked crate or a new advisory" >&2
    fi
fi

# shellcheck disable=SC2086 # $deny is a command plus its arguments, meant to split.
exec $deny "$@"
