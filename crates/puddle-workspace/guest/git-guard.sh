# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=sh
# The git guard shared by delete-check.sh and checkouts.sh. Not a script on its own: the Rust
# constants DELETE_CHECK_SH and CHECKOUTS_SH put this text in front of the script. `g` runs git in
# the checkout named by $repo, after read_filters has set $filters for it.

# Config in a checkout can name programs for git to run, and this script runs as root, so every
# git call switches them off: fsmonitor; hooks (with no optional locks `status` writes no index,
# so `post-index-change` has no cause to run either); signature checks in `log`; and each filter
# driver the checkout's config or the config of a repository nested in it names (blanked, so its
# clean or process program never runs; the -c settings reach the git that `status` starts in a
# nested repository). Any owner is accepted (the checkout may belong to the session user, not
# root). A file a filter would rewrite may then read as changed: more is reported, never less.
# A partial clone's remote can name a program (`uploadpack`) that git runs to fetch a missing object,
# so lazy fetch is off (`GIT_NO_LAZY_FETCH`, git 2.44 and later) and, for an older git, no transport
# is allowed (`protocol.allow=never`): a missing object is an error, never a program.
export GIT_NO_LAZY_FETCH=1 GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=protocol.allow GIT_CONFIG_VALUE_0=never
tab=$(printf '\t')

# The filter drivers named in the config of the repository in $1 and of those nested in it, at
# most $2 levels down, one per line; a line starting with `!` says why one cannot be switched off.
filter_names() {
    git -c safe.directory='*' -c core.fsmonitor=false -C "$1" config --name-only \
        --get-regexp '^filter\.' 2>/dev/null |
        awk '/^filter\.[A-Za-z0-9._-]+\.[A-Za-z0-9-]+$/ { sub(/^filter\./, ""); sub(/\.[A-Za-z0-9-]+$/, ""); print; next }
            { print "!the filter in config entry " $0 " has a name no setting can carry" }'
    git -c safe.directory='*' -c core.fsmonitor=false -C "$1" ls-files --stage 2>/dev/null |
        while IFS= read -r entry; do
            case $entry in 160000\ *) ;; *) continue ;; esac
            nested=${entry#*"$tab"}
            case $nested in \"*)
                echo "!the nested repository $nested has a name git quotes"
                continue
                ;;
            esac
            [ -e "$1/$nested/.git" ] || continue
            if [ "$2" -le 0 ]; then
                echo "!the repository $1/$nested is nested too deep to read its filters"
                continue
            fi
            filter_names "$1/$nested" $(($2 - 1))
        done
}

# Sets $filters to the filter drivers of checkout $1; fails, with the reason in $why, when one
# cannot be switched off.
read_filters() {
    filters=$(filter_names "$1" 8 | awk '!seen[$0]++')
    why=$(printf '%s\n' "$filters" | awk '/^!/ { print substr($0, 2); exit }')
    [ -z "$why" ]
}

g() {
    for f in $filters; do
        set -- -c "filter.$f.clean=" -c "filter.$f.smudge=" -c "filter.$f.process=" \
            -c "filter.$f.required=false" "$@"
    done
    git -c safe.directory='*' -c core.fsmonitor=false -c core.hooksPath=/dev/null \
        -c log.showSignature=false --no-optional-locks -C "$repo" "$@"
}
