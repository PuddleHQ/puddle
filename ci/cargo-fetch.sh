#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# `cargo fetch --locked` with a retry window, for the CI jobs that start with an empty cargo home.
# Cargo's own `net.retry` (.cargo/config.toml) covers a failed HTTP request, but a git dependency
# that has submodules (microsandbox, with its `skills` submodule) is fetched by one `git fetch`
# per repository, and a TLS error there ends the build at once: the msvc cross-check failed on
# `failed to fetch submodule skills ... SSL error` (2026-11-18, merge queue of #1164) and passed on
# the rerun. Up to CARGO_FETCH_ATTEMPTS tries (default 5), pausing 5, 10, 20, 40 s
# (FETCH_RETRY_BASE_S sets the first pause). Every failed try prints cargo's output; the last one
# prints a CI `::error::` annotation, which stays on the job when the merge queue cancels the run.
set -eu

attempts=${CARGO_FETCH_ATTEMPTS:-5}
pause=${FETCH_RETRY_BASE_S:-5}
attempt=1
while :; do
    if out=$(cargo fetch --locked 2>&1); then
        [ "$attempt" -eq 1 ] || echo "cargo fetch: ok on try $attempt of $attempts"
        exit 0
    fi
    reason=$(printf %s "$out" | tr '\n' ' ' | cut -c1-300)
    printf '%s\n' "$out" >&2
    if [ "$attempt" -ge "$attempts" ]; then
        echo "::error title=cargo fetch failed::$attempts tries failed ($reason)" >&2
        exit 1
    fi
    echo "cargo fetch: try $attempt of $attempts failed; retrying in ${pause}s (infrastructure retry)" >&2
    sleep "$pause"
    pause=$((pause * 2))
    attempt=$((attempt + 1))
done
