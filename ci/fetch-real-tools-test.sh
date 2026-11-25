#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/fetch-real-tools.sh against releases served from folders (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# shellcheck disable=SC1091 # the helper sits in scripts/
. "$(dirname "$0")/../scripts/test-fail.sh"

# A gh release and a GCM release as the script expects them (Linux archives; the Windows zips are cut the same way).
mkdir -p "$tmp/gh/v9.9.9" "$tmp/gcm/v8.8.8" "$tmp/stage/gh_9.9.9_linux_amd64/bin" "$tmp/stage/gcm" "$tmp/ci"
printf '#!/bin/sh\necho gh 9.9.9\n' >"$tmp/stage/gh_9.9.9_linux_amd64/bin/gh"
printf '#!/bin/sh\necho 8.8.8\n' >"$tmp/stage/gcm/git-credential-manager"
tar -czf "$tmp/gh/v9.9.9/gh_9.9.9_linux_amd64.tar.gz" -C "$tmp/stage" gh_9.9.9_linux_amd64
tar -czf "$tmp/gcm/v8.8.8/gcm-linux-x64-8.8.8.tar.gz" -C "$tmp/stage/gcm" git-credential-manager
python3 -I - "$tmp" <<'PY'
import sys, zipfile
tmp = sys.argv[1]
with zipfile.ZipFile(f"{tmp}/gh/v9.9.9/gh_9.9.9_windows_amd64.zip", "w") as z:
    z.writestr("bin/gh.exe", "MZ gh")
    z.writestr("LICENSE", "x")
with zipfile.ZipFile(f"{tmp}/gcm/v8.8.8/gcm-win-x64-8.8.8.zip", "w") as z:
    z.writestr("git-credential-manager.exe", "MZ gcm")
PY
sum() { sha256sum <"$1" | cut -d' ' -f1; }

cp "$here/fetch-real-tools.sh" "$here/fetch-retry.sh" "$tmp/ci/"
pins=$tmp/ci/real-tools.sha256
export GH_BASE_URL="file://$tmp/gh" GCM_BASE_URL="file://$tmp/gcm" FETCH_ATTEMPTS=1
write_pins() {
    cat >"$pins" <<PINS
# comment
gh 9.9.9 gh_9.9.9_linux_amd64.tar.gz $(sum "$tmp/gh/v9.9.9/gh_9.9.9_linux_amd64.tar.gz")
gh 9.9.9 gh_9.9.9_windows_amd64.zip $(sum "$tmp/gh/v9.9.9/gh_9.9.9_windows_amd64.zip")
gcm 8.8.8 gcm-linux-x64-8.8.8.tar.gz $(sum "$tmp/gcm/v8.8.8/gcm-linux-x64-8.8.8.tar.gz")
gcm 8.8.8 gcm-win-x64-8.8.8.zip $(sum "$tmp/gcm/v8.8.8/gcm-win-x64-8.8.8.zip")
PINS
}

write_pins
out=$("$tmp/ci/fetch-real-tools.sh" "$tmp/lin" linux 2>/dev/null) || fail "the pinned Linux tools did not install"
[ "$("$tmp/lin/gh/gh")" = "gh 9.9.9" ] || fail "the installed gh is not the download"
[ "$("$tmp/lin/gcm/git-credential-manager")" = 8.8.8 ] || fail "the installed GCM is not the download"
[ "$out" = "PUDDLE_REAL_GH_VERSION=9.9.9
PUDDLE_REAL_GCM_VERSION=8.8.8" ] || fail "the versions are not printed for the workflow: $out"

"$tmp/ci/fetch-real-tools.sh" "$tmp/win" windows >/dev/null 2>&1 || fail "the pinned Windows tools did not install"
[ -f "$tmp/win/gh/gh.exe" ] && [ -f "$tmp/win/gcm/git-credential-manager.exe" ] || fail "the Windows tools are not where the workflow looks"

write_pins
sed -i "s/^gcm 8.8.8 gcm-win-x64-8.8.8.zip .*/gcm 8.8.8 gcm-win-x64-8.8.8.zip $(printf 0 | sha256sum | cut -d' ' -f1)/" "$pins"
if "$tmp/ci/fetch-real-tools.sh" "$tmp/bad" windows 2>"$tmp/err" >/dev/null; then fail "a changed download was accepted"; fi
grep -q "checksum mismatch" "$tmp/err" || fail "the mismatch is not named"
[ ! -e "$tmp/bad/gcm/git-credential-manager.exe" ] || fail "a changed download was installed"

printf 'gh 1.0.0 other.zip %s\n' "$(printf 0 | sha256sum | cut -d' ' -f1)" >"$pins"
if "$tmp/ci/fetch-real-tools.sh" "$tmp/none" windows 2>"$tmp/err" >/dev/null; then fail "a missing pin was accepted"; fi
grep -q "nothing pinned" "$tmp/err" || fail "the missing pin is not named"

if "$tmp/ci/fetch-real-tools.sh" "$tmp/x" macos 2>"$tmp/err" >/dev/null; then fail "an unknown platform was accepted"; fi

# The real pin file: every line is well formed, and the version is in the asset name.
awk '!/^#/ && NF { n++; if (NF != 4 || $3 !~ $2 || $4 !~ /^[0-9a-f]{64}$/) bad = bad $0 "\n" } END { if (n != 4 || bad) { printf "bad pin lines (%d lines):\n%s", n, bad; exit 1 } }' "$here/real-tools.sha256" \
    || fail "the pin file is malformed"
