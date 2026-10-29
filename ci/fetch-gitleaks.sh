#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Puts the pinned gitleaks release for this host in <dest-dir> (gitleaks, or gitleaks.exe on
# Windows); the secrets gate (scripts/check-secrets.sh) needs it on PATH. The download must match
# the SHA-256 pinned in ci/gitleaks.sha256. Usage: ci/fetch-gitleaks.sh <dest-dir>
# Works in Linux sh, macOS sh and Git Bash on Windows. Network failures are retried
# (infrastructure) and each retry is printed; a checksum mismatch never is.
# GITLEAKS_CACHE_DIR (optional): a folder CI restores between runs. A cached archive is hashed
# against the pin like a download, so the pin stays the only trust; a bad one is ignored.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
[ "$#" -eq 1 ] || { echo "usage: $0 <dest-dir>" >&2; exit 2; }
dest=$1

case "$(uname -m)" in
x86_64 | amd64) arch=x64 ;;
arm64 | aarch64) arch=arm64 ;;
*) echo "fetch-gitleaks: unsupported arch $(uname -m)" >&2; exit 2 ;;
esac
case "$(uname -s)" in
Linux) os=linux ext=tar.gz exe=gitleaks ;;
Darwin) os=darwin ext=tar.gz exe=gitleaks ;;
MINGW* | MSYS* | CYGWIN*) os=windows ext=zip exe=gitleaks.exe ;;
*) echo "fetch-gitleaks: unsupported host $(uname -s)" >&2; exit 2 ;;
esac

# The pinned line for this host: `<version> <asset> <sha256>`.
line=$(awk -v s="_${os}_${arch}.${ext}" '!/^#/ && index($2, s) { print; exit }' "$here/gitleaks.sha256")
[ -n "$line" ] || { echo "fetch-gitleaks: nothing pinned for ${os}_${arch} in ci/gitleaks.sha256" >&2; exit 1; }
version=${line%% *}
rest=${line#* }
asset=${rest%% *}
want=${rest##* }

mkdir -p "$dest"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
url="https://github.com/gitleaks/gitleaks/releases/download/v$version/$asset"

# Hash from stdin: for a path with backslashes, Git Bash's sha256sum puts a backslash before the hash.
hash_of() { sha256sum < "$1" | cut -d' ' -f1; }
# The annotation is what the check page shows; without it the step reads only `exit code 1`.
fail() {
    echo "fetch-gitleaks: $1" >&2
    [ "${GITHUB_ACTIONS:-}" != true ] || echo "::error title=gitleaks fetch::$1"
    exit 1
}

archive=$work/$asset
cached=
if [ -n "${GITLEAKS_CACHE_DIR:-}" ] && [ -f "$GITLEAKS_CACHE_DIR/$asset" ]; then
    if [ "$(hash_of "$GITLEAKS_CACHE_DIR/$asset")" = "$want" ]; then
        cp "$GITLEAKS_CACHE_DIR/$asset" "$archive"
        cached=1
        echo "fetch-gitleaks: using the cached $asset (checksum matches the pin)" >&2
    else
        echo "fetch-gitleaks: ignoring the cached $asset (checksum differs from the pin), downloading" >&2
    fi
fi

if [ -z "$cached" ]; then
    # github.com has refused connections from one runner for over a minute (3 tries, 20 s apiece,
    # failed a queue run), and a runner that cannot connect now usually can in a few minutes, so
    # wait longer between tries: 10, 20, 40 and 60 s, about 4 minutes in all with the timeouts.
    tries=5
    attempt=1
    while :; do
        rc=0
        curl -fsSL --connect-timeout 20 --max-time 300 -o "$archive" "$url" || rc=$?
        [ "$rc" -ne 0 ] || break
        [ "$attempt" -lt "$tries" ] || fail "download failed $tries times (last curl exit code $rc): $url"
        delay=$((10 * (1 << (attempt - 1))))
        [ "$delay" -le 60 ] || delay=60
        echo "fetch-gitleaks: download attempt $attempt failed (curl exit code $rc), retrying in ${delay}s (infrastructure retry)" >&2
        attempt=$((attempt + 1))
        sleep "$delay"
    done
fi
got=$(hash_of "$archive")
[ "$got" = "$want" ] || fail "checksum mismatch for $asset: got $got, pinned $want"
if [ -z "$cached" ] && [ -n "${GITLEAKS_CACHE_DIR:-}" ]; then
    mkdir -p "$GITLEAKS_CACHE_DIR"
    cp "$archive" "$GITLEAKS_CACHE_DIR/$asset"
fi
if [ "$ext" = zip ]; then
    # Git for Windows may lack unzip; Windows PowerShell always has Expand-Archive.
    if command -v unzip >/dev/null 2>&1; then
        unzip -q -o "$work/$asset" "$exe" -d "$dest"
    else
        powershell.exe -NoProfile -Command "Expand-Archive -Force -LiteralPath '$(cygpath -w "$work/$asset")' -DestinationPath '$(cygpath -w "$work")\\x'"
        cp "$work/x/$exe" "$dest/$exe"
    fi
else
    tar -xzf "$work/$asset" -C "$dest" "$exe"
fi
chmod +x "$dest/$exe"
echo "fetch-gitleaks: $asset verified ($got), installed $dest/$exe" >&2
