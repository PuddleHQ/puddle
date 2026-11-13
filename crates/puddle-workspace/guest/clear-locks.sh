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
#    that still held data is kept as `index.puddle-corrupt-<time>`. An index with an extension this git
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

# True when $1 looks like a full object id.
is_sha() {
    case $1 in
        *[!0-9a-f]* | '') return 1 ;;
    esac
    case ${#1} in 40 | 64) return 0 ;; *) return 1 ;; esac
}

# True when some branch of git dir $1 points at commit $2.
branch_at() {
    grep -rqs -- "^$2" "$1/refs/heads" && return 0
    [ -f "$1/packed-refs" ] && grep -q -- "^$2 refs/heads/" "$1/packed-refs"
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
# The new file and the git dir are fsynced one by one: a plain `sync` flushes every filesystem of
# the machine and can take minutes on a busy one. A `sync` without file arguments is only the
# fallback for a sync that does not take them.
put() {
    chown --reference="$3" -- "$1" || return 1
    sync -- "$1" 2>/dev/null || sync
    mv -f -- "$1" "$2" || return 1
    sync -- "$3" 2>/dev/null || sync
}

# One problem record for $1 with the reason $2.
problem() {
    printf 'P\t%s\t%s\n' "$1" "$2"
}

# Rewrites a zero-filled HEAD. $1 is the git dir (it holds HEAD and logs/HEAD), $2 the common dir
# (objects, refs, packed-refs: the same dir for an ordinary repository or a submodule, the main
# repository's for a linked worktree), $3 the git dir as shown in the report (relative to the
# volume root).
repair_head() {
    gd=$1
    cd_=$2
    zero_filled "$gd/HEAD" || return 0
    [ -d "$cd_/objects" ] && [ -d "$cd_/refs" ] || return 0
    shown=$(clean "$3/HEAD")
    log=$gd/logs/HEAD
    last=
    [ -f "$log" ] && last=$(tail -n 1 "$log")
    sha=$(printf %s "$last" | cut -d ' ' -f 2)
    case $sha in *[!0]*) ;; *) sha= ;; esac
    is_sha "$sha" || sha=
    if [ -z "$sha" ]; then
        problem "$shown" "HEAD is damaged and logs/HEAD has no readable last entry"
        return 0
    fi
    if ! commit_exists "$cd_" "$sha"; then
        problem "$shown" "HEAD is damaged and the last commit in logs/HEAD ($sha) is not in the repository"
        return 0
    fi
    branch=$(grep -a "$(printf '\t')checkout: moving from .* to " "$log" | tail -n 1 | cut -f 2- | sed 's/^checkout: moving from .* to //')
    if [ -z "$branch" ] || ! git check-ref-format "refs/heads/$branch" 2>/dev/null; then
        problem "$shown" "HEAD is damaged and logs/HEAD does not say which branch was checked out"
        return 0
    fi
    tip=$(ref_value "$cd_" "refs/heads/$branch")
    if [ -z "$tip" ] && [ ! -e "$gd/refs/heads/$branch" ]; then
        # No such branch: a detached checkout, unless a branch sits on the commit (a renamed branch
        # writes no checkout entry), which a guess cannot settle.
        if branch_at "$cd_" "$sha"; then
            problem "$shown" "HEAD is damaged and logs/HEAD cannot tell a detached checkout from a renamed branch at $sha"
            return 0
        fi
        content=$sha
        what="detached at $sha"
    elif ! is_sha "$tip"; then
        problem "$shown" "HEAD is damaged and branch $(clean "$branch") is unreadable too"
        return 0
    elif [ "$tip" = "$sha" ]; then
        content="ref: refs/heads/$branch"
        what="branch $branch"
    else
        problem "$shown" "HEAD is damaged and branch $(clean "$branch") no longer points at the last commit in logs/HEAD"
        return 0
    fi
    rm -f -- "$gd/HEAD.puddle-new"
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

# Rebuilds an unreadable index from HEAD. $1 is the git dir, $2 the common dir, $3 the git dir as
# shown in the report.
repair_index() {
    gd=$1
    cd_=$2
    idx=$gd/index
    shown=$(clean "$3/index")
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
    if [ -s "$gd/info/sparse-checkout" ] || [ -s "$cd_/info/sparse-checkout" ] || [ "$(git config --bool core.sparseCheckout 2>/dev/null)" = true ]; then
        problem "$shown" "the index is damaged and the checkout is sparse, which a rebuild would lose"
        return 0
    fi
    if ! git rev-parse --verify -q 'HEAD^{commit}' >/dev/null 2>&1; then
        problem "$shown" "the index is damaged and HEAD has no commit to rebuild it from"
        return 0
    fi
    new=$gd/index.puddle-new
    rm -f -- "$new"
    if ! GIT_INDEX_FILE=$new git read-tree HEAD >/dev/null 2>&1; then
        problem "$shown" "the index is damaged and git read-tree HEAD failed"
        return 0
    fi
    if [ $held = yes ]; then
        kept=$gd/index.puddle-corrupt-$(date +%s)
        cp -p -- "$idx" "$kept" || {
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

# Repairs one repository: $1 its work tree, $2 its git dir, $3 the common dir, $4 the git dir as
# shown in the report. Absolute paths for $1 to $3. A subshell, so its git environment stays out
# of the rest of the script.
repair_repo() (
    export LC_ALL=C GIT_OPTIONAL_LOCKS=0 GIT_TERMINAL_PROMPT=0
    export GIT_DIR="$2" GIT_WORK_TREE="$1"
    export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*'
    repair_head "$2" "$3" "$4"
    repair_index "$2" "$3" "$4"
)

# A directory $1 resolved to an absolute path with no symlink in it, when it is a real directory
# inside the volume root $root (resolved once at the start); nothing otherwise.
inside_volume() {
    [ -d "$1" ] && [ ! -L "$1" ] || return 1
    r=$(cd -P -- "$1" 2>/dev/null && pwd -P) || return 1
    case $r in
        "$root"/*) printf %s "$r" ;;
        *) return 1 ;;
    esac
}

# Follows the `.git` file $1 (`gitdir: <path>`, relative to the file's directory) of a linked
# worktree or a submodule and repairs the git dir it points at. A pointer that leads out of the
# volume, to a dir that is no git dir, or through a symlink is reported and left alone: the script
# runs as root and the pointer is guest-controlled text.
repair_pointer() {
    f=$1
    shownf=$(clean "$f")
    line=
    read -r line <"$f" 2>/dev/null || true
    case $line in
        'gitdir: '?*) ;;
        *) return 0 ;;
    esac
    target=${line#gitdir: }
    case $target in /*) ;; *) target=$(dirname -- "$f")/$target ;; esac
    wt=$(inside_volume "$(dirname -- "$f")") || return 0
    gd=$(inside_volume "$target") || {
        problem "$shownf" "the gitdir it points at is missing, is a symlink or is outside the volume, so it was left alone"
        return 0
    }
    [ -f "$gd/HEAD" ] || [ -d "$gd/objects" ] || [ -f "$gd/commondir" ] || return 0
    common=$gd
    if [ -f "$gd/commondir" ]; then
        cline=
        read -r cline <"$gd/commondir" 2>/dev/null || true
        case $cline in /*) ;; *) cline=$gd/$cline ;; esac
        common=$(inside_volume "$cline") || {
            problem "$shownf" "the common git dir of the worktree is missing, is a symlink or is outside the volume, so it was left alone"
            return 0
        }
    fi
    out=$(repair_repo "$wt" "$gd" "$common" "${gd#"$root"/}")
    if [ -n "$out" ]; then
        printf '%s\n' "$out"
    fi
}

# The `.git` files (gitdir pointers) of submodules and linked worktrees below checkout $1, at most
# eight levels down; find does not follow symlinks and does not enter any `.git` dir.
repair_nested() {
    find "$1" -mindepth 1 -maxdepth 8 -name .git -type d -prune -o -name .git -type f -print 2>/dev/null |
        while IFS= read -r p; do
            [ "$p" = "$1/.git" ] && continue
            repair_pointer "$p"
        done
}

cd -- "$1" || {
    fail . "cannot enter $1"
    exit 3
}
root=$(pwd -P)

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

for d in * .[!.]* ..?*; do
    case $d in .puddle | lost+found) continue ;; esac
    [ -d "$d" ] && [ ! -L "$d" ] || continue
    if [ -f "$d/.git" ] && [ ! -L "$d/.git" ]; then
        # A linked worktree or a submodule checked out at the top level.
        repair_pointer "$d/.git"
    fi
    [ -d "$d/.git" ] && [ ! -L "$d/.git" ] || {
        repair_nested "$d"
        continue
    }
    find "$d/.git" -type f -name '*.lock' -exec sh -c '
        for f; do
            if rm -f -- "$f"; then
                printf "L\t%s\n" "$(printf %s "$f" | tr "\t\n" "??")"
            else
                printf "E\t%s\t%s\n" "$(printf %s "$f" | tr "\t\n" "??")" "cannot remove"
            fi
        done' sh {} + || fail "$(printf %s "$d" | tr '\t\n' '??')" "find failed"
    out=$(repair_repo "$root/$d" "$root/$d/.git" "$root/$d/.git" "$d/.git")
    if [ -n "$out" ]; then
        printf '%s\n' "$out"
    fi
    repair_nested "$d"
done
printf 'D\n'
