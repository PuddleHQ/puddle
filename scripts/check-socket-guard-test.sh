#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of the TMPDIR length guard of scripts/check.sh with a stand-in cargo (no gate runs for real). A temp dir
# whose path is 90 bytes must make the run fail before any gate starts, with a message that names the length and
# the limit; the limit itself (68 bytes) passes and one byte more fails.
set -eu
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
unset CARGO_TARGET_DIR CHECK_TMPDIR
here=$(cd "$(dirname "$0")" && pwd)
fail() {
    echo "check-socket-guard-test: $*" >&2
    exit 1
}
# Under /var/tmp, not the caller's TMPDIR: a gate run's own TMPDIR is a long chk.* dir, and the cases below need a
# root short enough to build a 68-byte base below it.
root=$(mktemp -d /var/tmp/chk-guard.XXXXXX)
trap 'rm -rf "$root"' EXIT
sys="$root/system-tmp"
mkdir -p "$sys" "$root/repo/scripts"
cp "$here/check.sh" "$here/gate-sched.sh" "$root/repo/scripts/"
cat >"$root/fake-cargo" <<'F'
#!/bin/sh
# Stand-in for `cargo fmt`: records the TMPDIR it is given.
echo "$TMPDIR" >"$FAKE_SEEN"
F
chmod +x "$root/fake-cargo"

# base_of <n>: a directory whose check.sh temp dir (`<base>/chk.XXXXXX`, 11 bytes more) is n bytes long; made here.
base_of() {
    p="$root/b"
    while [ "${#p}" -lt $(($1 - 11)) ]; do p="${p}x"; done
    mkdir -p "$p"
    echo "$p"
}
run() { # run <base>: runs the fmt gate with CHECK_TMPDIR=<base>; sets $out to its stderr, returns its status
    : >"$root/seen"
    out=$(cd "$root/repo" && env TMPDIR="$sys" CHECK_TMPDIR="$1" CARGO="$root/fake-cargo" FAKE_SEEN="$root/seen" \
        scripts/check.sh fmt 2>&1 >/dev/null) && return 0
    return 1
}

deep=$(base_of 90)
run "$deep" && fail "a 90-byte TMPDIR must fail"
case $out in
*"TMPDIR is 90 bytes"*"68-byte limit"*) ;;
*) fail "the message must name the length and the limit, was: $out" ;;
esac
[ ! -s "$root/seen" ] || fail "the guard must stop the run before the first gate, the stand-in ran"
[ -z "$(ls -A "$deep")" ] || fail "the refused temp dir must be removed, holds $(ls "$deep")"

edge=$(base_of 68)
run "$edge" || fail "a 68-byte TMPDIR is at the limit and must pass: $out"
case $(cat "$root/seen") in "$edge"/chk.*) ;; *) fail "the gate ran with the wrong TMPDIR" ;; esac

over=$(base_of 69)
run "$over" && fail "a 69-byte TMPDIR must fail"
case $out in *"TMPDIR is 69 bytes"*) ;; *) fail "the 69-byte message must name 69, was: $out" ;; esac
echo "check-socket-guard-test: ok"
