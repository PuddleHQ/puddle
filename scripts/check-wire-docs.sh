#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# A doc comment on a wire type is copied into the committed API contract (crates/puddle-api/openapi/openapi.json and
# ui/src/lib/api/schema.d.ts), so editing one without regenerating the contract fails `committed_spec_is_current` after a
# full test run. This gate fails the diff early and names the fix: a changed `///` or `//!` line in a Rust file that
# derives ToSchema, with either contract file missing from the diff, says `run cargo xtask openapi`.
# The check is per file, not per type: a doc edit on a non-wire item in a file that also holds wire types is flagged too,
# and `cargo xtask openapi` then changes nothing, which proves the edit harmless (commit it with the other files as is).
#   scripts/check-wire-docs.sh              check the diff from the diff base to the working tree
#   scripts/check-wire-docs.sh --self-test  prove the check on a throwaway repository
set -euo pipefail

spec_json=crates/puddle-api/openapi/openapi.json
spec_ts=ui/src/lib/api/schema.d.ts

# check <base>: prints the files at fault and returns 1 when a wire doc comment changed and a contract file did not.
check() {
    local base=$1 changed f bad=""
    changed=$(git diff --name-only "$base" --)
    for f in $changed; do
        case $f in crates/*.rs) ;; *) continue ;; esac
        [ -f "$f" ] || continue
        grep -q 'ToSchema' "$f" || continue
        if grep -qE '^[-+][[:space:]]*//[/!]' <<<"$(git diff -U0 "$base" -- "$f")"; then bad="$bad $f"; fi
    done
    [ -n "$bad" ] || return 0
    local miss=""
    for f in $spec_json $spec_ts; do
        grep -qxF "$f" <<<"$changed" || miss="$miss $f"
    done
    [ -n "$miss" ] || return 0
    echo "a doc comment changed in a file with wire types, and the API contract did not change with it:" >&2
    for f in $bad; do echo "  doc comment changed: $f" >&2; done
    for f in $miss; do echo "  not in the diff: $f" >&2; done
    echo "run \`cargo xtask openapi\` and commit the regenerated files (a doc comment on a wire type is part of the contract)" >&2
    return 1
}

if [ "${1:-}" = --self-test ]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/wire-docs.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    fail() { echo "check-wire-docs self-test: $*" >&2; exit 1; }
    cd "$tmp"
    git init -q . && git config user.email t@t && git config user.name t
    mkdir -p crates/puddle-api/openapi crates/a ui/src/lib/api
    printf '/// Old.\n#[derive(ToSchema)]\npub struct W;\n' >crates/a/wire.rs
    printf '/// Old.\npub struct P;\n' >crates/a/plain.rs
    echo '{}' >$spec_json
    echo 'x' >$spec_ts
    git add -A && git commit -qm base
    base=$(git rev-parse HEAD)
    check "$base" || fail "a clean tree was flagged"
    sed -i 's|Old|New|' crates/a/plain.rs
    check "$base" || fail "a doc edit in a file with no wire type was flagged"
    sed -i 's|^pub struct W|pub struct Wide|' crates/a/wire.rs
    check "$base" || fail "a code edit in a wire file was flagged"
    sed -i 's|Old|New|' crates/a/wire.rs
    out=$(check "$base" 2>&1) && fail "a wire doc edit without the contract was not flagged"
    case $out in *"cargo xtask openapi"*) ;; *) fail "the message does not name the fix" ;; esac
    echo '{"a":1}' >$spec_json
    check "$base" 2>/dev/null && fail "one contract file of two passed"
    echo 'y' >$spec_ts
    check "$base" || fail "a wire doc edit with both contract files was flagged"
    echo "check-wire-docs self-test: ok"
    exit 0
fi

cd "$(dirname "$0")/.."
base=$(scripts/diff-base.sh)
[ -n "$base" ] || { echo "check-wire-docs: no diff base, skipped" >&2; exit 0; }
check "$base"
echo "wire docs ok"
