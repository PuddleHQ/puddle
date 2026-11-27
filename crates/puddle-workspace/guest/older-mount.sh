#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Shows the workspace volume of a sandbox an older Puddle made at /workspace too.
#
# Usage: PUDDLE_OLDER_MOUNT=<older mount point> sh older-mount.sh
# A boot step (puddle-boot runs it as root at every boot, before the agent and before any session).
# Puddle mounted the volume at /workspaces/<id> until 2026-11-25 and at /workspace since. A runtime
# cannot move the mount of a sandbox it has made, and making the sandbox again would throw away its
# root disk (the home folder, installed packages, toolchains). So the sandbox is kept and the
# volume is bind-mounted at /workspace as well, where every path Puddle uses points. The older
# folder stays, so what the user wrote down with it still works.
#
# Idempotent: when /workspace is a mount point already (an earlier run of this boot), it does
# nothing. A failure exits non-zero with the reason, which fails the boot: a /workspace without the
# volume would have Puddle write checkouts to the root disk.
#
# PUDDLE_ROOT, PUDDLE_MOUNTINFO and PUDDLE_MOUNT are for the unit tests only (Puddle passes none):
# a fake root the folders are made below, a fake /proc/self/mountinfo and a `mount` stub.
set -u

older=${PUDDLE_OLDER_MOUNT:?PUDDLE_OLDER_MOUNT is not set}
new=/workspace
root=${PUDDLE_ROOT:-}
mountinfo=${PUDDLE_MOUNTINFO:-/proc/self/mountinfo}
mount_cmd=${PUDDLE_MOUNT:-mount}

# mounted <path>: whether <path> is a mount point (field 5 of mountinfo; Puddle's paths hold no
# character that mountinfo escapes).
mounted() {
    while read -r _ _ _ _ point _; do
        [ "$point" = "$1" ] && return 0
    done <"$mountinfo"
    return 1
}

if mounted "$new"; then
    echo "$new is mounted already"
    exit 0
fi
mounted "$older" || {
    echo "the volume is not mounted at $older" >&2
    exit 1
}
mkdir -p "$root$new" || exit 1
"$mount_cmd" -o bind "$root$older" "$root$new" || {
    echo "cannot bind $older at $new" >&2
    exit 1
}
echo "the volume at $older is at $new too"
