#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Installs the pinned real GitHub CLI and Git Credential Manager for the real-tools contract:
#   <dest-dir>/gh/gh[.exe]      <dest-dir>/gcm/git-credential-manager[.exe]
# and prints their versions as `PUDDLE_REAL_GH_VERSION=...` and `PUDDLE_REAL_GCM_VERSION=...` lines, which
# the workflow appends to $GITHUB_ENV (the test fails when the tool on PATH reports another version).
# Each download must match its SHA-256 in ci/real-tools.sha256.
# Usage: ci/fetch-real-tools.sh <dest-dir> <windows|linux>
# GH_BASE_URL replaces https://github.com/cli/cli/releases/download and GCM_BASE_URL
# https://github.com/git-ecosystem/git-credential-manager/releases/download (tests); FETCH_ATTEMPTS and
# FETCH_RETRY_BASE_S set the retry window (ci/fetch-retry.sh). Network failures are retried, a checksum mismatch never is.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
[ "$#" -eq 2 ] || { echo "usage: $0 <dest-dir> <windows|linux>" >&2; exit 2; }
dest=$1
case "$2" in
    windows) gh_kind='_windows_amd64[.]zip$' gcm_kind='^gcm-win-x64-.*[.]zip$' ;;
    linux) gh_kind='_linux_amd64[.]tar[.]gz$' gcm_kind='^gcm-linux-x64-.*[.]tar[.]gz$' ;;
    *) echo "fetch-real-tools: the platform is windows or linux, not $2" >&2; exit 2 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# unpack <archive> <into>: a zip with python (every runner has it, Git Bash has no unzip), a tarball with tar.
unpack() {
    mkdir -p "$2"
    case "$1" in
        *.zip) python3 -I -m zipfile -e "$1" "$2" ;;
        *) tar -xzf "$1" -C "$2" ;;
    esac
}

pin() { # pin <tool> <asset-regex>: prints "<version> <asset> <sha256>"
    awk -v t="$1" -v kind="$2" '!/^#/ && NF >= 4 && $1 == t && $3 ~ kind { print $2, $3, $4; exit }' "$here/real-tools.sha256"
}

fetch() { # fetch <tool> <asset-regex> <base-url> <tag-prefix>: downloads and verifies into $work, prints version
    line=$(pin "$1" "$2")
    [ -n "$line" ] || { echo "fetch-real-tools: nothing pinned for $1 on $platform in ci/real-tools.sha256" >&2; exit 1; }
    # shellcheck disable=SC2086 # the pin line is three words, split on purpose
    set -- $1 $line "$3" "$4"
    version=$2 asset=$3 want=$4 base=$5 prefix=$6
    download fetch-real-tools "$base/$prefix$version/$asset" "$work/$asset" 600
    got=$(sha256sum <"$work/$asset" | cut -d' ' -f1)
    if [ "$got" != "$want" ]; then
        echo "fetch-real-tools: checksum mismatch for $asset: got $got, pinned $want" >&2
        exit 1
    fi
}

platform=$2
fetch gh "$gh_kind" "${GH_BASE_URL:-https://github.com/cli/cli/releases/download}" v
gh_version=$version gh_asset=$asset
fetch gcm "$gcm_kind" "${GCM_BASE_URL:-https://github.com/git-ecosystem/git-credential-manager/releases/download}" v
gcm_version=$version gcm_asset=$asset

mkdir -p "$dest/gh" "$dest/gcm"
unpack "$work/$gh_asset" "$work/gh-x"
unpack "$work/$gcm_asset" "$dest/gcm"
if [ "$platform" = windows ]; then
    cp "$work/gh-x/bin/gh.exe" "$dest/gh/gh.exe"
    [ -f "$dest/gcm/git-credential-manager.exe" ] || { echo "fetch-real-tools: no git-credential-manager.exe in $gcm_asset" >&2; exit 1; }
else
    cp "$work/gh-x/"*/bin/gh "$dest/gh/gh"
    chmod +x "$dest/gh/gh" "$dest/gcm/git-credential-manager"
fi
echo "PUDDLE_REAL_GH_VERSION=$gh_version"
echo "PUDDLE_REAL_GCM_VERSION=$gcm_version"
