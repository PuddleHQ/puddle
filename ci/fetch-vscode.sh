#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puts the pinned desktop VS Code (Windows x64 zip) and its Remote-SSH package in <dest-dir>, for the desktop VS Code VM
# tests (PUDDLE_VSCODE_DIR): <dest-dir>/app/Code.exe and <dest-dir>/remote-ssh.vsix. Each download must match the SHA-256
# pinned in ci/vscode.sha256. The release is not made portable (no `data` folder): the tests pass their own user-data and
# extensions folders.
# Usage: ci/fetch-vscode.sh <dest-dir>
# FETCH_ATTEMPTS and FETCH_RETRY_BASE_S set the retry window (ci/fetch-retry.sh). Works in Linux sh and Git Bash.
# Network failures are retried (infrastructure) and each retry is printed; a checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -eq 1 ] || { echo "usage: $0 <dest-dir>" >&2; exit 2; }
dest=$1

# pin <name>: sets $version, $url and $want from the pinned line.
pin() {
    line=$(awk -v n="$1" '!/^#/ && NF >= 4 && $1 == n { print; exit }' "$here/vscode.sha256")
    [ -n "$line" ] || { echo "fetch-vscode: nothing pinned for $1 in ci/vscode.sha256" >&2; exit 1; }
    # shellcheck disable=SC2086 # four words, split on purpose
    set -- $line
    version=$2 url=$3 want=$4
}

if [ -e "$dest" ] && [ -n "$(ls -A "$dest" 2>/dev/null)" ]; then
    echo "fetch-vscode: $dest exists and is not empty" >&2
    exit 1
fi
mkdir -p "$dest"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# fetch <name> <file>: downloads the pinned file to $work/<file> and checks it.
fetch() {
    pin "$1"
    download fetch-vscode "$url" "$work/$2.raw" 900
    # The Marketplace answers with Content-Encoding: gzip, which curl (no --compressed) saves as it came.
    if [ "$(head -c 2 "$work/$2.raw" | od -An -tx1 | tr -d ' \n')" = 1f8b ]; then
        gzip -dc <"$work/$2.raw" >"$work/$2"
    else
        mv "$work/$2.raw" "$work/$2"
    fi
    # Hash from stdin: for a path with backslashes, Git Bash's sha256sum puts a backslash before the hash.
    got=$(sha256sum <"$work/$2" | cut -d' ' -f1)
    if [ "$got" != "$want" ]; then
        echo "fetch-vscode: checksum mismatch for $1 $version: got $got, pinned $want" >&2
        exit 1
    fi
}

# unpack <zip> <dir>: unzip where there is one, PowerShell's Expand-Archive on a Windows runner without it.
unpack() {
    if command -v unzip >/dev/null 2>&1; then
        unzip -q "$1" -d "$2"
    else
        powershell.exe -NoProfile -NonInteractive -Command \
            "Expand-Archive -LiteralPath '$(cygpath -w "$1")' -DestinationPath '$(cygpath -w "$2")' -Force"
    fi
}

fetch vscode vscode.zip
vscode_version=$version
vscode_sum=$got
unpack "$work/vscode.zip" "$dest/app"
[ -f "$dest/app/Code.exe" ] || { echo "fetch-vscode: the release has no Code.exe" >&2; exit 1; }
[ -f "$dest/app/bin/code.cmd" ] || { echo "fetch-vscode: the release has no bin/code.cmd" >&2; exit 1; }
fetch remote-ssh remote-ssh.vsix
cp "$work/remote-ssh.vsix" "$dest/remote-ssh.vsix"
printf '{\n  "vscode": "%s",\n  "vscode_sha256": "%s",\n  "remote_ssh": "%s",\n  "remote_ssh_sha256": "%s"\n}\n' \
    "$vscode_version" "$vscode_sum" "$version" "$got" >"$dest/vscode.json"
echo "fetch-vscode: VS Code $vscode_version and Remote-SSH $version verified, installed in $dest" >&2
