#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# A test that returns early because something is missing passes without testing anything. This gate
# fails the forms that hide such a skip, so a missing prerequisite has to be either a failure in CI
# or a skip a reader can see and read the reason for.
#
# What it fails
#   Rust
#     skip-message   a print macro whose text says "skip", with no `PUDDLE_REQUIRE_<WHAT>` check in
#                    the 12 lines above it and no skip-ok marker.
#     early-return   a `return` in the first 30 lines of a #[test] / #[tokio::test] function, directly
#                    under an `if`, `else` or `match` (not inside a spawned task or a loop), with no
#                    marker, no `PUDDLE_REQUIRE_` check and no call to a function that holds one
#                    in the 4 lines above it.
#     bare-ignore    `#[ignore]` without `= "reason"`.
#     unset-flag     a `PUDDLE_REQUIRE_<WHAT>` named in Rust that no workflow sets: the test could
#                    never fail in CI, which is the silent skip again.
#   TypeScript / JavaScript
#     skip-no-reason `.skip(` / `.skipIf(` / `.fixme(` / `.todo(` / the x-prefixed disabled forms (`xit(`, `xtest(`)
#                    without a quoted reason after a comma, with no marker (Playwright's
#                    `test.skip(isMobile, "reason")` form passes).
#     only           `.only(` (every other test silently stops running).
#
# The legitimate forms
#   - the prerequisite is a platform fact: put `#[cfg(...)]` on the test, or `test.skip(cond, "why")`;
#     the reader sees the guard and the compiler or runner reports it.
#   - the prerequisite is a tool or a service a CI job provides: check `PUDDLE_REQUIRE_<WHAT>` and
#     panic when it is set and the prerequisite is missing, then skip with a message when it is not
#     (a developer's machine). The workflow job that provides it sets the variable.
#   - anything else that really is not a skip (a test that plays a helper role for another test, a
#     snapshot refresh, a hand-run measurement): a comment `skip-ok: <reason>` on the line or one of
#     the 3 lines above it. The reason is required.
#
# What it cannot see (these need the read-through review)
#   - a skip deeper than 30 lines into a test, or one made by a helper that returns `None` or `false`
#     without printing "skip" and without being called under a REQUIRE-holding function;
#   - a REQUIRE check whose condition is wrong (a flag set in a workflow but the check inverted), or a
#     workflow job that sets the flag but does not run the test (the test binary excluded or filtered);
#   - a `return` that hides in a match arm or a closure that is not recognised as one;
#   - a vacuous test that asserts nothing without ever returning early;
#   - a skip in a shell script, a PowerShell script or a workflow `if:`;
#   - braces inside raw strings and char literals (the nesting count can drift in such a function);
#   - a REQUIRE-holding function name that is also an ordinary one (`start`): any call of that name
#     counts as governed.
# Also:  scripts/check-silent-skips.sh --self-test   shows the gate catch planted skips and pass the
# legitimate forms.
set -eu
cd "$(dirname "$0")/.."

# ---- the scanner -----------------------------------------------------------------------------
# File lists are newline-separated paths held in variables and expanded unquoted with globbing off
# (`set -f`): the repository has no path with a space, and `[id]` route folders must stay literal.
set -f

# Names of the functions whose body names a PUDDLE_REQUIRE_ flag, "|"-separated with a leading "|".
governed_names() {
    # shellcheck disable=SC2086 # a list of paths, see above
    awk -v req="$2" '
        FNR == 1 { cur = "" }
        {
            if (match($0, /(^|[^A-Za-z0-9_])fn [A-Za-z_][A-Za-z0-9_]*/)) {
                t = substr($0, RSTART, RLENGTH)
                sub(/.*fn /, "", t)
                cur = t
            }
            if (cur != "" && $0 ~ req && $0 !~ /^[ \t]*\/\//) gov[cur] = 1
        }
        END { out = "|"; for (n in gov) out = out n "|"; print out }
    ' $1
}

# The Rust problems in the files $1, given the governed names $2. Prints one line each.
scan_rust() {
    # shellcheck disable=SC2086 # a list of paths, see above
    awk -v gov="$2" -v req="$3" '
        function marker(back,   k) {
            for (k = 0; k <= back; k++) if (hist[(FNR - k + 64) % 32] ~ /skip-ok:[ \t]*[^ \t]/) return 1
            return 0
        }
        function required(back,   k) {
            for (k = 0; k <= back; k++) if (hist[(FNR - k + 64) % 32] ~ req) return 1
            return 0
        }
        function governed_call(back,   k, n, h) {
            for (k = 0; k <= back; k++) {
                h = hist[(FNR - k + 64) % 32]
                for (n in names) if (h ~ ("(^|[^A-Za-z0-9_])" n "[ \t]*\\(")) return 1
            }
            return 0
        }
        function report(rule) { printf "%s:%d: %s: %s\n", FILENAME, FNR, rule, orig; bad = 1 }
        BEGIN {
            n = split(gov, parts, "|")
            for (i = 1; i <= n; i++) if (parts[i] != "") names[parts[i]] = 1
        }
        FNR == 1 { armed = 0; head = 0; depth = 0; printctx = -9; for (i = 0; i < 32; i++) hist[i] = "" }
        {
            orig = $0
            hist[FNR % 32] = orig
            comment = (orig ~ /^[ \t]*\/\//)
            strline = orig
            gsub(/"([^"\\]|\\.)*"/, "\"\"", strline)
            sub(/\/\/.*/, "", strline)

            # A print macro (its first line, or the line above the text) whose text says skip.
            if (!comment && orig ~ /(eprintln|println|eprint|print)!/) printctx = FNR
            if (!comment && orig ~ /"[^"]*[Ss][Kk][Ii][Pp][^"]*"/ && FNR - printctx <= 1) {
                if (!required(12) && !marker(3)) report("skip-message")
            }
            if (!comment && orig ~ /#\[[^]]*ignore/ && orig !~ /ignore[ \t]*=/ && !marker(3)) report("bare-ignore")

            if (strline ~ /#\[(tokio::test|test|rstest|test_case)/) { armed = 1; next }
            if (armed && strline ~ /(^|[^A-Za-z0-9_])fn [A-Za-z_]/) { armed = 0; head = 1; depth = 0; opened = 0; since = 0 }
            if (head) {
                since++
                inline_else = (strline ~ /else[ \t]*\{[ \t]*return/)
                bare = (strline ~ /^[ \t]*return(;| Ok\(\(\)\);?)[ \t]*(\}.*)?$/)
                if (inline_else || bare) {
                    top = (depth >= 1) ? opener[depth] : ""
                    inner = 0
                    for (d = 2; d < depth; d++) if (opener[d] ~ /(spawn|loop|while |for |move |(^|[^A-Za-z0-9_])fn |\|[^|]*\|)/) inner = 1
                    if (inner) next
                    if (inline_else || depth == 1 || top ~ /(^|[^A-Za-z0-9_])(else|if|match)([^A-Za-z0-9_]|$)/) {
                        if (!marker(4) && !required(4) && !governed_call(4)) report("early-return")
                    }
                }
                o = strline
                while (match(o, /[{}]/)) {
                    if (substr(o, RSTART, 1) == "{") { depth++; opener[depth] = strline; opened = 1 }
                    else depth--
                    o = substr(o, RSTART + 1)
                }
                if ((opened && depth <= 0) || since > 30) head = 0
            }
        }
        END { exit bad ? 1 : 0 }
    ' $1
}

# The TypeScript / JavaScript problems in the files $1.
scan_ts() {
    # shellcheck disable=SC2086 # a list of paths, see above
    awk '
        function marker(back,   k) {
            for (k = 0; k <= back; k++) if (hist[(FNR - k + 16) % 8] ~ /skip-ok:[ \t]*[^ \t]/) return 1
            return 0
        }
        FNR == 1 { for (i = 0; i < 8; i++) hist[i] = "" }
        {
            hist[FNR % 8] = $0
            if ($0 ~ /^[ \t]*(\/\/|\*|\/\*)/) next
            if ($0 ~ /\.only\(/ && !marker(3)) { printf "%s:%d: only: %s\n", FILENAME, FNR, $0; bad = 1 }
            if ($0 ~ /(\.skip|\.skipIf|\.fixme|\.todo)\(/ || $0 ~ /(^|[^A-Za-z0-9_.])(xit|xtest|x[d]escribe)\(/) {
                if ($0 !~ /\.(skip|skipIf|fixme)\([^)]*,[ \t]*["\047`]/ && !marker(3)) {
                    printf "%s:%d: skip-no-reason: %s\n", FILENAME, FNR, $0; bad = 1
                }
            }
        }
        END { exit bad ? 1 : 0 }
    ' $1
}

# Fails when a PUDDLE_REQUIRE_ name used in the Rust files $1 is set by no workflow under $2.
check_flags() {
    # shellcheck disable=SC2086 # a list of paths, see above
    used=$(grep -ho 'PUDDLE_REQUIRE_[A-Z0-9_]*' $1 | sort -u)
    flags_status=0
    for flag in $used; do
        if ! grep -rq -e "$flag" "$2"; then
            echo "unset-flag: $flag is checked by a test but no workflow sets it"
            flags_status=1
        fi
    done
    return $flags_status
}

# Scans the tree at $1 (a git checkout, or any directory in the self-test). Exit 1 on any problem.
scan_tree() {
    cd "$1"
    if [ -z "${SILENT_SKIPS_NO_GIT:-}" ]; then
        rust=$(git ls-files --cached --others --exclude-standard -- 'crates/*.rs')
        ts=$(git ls-files --cached --others --exclude-standard -- 'ui/*.ts' 'ui/*.js' 'ui/*.mjs' 'crates/*.mjs')
    else
        rust=$(find crates -name '*.rs' 2>/dev/null | sort)
        ts=$(find ui crates \( -name '*.ts' -o -name '*.js' -o -name '*.mjs' \) 2>/dev/null | sort)
    fi
    tree_status=0
    if [ -n "$rust" ]; then
        # A flag name held in a constant (`const REQUIRE_X: &str = "PUDDLE_REQUIRE_X"`) counts where the constant is used.
        req='PUDDLE_REQUIRE_[A-Z0-9_]+'
        # shellcheck disable=SC2086,SC2013 # a list of paths, see above; the names are single words
        for c in $(grep -ho 'const [A-Z0-9_]*: &str = "PUDDLE_REQUIRE_' $rust | sed 's/const \([A-Z0-9_]*\):.*/\1/' | sort -u); do
            req="$req|(^|[^A-Za-z0-9_])$c([^A-Za-z0-9_]|\$)"
        done
        gov=$(governed_names "$rust" "$req")
        scan_rust "$rust" "$gov" "$req" || tree_status=1
        check_flags "$rust" .github/workflows || tree_status=1
    fi
    if [ -n "$ts" ]; then scan_ts "$ts" || tree_status=1; fi
    return $tree_status
}

# ---- the self-test ---------------------------------------------------------------------------
# Plants each silent skip the gate must catch, and each legitimate form it must pass.
self_test() {
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    fail=0
    # case_run <label> <catch|pass> [rule the output must name]; the file comes on stdin, as
    # crates/x/tests/t.rs unless $SELF_PATH says otherwise.
    case_run() {
        rm -rf "$tmp/t"
        mkdir -p "$tmp/t/crates/x/tests" "$tmp/t/ui/e2e" "$tmp/t/.github/workflows"
        printf 'env:\n  PUDDLE_REQUIRE_TOOL: "1"\n' >"$tmp/t/.github/workflows/ci.yml"
        cat >"$tmp/t/${SELF_PATH:-crates/x/tests/t.rs}"
        if out=$(SILENT_SKIPS_NO_GIT=1 scan_tree "$tmp/t" 2>&1); then got=pass; else got=catch; fi
        if [ "$got" != "$2" ]; then
            echo "self-test: $1: expected $2, got $got" >&2
            printf '%s\n' "$out" >&2
            fail=1
        elif [ "$2" = catch ] && ! printf '%s\n' "$out" | grep -q -e "$3"; then
            echo "self-test: $1: caught, but not as $3" >&2
            printf '%s\n' "$out" >&2
            fail=1
        fi
    }

    # --- must be caught
    case_run "skip message with no flag" catch skip-message <<'EOF'
#[test]
fn needs_squid() {
    if which::which("squid").is_err() {
        eprintln!("skipped: no squid");
        return;
    }
}
EOF
    case_run "skip message over two lines" catch skip-message <<'EOF'
fn helper() {
    eprintln!(
        "skipped: this account cannot log on"
    );
}
EOF
    case_run "early return after an if" catch early-return <<'EOF'
#[test]
fn needs_kvm() {
    if !Path::new("/dev/kvm").exists() {
        return;
    }
    assert!(boot());
}
EOF
    case_run "early return in a one-line let-else" catch early-return <<'EOF'
#[tokio::test]
async fn needs_node() {
    let Some(node) = find_node() else { return };
    assert!(run(node).await);
}
EOF
    case_run "early return in a let-else block" catch early-return <<'EOF'
#[tokio::test]
async fn needs_node() {
    let Some(node) = find_node() else {
        return;
    };
    assert!(run(node).await);
}
EOF
    case_run "return Ok in a test" catch early-return <<'EOF'
#[test]
fn needs_x() -> Result<(), String> {
    if std::env::var_os("X").is_none() {
        return Ok(());
    }
    Ok(())
}
EOF
    case_run "bare ignore" catch bare-ignore <<'EOF'
#[test]
#[ignore]
fn slow() {}
EOF
    case_run "a skip marker with no reason" catch early-return <<'EOF'
#[test]
fn needs_x() {
    // skip-ok:
    if no_x() {
        return;
    }
}
EOF
    case_run "a flag no workflow sets" catch unset-flag <<'EOF'
fn tool() -> bool {
    assert!(std::env::var_os("PUDDLE_REQUIRE_NEVER_SET").is_none(), "needed");
    true
}
EOF
    SELF_PATH=ui/e2e/a.spec.ts case_run "playwright skip with no reason" catch skip-no-reason <<'EOF'
test.skip(isMobile);
EOF
    SELF_PATH=ui/e2e/a.spec.ts case_run "vitest it.skip" catch skip-no-reason <<'EOF'
it.skip("does a thing", () => {});
EOF
    SELF_PATH=ui/e2e/a.spec.ts case_run "test.only" catch only <<'EOF'
test.only("does a thing", async () => {});
EOF

    # --- must pass
    case_run "flag check, then a skip message" pass <<'EOF'
fn tool_available() -> bool {
    if which("tool") {
        return true;
    }
    assert!(
        std::env::var_os("PUDDLE_REQUIRE_TOOL").is_none(),
        "PUDDLE_REQUIRE_TOOL is set but tool is not on the path"
    );
    eprintln!("skipped: tool not found");
    false
}

#[test]
fn uses_tool() {
    if !tool_available() {
        return;
    }
    assert!(true);
}
EOF
    case_run "a return after a call to the flag-checking function" pass <<'EOF'
fn tool_available() -> bool {
    assert!(std::env::var_os("PUDDLE_REQUIRE_TOOL").is_none());
    false
}
#[test]
fn uses_tool() {
    let Some(t) = make() else {
        tool_available();
        return;
    };
}
EOF
    case_run "a flag name held in a constant" pass <<'EOF'
const REQUIRE_TOOL: &str = "PUDDLE_REQUIRE_TOOL";
fn tool() -> Option<Tool> {
    if missing() {
        assert!(std::env::var_os(REQUIRE_TOOL).is_none(), "needed");
        eprintln!("SKIPPED: no tool");
        return None;
    }
    Some(Tool)
}
#[test]
fn uses_tool() {
    let Some(t) = tool() else {
        return;
    };
}
EOF
    case_run "a stated reason (marker)" pass <<'EOF'
#[test]
fn plays_a_role() {
    // skip-ok: runs as a helper process for another test; as a plain test it does nothing.
    if std::env::var("ROLE").as_deref() != Ok("host") {
        return;
    }
}
EOF
    case_run "an ignore with a reason" pass <<'EOF'
#[test]
#[ignore = "a 256 MiB transfer, meaningful only in a release build; run it by hand"]
fn big() {}
EOF
    case_run "a platform guard" pass <<'EOF'
#[cfg(windows)]
#[test]
fn only_on_windows() {
    assert!(true);
}
EOF
    case_run "returns inside spawned tasks and loops" pass <<'EOF'
#[tokio::test]
async fn serves() {
    let task = tokio::spawn(async move {
        loop {
            let Ok((conn, _)) = listener.accept().await else {
                return;
            };
            if conn.is_closed() {
                return;
            }
        }
    });
    assert!(task.await.is_ok());
}
EOF
    {
        echo '#[test]'
        echo 'fn long() {'
        i=0
        while [ "$i" -lt 32 ]; do
            echo "    let v$i = $i;"
            i=$((i + 1))
        done
        echo '    if v1 == v2 {'
        echo '        return;'
        echo '    }'
        echo '}'
    } | case_run "a return late in a long test" pass
    case_run "the word skip in a lint reason, not in a print" pass <<'EOF'
#![expect(
    clippy::print_stderr,
    reason = "a skipped test says so"
)]
EOF
    SELF_PATH=ui/e2e/a.spec.ts case_run "playwright skip with a reason" pass <<'EOF'
test.skip(isMobile, "the layout has no hover on a phone");
EOF
    SELF_PATH=ui/e2e/a.spec.ts case_run "a ts skip with a marker" pass <<'EOF'
// skip-ok: waits on an upstream release, tracked in the issue named in the file header.
it.skip("does a thing", () => {});
EOF
    if [ "$fail" -ne 0 ]; then exit 1; fi
    echo "silent-skips self-test ok"
}

if [ "${1:-}" = --self-test ]; then
    self_test
    exit 0
fi

out=$(scan_tree "$PWD" 2>&1) || {
    printf '%s\n' "$out" >&2
    cat >&2 <<'EOF'

silent-skips: a test that returns early passes without testing. Pick the form that fits
(scripts/check-silent-skips.sh explains each, and what the gate cannot see):
  - a tool or service a CI job provides: panic when PUDDLE_REQUIRE_<WHAT> is set and it is missing,
    skip with a message otherwise, and set the variable in the workflow job that provides it;
  - a platform fact: #[cfg(...)] on the test, or test.skip(cond, "reason");
  - not a skip (a helper role, a hand-run measurement): a `skip-ok: <reason>` comment on or above the line.
EOF
    exit 1
}
echo "silent-skips: ok"
