#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Puts the pinned code-server release in <dest-dir>, for the VM tests (PUDDLE_CODE_SERVER_DIR): the
# folder the host mounts read-only into guests. The download must match the SHA-256 pinned in
# ci/code-server.sha256. `cargo xtask code-server` builds the same folder for a release; this
# script needs no Rust build.
# Usage: ci/fetch-code-server.sh <dest-dir> [<version>]    version defaults to the first pinned one
# CODE_SERVER_BASE_URL replaces https://github.com/coder/code-server/releases/download (tests);
# CODE_SERVER_ATTEMPTS and CODE_SERVER_RETRY_BASE_S set the retry window (below).
# Works in Linux sh and Git Bash. Network failures are retried (infrastructure) and each retry is
# printed; a checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: $0 <dest-dir> [<version>]" >&2; exit 2; }
dest=$1
version=${2:-}

# The pinned line: `<version> <asset> <sha256>`.
line=$(awk -v v="$version" '!/^#/ && NF >= 3 && (v == "" || $1 == v) { print; exit }' "$here/code-server.sha256")
[ -n "$line" ] || { echo "fetch-code-server: nothing pinned for ${version:-any version} in ci/code-server.sha256" >&2; exit 1; }
version=${line%% *}
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

if [ -e "$dest" ] && [ -n "$(ls -A "$dest" 2>/dev/null)" ]; then
    echo "fetch-code-server: $dest exists and is not empty" >&2
    exit 1
fi
mkdir -p "$dest"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
base=${CODE_SERVER_BASE_URL:-https://github.com/coder/code-server/releases/download}
url="$base/v$version/$asset"
# A GitHub download outage lasts minutes, not seconds (one merge-queue run lost three partitions to
# `curl: (92) ... PROTOCOL_ERROR` after 3 tries in 13 s): up to CODE_SERVER_ATTEMPTS tries (default 8) with
# a doubling pause from CODE_SERVER_RETRY_BASE_S (default 5 s, capped at 60 s), about 4 minutes in all.
# After a stream error (curl 92, 16, 56) the next tries use HTTP/1.1, which does not share the broken
# multiplexed connection. Each failed try prints curl's exit code and message; the last failure names them
# in an `::error::` annotation, which stays on the job even when the merge queue cancels the run.
attempts=${CODE_SERVER_ATTEMPTS:-8}
pause=${CODE_SERVER_RETRY_BASE_S:-5}
attempt=1
http=
while :; do
    # $http is empty or one flag, so it is meant to split.
    # shellcheck disable=SC2086
    if curl -fsSL $http --connect-timeout 20 --max-time 600 -o "$work/$asset" "$url" 2>"$work/curl.err"; then
        break
    else
        code=$?
    fi
    reason="curl exit $code: $(tr '\n' ' ' <"$work/curl.err" | cut -c1-300)"
    if [ "$attempt" -ge "$attempts" ]; then
        echo "::error title=code-server download failed::$attempts tries failed for $url ($reason)" >&2
        echo "fetch-code-server: download failed $attempts times: $url ($reason)" >&2
        exit 1
    fi
    case $code in 92 | 16 | 56) http=--http1.1 ;; esac
    echo "fetch-code-server: download attempt $attempt of $attempts failed ($reason); retrying in ${pause}s (infrastructure retry)" >&2
    sleep "$pause"
    pause=$((pause * 2))
    [ "$pause" -le 60 ] || pause=60
    attempt=$((attempt + 1))
done
# Hash from stdin: for a path with backslashes, Git Bash's sha256sum puts a backslash before the hash.
got=$(sha256sum < "$work/$asset" | cut -d' ' -f1)
if [ "$got" != "$want" ]; then
    echo "fetch-code-server: checksum mismatch for $asset: got $got, pinned $want" >&2
    exit 1
fi
# Without the release's top folder, and without the command shims of node_modules/.bin (the only
# symbolic links in it; nothing uses them at run time).
tar -xzf "$work/$asset" -C "$dest" --strip-components=1 --exclude='*/node_modules/.bin'
for f in bin/code-server lib/node package.json LICENSE; do
    [ -f "$dest/$f" ] || { echo "fetch-code-server: $asset has no $f" >&2; exit 1; }
done
printf '{\n  "version": "%s",\n  "asset": "%s",\n  "sha256": "%s"\n}\n' "$version" "$asset" "$got" >"$dest/code-server.json"
echo "fetch-code-server: $asset verified ($got), installed in $dest" >&2
