# shellcheck shell=sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Sourced (`. "$here/fetch-retry.sh"`) by the ci/fetch-*.sh scripts that download a pinned file.
# download <label> <url> <out> <max-seconds>: curl with a retry window sized for a GitHub download
# outage, which lasts minutes, not seconds (one merge-queue run lost three partitions to
# `curl: (92) ... PROTOCOL_ERROR` after 3 tries in 13 s). Up to FETCH_ATTEMPTS tries (default 8) with a
# doubling pause from FETCH_RETRY_BASE_S (default 5 s, capped at 60 s): 5, 10, 20, 40, 60, 60, 60 s.
# After a stream error (curl 92, 16, 56) the next tries use HTTP/1.1, which does not share the broken
# multiplexed connection. Each failed try prints curl's exit code and message; the last failure prints an
# `::error::` annotation naming them, which stays on the job even when the merge queue cancels the run,
# and exits 1. Every curl failure is retried; a checksum mismatch is the caller's, and never is.
# download_from <label> <out> <max-seconds> <url>...: the same, over several hosts that serve the same
# pinned file (upstream first, then a mirror). Each host but the last gets FETCH_SOURCE_ATTEMPTS tries
# (default 3: a host that is down costs 15 s, not the whole window), the last gets FETCH_ATTEMPTS; a
# move to the next host is printed, and only the last failure is an `::error::`. The caller's checksum
# check is what makes any host acceptable, so a mirror adds no trust.
# cache_get <label> <name> <sha256> <out>: when FETCH_CACHE_DIR holds <name> and it hashes to <sha256>, copies it to
# <out> and returns 0; a copy that does not match is named and ignored (return 1, download as usual). CI restores
# that folder from an Actions cache keyed on the pin file, so a run needs no host at all once one run got the file.
# cache_put <name> <file>: keeps a file the caller has just verified in FETCH_CACHE_DIR (nothing when it is unset).

download() {
    _label=$1 _url=$2 _out=$3 _max=$4
    _try "$_label" "$_url" "$_out" "$_max" "${FETCH_ATTEMPTS:-8}" || {
        echo "::error title=$_label download failed::$_attempts tries failed for $_url ($_reason)" >&2
        echo "$_label: download failed $_attempts times: $_url ($_reason)" >&2
        exit 1
    }
}

download_from() {
    _label=$1 _out=$2 _max=$3
    shift 3
    _tried=
    while [ "$#" -gt 0 ]; do
        _url=$1
        shift
        _n=${FETCH_ATTEMPTS:-8}
        [ "$#" -eq 0 ] || _n=${FETCH_SOURCE_ATTEMPTS:-3}
        _try "$_label" "$_url" "$_out" "$_max" "$_n" && return 0
        _tried="$_tried $_url ($_reason);"
        [ "$#" -eq 0 ] || echo "$_label: $_url failed $_attempts times ($_reason); trying the next host $1" >&2
    done
    echo "::error title=$_label download failed::every host failed:$_tried" >&2
    echo "$_label: download failed from every host:$_tried" >&2
    exit 1
}

# _try <label> <url> <out> <max-seconds> <attempts>: 0 on success; 1 with _attempts and _reason set.
_try() {
    _label=$1 _url=$2 _out=$3 _max=$4
    _attempts=$5
    _pause=${FETCH_RETRY_BASE_S:-5}
    _attempt=1
    _http=
    while :; do
        # $_http is empty or one flag, so it is meant to split.
        # shellcheck disable=SC2086
        # fetch-ok: this loop is the retry.
        if _msg=$(curl -fsSL $_http --connect-timeout 20 --max-time "$_max" -o "$_out" "$_url" 2>&1); then
            return 0
        else
            _code=$?
        fi
        _reason="curl exit $_code: $(printf %s "$_msg" | tr '\n' ' ' | cut -c1-300)"
        [ "$_attempt" -lt "$_attempts" ] || return 1
        case $_code in 92 | 16 | 56) _http=--http1.1 ;; esac
        echo "$_label: download attempt $_attempt of $_attempts failed ($_reason); retrying in ${_pause}s (infrastructure retry)" >&2
        sleep "$_pause"
        _pause=$((_pause * 2))
        [ "$_pause" -le 60 ] || _pause=60
        _attempt=$((_attempt + 1))
    done
}

cache_get() {
    [ -n "${FETCH_CACHE_DIR:-}" ] && [ -f "$FETCH_CACHE_DIR/$2" ] || return 1
    if [ "$(sha256sum <"$FETCH_CACHE_DIR/$2" | cut -d' ' -f1)" = "$3" ]; then
        cp "$FETCH_CACHE_DIR/$2" "$4"
        echo "$1: using the cached $2 (checksum matches the pin)" >&2
        return 0
    fi
    echo "$1: ignoring the cached $2 (checksum differs from the pin), downloading" >&2
    return 1
}

cache_put() {
    [ -n "${FETCH_CACHE_DIR:-}" ] || return 0
    mkdir -p "$FETCH_CACHE_DIR"
    cp "$2" "$FETCH_CACHE_DIR/$1"
}
