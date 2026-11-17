#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Puddle's unsaved-work check before a workspace is deleted (ADR 0006 point 3).
#
# Usage: sh delete-check.sh <volume mount point>
# Runs as root in the guest. Lists, for every git checkout directly under the volume root,
# uncommitted changes, commits on no remote and stashes; and every other top-level entry (data
# outside a checkout). Skips Puddle's own .puddle and ext4's lost+found.
#
# Output, one record per line, tab-separated (puddle-workspace parses it):
#   R <dir>            a checkout; the U/C/S/M lines after it belong to it
#   U <status line>    uncommitted change (git status --porcelain)
#   C <hash subject>   commit not on any remote-tracking branch
#   S <stash line>     stash entry
#   O <name>           other top-level entry
#   M <kind> <count>   <count> more lines of <kind> (U, C, S, O) left out
#   E <dir> <message>  something could not be checked
#   D                  the check ran to the end
# Names have tabs and newlines replaced by '?'. Lines are cut at 500 characters, lists at 200.
set -u

cap=200

emit() {
    awk -v k="$1" -v cap="$cap" '
        NR <= cap { print k "\t" substr($0, 1, 500) }
        END { if (NR > cap) print "M\t" k "\t" (NR - cap) }'
}

clean() {
    printf '%s' "$1" | tr '\t\n' '??' | cut -c1-300
}

# Config in a checkout can name programs for git to run, and this script runs as root, so every
# git call switches them off: fsmonitor; hooks (with no optional locks `status` writes no index,
# so `post-index-change` has no cause to run either); signature checks in `log`; and each filter
# driver the checkout's config or the config of a repository nested in it names (blanked, so its
# clean or process program never runs; the -c settings reach the git that `status` starts in a
# nested repository). Any owner is accepted (the checkout may belong to the session user, not
# root). A file a filter would rewrite may then read as changed: more is reported, never less.
tab=$(printf '\t')

# The filter drivers named in the config of the repository in $1 and of those nested in it, at
# most $2 levels down, one per line; `!` for one no -c setting can name, or a nesting too deep.
filter_names() {
    git -c safe.directory='*' -c core.fsmonitor=false -C "$1" config --name-only \
        --get-regexp '^filter\.' 2>/dev/null |
        awk '/^filter\.[A-Za-z0-9._-]+\.[A-Za-z0-9-]+$/ { sub(/^filter\./, ""); sub(/\.[A-Za-z0-9-]+$/, ""); print; next }
            { print "!" }'
    git -c safe.directory='*' -c core.fsmonitor=false -C "$1" ls-files --stage 2>/dev/null |
        while IFS= read -r entry; do
            case $entry in 160000\ *) ;; *) continue ;; esac
            nested=${entry#*"$tab"}
            case $nested in \"*)
                echo '!'
                continue
                ;;
            esac
            [ -e "$1/$nested/.git" ] || continue
            if [ "$2" -le 0 ]; then
                echo '!'
                continue
            fi
            filter_names "$1/$nested" $(($2 - 1))
        done
}

# Sets $filters to the filter drivers of checkout $1; fails when one cannot be switched off.
read_filters() {
    filters=$(filter_names "$1" 8 | awk '!seen[$0]++')
    case $filters in *'!'*) return 1 ;; esac
}

g() {
    for f in $filters; do
        set -- -c "filter.$f.clean=" -c "filter.$f.smudge=" -c "filter.$f.process=" \
            -c "filter.$f.required=false" "$@"
    done
    git -c safe.directory='*' -c core.fsmonitor=false -c core.hooksPath=/dev/null \
        -c log.showSignature=false --no-optional-locks -C "$repo" "$@"
}

fail() {
    printf 'E\t%s\t%s\n' "$1" "$(clean "$2")"
}

# Lists `git <args>` output as kind $1, or an error for checkout $name.
list() {
    kind=$1
    shift
    if out=$(g "$@" 2>&1); then
        if [ -n "$out" ]; then printf '%s\n' "$out" | emit "$kind"; fi
    else
        fail "$name" "git $1 failed: $out"
    fi
}

cd -- "$1" || {
    fail . "cannot enter $1"
    exit 3
}
has_git=1
command -v git >/dev/null 2>&1 || has_git=0
others=0
for d in * .[!.]* ..?*; do
    [ -e "$d" ] || [ -L "$d" ] || continue
    case $d in .puddle | lost+found) continue ;; esac
    name=$(clean "$d")
    if [ -d "$d" ] && [ ! -L "$d" ] && [ -e "$d/.git" ]; then
        printf 'R\t%s\n' "$name"
        if [ "$has_git" = 0 ]; then
            fail "$name" "git is not installed in the image"
            continue
        fi
        repo=$d
        if ! read_filters "$repo"; then
            fail "$name" "its git config names a filter Puddle cannot switch off, so it was not checked"
            continue
        fi
        list U status --porcelain=v1 --untracked-files=normal
        # A shallow clone's history is grafted, so a tag that `fetch --tags` brought in points at
        # upstream commits no remote-tracking branch reaches. There a tag counts only when its
        # commit descends from a remote-tracking commit (work made on top of the clone); a
        # fetched upstream tag is never a descendant of the clone's own remote refs.
        shallow=$(g rev-parse --git-path shallow 2>/dev/null)
        case $shallow in /*) ;; *) shallow=$repo/$shallow ;; esac
        head=
        if g rev-parse -q --verify HEAD >/dev/null 2>&1; then head=HEAD; fi
        # shellcheck disable=SC2086 # $head is one revision or nothing
        if [ -s "$shallow" ]; then
            list C log --format='%h %s' $head --branches --not --remotes
            list C log --format='%h %s' --tags --ancestry-path --not --remotes --branches $head
        else
            list C log --format='%h %s' $head --branches --tags --not --remotes
        fi
        list S stash list
    else
        others=$((others + 1))
        if [ "$others" -le "$cap" ]; then printf 'O\t%s\n' "$name"; fi
    fi
done
if [ "$others" -gt "$cap" ]; then printf 'M\tO\t%s\n' "$((others - cap))"; fi
printf 'D\n'
