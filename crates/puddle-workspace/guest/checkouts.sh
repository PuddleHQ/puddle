#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Puddle's list of the git checkouts on a workspace's volume.
#
# Usage: sh checkouts.sh <volume mount point> [<one directory under it>]
# Runs as root in the guest. Lists the checkouts directly under the volume root by the delete
# check's test (a directory, not a link, with a `.git` entry; Puddle's own .puddle and ext4's
# lost+found skipped), at most 100. With a directory it lists only that one, and nothing when it
# is not a checkout.
#
# Output, one record per line, tab-separated (puddle-workspace parses it):
#   C <dir> <remote> <branch> <head> <dirty>
#       a checkout. <remote> is the address of `origin`, else of the only remote, empty when
#       there is none. <branch> is empty when HEAD is detached. <head> is the short commit,
#       empty before the first commit. <dirty> is 1 with uncommitted changes or untracked files.
#   M <count>          <count> more checkouts left out (past 100)
#   O <count>          <count> other entries directly under the volume root (loose files, folders
#                      that are no checkout); only when listing all, before D
#   E <dir> <message>  something could not be read
#   D                  the listing ran to the end
# Fields have control characters replaced by '?' and are cut at 500 characters.
set -u

limit=100

# One field: control characters (tab and line breaks too) replaced, cut at 500 characters.
clean() {
    printf '%s' "$1" | tr '\000-\037\177' '?' | cut -c1-500
}

# Repo config can't run anything here: fsmonitor off, any owner accepted (the checkout may belong
# to the session user, not root).
g() {
    git -c safe.directory='*' -c core.fsmonitor=false -C "./$d" "$@"
}

# Prints the record of checkout $d.
describe() {
    if ! remotes=$(g remote 2>&1); then
        printf 'E\t%s\t%s\n' "$(clean "$d")" "$(clean "git remote failed: $remotes")"
        return
    fi
    remote=
    name=
    if printf '%s\n' "$remotes" | grep -qx origin; then
        name=origin
    elif [ -n "$remotes" ] && [ "$(printf '%s\n' "$remotes" | wc -l)" -eq 1 ]; then
        name=$remotes
    fi
    if [ -n "$name" ]; then
        remote=$(g config --get "remote.$name.url" 2>/dev/null || true)
    fi
    branch=$(g symbolic-ref -q --short HEAD 2>/dev/null || true)
    head=$(g rev-parse -q --verify --short HEAD 2>/dev/null || true)
    dirty=0
    if [ -n "$(g status --porcelain=v1 --untracked-files=normal 2>/dev/null | head -n 1)" ]; then
        dirty=1
    fi
    printf 'C\t%s\t%s\t%s\t%s\t%s\n' "$(clean "$d")" "$(clean "$remote")" "$(clean "$branch")" \
        "$(clean "$head")" "$dirty"
}

is_checkout() {
    [ -d "$1" ] && [ ! -L "$1" ] && [ -e "$1/.git" ]
}

cd -- "$1" || {
    printf 'E\t.\t%s\n' "$(clean "cannot enter $1")"
    exit 3
}
if ! command -v git >/dev/null 2>&1; then
    printf 'E\t.\tgit is not installed in the image\n'
    exit 3
fi

if [ $# -ge 2 ]; then
    d=$2
    case $d in
    '' | . | .. | .puddle | lost+found | */* | -*)
        printf 'D\n'
        exit 0
        ;;
    esac
    if is_checkout "$d"; then describe; fi
    printf 'D\n'
    exit 0
fi

count=0
others=0
for d in * .[!.]* ..?*; do
    [ -e "$d" ] || [ -L "$d" ] || continue
    case $d in .puddle | lost+found) continue ;; esac
    if ! is_checkout "$d"; then
        others=$((others + 1))
        continue
    fi
    count=$((count + 1))
    if [ "$count" -le "$limit" ]; then describe; fi
done
if [ "$count" -gt "$limit" ]; then printf 'M\t%s\n' "$((count - limit))"; fi
printf 'O\t%s\n' "$others"
printf 'D\n'
