#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/fetch-gitleaks.sh with a fake `curl` and `sleep` on PATH (no network, no waiting):
# retries that end well, a download that never works (the cause is named, as a CI annotation
# too), a wrong checksum, and the cache folder.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "fetch-gitleaks-test: FAIL: $1" >&2; exit 1; }

mkdir -p "$tmp/ci" "$tmp/bin" "$tmp/src"
cp "$here/fetch-gitleaks.sh" "$tmp/ci/"
printf '#!/bin/sh\necho fake gitleaks\n' >"$tmp/src/gitleaks"
asset=gitleaks_9.9.9_test.tar.gz
tar -czf "$tmp/$asset" -C "$tmp/src" gitleaks
sum=$(sha256sum <"$tmp/$asset" | cut -d' ' -f1)
# One pin per host the script knows (it picks the line by OS and architecture).
: >"$tmp/ci/gitleaks.sha256"
for target in linux_x64 linux_arm64 darwin_x64 darwin_arm64; do
    echo "9.9.9 gitleaks_9.9.9_$target.tar.gz $sum" >>"$tmp/ci/gitleaks.sha256"
done
host_asset=$(awk -v m="$(uname -m)" -v s="$(uname -s)" 'BEGIN { a = (m ~ /arm64|aarch64/) ? "arm64" : "x64"; o = tolower(s) }
    index($2, "_" o "_" a ".") { print $2; exit }' "$tmp/ci/gitleaks.sha256")
[ -n "$host_asset" ] || fail "this test serves Linux and macOS hosts only"

# The fake curl fails the first $FAKE_CURL_FAILS calls with exit code 28 (a connect timeout),
# then serves the archive (or $FAKE_CURL_BODY when set). Every call is counted in $FAKE_DIR/calls.
cat >"$tmp/bin/curl" <<'STUB'
#!/bin/sh
out=
while [ "$#" -gt 0 ]; do
    case "$1" in -o) out=$2 ;; esac
    shift
done
n=$(($(cat "$FAKE_DIR/calls" 2>/dev/null || echo 0) + 1))
echo "$n" >"$FAKE_DIR/calls"
if [ "$n" -le "$FAKE_CURL_FAILS" ]; then
    echo "curl: (28) Failed to connect to github.com port 443 after 20002 ms" >&2
    exit 28
fi
cp "${FAKE_CURL_BODY:-$FAKE_DIR/$FAKE_ASSET}" "$out"
STUB
cat >"$tmp/bin/sleep" <<'STUB'
#!/bin/sh
echo "$1" >>"$FAKE_DIR/sleeps"
STUB
chmod +x "$tmp/bin/curl" "$tmp/bin/sleep"
cp "$tmp/$asset" "$tmp/$host_asset"
export FAKE_DIR="$tmp" FAKE_ASSET="$host_asset"

# run <name> <curl failures> [env assignments...]: runs the script, leaves out-, err- and rc- files.
run() {
    name=$1 fails=$2
    shift 2
    rm -f "$tmp/calls" "$tmp/sleeps"
    rc=0
    env -u GITHUB_ACTIONS -u GITLEAKS_CACHE_DIR FAKE_CURL_FAILS="$fails" "$@" PATH="$tmp/bin:$PATH" \
        "$tmp/ci/fetch-gitleaks.sh" "$tmp/dest-$name" >"$tmp/out-$name" 2>"$tmp/err-$name" || rc=$?
    echo "$rc" >"$tmp/rc-$name"
}
rc_of() { cat "$tmp/rc-$1"; }
calls() { cat "$tmp/calls" 2>/dev/null || echo 0; }
sleeps() { tr '\n' ' ' <"$tmp/sleeps"; }

# Failures that end: the script retries until the download works, then installs.
run retry 3
[ "$(rc_of retry)" = 0 ] || { cat "$tmp/err-retry" >&2; fail "three failed downloads were not retried to success"; }
[ "$(calls)" = 4 ] || fail "expected 4 curl calls, saw $(calls)"
[ -x "$tmp/dest-retry/gitleaks" ] || fail "no gitleaks installed after the retries"
grep -q "curl exit code 28" "$tmp/err-retry" || fail "a retry does not name the curl exit code"
[ "$(sleeps)" = "10 20 40 " ] || fail "the backoff is not 10, 20, 40 s: $(sleeps)"

# Four failures still fit the budget (five tries, the last one works).
run four 4
[ "$(rc_of four)" = 0 ] || fail "the fifth try was not made"
[ "$(sleeps)" = "10 20 40 60 " ] || fail "the longest wait is not capped at 60 s: $(sleeps)"

# A download that never works: non-zero exit, the cause named on stderr and as an annotation.
run never 99 GITHUB_ACTIONS=true
[ "$(rc_of never)" != 0 ] || fail "an always failing download exited 0"
[ "$(calls)" = 5 ] || fail "expected 5 tries, saw $(calls)"
grep -q "download failed 5 times (last curl exit code 28)" "$tmp/err-never" || fail "stderr does not name the download and the exit code"
grep -q "^::error title=gitleaks fetch::.*download failed 5 times (last curl exit code 28)" "$tmp/out-never" ||
    fail "no ::error annotation naming the download and the exit code"
[ ! -e "$tmp/dest-never/gitleaks" ] || fail "something was installed from a failed download"

# Outside GitHub Actions no annotation is printed.
run quiet 99
if grep -q '^::error' "$tmp/out-quiet"; then fail "an annotation was printed outside GitHub Actions"; fi

# A wrong checksum is fatal at once, never retried, and nothing is unpacked.
printf 'not gitleaks' >"$tmp/wrong"
run wrong 0 FAKE_CURL_BODY="$tmp/wrong" GITHUB_ACTIONS=true
[ "$(rc_of wrong)" != 0 ] || fail "a download with a wrong checksum was accepted"
grep -q "checksum mismatch" "$tmp/err-wrong" || fail "the mismatch is not named"
grep -q "^::error.*checksum mismatch" "$tmp/out-wrong" || fail "the mismatch has no annotation"
[ "$(calls)" = 1 ] || fail "a checksum mismatch was retried"
[ ! -e "$tmp/dest-wrong/gitleaks" ] || fail "a download that failed its checksum was unpacked"

# A miss fills the cache folder; the next run uses it without touching the network.
cache=$tmp/cache
run fill 0 GITLEAKS_CACHE_DIR="$cache"
[ -f "$cache/$host_asset" ] || fail "a verified download was not cached"
run hit 99 GITLEAKS_CACHE_DIR="$cache"
[ "$(rc_of hit)" = 0 ] || { cat "$tmp/err-hit" >&2; fail "a cached archive was not used while github.com was down"; }
[ "$(calls)" = 0 ] || fail "a cache hit still called curl"
[ -x "$tmp/dest-hit/gitleaks" ] || fail "a cache hit installed nothing"

# A cached archive that does not match the pin is ignored (downloaded again), never used.
printf 'tampered' >"$cache/$host_asset"
run bad 0 GITLEAKS_CACHE_DIR="$cache"
[ "$(rc_of bad)" = 0 ] || fail "a bad cache entry stopped the run"
[ "$(calls)" = 1 ] || fail "a bad cache entry was used instead of downloading"
grep -q "ignoring the cached" "$tmp/err-bad" || fail "the bad cache entry is not reported"
[ "$(sha256sum <"$cache/$host_asset" | cut -d' ' -f1)" = "$sum" ] || fail "the bad cache entry was not replaced"
# With the network down too, a bad cache entry fails rather than being installed.
printf 'tampered' >"$cache/$host_asset"
run badnet 99 GITLEAKS_CACHE_DIR="$cache"
[ "$(rc_of badnet)" != 0 ] || fail "a bad cache entry was installed with no network"

echo "fetch-gitleaks-test: ok"
