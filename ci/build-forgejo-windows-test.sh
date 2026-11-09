#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/build-forgejo-windows.sh against a source tarball served from a folder, with a stand-in `go`
# (no network, no compiler): the pin is checked before anything is unpacked or built, and the build is
# asked for the SQLite tags, the pinned version and the output beside the source tree.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "build-forgejo-windows-test: FAIL: $1" >&2; exit 1; }

asset=forgejo-src-9.9.9.tar.gz
mkdir -p "$tmp/site/v9.9.9" "$tmp/ci" "$tmp/tree/forgejo-src-9.9.9" "$tmp/bin"
echo "package main" >"$tmp/tree/forgejo-src-9.9.9/main.go"
tar -czf "$tmp/site/v9.9.9/$asset" -C "$tmp/tree" forgejo-src-9.9.9
sum=$(sha256sum <"$tmp/site/v9.9.9/$asset" | cut -d' ' -f1)

# A `go` that records how it was called and writes the program it was asked for.
cat >"$tmp/bin/go" <<'GO'
#!/bin/sh
echo "$PWD $*" >"$GO_CALLS"
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then shift; echo built >"$1"; break; fi
    shift
done
GO
chmod +x "$tmp/bin/go"
export GO_CALLS="$tmp/calls"

cp "$here/build-forgejo-windows.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/forgejo.sha256
export FORGEJO_BASE_URL="file://$tmp/site"

# The linux asset on the first line is not a source: the script must find the source line.
printf '# comment\n9.9.9 forgejo-9.9.9-linux-amd64 %s\n9.9.9 %s %s\n' "$(printf 1 | sha256sum | cut -d' ' -f1)" "$asset" "$sum" >"$pins"
PATH="$tmp/bin:$PATH" "$tmp/ci/build-forgejo-windows.sh" "$tmp/out" 2>/dev/null || fail "a pinned source did not build"
[ -f "$tmp/out/src/main.go" ] || fail "the source tree was not unpacked beside the program"
[ -f "$tmp/out/forgejo" ] || [ -f "$tmp/out/forgejo.exe" ] || fail "no program was built beside the source tree"
grep -q -- "-tags sqlite sqlite_unlock_notify" "$tmp/calls" || fail "the build has no SQLite tags: $(cat "$tmp/calls")"
grep -q -- "main.Version=9.9.9" "$tmp/calls" || fail "the build does not carry the pinned version: $(cat "$tmp/calls")"

printf '9.9.9 %s %s\n' "$asset" "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
mv "$tmp/calls" "$tmp/calls-before"
if PATH="$tmp/bin:$PATH" "$tmp/ci/build-forgejo-windows.sh" "$tmp/bad" 2>"$tmp/err"; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"
[ ! -e "$tmp/bad" ] || fail "a changed download was unpacked"
[ ! -e "$tmp/calls" ] || fail "a changed download was built"

printf '9.9.9 forgejo-9.9.9-linux-amd64 %s\n' "$sum" >"$pins"
if PATH="$tmp/bin:$PATH" "$tmp/ci/build-forgejo-windows.sh" "$tmp/none" 2>"$tmp/err"; then fail "a missing source pin was accepted"; fi
grep -q "no source pinned" "$tmp/err" || fail "the missing pin is not named"

# The real pin file: a source line for the same version as the first release line, checksum well formed.
line=$(awk '!/^#/ && $2 ~ /^forgejo-src-/ { print; exit }' "$here/forgejo.sha256")
# shellcheck disable=SC2086 # the pin line is three words, split on purpose
set -- $line
[ "$2" = "forgejo-src-$1.tar.gz" ] || fail "the pinned source is not forgejo-src-<version>.tar.gz: $line"
printf %s "$3" | grep -Eq '^[0-9a-f]{64}$' || fail "the pinned checksum is not 64 hex digits: $line"
first=$(awk '!/^#/ && NF >= 3 && $2 !~ /^forgejo-src-/ { print $1; exit }' "$here/forgejo.sha256")
[ "$first" = "$1" ] || fail "the source ($1) and the release ($first) pin different versions"

echo "build-forgejo-windows-test: ok"
