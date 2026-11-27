#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Secret scanning: gitleaks over the commits being checked, with the rules and the reviewed
# allowlist of fake test values in .gitleaks.toml. The gitleaks version is pinned in
# ci/gitleaks.sha256; install it with ci/fetch-gitleaks.sh <dir-on-PATH>.
# Usage: scripts/check-secrets.sh [--self-test]
#
# Scope (PUDDLE_SECRETS_SCOPE=history|head|all; unset means history when CI is set, else head):
#   head     HEAD's commits that its base does not have yet, which is what a push adds. The base is
#            $DIFF_BASE if it resolves (the same variable the diff-coverage gate uses), else
#            origin/develop; with neither, all of HEAD's history. A dirty commit on some other
#            local branch (another task's unlanded work) never matters.
#   history  every commit HEAD can reach, and nothing else. CI runs this, so a secret anywhere in
#            the history being checked, a removed one included, still fails there, while a finding
#            on another branch of the clone (another pull request's) fails only that branch's own
#            run. A pull request's HEAD in CI is its merge with the base, so its history is the
#            base's plus every commit of the pull request.
#   all      every ref the clone has, gitleaks' own default: an audit of the whole repository. A
#            CI clone fetches every branch, so in CI this would fail every pull request on any one
#            branch's finding; nothing runs it by default.
#
# PUDDLE_SECRETS_REPORT=<path> also writes gitleaks' JSON report there (secrets redacted); the nightly audit
# (.github/workflows/secrets-audit.yml, ci/secrets-audit-issue.sh) reads file, line, rule and commit from it.
#
# --self-test proves the gate works, on throwaway repositories with the real configuration: a
# real-shaped token fails, a listed fake fixture passes, a real-shaped token in the fixture's
# own file still fails, a canary outside the listed paths fails, and each scope sees exactly the
# commits it should (a dirty side branch fails `all` and passes `head` and `history`).
#
# Rule: a canary token and its allowlist entry in .gitleaks.toml land in one commit. A commit
# that adds the token without the entry fails a scan of that commit under its own config, so it
# breaks bisect, cherry-picks and a scan of any ref that has the commit but not the later entry.
# The self-test proves that check (scan_commit) on both shapes of commit.
#
# What it can't see: a secret with no recognisable shape (a bare password, a low-entropy value),
# one only in a stash, a file that never reached a commit, and in `head` scope a secret already
# in the base (the full CI scan covers it). A pass is a safety net, not proof.
set -eu
cd "$(dirname "$0")/.."
root=$(pwd)

command -v gitleaks >/dev/null 2>&1 || {
    echo "secrets gate needs gitleaks on PATH: ci/fetch-gitleaks.sh <dir on PATH> (docs/STANDARDS.md, \"Toolchain\")" >&2
    exit 1
}
want=$(awk '!/^#/ && NF { print $1; exit }' ci/gitleaks.sha256)
have=$(gitleaks version | tr -d '\r' | sed 's/^v//')
if [ "$have" != "$want" ]; then
    echo "secrets gate: gitleaks $have is on PATH, ci/gitleaks.sha256 pins $want (ci/fetch-gitleaks.sh <dir on PATH>)" >&2
    exit 1
fi

# default_scope <PUDDLE_SECRETS_SCOPE value> <CI value>: the scope to use; a bad value exits 2.
default_scope() {
    case ${1:-} in
    all | head | history) echo "$1" ;;
    "") if [ -n "${2:-}" ]; then echo history; else echo head; fi ;;
    *)
        echo "secrets gate: PUDDLE_SECRETS_SCOPE must be history, head or all, not '$1'" >&2
        exit 2
        ;;
    esac
}

# base_of <repo dir> <diff base>: what the head scope subtracts: the diff base if it names a commit
# there, else origin/develop; nothing when neither exists.
base_of() {
    if [ -n "${2:-}" ] && git -C "$1" cat-file -e "$2^{commit}" 2>/dev/null; then
        echo "$2"
    elif git -C "$1" rev-parse --verify -q origin/develop >/dev/null; then
        echo origin/develop
    fi
}

# log_opts <repo dir> <scope> <diff base>: the `git log` arguments that select the commits to
# scan, empty for all refs. No spaces inside an argument: gitleaks splits the string on them.
log_opts() {
    case $2 in
    all) return 0 ;;
    history)
        echo "--full-history HEAD"
        return 0
        ;;
    esac
    from=$(base_of "$1" "${3:-}")
    if [ -n "$from" ]; then
        echo "--full-history $from..HEAD"
    else
        echo "--full-history HEAD"
    fi
}

scan() { # <repo dir> <scope> <diff base>: exit 1 when gitleaks reports a finding (it prints each one, secret redacted)
    opts=$(log_opts "$1" "$2" "${3:-}")
    gitleaks git --no-banner --verbose --redact --no-color --exit-code 1 --config "$root/.gitleaks.toml" ${PUDDLE_SECRETS_REPORT:+--report-format json --report-path "$PUDDLE_SECRETS_REPORT"} ${opts:+"--log-opts=$opts"} "$1"
}

# scan_commit <repo dir> <sha>: scan that one commit under the .gitleaks.toml it holds itself (not
# today's), so an allowlist entry added by a later commit does not count. Exit 1 on a finding.
scan_commit() {
    own_dir=$(mktemp -d)
    git -C "$1" show "$2:.gitleaks.toml" >"$own_dir/c.toml" 2>/dev/null || cp "$root/.gitleaks.toml" "$own_dir/c.toml"
    rc=0
    gitleaks git --no-banner --redact --no-color --exit-code 1 --config "$own_dir/c.toml" "--log-opts=$2^!" "$1" >/dev/null 2>&1 || rc=$?
    rm -rf "$own_dir"
    return "$rc"
}

if [ "${1:-}" != "--self-test" ]; then
    scope=$(default_scope "${PUDDLE_SECRETS_SCOPE:-}" "${CI:-}")
    if [ "$scope" = history ]; then
        echo "secrets: scanning every commit HEAD can reach, not other branches (PUDDLE_SECRETS_SCOPE=all scans every ref)" >&2
    fi
    if [ "$scope" = head ]; then
        from=$(base_of . "${DIFF_BASE:-}")
        if [ -n "${DIFF_BASE:-}" ] && [ "$from" != "$DIFF_BASE" ]; then
            echo "secrets: DIFF_BASE $DIFF_BASE is not a commit here, so it is ignored" >&2
        fi
        if [ -n "$from" ]; then
            echo "secrets: scanning HEAD's commits that $from lacks, not other branches (PUDDLE_SECRETS_SCOPE=all scans every ref, as CI does)" >&2
        else
            echo "secrets: no origin/develop to subtract (git fetch origin develop): scanning all of HEAD's history, not other branches" >&2
        fi
    fi
    scan . "$scope" "${DIFF_BASE:-}"
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
# commit_file <repo> <path> <line>: commit one file holding that line in the repo (named, under $tmp).
commit_file() {
    mkdir -p "$tmp/$1/$(dirname "$2")"
    printf '%s\n' "$3" >"$tmp/$1/$2"
    git -C "$tmp/$1" add -A
    git -C "$tmp/$1" -c user.name=t -c user.email=t@example.test -c commit.gpgsign=false commit -q -m fixture
}
# A repo with one commit holding $3 (a line) at path $2 (the repo is named $1).
make_repo() {
    git init -q "$tmp/$1"
    commit_file "$1" "$2" "$3"
}
# check <pass|fail> <label> <repo> <scope> [<diff base>]: scan the repo and compare the verdict.
check() {
    # Exit 1 is "a finding"; any other failure (bad config, crash) is neither pass nor fail.
    rc=0
    scan "$tmp/$3" "$4" "${5:-}" >/dev/null 2>&1 || rc=$?
    case $rc in 0) got=pass ;; 1) got=fail ;; *) got="error (exit $rc)" ;; esac
    if [ "$got" = "$1" ]; then
        echo "self-test ok: $2 ($got)"
    else
        echo "self-test FAILED: $2 should $1, got $got" >&2
        fail=1
    fi
}
# expect <pass|fail> <name> <path> <line>: scan a repo of that one file.
expect() {
    make_repo "$2" "$3" "$4"
    check "$1" "$2" "$2" all
}
# same <label> <want> <got>: a plain comparison.
same() {
    if [ "$2" = "$3" ]; then
        echo "self-test ok: $1 ($3)"
    else
        echo "self-test FAILED: $1 should be $2, got $3" >&2
        fail=1
    fi
}
# Built from pieces so this file never holds a token-shaped string itself.
token="gh""p_R7qL2xVn9TzK4mWb8YcD3sFh6JgA0eUoPi5N"
expect fail real-token src/app.rs "const T: &str = \"$token\";"
expect pass listed-canary crates/puddle-proxy/tests/integration/route.rs '    .request("GET http://web.test/a?token=CANARY-route-1 HTTP/1.1")'
expect pass listed-docker-auth crates/puddle-boot/tests/vm_boot.rs 'json!({"registry.example.test": {"auth": "dXNlcjpwdWRkbGU="}});'
canary_token="gh""p_R7mQ2xVb9TnLk4Zs8WdYc1HfJe6UoPa3GiXt"
expect pass listed-log-canary-token crates/puddle-proxy/tests/terminate_suite/log_canary.rs "const STORED_TOKEN: &str = \"$canary_token\";"
expect fail other-token-in-log-canary-file crates/puddle-proxy/tests/terminate_suite/log_canary.rs "const STORED_TOKEN: &str = \"$token\";"
expect fail log-canary-token-outside-listed-path src/app.rs "const T: &str = \"$canary_token\";"
expect fail token-in-listed-file crates/puddle-proxy/tests/integration/route.rs "let t = \"$token\";"
expect fail canary-outside-listed-paths src/lib.rs '    password: "CANARY-s3cret".into(),'
# The history-only entry for the old app errors e2e fixture is anchored by commit id AND path: a
# fresh commit holding a token-shaped value at that very path, or the old shape elsewhere, still fails.
hex="9f3a7c1e5b2d48a6"; hex="$hex$hex$hex$hex"
expect fail history-only-entry-not-reusable-at-its-path ui/e2e/app-errors.spec.ts "const apiKey = \"$hex\";"
expect fail history-only-entry-not-reusable-at-another-path ui/e2e/other.spec.ts "const apiKey = \"$hex\";"

# Canary and allowlist entry in one commit. The fixture config lists the canary token for its path.
canary_config() { # <with|without>: a minimal config, with the allowlist entry or without it
    printf 'title = "fixture"\n[extend]\nuseDefault = true\n'
    if [ "$1" = with ]; then
        printf '[[allowlists]]\ncondition = "AND"\npaths = [%s]\nregexTarget = "match"\nregexes = [%s]\n' \
            "'''^src/canary\\.rs\$'''" "'''$canary_token'''"
    fi
}
git init -q "$tmp/pair"
canary_config without >"$tmp/pair/.gitleaks.toml"
commit_file pair src/canary.rs "const T: &str = \"$canary_token\";"
only_canary=$(git -C "$tmp/pair" rev-parse HEAD)
git init -q "$tmp/both"
canary_config with >"$tmp/both/.gitleaks.toml"
commit_file both src/canary.rs "const T: &str = \"$canary_token\";"
both=$(git -C "$tmp/both" rev-parse HEAD)
verdict() { if scan_commit "$1" "$2"; then echo pass; else echo fail; fi; }
same commit-with-canary-only "fail" "$(verdict "$tmp/pair" "$only_canary")"
same commit-with-canary-and-entry "pass" "$(verdict "$tmp/both" "$both")"

# Scope. A repo whose base (origin/develop) is clean, with a dirty commit on a side branch.
make_repo side README.md 'clean base'
git -C "$tmp/side" update-ref refs/remotes/origin/develop HEAD
git -C "$tmp/side" checkout -q -b dirty
commit_file side src/app.rs "const T: &str = \"$token\";"
git -C "$tmp/side" checkout -q -
commit_file side src/lib.rs '// clean'
check pass side-branch-dirty-head-clean-scope-head side head
check fail side-branch-dirty-scope-all side all
# The CI scope: another branch's finding (another pull request's) must not fail this one.
check pass side-branch-dirty-head-clean-scope-history side history
git -C "$tmp/side" checkout -q dirty
check fail dirty-commit-in-heads-range side head
check fail dirty-commit-in-heads-history side history
commit_file side src/app.rs '// the token is gone'
check fail removed-token-still-in-heads-range side head
check fail removed-token-still-in-heads-history side history
dbase=$(git -C "$tmp/side" rev-parse HEAD)
commit_file side src/lib.rs '// clean again'
check pass diff-base-after-the-dirty-commit side head "$dbase"
check fail no-diff-base-uses-origin-develop side head
check fail unknown-diff-base-uses-origin-develop side head 0000000000000000000000000000000000000001
same base-of-diff-base "$dbase" "$(base_of "$tmp/side" "$dbase")"
same base-of-unknown-diff-base origin/develop "$(base_of "$tmp/side" 0000000000000000000000000000000000000001)"
git -C "$tmp/side" update-ref refs/remotes/origin/develop HEAD
check pass dirty-commit-already-on-the-base side head
check fail dirty-commit-already-on-the-base-scope-all side all
check fail dirty-commit-already-on-the-base-scope-history side history
# No origin/develop to subtract: all of HEAD's history, still nothing from other branches.
git init -q "$tmp/nobase"
commit_file nobase README.md 'clean'
git -C "$tmp/nobase" checkout -q -b dirty
commit_file nobase src/app.rs "const T: &str = \"$token\";"
git -C "$tmp/nobase" checkout -q -
commit_file nobase src/lib.rs '// clean'
check pass no-base-side-branch-dirty nobase head
same base-of-no-base-ref '' "$(base_of "$tmp/nobase" '')"
git -C "$tmp/nobase" -c user.name=t -c user.email=t@example.test -c commit.gpgsign=false merge -q --no-edit dirty
check fail no-base-dirty-commit-in-history nobase head
# A CI clone of a pull request: every branch of the repository is fetched, HEAD is the pull
# request merged into its base, and another pull request's branch holds a finding. The CI scope
# passes this pull request and still fails the other one's own run.
make_repo prs README.md 'clean base'
git -C "$tmp/prs" update-ref refs/remotes/origin/develop HEAD
git -C "$tmp/prs" checkout -q -b other
commit_file prs src/app.rs "const T: &str = \"$token\";"
git -C "$tmp/prs" update-ref refs/remotes/origin/task/other HEAD
git -C "$tmp/prs" checkout -q origin/develop 2>/dev/null
git -C "$tmp/prs" checkout -q -b mine
commit_file prs src/lib.rs '// clean change'
git -C "$tmp/prs" update-ref refs/remotes/origin/task/mine HEAD
git -C "$tmp/prs" checkout -q --detach origin/develop
commit_file prs README.md 'base moved on'
git -C "$tmp/prs" update-ref refs/remotes/origin/develop HEAD
git -C "$tmp/prs" -c user.name=t -c user.email=t@example.test -c commit.gpgsign=false merge -q --no-edit origin/task/mine
check pass ci-pull-request-ignores-other-pull-requests-branch prs "$(default_scope '' true)"
check fail ci-pull-request-scope-all-sees-other-branch prs all
git -C "$tmp/prs" checkout -q --detach origin/develop
git -C "$tmp/prs" -c user.name=t -c user.email=t@example.test -c commit.gpgsign=false merge -q --no-edit origin/task/other
check fail ci-pull-request-with-the-finding-fails prs "$(default_scope '' true)"
same scope-ci-default history "$(default_scope '' true)"
same scope-local-default head "$(default_scope '' '')"
same scope-explicit-wins head "$(default_scope head true)"
exit "$fail"
