#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/fetch-msb.sh's git environment: run with the variables a git hook exports
# (GIT_DIR, GIT_INDEX_FILE, GIT_WORK_TREE), the script's own `git clone` must not see them, or it
# would act on the calling repository. A copy of the script runs on Linux x86_64 with a stub `curl`
# (writes a file whose SHA-256 is pinned in a scratch pin list) and a stub `git` that records the
# GIT_* variables it receives and then fails. Not seen: the real download and build, other hosts
# (the script's Windows and upstream-release paths run no git).
set -eu
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || {
    echo "fetch-msb-test: skipped (Linux x86_64 only)"
    exit 0
}
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fetch-msb-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/ci" "$tmp/bin"
cp "$here/fetch-msb.sh" "$here/fetch-retry.sh" "$tmp/ci/"
# On a hosted runner the source-build path installs libcap-ng through these helpers first; the test
# must not touch apt or sudo, so the copies are no-ops (GITHUB_ACTIONS is set there, unset locally).
for helper in apt-setup.sh apt-retry.sh; do
    printf '#!/bin/sh\nexit 0\n' >"$tmp/ci/$helper"
    chmod +x "$tmp/ci/$helper"
done
sum=$(printf 'x' | sha256sum | cut -d' ' -f1)
{
    echo "v1.0.0-puddle.1 source-commit 0000000000000000000000000000000000000001"
    echo "v1.0.0 libkrunfw-linux-x86_64.so $sum"
} >"$tmp/ci/msb-runtime.sha256"
# curl <flags> -o <out> <url>: writes the file the pin list expects.
cat >"$tmp/bin/curl" <<'STUB'
#!/bin/sh
while [ "$#" -gt 0 ]; do
    [ "$1" != -o ] || { printf 'x' >"$2"; exit 0; }
    shift
done
exit 1
STUB
# git: `rev-parse --local-env-vars` answers like git; everything else records its GIT_* and fails.
cat >"$tmp/bin/git" <<'STUB'
#!/bin/sh
if [ "$1" = rev-parse ] && [ "$2" = --local-env-vars ]; then
    exec "$REAL_GIT" "$@"
fi
env | grep '^GIT_' >>"$GIT_SEEN" || true
echo "git $*" >>"$GIT_SEEN"
exit 1
STUB
chmod +x "$tmp/bin/curl" "$tmp/bin/git"
REAL_GIT=$(command -v git)
export REAL_GIT
: >"$tmp/seen"
GIT_SEEN=$tmp/seen
export GIT_SEEN
if PATH="$tmp/bin:$PATH" GIT_DIR=/nonexistent/.git GIT_INDEX_FILE=/nonexistent/index \
    GIT_WORK_TREE=/nonexistent "$tmp/ci/fetch-msb.sh" v1.0.0-puddle.1 "$tmp/dest" >"$tmp/out" 2>&1; then
    echo "fetch-msb-test: the stub git should have failed the script" >&2
    exit 1
fi
grep -q '^git clone' "$tmp/seen" || {
    cat "$tmp/out" >&2
    echo "fetch-msb-test: the script never reached its git clone" >&2
    exit 1
}
# Only the repository variables count (GIT_EDITOR, GIT_ASKPASS and the test's own GIT_SEEN stay).
"$REAL_GIT" rev-parse --local-env-vars | sed 's/$/=/' >"$tmp/local"
if grep -qF -f "$tmp/local" "$tmp/seen"; then
    echo "fetch-msb-test: git received inherited repository variables:" >&2
    grep -F -f "$tmp/local" "$tmp/seen" >&2
    exit 1
fi
echo "fetch-msb-test: ok (git clone saw no inherited GIT_* variables)"

# A tag with a pinned Linux msb: download, verify, install; git must not run at all.
msb_body='#!/bin/sh
echo "msb 1.0.0-puddle.2"'
msb_sum=$(printf '%s\n' "$msb_body" | sha256sum | cut -d' ' -f1)
fw_sum=$(printf 'fw' | sha256sum | cut -d' ' -f1)
{
    echo "v1.0.0-puddle.2 msb-linux-x86_64 $msb_sum"
    echo "v1.0.0-puddle.2 libkrunfw-linux-x86_64.so $fw_sum"
    echo "v1.0.0-puddle.2 libkrunfw-linux-version 5.6.1"
} >"$tmp/ci/msb-runtime.sha256"
cat >"$tmp/bin/curl" <<'STUB'
#!/bin/sh
out= url=
while [ "$#" -gt 0 ]; do
    case "$1" in -o) out=$2 ;; http*) url=$1 ;; esac
    shift
done
case "$url" in
*/msb-linux-x86_64) printf '%s\n' "$MSB_BODY" >"$out" ;;
*/libkrunfw-linux-x86_64.so) printf 'fw' >"$out" ;;
*) exit 1 ;;
esac
STUB
: >"$tmp/seen"
export MSB_BODY="$msb_body"
PATH="$tmp/bin:$PATH" "$tmp/ci/fetch-msb.sh" v1.0.0-puddle.2 "$tmp/prebuilt" >"$tmp/out2" 2>&1 || {
    cat "$tmp/out2" >&2
    echo "fetch-msb-test: the pinned Linux msb was not installed" >&2
    exit 1
}
[ ! -s "$tmp/seen" ] || { echo "fetch-msb-test: a pinned Linux msb ran git" >&2; exit 1; }
[ "$("$tmp/prebuilt/msb" --version)" = "msb 1.0.0-puddle.2" ] || { echo "fetch-msb-test: installed msb is not the download" >&2; exit 1; }
[ "$(cat "$tmp/prebuilt/libkrunfw.so.5.6.1")" = fw ] && [ "$(cat "$tmp/prebuilt/libkrunfw.so.5")" = fw ] || {
    echo "fetch-msb-test: firmware not installed under both names" >&2
    exit 1
}
sed -i "s/ msb-linux-x86_64 .*/ msb-linux-x86_64 $(printf 'z' | sha256sum | cut -d' ' -f1)/" "$tmp/ci/msb-runtime.sha256"
if PATH="$tmp/bin:$PATH" "$tmp/ci/fetch-msb.sh" v1.0.0-puddle.2 "$tmp/bad" >"$tmp/out3" 2>&1; then
    echo "fetch-msb-test: a wrong checksum for the Linux msb was accepted" >&2
    exit 1
fi
grep -q 'checksum mismatch for v1.0.0-puddle.2 msb-linux-x86_64' "$tmp/out3" || {
    cat "$tmp/out3" >&2
    echo "fetch-msb-test: no checksum mismatch message" >&2
    exit 1
}
echo "fetch-msb-test: ok (a pinned Linux msb is downloaded and checked, no git)"

# A download that fails at first (a stub curl: exit 92, the stream error of a GitHub outage) is retried
# with the shared window, and one that never works stops after FETCH_ATTEMPTS tries with an annotation.
# A stub `sleep` keeps the pauses out of the test run.
sed -i "s/ msb-linux-x86_64 .*/ msb-linux-x86_64 $msb_sum/" "$tmp/ci/msb-runtime.sha256"
cat >"$tmp/bin/sleep" <<'STUB'
#!/bin/sh
echo "$1" >>"$FAKE_SLEEPS"
STUB
cat >"$tmp/bin/curl" <<'STUB'
#!/bin/sh
out= url=
while [ "$#" -gt 0 ]; do
    case "$1" in -o) out=$2 ;; http*) url=$1 ;; esac
    shift
done
n=$(($(cat "$FAKE_CALLS") + 1))
echo "$n" >"$FAKE_CALLS"
if [ "$n" -le "$FAKE_FAILS" ]; then
    echo "curl: (92) HTTP/2 stream 1 was not closed cleanly: PROTOCOL_ERROR" >&2
    exit 92
fi
case "$url" in
*/msb-linux-x86_64) printf '%s\n' "$MSB_BODY" >"$out" ;;
*/libkrunfw-linux-x86_64.so) printf 'fw' >"$out" ;;
*) exit 1 ;;
esac
STUB
chmod +x "$tmp/bin/sleep" "$tmp/bin/curl"
export FAKE_SLEEPS="$tmp/sleeps" FAKE_CALLS="$tmp/calls"
: >"$FAKE_SLEEPS"
echo 0 >"$FAKE_CALLS"
FAKE_FAILS=2 PATH="$tmp/bin:$PATH" "$tmp/ci/fetch-msb.sh" v1.0.0-puddle.2 "$tmp/retried" >"$tmp/out4" 2>&1 || {
    cat "$tmp/out4" >&2
    echo "fetch-msb-test: a download that fails twice was not retried to success" >&2
    exit 1
}
[ "$(tr '\n' ' ' <"$FAKE_SLEEPS")" = "5 10 " ] || { echo "fetch-msb-test: the pauses are not 5, 10 s" >&2; exit 1; }
grep -q 'download attempt 1 of 8 failed (curl exit 92: curl: (92)' "$tmp/out4" || {
    echo "fetch-msb-test: a failed try does not name curl's exit code and message" >&2
    exit 1
}
echo 0 >"$FAKE_CALLS"
if FAKE_FAILS=99 FETCH_ATTEMPTS=3 PATH="$tmp/bin:$PATH" "$tmp/ci/fetch-msb.sh" v1.0.0-puddle.2 "$tmp/never" >"$tmp/out5" 2>&1; then
    echo "fetch-msb-test: a download that never works was accepted" >&2
    exit 1
fi
[ "$(cat "$FAKE_CALLS")" = 3 ] || { echo "fetch-msb-test: expected exactly 3 tries, saw $(cat "$FAKE_CALLS")" >&2; exit 1; }
grep -q '^::error title=fetch-msb download failed::3 tries failed for .*curl exit 92' "$tmp/out5" || {
    cat "$tmp/out5" >&2
    echo "fetch-msb-test: the final failure has no annotation naming curl's exit code" >&2
    exit 1
}
echo "fetch-msb-test: ok (a failing download is retried, then named in an annotation)"

# The tag the build is on must pin its release's Linux msb and firmware. Without the msb line
# fetch-msb.sh falls back to a ten-minute build from source, in the build job and in each of the
# four test partitions, and only a warning annotation says so.
tag=$("$here/msb-tag.sh")
for asset in msb-linux-x86_64 libkrunfw-linux-x86_64.so; do
    awk -v t="$tag" -v a="$asset" '$1 == t && $2 == a { found = 1 } END { exit !found }' "$here/msb-runtime.sha256" || {
        echo "fetch-msb-test: $tag has no $asset line in ci/msb-runtime.sha256: every VM job would build msb from source (about 10 minutes); pin the release's asset" >&2
        exit 1
    }
done
echo "fetch-msb-test: ok ($tag pins its Linux msb and firmware)"
