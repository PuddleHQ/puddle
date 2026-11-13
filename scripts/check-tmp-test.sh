#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of the private temp dir of scripts/check.sh with a stand-in cargo (no gate runs for real): the
# stand-in writes a 1 GB file to the temp dir it is given, the way a test fixture or a build step would. The
# stand-in for the system temp dir is a directory of the test (the caller's TMPDIR); after a passing run, after a
# failing run and after a run killed by SIGTERM it must hold nothing new, the 1 GB file must be gone, and the
# file must have sat under the target dir (CARGO_TARGET_DIR) or CHECK_TMPDIR while the run lasted.
# Not seen here: which real tools ignore TMPDIR (see the temp dir note in scripts/check.sh).
set -eu
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
# The caller (check.sh, CI, a host run) sets these; each case sets its own.
unset CARGO_TARGET_DIR CHECK_TMPDIR
here=$(cd "$(dirname "$0")" && pwd)
fail() {
    echo "check-tmp-test: $*" >&2
    exit 1
}
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
sys="$root/system-tmp"
mkdir -p "$sys" "$root/repo/scripts"
cp "$here/check.sh" "$here/gate-sched.sh" "$root/repo/scripts/"
cat >"$root/fake-cargo" <<'F'
#!/bin/sh
# Stand-in for `cargo fmt`: records TMPDIR, writes 1 GB into it, then does what FAKE_MODE says.
echo "$TMPDIR" >"$FAKE_SEEN"
dd if=/dev/zero of="$TMPDIR/big.bin" bs=1048576 count=1024 2>/dev/null
echo "$(wc -c <"$TMPDIR/big.bin")" >>"$FAKE_SEEN"
case ${FAKE_MODE:-pass} in
fail) exit 1 ;;
hang) kill -TERM "$PPID"; sleep 5 ;;
esac
F
chmod +x "$root/fake-cargo"

run() { # run <mode> [env...]: runs the fmt gate with the stand-in; prints nothing, returns its status
    mode=$1
    shift
    : >"$root/seen"
    (cd "$root/repo" && env "$@" TMPDIR="$sys" CARGO="$root/fake-cargo" FAKE_SEEN="$root/seen" FAKE_MODE="$mode" \
        scripts/check.sh fmt >/dev/null 2>&1)
}
check_clean() { # check_clean <label> <target dir>
    [ -z "$(ls -A "$sys")" ] || fail "$1: the system temp dir must stay empty, holds $(ls "$sys")"
    used=$(sed -n 1p "$root/seen")
    [ "$(sed -n 2p "$root/seen")" = 1073741824 ] || fail "$1: the stand-in did not write its 1 GB file"
    case $used in "$2"/chk.*) ;; *) fail "$1: TMPDIR must be a chk dir under $2, was $used" ;; esac
    [ ! -e "$used" ] || fail "$1: the private TMPDIR (and its 1 GB file) must be gone after the run"
    [ -z "$(ls -A "$2")" ] || fail "$1: $2 must be left empty, holds $(ls "$2")"
}

run pass CARGO_TARGET_DIR="$root/t1" || fail "a passing run must pass"
check_clean "pass" "$root/t1"
run fail CARGO_TARGET_DIR="$root/t2" && fail "a failing run must fail"
check_clean "fail" "$root/t2"
run hang CARGO_TARGET_DIR="$root/t3" && fail "a terminated run must not pass"
check_clean "SIGTERM" "$root/t3"
run pass CARGO_TARGET_DIR="$root/t4" CHECK_TMPDIR="$root/elsewhere" || fail "CHECK_TMPDIR run must pass"
check_clean "CHECK_TMPDIR" "$root/elsewhere"
# A target dir inside the repository (the default `target`) keeps the system temp dir: scratch repositories that
# tests build under TMPDIR would find this repository above them.
run pass || fail "a run with the default target dir must pass"
[ "$(sed -n 1p "$root/seen")" = "$sys" ] || fail "an in-repository target dir must leave TMPDIR alone, was $(sed -n 1p "$root/seen")"
rm -f "$sys/big.bin"
# --list runs no gate and makes no dir.
(cd "$root/repo" && TMPDIR="$sys" scripts/check.sh --list fast >/dev/null)
[ -z "$(ls -A "$sys")" ] || fail "--list must not touch the system temp dir"
echo "check-tmp-test: ok"
