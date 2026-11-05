#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Removes stale git lock files after a crash or hard kill, then repairs the two git files a power
# cut leaves zero-filled: `HEAD` and the index.
#
# Usage: sh clear-locks.sh <volume mount point> [proc dir, default /proc]
# Runs as root in the guest after a boot. A VMM kill leaves `*.lock` files (index.lock, HEAD.lock,
# refs/**.lock, packed-refs.lock, ...) in checkouts, and git then says "Another git process seems
# to be running". A lock is stale only if no git process runs, so nothing is removed while any
# process is named `git` or `git-*` (checked in the proc dir, one `comm` file per process).
#
# Only regular files named *.lock inside the .git directory of a checkout directly under the
# volume root are removed; a `.git` that is a symlink or a file is skipped, and find never
# follows symlinks. Other files in the checkout are never touched.
#
# Repair, per checkout and after its locks are gone (never while a git process runs):
#  - A `HEAD` that is empty or only NUL bytes (git then calls the checkout "not a git repository")
#    is rewritten from the last entry of `logs/HEAD`, and only when that entry names a commit that
#    exists in the object store. The branch is the target of the last `checkout: moving from X to Y`
#    entry when `refs/heads/Y` points at that commit, a detached `HEAD` when no such branch
#    exists. Anything else (a branch somewhere else, no checkout entry, a missing object) is left
#    alone and reported.
#  - An index that is empty, only NUL bytes or fails git's own checksum is rebuilt with
#    `git read-tree HEAD`, which writes the index only, never the working tree. It is left alone
#    and reported when `HEAD` has no commit, a merge, rebase, cherry-pick or revert is in progress,
#    or a sparse checkout is on (the index held state a rebuild cannot restore). A damaged index
#    that still held data is kept as `index.puddle-corrupt`. An index with an extension this git
#    does not know is a newer git's valid index: reported, not rebuilt.
# What was staged and not committed cannot be read back from an unreadable index: it is gone, the
# files themselves are not.
#
# Output, one record per line, tab-separated (puddle-workspace parses it):
#   B                  a git process runs: nothing was removed
#   L <path>           a lock file that was removed (relative to the volume root)
#   R <path> <what>    a repair that was made (path relative to the volume root)
#   P <path> <why>     damage that was found and left alone
#   E <dir> <message>  something could not be done
#   D                  the run reached the end
# Names have tabs and newlines replaced by '?'.
set -u

proc=${2:-/proc}

fail() {
    printf 'E\t%s\t%s\n' "$1" "$2"
}

clean() {
    printf %s "$1" | tr '\t\n' '??'
}

# True when the file is empty or only NUL bytes.
zero_filled() {
    [ -f "$1" ] && [ ! -L "$1" ] || return 1
    [ "$(tr -d '\000' <"$1" | wc -c)" -eq 0 ]
}

# The value of a branch ref (loose or packed), or nothing.
ref_value() {
    if [ -f "$1/$2" ] && [ ! -L "$1/$2" ]; then
        v=
        read -r v <"$1/$2" || true
        printf %s "$v"
    elif [ -f "$1/packed-refs" ]; then
        awk -v r="$2" '$2 == r { print $1; exit }' "$1/packed-refs"
    fi
}

# True when commit $2 exists in the object store of git dir $1. HEAD is zero-filled, so git
# cannot open the repository itself: a scratch git dir borrows the object store instead.
commit_exists() {
    t=$(mktemp -d) || return 1
    mkdir "$t/refs"
    printf 'ref: refs/heads/x\n' >"$t/HEAD"
    GIT_DIR=$t GIT_OBJECT_DIRECTORY=$1/objects git cat-file -e "${2}^{commit}" 2>/dev/null
    rc=$?
    rm -f "$t/HEAD"
    rmdir "$t/refs" "$t"
    return $rc
}

# Moves the finished file $1 over $2, owned like the git dir $3 (the script runs as root).
put() {
    chown --reference="$3" -- "$1" && mv -f -- "$1" "$2"
}

# One problem record for $1 with the reason $2.
problem() {
    printf 'P\t%s\t%s\n' "$1" "$2"
}

# Rewrites a zero-filled HEAD of the checkout $1 (a path under the volume root).
repair_head() {
    gd=$1/.git
    zero_filled "$gd/HEAD" || return 0
    [ -d "$gd/objects" ] && [ -d "$gd/refs" ] || return 0
    shown=$(clean "$1/.git/HEAD")
    log=$gd/logs/HEAD
    last=
    [ -f "$log" ] && last=$(tail -n 1 "$log")
    sha=$(printf %s "$last" | cut -d ' ' -f 2)
    case $sha in *[!0-9a-f]*) sha= ;; esac
    case $sha in *[!0]*) ;; *) sha= ;; esac
    case ${#sha} in 40 | 64) ;; *) sha= ;; esac
    if [ -z "$sha" ]; then
        problem "$shown" "HEAD is damaged and logs/HEAD has no readable last entry"
        return 0
    fi
    if ! commit_exists "$gd" "$sha"; then
        problem "$shown" "HEAD is damaged and the last commit in logs/HEAD ($sha) is not in the repository"
        return 0
    fi
    branch=$(grep -a 'checkout: moving from .* to ' "$log" | tail -n 1 | cut -f 2- | sed 's/^checkout: moving from .* to //')
    if [ -z "$branch" ] || ! git check-ref-format "refs/heads/$branch" 2>/dev/null; then
        problem "$shown" "HEAD is damaged and logs/HEAD does not say which branch was checked out"
        return 0
    fi
    tip=$(ref_value "$gd" "refs/heads/$branch")
    if [ -z "$tip" ]; then
        content=$sha
        what="detached at $sha"
    elif [ "$tip" = "$sha" ]; then
        content="ref: refs/heads/$branch"
        what="branch $branch"
    else
        problem "$shown" "HEAD is damaged and branch $(clean "$branch") no longer points at the last commit in logs/HEAD"
        return 0
    fi
    if ! printf '%s\n' "$content" >"$gd/HEAD.puddle-new"; then
        fail "$shown" "cannot write HEAD"
        return 0
    fi
    if ! put "$gd/HEAD.puddle-new" "$gd/HEAD" "$gd"; then
        fail "$shown" "cannot write HEAD"
        return 0
    fi
    printf 'R\t%s\t%s\n' "$shown" "HEAD was zero-filled, rewritten: $(clean "$what")"
}

# Rebuilds an unreadable index of the checkout $1 from HEAD.
repair_index() {
    gd=$1/.git
    idx=$gd/index
    shown=$(clean "$1/.git/index")
    [ -f "$idx" ] && [ ! -L "$idx" ] && [ -f "$gd/HEAD" ] || return 0
    zero_filled "$gd/HEAD" && return 0
    if zero_filled "$idx"; then
        held=no
    else
        held=yes
        err=$(git ls-files --stage 2>&1 >/dev/null) && return 0
        case $err in
            # An extension a newer git wrote is "corrupt" to this one, and a valid index.
            *'index uses '[A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9]' extension, which we do not understand'*)
                problem "$shown" "the index uses a git extension this git does not know, so it was left as it is"
                return 0
                ;;
            *'index file corrupt'* | *'index file smaller than expected'* | *'bad signature'* | *'bad index file sha1 signature'*) ;;
            *) return 0 ;;
        esac
    fi
    for state in MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD rebase-merge rebase-apply sequencer; do
        if [ -e "$gd/$state" ]; then
            problem "$shown" "the index is damaged and a merge, rebase or cherry-pick is in progress ($state), which a rebuild would lose"
            return 0
        fi
    done
    if [ -s "$gd/info/sparse-checkout" ] || [ "$(git config --bool core.sparseCheckout 2>/dev/null)" = true ]; then
        problem "$shown" "the index is damaged and the checkout is sparse, which a rebuild would lose"
        return 0
    fi
    if ! git rev-parse --verify -q 'HEAD^{commit}' >/dev/null 2>&1; then
        problem "$shown" "the index is damaged and HEAD has no commit to rebuild it from"
        return 0
    fi
    new=$gd/index.puddle-new
    if ! GIT_INDEX_FILE=$new git read-tree HEAD >/dev/null 2>&1; then
        problem "$shown" "the index is damaged and git read-tree HEAD failed"
        return 0
    fi
    if [ $held = yes ]; then
        cp -p -- "$idx" "$gd/index.puddle-corrupt" || {
            fail "$shown" "cannot keep the damaged index"
            return 0
        }
    fi
    put "$new" "$idx" "$gd" || {
        fail "$shown" "cannot write the index"
        return 0
    }
    printf 'R\t%s\t%s\n' "$shown" "index was damaged, rebuilt from HEAD (staged changes are not kept)"
}

# Repairs checkout $1 (a directory under the volume root). A subshell, so its git environment
# stays out of the rest of the script.
repair_checkout() (
    export LC_ALL=C GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0
    export GIT_DIR="$PWD/$1/.git" GIT_WORK_TREE="$PWD/$1"
    export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*'
    repair_head "$1"
    repair_index "$1"
)

cd -- "$1" || {
    fail . "cannot enter $1"
    exit 3
}

for comm in "$proc"/[0-9]*/comm; do
    [ -r "$comm" ] || continue
    name=
    read -r name <"$comm" 2>/dev/null || true
    case $name in
        git | git-*)
            printf 'B\nD\n'
            exit 0
            ;;
    esac
done

repaired=0
for d in * .[!.]* ..?*; do
    case $d in .puddle | lost+found) continue ;; esac
    [ -d "$d" ] && [ ! -L "$d" ] || continue
    [ -d "$d/.git" ] && [ ! -L "$d/.git" ] || continue
    find "$d/.git" -type f -name '*.lock' -exec sh -c '
        for f; do
            if rm -f -- "$f"; then
                printf "L\t%s\n" "$(printf %s "$f" | tr "\t\n" "??")"
            else
                printf "E\t%s\t%s\n" "$(printf %s "$f" | tr "\t\n" "??")" "cannot remove"
            fi
        done' sh {} + || fail "$(printf %s "$d" | tr '\t\n' '??')" "find failed"
    out=$(repair_checkout "$d")
    if [ -n "$out" ]; then
        printf '%s\n' "$out"
        printf '%s\n' "$out" | grep -q '^R' && repaired=1
    fi
done
# A repaired file must reach the disk before anyone works on it.
[ $repaired = 1 ] && sync
printf 'D\n'
