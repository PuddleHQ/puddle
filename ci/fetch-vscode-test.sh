#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/fetch-vscode.sh against a small release and a gzip-coded package served from a folder (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
. "$(dirname "$0")/../scripts/test-fail.sh"

# A release the way upstream packs it: Code.exe at the top and the command line tool under bin.
mkdir -p "$tmp/site"
python3 -I - "$tmp/site" <<'PY'
import sys, zipfile
site = sys.argv[1]
with zipfile.ZipFile(site + "/vscode.zip", "w") as z:
    z.writestr("Code.exe", "exe")
    z.writestr("bin/code.cmd", "cmd")
with zipfile.ZipFile(site + "/ssh.vsix", "w") as z:
    z.writestr("extension/package.json", "{}")
PY
# The Marketplace serves the package gzip-coded, which curl without --compressed saves as it came.
gzip -c <"$tmp/site/ssh.vsix" >"$tmp/site/ssh.vsix.gz"
zsum=$(sha256sum <"$tmp/site/vscode.zip" | cut -d' ' -f1)
vsum=$(sha256sum <"$tmp/site/ssh.vsix" | cut -d' ' -f1)
zero=$(printf 0 | sha256sum | cut -d' ' -f1)

# The script reads ci/vscode.sha256 beside itself: run a copy next to a test pin list.
mkdir -p "$tmp/ci"
cp "$here/fetch-vscode.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/vscode.sha256
export FETCH_RETRY_BASE_S=0 FETCH_ATTEMPTS=2

pin_both() { printf '# comment\nvscode 9.9.9 file://%s/vscode.zip %s\nremote-ssh 1.2.3 file://%s/%s %s\n' "$tmp/site" "$1" "$tmp/site" "$2" "$3" >"$pins"; }

for package in ssh.vsix ssh.vsix.gz; do
    out=$tmp/out-$package
    pin_both "$zsum" "$package" "$vsum"
    "$tmp/ci/fetch-vscode.sh" "$out" 2>/dev/null || fail "pinned files ($package) did not install"
    [ -f "$out/app/Code.exe" ] && [ -f "$out/app/bin/code.cmd" ] || fail "release files missing ($package)"
    cmp -s "$out/remote-ssh.vsix" "$tmp/site/ssh.vsix" || fail "the package was not decoded to the zip itself ($package)"
    grep -q '"vscode": "9.9.9"' "$out/vscode.json" || fail "no VS Code version in vscode.json ($package)"
    grep -q '"remote_ssh": "1.2.3"' "$out/vscode.json" || fail "no Remote-SSH version in vscode.json ($package)"
    grep -q "$zsum" "$out/vscode.json" || fail "no release checksum in vscode.json ($package)"
    grep -q "$vsum" "$out/vscode.json" || fail "no package checksum in vscode.json ($package)"
done

"$tmp/ci/fetch-vscode.sh" "$tmp/out-ssh.vsix" 2>/dev/null && fail "a folder with content was overwritten"
"$tmp/ci/fetch-vscode.sh" 2>/dev/null && fail "a missing destination was accepted"

pin_both "$zero" ssh.vsix "$vsum"
if "$tmp/ci/fetch-vscode.sh" "$tmp/bad" 2>"$tmp/err"; then fail "a changed release was accepted"; fi
grep -q "checksum mismatch for vscode 9.9.9" "$tmp/err" || fail "the release mismatch is not named"

pin_both "$zsum" ssh.vsix "$zero"
if "$tmp/ci/fetch-vscode.sh" "$tmp/bad2" 2>"$tmp/err"; then fail "a changed package was accepted"; fi
grep -q "checksum mismatch for remote-ssh 1.2.3" "$tmp/err" || fail "the package mismatch is not named"

printf 'vscode 9.9.9 file://%s/vscode.zip %s\n' "$tmp/site" "$zsum" >"$pins"
if "$tmp/ci/fetch-vscode.sh" "$tmp/none" 2>"$tmp/err"; then fail "a missing pin was accepted"; fi
grep -q "nothing pinned for remote-ssh" "$tmp/err" || fail "the missing pin is not named"

printf 'vscode 9.9.9 file://%s/missing.zip %s\n' "$tmp/site" "$zsum" >"$pins"
if "$tmp/ci/fetch-vscode.sh" "$tmp/gone" 2>"$tmp/err"; then fail "a download that fails was accepted"; fi
grep -q '^::error title=fetch-vscode download failed::2 tries failed' "$tmp/err" || fail "the failed download has no annotation"

# The cache (FETCH_CACHE_DIR) keeps the decoded, verified files; a run with every host gone installs from it.
pin_both "$zsum" ssh.vsix.gz "$vsum"
FETCH_CACHE_DIR=$tmp/cache "$tmp/ci/fetch-vscode.sh" "$tmp/c1" 2>/dev/null || fail "a cached run did not install"
mv "$tmp/site" "$tmp/site-gone"
FETCH_CACHE_DIR=$tmp/cache "$tmp/ci/fetch-vscode.sh" "$tmp/c2" 2>"$tmp/err" || fail "the cache was not used with the host down: $(cat "$tmp/err")"
[ "$(grep -c 'using the cached' "$tmp/err")" -eq 2 ] || fail "not both files came from the cache: $(cat "$tmp/err")"
cmp -s "$tmp/c2/remote-ssh.vsix" "$tmp/site-gone/ssh.vsix" || fail "the cached package is not the decoded zip"
[ -f "$tmp/c2/app/Code.exe" ] || fail "the cached release was not unpacked"
mv "$tmp/site-gone" "$tmp/site"

echo "fetch-vscode-test: ok"
