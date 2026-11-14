#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Keeps the tests' ways around the machine's DNS honest. Two checks:
#
# 1. A test-only feature never reaches a release build. `puddle-agent`'s `test-lookup` feature makes the
#    built binary read `PUDDLE_AGENT_TEST_LOOKUP` and skip the resolver; only the crate's own dev-dependency
#    on itself turns it on. `cargo tree` over the normal and build edges (what a release build compiles,
#    dev-dependencies left out) must not list the feature on any crate, and the variable's name must
#    appear nowhere in the product but connect.rs, where the feature's functions hold it.
# 2. A test that starts a real doctor sets `az_search_path`. The doctor runs the machine's `az --version`,
#    which waits on the network (an update check, telemetry); `HostOptions::default()` and a bare
#    `SystemProbe::new()` do that unless the test names the folders to search (empty in tests). A test file
#    that builds either must say `az_search_path` or `with_az_search_path` somewhere in the same file (the
#    options helper of that file), or carry `az-ok: <why this doctor never runs az>` on that line or the next two.
#
# usage: scripts/check-test-hooks.sh [--self-test]
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--self-test" ]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/check-test-hooks.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/crates/x/tests"
    printf 'fn t() { let o = HostOptions::default(); }\n' >"$tmp/crates/x/tests/bad.rs"
    printf 'fn t() { let mut o = HostOptions::default(); o.az_search_path = None; }\n' >"$tmp/crates/x/tests/good.rs"
    printf 'fn t() { let o = HostOptions::default(); } // az-ok: never starts a doctor\n' >"$tmp/crates/x/tests/marked.rs"
    out=$(CHECK_TEST_HOOKS_ROOT="$tmp" "$0" --only-doctor 2>&1) && {
        echo "check-test-hooks: the self-test file without az_search_path passed" >&2
        exit 1
    }
    case $out in *bad.rs*) ;; *)
        echo "check-test-hooks: the failure does not name bad.rs: $out" >&2
        exit 1
        ;;
    esac
    case $out in *good.rs* | *marked.rs*)
        echo "check-test-hooks: a good file was named: $out" >&2
        exit 1
        ;;
    esac
    exit 0
fi

root=${CHECK_TEST_HOOKS_ROOT:-.}
status=0

if [ "${1:-}" != "--only-doctor" ]; then
    release_tree=$(cargo tree --locked --workspace -e normal,build -e features --prefix none 2>/dev/null) || {
        echo "check-test-hooks: cargo tree failed" >&2
        exit 1
    }
    if printf '%s\n' "$release_tree" | grep -c 'puddle-agent feature "test-lookup"' >/dev/null; then
        echo "check-test-hooks: the feature \`test-lookup\` of puddle-agent is on in a build without dev-dependencies" >&2
        status=1
    fi
    test_tree=$(cargo tree --locked -p puddle-agent -e dev,features --prefix none 2>/dev/null) || {
        echo "check-test-hooks: cargo tree failed" >&2
        exit 1
    }
    if ! printf '%s\n' "$test_tree" | grep -c 'puddle-agent feature "test-lookup"' >/dev/null; then
        echo "check-test-hooks: the tests no longer turn \`test-lookup\` on (no dev-dependency on puddle-agent itself)" >&2
        status=1
    fi
    # The only mentions of the variable are behind the feature (connect.rs) or in tests.
    stray=$(grep -rn 'PUDDLE_AGENT_TEST_LOOKUP' crates --include=*.rs | grep -v '/tests/' | grep -v '^crates/puddle-agent/src/connect.rs' || true)
    if [ -n "$stray" ]; then
        printf 'check-test-hooks: PUDDLE_AGENT_TEST_LOOKUP outside connect.rs and tests:\n%s\n' "$stray" >&2
        status=1
    fi
fi

# Every Rust file of the tests (a tests/ folder, a tests.rs or tests/mod.rs beside a module) that builds the
# default options or a bare probe.
files=$(find "$root/crates" \( -path '*/tests/*' -o -name 'tests.rs' \) -name '*.rs' | sort)
for f in $files; do
    grep -q 'HostOptions::default()\|SystemProbe::new(\|SystemProbe::default()' "$f" || continue
    grep -q 'az_search_path' "$f" && continue
    # A site is accepted with `az-ok:` on its line or one of the two after it.
    if awk 'BEGIN { want = -1 } /HostOptions::default\(\)|SystemProbe::new\(|SystemProbe::default\(\)/ { want = 3 }
            want > 0 { if ($0 ~ /az-ok:/) want = -1; else want-- }
            want == 0 { bad = 1 }
            END { if (want > 0) bad = 1; exit bad }' "$f"; then
        continue
    fi
    echo "check-test-hooks: $f builds HostOptions::default() or SystemProbe without az_search_path: a real doctor runs the machine's az and waits on the network. Set az_search_path (empty) or use the file's hermetic options, or mark the site az-ok: <why>" >&2
    status=1
done
exit "$status"
