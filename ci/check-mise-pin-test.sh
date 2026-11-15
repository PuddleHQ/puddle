#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/check-mise-pin.sh against two release folders served as files (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "check-mise-pin-test: FAIL: $1" >&2; exit 1; }

asset=mise-v9.9.9-linux-x64-musl.tar.gz
good=$(printf good | sha256sum | cut -d' ' -f1)
other=$(printf other | sha256sum | cut -d' ' -f1)
mkdir -p "$tmp/site/v9.9.9" "$tmp/github/v9.9.9"
printf '%s  ./%s\n%s  ./other.tar.gz\n' "$good" "$asset" "$other" >"$tmp/site/v9.9.9/SHASUMS256.txt"
cp "$tmp/site/v9.9.9/SHASUMS256.txt" "$tmp/github/v9.9.9/SHASUMS256.txt"
export MISE_SITE_URL="file://$tmp/site" MISE_GITHUB_URL="file://$tmp/github" MISE_PIN_FILE="$tmp/pins" FETCH_ATTEMPTS=1

printf '# comment\n9.9.9 %s %s\n' "$asset" "$good" >"$tmp/pins"
"$here/check-mise-pin.sh" >"$tmp/out" 2>&1 || fail "a matching pin failed: $(cat "$tmp/out")"
grep -q "2 checks match" "$tmp/out" || fail "two checks (one per host) were not counted"

printf '9.9.9 %s %s\n' "$asset" "$other" >"$tmp/pins"
if "$here/check-mise-pin.sh" 2>"$tmp/err"; then fail "a wrong hash was accepted"; fi
grep -q "mise.jdx.dev lists $good" "$tmp/err" || fail "the site's mismatch is not named"
grep -q "github.com lists $good" "$tmp/err" || fail "GitHub's mismatch is not named"

# One host agreeing is not enough.
printf '%s  ./%s\n' "$other" "$asset" >"$tmp/github/v9.9.9/SHASUMS256.txt"
printf '9.9.9 %s %s\n' "$asset" "$good" >"$tmp/pins"
if "$here/check-mise-pin.sh" 2>"$tmp/err"; then fail "a pin only one host confirms was accepted"; fi
grep -q "github.com lists $other" "$tmp/err" || fail "GitHub's different hash is not named"
if grep -q "mise.jdx.dev lists" "$tmp/err"; then fail "the agreeing host was reported"; fi

# An asset the release does not list.
printf '9.9.9 mise-v9.9.9-linux-arm64-musl.tar.gz %s\n' "$good" >"$tmp/pins"
if "$here/check-mise-pin.sh" 2>"$tmp/err"; then fail "a missing asset was accepted"; fi
grep -q "does not list mise-v9.9.9-linux-arm64-musl.tar.gz" "$tmp/err" || fail "the missing asset is not named"

# A release that is not there at all.
printf '8.8.8 %s %s\n' "$asset" "$good" >"$tmp/pins"
if "$here/check-mise-pin.sh" 2>"$tmp/err"; then fail "a missing release was accepted"; fi

# A pin file with no pins checks nothing, which is a failure.
printf '# nothing\n' >"$tmp/pins"
if "$here/check-mise-pin.sh" 2>"$tmp/err"; then fail "an empty pin file passed"; fi
grep -q "nothing was checked" "$tmp/err" || fail "the empty pin file is not named"

# The real pin file is well formed: three fields, a 64-digit hash, the same version on every line.
awk '!/^#/ && NF { n++; if (NF != 3 || length($3) != 64 || $3 !~ /^[0-9a-f]+$/ || index($2, "v" $1) == 0) bad = 1 } END { exit (n == 0 || bad) }' "$here/mise.sha256" || fail "ci/mise.sha256 is malformed"
echo "check-mise-pin-test: ok"
