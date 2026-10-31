# shellcheck shell=sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Sourced (`. "$here/fetch-retry.sh"`) by the ci/fetch-*.sh scripts that download a pinned file.
# download <label> <url> <out> <max-seconds>: curl with a retry window sized for a GitHub download
# outage, which lasts minutes, not seconds (one merge-queue run lost three partitions to
# `curl: (92) ... PROTOCOL_ERROR` after 3 tries in 13 s). Up to FETCH_ATTEMPTS tries (default 8) with a
# doubling pause from FETCH_RETRY_BASE_S (default 5 s, capped at 60 s): 5, 10, 20, 40, 60, 60, 60 s.
# After a stream error (curl 92, 16, 56) the next tries use HTTP/1.1, which does not share the broken
# multiplexed connection. Each failed try prints curl's exit code and message; the last failure prints an
# `::error::` annotation naming them, which stays on the job even when the merge queue cancels the run,
# and exits 1. Every curl failure is retried; a checksum mismatch is the caller's, and never is.

download() {
    _label=$1 _url=$2 _out=$3 _max=$4
    _attempts=${FETCH_ATTEMPTS:-8}
    _pause=${FETCH_RETRY_BASE_S:-5}
    _attempt=1
    _http=
    while :; do
        # $_http is empty or one flag, so it is meant to split.
        # shellcheck disable=SC2086
        if _msg=$(curl -fsSL $_http --connect-timeout 20 --max-time "$_max" -o "$_out" "$_url" 2>&1); then
            return 0
        else
            _code=$?
        fi
        _reason="curl exit $_code: $(printf %s "$_msg" | tr '\n' ' ' | cut -c1-300)"
        if [ "$_attempt" -ge "$_attempts" ]; then
            echo "::error title=$_label download failed::$_attempts tries failed for $_url ($_reason)" >&2
            echo "$_label: download failed $_attempts times: $_url ($_reason)" >&2
            exit 1
        fi
        case $_code in 92 | 16 | 56) _http=--http1.1 ;; esac
        echo "$_label: download attempt $_attempt of $_attempts failed ($_reason); retrying in ${_pause}s (infrastructure retry)" >&2
        sleep "$_pause"
        _pause=$((_pause * 2))
        [ "$_pause" -le 60 ] || _pause=60
        _attempt=$((_attempt + 1))
    done
}
