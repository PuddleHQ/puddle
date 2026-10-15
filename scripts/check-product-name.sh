#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# The product name is "Puddle" in prose (docs, UI text, messages, comments) and `puddle` in
# identifiers (the command, crates, paths, packages, the `PUDDLE_` prefix). This gate flags a bare
# lowercase `puddle` that reads as prose: it stands alone as a word, in a sentence, in a heading,
# in a string or in a comment.
#
# What it skips, by rule:
#   - a word glued to code punctuation: `puddle-api`, `puddle_host`, `puddle.exe`, `puddle/x`,
#     `x/puddle`, `dev.puddle.owner`, `puddle:inbox`, `puddle@host`, `PUDDLE_ROOT`;
#   - a quoted value on its own (`"puddle"`, `'puddle'`), which is an identifier or a label value;
#   - the command: `puddle` followed by a subcommand (`doctor`, `serve`, `ssh-bridge`, `help`,
#     `version`), a flag, a digit (a version line) or an argument list (`[`, `<`);
#   - a `puddle:` at the start of a quoted message (a command's diagnostic prefix);
#   - text inside backticks in Markdown and in comments (code spans);
#   - fenced code blocks in Markdown, and code (outside strings and comments) in source files.
# Everything else, a bare `puddle` followed by a space, a comma, a full stop, `'s` and so on, is
# prose and must read "Puddle".
#
# Allowlist: scripts/product-name-allow.txt, one entry per line, `<path><TAB><fixed substring>`;
# a hit is ignored when its line contains the substring. Keep it for real identifiers that the
# rules cannot tell from prose.
#
# What it cannot see: a prose `puddle` inside a code-like context (a template literal that the
# parity check reads as code, a Markdown table cell inside backticks), and prose spelled with
# markup between the letters. Those need the read-through review, not this gate. A pass is not
# proof that every UI string is capitalised.
set -eu
cd "$(dirname "$0")/.."

allow=scripts/product-name-allow.txt

# The scanner reads files (kind from the extension) or, in the self-test, `<kind><TAB><text>`
# lines. Kinds: md (Markdown), rs and js (a source file: text counts only in strings and comments),
# svelte (markup is text, the <script> block is code), text (everything counts). It prints
# `path:line:text` for each hit.
# shellcheck disable=SC2016 # an awk program: nothing in it is for the shell
scan='
function mask(s,    out, a, b, span) {
    # replace every `...` span with X characters of the same length
    out = ""
    while ((a = index(s, "`")) > 0) {
        b = index(substr(s, a + 1), "`")
        if (b == 0) break
        span = sprintf("%*s", b + 1, " ")
        gsub(/ /, "X", span)
        out = out substr(s, 1, a - 1) span
        s = substr(s, a + b + 2)
    }
    return out s
}
function nq(t, q) { return gsub(q, "&", t) }
function clean(t) {
    gsub(/\\\\/, "", t); gsub(/\\"/, "", t); gsub(/\047"\047/, "", t)
    return t
}
# the text before a `//` that starts a comment (even quote parity), or "" when there is none
function strip_rs(c,    p, rest, off) {
    off = 0; rest = c
    while ((p = index(rest, "//")) > 0) {
        if ((strst + nq(substr(c, 1, off + p - 1), "\"")) % 2 == 0) return substr(c, 1, off + p - 1)
        off += p + 1; rest = substr(c, off + 1)
    }
    return c
}
function instring(s, a, js,    pre, c, m, p) {
    pre = substr(s, 1, a - 1)
    if (pre ~ /^[ \t]*(\*|\/\*)/) return 1
    if (!js) {
        c = clean(pre)
        if (length(strip_rs(c)) < length(c)) return 1
        return (strst + nq(c, "\"")) % 2 == 1
    }
    p = index(pre, "//")
    if (p > 0) {
        m = substr(pre, 1, p - 1)
        if (nq(m, "\"") % 2 == 0 && nq(m, "\047") % 2 == 0 && (tick + nq(m, "`")) % 2 == 0) return 1
    }
    if (pre ~ /([(,=:]|^)[ \t]*\/[^\/]*$/) return 1
    return (tick + nq(pre, "`")) % 2 == 1 || nq(pre, "\"") % 2 == 1 || nq(pre, "\047") % 2 == 1
}
function advance(t, js,    c, p, m) {
    if (!js) {
        c = strip_rs(clean(t))
        strst = (strst + nq(c, "\"")) % 2
    } else {
        c = t
        p = index(c, "//")
        if (p > 0) {
            m = substr(c, 1, p - 1)
            if (nq(m, "\"") % 2 == 0 && nq(m, "\047") % 2 == 0) c = m
        }
        tick = (tick + nq(c, "`")) % 2
    }
}
function hit(s, a, code, js,    pc, nc, nn, rest) {
    pc = (a > 1) ? substr(s, a - 1, 1) : " "
    nc = (a + 6 <= length(s)) ? substr(s, a + 6, 1) : " "
    rest = substr(s, a + 6)
    # previous character: blank, an opening quote or bracket, a template-literal tick
    if (a > 2 && substr(s, a - 2, 2) ~ /^\\[nt]$/) ;
    else if (a == 1) ;
    else if (pc == "/" && code && js && substr(s, 1, a - 1) ~ /([(,=:]|^)[ \t]*\/$/) ;
    else if (index(" \t\"\047([*>|`", pc) == 0) return 0
    if ((pc == "\"" || pc == "\047") && nc == pc) return 0
    if (nc == ".") {
        nn = substr(s, a + 7, 1)
        if (nn != "" && index(" \t\"\047)", nn) == 0 && !((nn == "*" || nn == "+") && code && js)) return 0
    } else if (nc == ":") {
        nn = substr(s, a + 7, 1)
        if ((nn != "" && nn != " " && nn != "\t") || pc != " ") return 0
    } else if (rest ~ /^-(owned|side)([^A-Za-z0-9_]|$)/) {
    } else if (rest ~ /^\047/) {
    } else if (rest ~ /^\\n/) {
    } else if ((nc == "/" || nc == "\\") && code && js) {
    } else if (index(" \t,;)]*<?!\"\047", nc) == 0 && a + 5 < length(s)) return 0
    if (rest ~ /^[ \t]+(doctor|serve|ssh-bridge|help|[0-9]|-[-A-Za-z]|\[|<)/) return 0
    if (code && !instring(s, a, js)) return 0
    return 1
}
{
    if (selftest) {
        kind = $1; path = "self"; n = 1
        text = $0; sub(/^[^\t]*\t/, "", text)
    } else {
        path = FILENAME; n = FNR; text = $0
        kind = "text"
        if (path ~ /\.md$/) kind = "md"
        else if (path ~ /\.rs$/) kind = "rs"
        else if (path ~ /\.(ts|mjs|js)$/) kind = "js"
        else if (path ~ /\.svelte$/) kind = "svelte"
    }
    if (path != lastpath) { lastpath = path; fence = 0; insc = 0; strst = 0; tick = 0 }
    code = (kind == "rs" || kind == "js"); js = (kind != "rs")
    if (kind == "md") {
        if (text ~ /^[ \t]*(```|~~~)/) { fence = !fence; next }
        if (fence) next
    }
    if (kind == "svelte") {
        if (text ~ /^[ \t]*<script/) insc = 1
        code = insc
        if (text ~ /^[ \t]*<\/script/) { insc = 0; code = 0 }
    }
    iscom = (text ~ /^[ \t]*(\/\/|\*|\/\*|#|--)/)
    if (index(text, "puddle") > 0) {
        s = text
        if (kind == "md" || iscom) s = mask(s)
        off = 0; rem = s
        while ((i = index(rem, "puddle")) > 0) {
            a = off + i
            if (substr(s, a - 1, 1) == "`" && (kind == "md" || iscom)) { }
            else if (hit(s, a, code, js)) { print path ":" n ":" text; break }
            off = a + 5; rem = substr(s, off + 1)
        }
    }
    if (code) advance(text, js)
}
'

if [ "${1:-}" = "--self-test" ]; then
    fail=0
    t=$(printf '\t')
    must_catch="md${t}# puddle
md${t}puddle runs each workspace
md${t}It is puddle's own proxy.
md${t}Run puddle, then wait.
md${t}Thanks to puddle.
md${t}A puddle-owned sandbox.
md${t}Use \`puddle doctor\` when puddle is stuck.
rs${t}    \"puddle could not read the file\",
rs${t}    /// A failure inside puddle.
rs${t}    // not created by puddle: keeps the file
js${t}  return \`puddle asks again \${when}.\`;
rs${t}error!(\"{path} was written by a newer puddle\")
svelte${t}      <p>puddle couldn't save that.</p>
svelte${t}  // puddle's own default
text${t}Written by puddle at every boot
text${t}\"description\": \"puddle's local API\"
js${t}    screen.getByText(/not a repository address puddle can read/),"
    must_pass="md${t}Run \`puddle doctor\` first.
md${t}The crate puddle-api and puddle_host.
md${t}See <puddle@example.org> and puddle.exe.
md${t}Config lives in \`%LOCALAPPDATA%\\puddle\`.
md${t}Run puddle doctor again.
md${t}Run puddle --version.
md${t}usage: puddle [--help]
md${t}Puddle runs each workspace
rs${t}    let path = base.join(\"puddle\");
rs${t}    let puddle = Inventory::new();
rs${t}    eprintln!(\"puddle: no endpoint at {gone}\");
rs${t}    .title(\"Puddle\")
rs${t}const APP = 'puddle';
rs${t}  performance.mark(\"puddle:inbox-painted\");
rs${t}  \"dev.puddle.owner=puddle\",
rs${t}  use puddle_types::Thing;
rs${t}  // see \`puddle\` in the data folder
text${t}x-puddle-blocked: yes
text${t}name = \"puddle\"
text${t}puddle 0.1.0"
    while IFS= read -r l; do
        [ -n "$l" ] || continue
        kind=${l%%"$t"*}
        body=${l#*"$t"}
        out=$(printf '%s\t%s\n' "$kind" "$body" | awk -v selftest=1 "$scan")
        [ -n "$out" ] || {
            echo "self-test: should have caught: $l" >&2
            fail=1
        }
    done <<EOF
$must_catch
EOF
    while IFS= read -r l; do
        [ -n "$l" ] || continue
        kind=${l%%"$t"*}
        body=${l#*"$t"}
        out=$(printf '%s\t%s\n' "$kind" "$body" | awk -v selftest=1 "$scan")
        [ -z "$out" ] || {
            echo "self-test: should have passed: $l" >&2
            fail=1
        }
    done <<EOF
$must_pass
EOF
    # A Rust string that runs over several lines is text on every line; code after it is code again.
    out=$(printf 'rs\t    "Run it where \\\nrs\t    puddle can not change that.",\nrs\tlet puddle = 1;\n' | awk -v selftest=1 "$scan")
    [ "$out" = "self:1:    puddle can not change that.\"," ] || {
        echo "self-test: multi-line string: got [$out]" >&2
        fail=1
    }
    [ "$fail" -eq 0 ] || exit 1
    echo "product-name self-test ok"
    exit 0
fi

# Generated and third-party text is not ours to edit: lock files, the vendored crate, the licence.
# This script and its allowlist quote the lowercase name on purpose, so they are skipped.
files=$(git ls-files -- . ':!:Cargo.lock' ':!:**/package-lock.json' ':!:vendor' ':!:LICENSE' \
    ':!:ci/msb-repros/SOURCE' ':!:scripts/check-product-name.sh' ':!:scripts/product-name-allow.txt')

# shellcheck disable=SC2086 # file names in this repo have no whitespace
hits=$(git grep -l -I -z -e puddle -- $files | xargs -0 awk -v selftest=0 "$scan" | sort -u)

if [ -n "$hits" ] && [ -f "$allow" ]; then
    hits=$(printf '%s\n' "$hits" | while IFS= read -r line; do
        path=${line%%:*}
        skip=
        while IFS="$(printf '\t')" read -r apath asub; do
            if [ -z "$apath" ] || [ "${apath#'#'}" != "$apath" ]; then continue; fi
            if [ "$apath" = "$path" ]; then
                if [ -z "$asub" ] || [ "${line#*"$asub"}" != "$line" ]; then skip=1; fi
            fi
        done <"$allow"
        [ -n "$skip" ] || printf '%s\n' "$line"
    done)
fi

if [ -n "$hits" ]; then
    printf '%s\n' "$hits" >&2
    # shellcheck disable=SC2016 # the backticks are literal
    echo 'product-name: write "Puddle" in prose; the lowercase `puddle` is for the command, crates, paths and other identifiers (see scripts/check-product-name.sh)' >&2
    exit 1
fi
echo "product-name ok"
