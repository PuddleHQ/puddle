#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Makes a workspace volume ready for its session user.
#
# Usage: sh prepare.sh <volume mount point> <session user>
# Runs as root in the guest after every boot (the boot hook has already added the user). It
# creates Puddle's directory `.puddle` (mode 0700) and gives the volume root, `.puddle` and the
# editor files below `.puddle/editor` to the session user.
#
# When the volume root belongs to another uid (a new volume, a workspace switched to another
# session user, a sandbox rebuilt with another uid), every file on the volume is given to the
# user first, `lost+found` aside: otherwise the user could not write what the old owner left and
# git would refuse the checkouts as "dubious ownership". The volume root is changed last, so a
# run that is stopped half way starts over at the next boot. `find` stays on the volume and
# symlinks are changed themselves, never what they point to.
#
# Output, one record per line, tab-separated (puddle-workspace parses it):
#   R <n>   n files and folders were given to the user (the volume root not counted)
#   D       the run reached the end
# A failure exits non-zero with the reason on stderr.
#
# PUDDLE_CHOWN replaces `chown` for the unit tests only: a test that is not root cannot chown.
set -u

mount=$1
user=$2
chown_cmd=${PUDDLE_CHOWN:-chown}

fail() {
    printf '%s\n' "$1" >&2
    exit "${2:-1}"
}

uid=$(id -u "$user" 2>/dev/null) || fail "the image has no user $user" 3
gid=$(id -g "$user" 2>/dev/null) || fail "the image has no group for $user" 3
owner=$uid:$gid

[ -d "$mount" ] || fail "no volume at $mount" 3
mkdir -p "$mount/.puddle" || fail "cannot create $mount/.puddle"
chmod 0700 "$mount/.puddle" || fail "cannot set the mode of $mount/.puddle"

had=$(stat -c %u "$mount") || fail "cannot read the owner of $mount"
if [ "$had" != "$uid" ]; then
    n=$(find "$mount" -xdev -mindepth 1 -path "$mount/lost+found" -prune -o -print | wc -l)
    find "$mount" -xdev -mindepth 1 -path "$mount/lost+found" -prune -o \
        -exec "$chown_cmd" -h "$owner" {} + ||
        fail "cannot give the files on $mount to $user"
    printf 'R\t%s\n' "$((n))"
fi
if [ -d "$mount/.puddle/editor" ]; then
    "$chown_cmd" -R -h "$owner" "$mount/.puddle/editor" || fail "cannot give $mount/.puddle/editor to $user"
fi
"$chown_cmd" -h "$owner" "$mount/.puddle" "$mount" || fail "cannot give $mount to $user"
printf 'D\n'
