#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of scripts/check.sh against a temp dir that an enclosing git repository would see, with a stand-in cargo
# (no gate runs for real). A target dir below another repository (the workspace repository's .scratch/target/<task>)
# must not hold TMPDIR: the scratch repositories the tests build under TMPDIR would find that repository above
# them. The run moves TMPDIR to a private directory under the system temp dir and removes it afterwards; when the
# system temp dir is inside a repository too it refuses; and the socket-length limit still applies to the new path.
set -eu
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
unset CARGO_TARGET_DIR CHECK_TMPDIR
here=$(cd "$(dirname "$0")" && pwd)
fail() {
    echo "check-enclosing-repo-test: $*" >&2
    exit 1
}
# Under /var/tmp, not the caller's TMPDIR: a gate run's TMPDIR is already a long chk.* dir, and the paths below it
# would trip the socket-length limit this test then checks the wrong thing about.
root=$(mktemp -d /var/tmp/chk-enclosing.XXXXXX)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/repo/scripts" "$root/outer/.git" "$root/outer/.scratch/target/task"
cp "$here/check.sh" "$here/gate-sched.sh" "$root/repo/scripts/"
cat >"$root/fake-cargo" <<'F'
#!/bin/sh
# Stand-in for `cargo fmt`: records TMPDIR and whether git, run in a fresh directory there, finds a repository.
echo "$TMPDIR" >"$FAKE_SEEN"
mkdir "$TMPDIR/scratch"
if git -C "$TMPDIR/scratch" rev-parse --git-dir >/dev/null 2>&1; then echo found >>"$FAKE_SEEN"; else echo none >>"$FAKE_SEEN"; fi
F
chmod +x "$root/fake-cargo"

sys="$root/system-tmp"
mkdir -p "$sys"
# The socket limit is 68 bytes with a usable XDG_RUNTIME_DIR; the root is a directory, so it counts as one.
run() { # run <system temp> <target dir>: runs the fmt gate; sets $out to its stderr, returns its status
    : >"$root/seen"
    out=$(cd "$root/repo" && env TMPDIR="$1" CARGO_TARGET_DIR="$2" XDG_RUNTIME_DIR="$root" CARGO="$root/fake-cargo" \
        FAKE_SEEN="$root/seen" scripts/check.sh fmt 2>&1 >/dev/null) && return 0
    return 1
}

target="$root/outer/.scratch/target/task"
run "$sys" "$target" || fail "a target dir in an enclosing repository must still pass: $out"
case $out in *"inside the repository $root/outer"*) ;; *) fail "the notice must name the enclosing repository, was: $out" ;; esac
used=$(sed -n 1p "$root/seen")
case $used in "$sys"/chk.*) ;; *) fail "TMPDIR must move to a chk dir under the system temp dir, was $used" ;; esac
[ "$(sed -n 2p "$root/seen")" = none ] || fail "git found a repository above the temp dir $used"
[ ! -e "$used" ] || fail "the relocated temp dir must be removed after the run"
[ -z "$(ls -A "$sys")" ] || fail "the system temp dir must be left empty, holds $(ls "$sys")"
[ -z "$(ls -A "$target")" ] || fail "the target dir must be left empty, holds $(ls "$target")"

# A repository checked out as a linked worktree has a .git file, not a directory: it encloses too.
mkdir -p "$root/linked/deep"
echo "gitdir: elsewhere" >"$root/linked/.git"
run "$sys" "$root/linked/deep" || fail "a target dir in a linked worktree must still pass: $out"
case $(sed -n 1p "$root/seen") in "$sys"/chk.*) ;; *) fail "a .git file must count as an enclosing repository" ;; esac

# A target dir outside every repository keeps holding TMPDIR.
mkdir -p "$root/free"
run "$sys" "$root/free" || fail "a free target dir must pass: $out"
case $(sed -n 1p "$root/seen") in "$root/free"/chk.*) ;; *) fail "a target dir in no repository must hold TMPDIR" ;; esac

# The system temp dir inside a repository too: nowhere safe, so refuse before any gate and leave nothing behind.
mkdir -p "$root/outer/sys"
run "$root/outer/sys" "$target" && fail "a system temp dir inside a repository must be refused"
case $out in *"inside the repository $root/outer"*"CHECK_TMPDIR"*) ;; *) fail "the refusal must name the repository and CHECK_TMPDIR, was: $out" ;; esac
[ ! -s "$root/seen" ] || fail "the refusal must come before the first gate"
[ -z "$(ls -A "$root/outer/sys")" ] || fail "the refused run must leave the system temp dir empty"

# The socket length limit applies to the relocated path: a system temp dir whose chk dir is 68 bytes passes, 69 fails.
base_of() { # base_of <n>: a directory whose chk dir (11 bytes more) is n bytes long
    p="$root/s"
    while [ "${#p}" -lt $(($1 - 11)) ]; do p="${p}x"; done
    mkdir -p "$p"
    echo "$p"
}
edge=$(base_of 68)
run "$edge" "$target" || fail "a relocated 68-byte TMPDIR is at the limit and must pass: $out"
case $(sed -n 1p "$root/seen") in "$edge"/chk.*) ;; *) fail "the relocated run used the wrong TMPDIR" ;; esac
over=$(base_of 69)
run "$over" "$target" && fail "a relocated 69-byte TMPDIR must fail"
case $out in *"TMPDIR is 69 bytes"*) ;; *) fail "the 69-byte message must name 69, was: $out" ;; esac
[ ! -s "$root/seen" ] || fail "the length refusal must come before the first gate"
[ -z "$(ls -A "$over")" ] || fail "the refused relocated temp dir must be removed, holds $(ls "$over")"
echo "check-enclosing-repo-test: ok"
