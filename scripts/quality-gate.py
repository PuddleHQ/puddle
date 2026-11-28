#!/usr/bin/env python3
"""Check that every risky site in the repository carries its reason in place, and count the sites.

Usage:
  python3 -I scripts/quality-gate.py [ROOT]            # counts as JSON (stable keys)
  python3 -I scripts/quality-gate.py [ROOT] --list     # one TSV row per hit
  python3 -I scripts/quality-gate.py [ROOT] --list --kind unsafe-undocumented
  python3 -I scripts/quality-gate.py [ROOT] --missing  # every site lacking a written reason; exit 1 if any
  python3 -I scripts/quality-gate.py [ROOT] --flakes   # per flake rule: sites, sites without a reason, counted or failing, the fix
  python3 -I scripts/quality-gate.py --self-test

A site needs its reason in place: `// SAFETY:` above an unsafe block or impl (`# Safety` doc or
`SAFETY:` for an unsafe fn), `reason = "..."` on #[allow]/#[expect], `#[ignore = "..."]`, a comment
on or above an `.unwrap()` outside tests, a non-empty `.expect("...")` message, `# why` after a
shellcheck disable (in `*.sh` and in the `run:` blocks of workflow YAML), ` -- why` after eslint-disable (text after @ts-expect-error / svelte-ignore),
`reason` on a deny.toml advisory ignore, a reason column on a coverage exclusion, and a comment on or
within three lines above a fixed sleep in a test (not in a polling loop, not in a `start_paused` test), and
`#![forbid(unsafe_code)]` or `#![expect(unsafe_code, reason = "...")]` in each crate's `lib.rs` and `main.rs`; it checks that a comment is there, not what it says. A TODO, HACK,
FIXME or XXX must name where it is tracked: `TODO(owner)`, an issue number (`#123`) or a URL. Those
gaps are the `noreason` rows (`scripts/check.sh quality` fails on any). There is no baseline file and
no count to hold: a new site with a good reason is fine, a site without one fails.

ROOT defaults to `puddle` (a checkout or a `git archive` export of it). Skips target/, vendor/,
node_modules/, fuzz/corpus and build output. `vendor/` is third-party code we patch, counted apart.

Flake rules (`flake-<id>` kinds; `--flakes` lists them with the fix each names) count test shapes that
raced in the past: a hand-rolled wait helper (every one outside the shared test kit counts; the row says
whether it lacks a name or a bound), a hand-rolled wait loop (a `for`, `while` or `loop` in a test that sleeps or yields between looks), `file.lock()`, `try_recv()`, a port picked
and dropped, a literal port bound or connected in a test (a name's authority and a URL are fakes and stay out), a name built from the clock, a wall-clock bar, records matched by position, an awaited page read
asserted once, a reserved name (`.test`, `.example`, `example.com`) in a test that reaches the resolver, a script written in-process and then made executable (`flake-e1`, ETXTBSY: use `puddle_test_kit::write_executable`), a child's piped stdin written with the result unwrapped (`flake-s1`, BrokenPipe: use `puddle_test_kit::feed_stdin`), a Unix path literal (`/tmp`, `/home`, `/no...`) used as a host path or a path joined by hand with `format!` (`flake-f1`, it fails on Windows). Six bans have kinds of their own: `ban-untagged` (`#[serde(untagged)]`), `ban-error-capital` and `ban-error-period` (thiserror messages), `ban-allow` (any `#[allow]` or `#![allow]`, which clippy's `allow_attributes` skips in the inner form: write `#[expect(lint, reason = "...")]`; a file in `ALLOW_EXEMPT` (none now) keeps its `allow` until its reason is gone, and the entry fails once the file has no `allow`), `ban-result-string` (`Result<_, String>` in production code: return a typed error; no reason comment excuses one) and `ban-html-sink` (an HTML injection sink in `ui/src`: `{@html`, `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `document.write`; no reason comment excuses one). A site is reasoned by `flake-ok: <why>` on the line or within three lines above. A rule is
counted (`flake-<id>`, `flake-<id>-unreasoned`) until its sites are fixed or reasoned, then its `scripts/flake-rules/<id>.rule` says
`status: failing` and an unreasoned site becomes a `noreason` row. Their self-test replays the original code of
fixed flakes from `scripts/flake-fixtures/`.

Context per Rust line: `test` (under tests/, benches/, examples/; in a `#[cfg(test)]` or
`#[cfg(any(test, feature = "testing"))]` item (never `any(windows, test)`: it builds outside tests) or a file module so declared; or in the test-only
crates puddle-e2e, puddle-vm-tests, puddle-test-kit, puddle-proxy-test-support, puddle-host-test-support and puddle-compute-msb-test-support), else `prod`. Rows: kind, context, file:line, detail.

It is a lexer-free scanner (strings and comments are blanked by a small state machine), so it
can miss or misplace a hit in macro-generated or very unusual code: a count is a safety net, the
review still reads the code.

Shellcheck cannot be seen in two places: a directive built at run time (`eval`, a variable) and a
`# shellcheck disable` inside a heredoc or a quoted string that is not a script; a directive in a
file type outside TEXT_EXT (a `.bash` script) is not scanned either.
"""
import json
import os
import re
import sys
import tomllib
from collections import Counter


def load_sibling(name):
    """Load scripts/<name>.py by path: `python3 -I` leaves the script's folder off sys.path."""
    import importlib.util
    spec = importlib.util.spec_from_file_location(name, os.path.join(os.path.dirname(os.path.abspath(__file__)), name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_flakes = load_sibling("quality_gate_flakes")
item_end, flake_rust, flake_e2e, flake_fixed_port, flake_port_calls, flake_summary = (
    _flakes.item_end, _flakes.flake_rust, _flakes.flake_e2e, _flakes.flake_fixed_port, _flakes.flake_port_calls, _flakes.flake_summary)
FLAKE_RULES, FLAKE_FAILING, FLAKE_FIX, load_flake_rules = _flakes.FLAKE_RULES, _flakes.FLAKE_FAILING, _flakes.FLAKE_FIX, _flakes.load_flake_rules

SKIP_DIRS = {"target", "node_modules", ".git", ".svelte-kit", "build", "corpus", "dist", "test-results",
             "playwright-report", ".scratch", "coverage"}  # coverage: vitest's lcov-report (ui/coverage, gitignored) ships istanbul's own eslint-disable lines
TEST_CRATES = ("crates/puddle-e2e/", "crates/puddle-vm-tests/", "crates/puddle-test-kit/",
              "crates/puddle-proxy-test-support/", "crates/puddle-host-test-support/",
              "crates/puddle-compute-msb-test-support/")
_TEST_ONLY = r"(?:test|feature\s*=\s*\"testing\")"
# Test code is `cfg(test)`, `all(test, ..)` and `any(..)` whose every member is `test` or `feature = "testing"`.
# `any(windows, test)` and `any(test, windows)` also build outside tests, so they stay production code.
TEST_CFG = re.compile(r"#\[cfg\((?:test|all\(test\b[^\]]*|any\(\s*" + _TEST_ONLY + r"(?:\s*,\s*" + _TEST_ONLY + r")*\s*,?\s*\)|feature\s*=\s*\"testing\")\)\]")
TEST_ATTR = re.compile(r"#\[(?:tokio::|rstest|test_case|test_log::)?test\b[^\]]*\]|#\[tokio::test\b")
SCANNER_FILES = ("scripts/quality-gate.py", "scripts/quality_gate_flakes.py", "scripts/quality_gate_selftest.py")  # their docs and fixtures name the markers
TEXT_EXT = {".rs", ".ts", ".js", ".svelte", ".sh", ".ps1", ".py", ".toml", ".yml", ".yaml", ".mjs", ".cjs"}


def strip_code(lines):
    """Return (code, comments): per line the code with string/char literals and comments blanked,
    and the comment text. Handles //, nested /* */, "..", r#".."#, b".." and char literals."""
    code_out, com_out = [], []
    in_block = 0
    in_str = None  # None, '"', or a raw terminator like '"##'
    for line in lines:
        code, com = [], []
        i, n = 0, len(line)
        while i < n:
            c = line[i]
            if in_block:
                if line.startswith("*/", i):
                    in_block -= 1
                    i += 2
                    continue
                if line.startswith("/*", i):
                    in_block += 1
                    i += 2
                    continue
                com.append(c)
                i += 1
                continue
            if in_str is not None:
                if in_str == '"':
                    if c == "\\":
                        i += 2
                        continue
                    if c == '"':
                        in_str = None
                        code.append('"')
                    i += 1
                    continue
                if line.startswith(in_str[1:], i):
                    i += len(in_str) - 1
                    in_str = None
                    code.append('"')
                    continue
                i += 1
                continue
            if line.startswith("//", i):
                com.append(line[i + 2:])
                break
            if line.startswith("/*", i):
                in_block = 1
                i += 2
                continue
            m = re.match(r'b?r(#*)"', line[i:])
            if m and (i == 0 or not (line[i - 1].isalnum() or line[i - 1] == "_")):
                in_str = "r" + '"' + m.group(1)  # raw: no escapes, ends at `"` plus the hashes
                code.append('"')
                i += len(m.group(0))
                continue
            if c == '"':
                in_str = '"'
                code.append('"')
                i += 1
                continue
            if c == "'":
                m = re.match(r"'(?:\\.|\\u\{[0-9a-fA-F]+\}|[^'\\])'", line[i:])
                if m:
                    code.append("' '")
                    i += len(m.group(0))
                    continue
            code.append(c)
            i += 1
        code_out.append("".join(code))
        com_out.append("".join(com))
    return code_out, com_out


def indent(line):
    return len(line) - len(line.lstrip())


def in_loop(code, i):
    """Whether line i sits inside a `loop`/`while`/`for` body (by indentation, rustfmt layout), or
    a polling helper's closure; a sleep outside one is a fixed wait."""
    me = indent(code[i])
    floor = me
    cond_end = False  # the line at `floor` ends a multi-line `while` condition: `}) {` or a lone `{`
    for k in range(i - 1, max(-1, i - 80), -1):
        t = code[k].strip()
        if not t or re.fullmatch(r'"[,;)\s]*', t):  # a blank, or the end of a multi-line string
            continue
        ind = indent(code[k])
        if cond_end and ind == floor:
            if re.match(r"(?:'\w+:\s*)?(loop\b|while\b|for\b)", t):
                return True
            if re.match(r"(pub(\([^)]*\))?\s+)?(async\s+)?fn\b", t):
                return False  # `) {` closed a multi-line signature, not a condition
        if ind < floor:
            floor = ind
            cond_end = t == "{" or (t.startswith((")", "}")) and t.endswith("{") and not t.startswith("} else"))
            if re.match(r"(?:'\w+:\s*)?(loop\b|while\b|for\b)", t) or re.search(r"=\s*loop\s*\{|\.retry|poll|until|wait_for|eventually", t):
                return True
            if re.match(r"(pub(\([^)]*\))?\s+)?(async\s+)?fn\b", t):
                return False
    return False


def walk(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        for f in sorted(filenames):
            p = os.path.join(dirpath, f)
            yield os.path.relpath(p, root).replace(os.sep, "/")


def module_file(decl_file, name):
    d, base = os.path.split(decl_file)
    stem = os.path.splitext(base)[0]
    if stem not in ("mod", "lib", "main"):
        d = os.path.join(d, stem)
    return [os.path.join(d, name + ".rs").replace(os.sep, "/"), os.path.join(d, name).replace(os.sep, "/") + "/"]


def scan(root):
    files = list(walk(root))
    rs = [f for f in files if f.endswith(".rs")]
    texts = {}
    for f in files:
        if os.path.splitext(f)[1] in TEXT_EXT:
            try:
                with open(os.path.join(root, f), encoding="utf-8") as h:
                    texts[f] = h.read().split("\n")
            except (UnicodeDecodeError, OSError):
                pass
    stripped = {f: strip_code(texts[f]) for f in rs if f in texts}

    # Pass 1: file modules declared under a test cfg.
    test_prefixes = []
    for f, (code, _) in stripped.items():
        for i, line in enumerate(code):
            if TEST_CFG.search(texts[f][i]) and code[i].lstrip().startswith("#["):
                for j in range(i + 1, min(i + 4, len(code))):
                    m = re.match(r"\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", code[j])
                    if m:
                        test_prefixes += module_file(f, m.group(1))
                        break
                    if code[j].strip() and not code[j].strip().startswith("#["):
                        break

    rows = []
    port_pickers = {}
    file_ctx = {}

    def add(kind, ctx, f, i, detail=""):
        # A failing flake rule's row carries the whole fix text, so it is not cut.
        rows.append((kind, ctx, f"{f}:{i + 1}", detail.strip() if detail.startswith("flake-") else detail.strip()[:160]))

    for f, (code, com) in stripped.items():
        raw = texts[f]
        vendor = f.startswith("vendor/")
        file_test = vendor is False and (
            any(seg in f.split("/") for seg in ("tests", "benches", "examples"))
            or f.startswith(TEST_CRATES)
            or any(f == p or f.startswith(p) for p in test_prefixes)
            or f.startswith("fuzz/"))
        test_lines = [file_test] * len(code)
        if not file_test:
            for i, line in enumerate(code):
                if TEST_CFG.search(raw[i]) and line.lstrip().startswith("#["):
                    s = i + 1
                    while s < len(code) and (not code[s].strip() or code[s].strip().startswith("#[")):
                        s += 1
                    e, _ = item_end(code, s)
                    for k in range(i, e + 1):
                        test_lines[k] = True

        def ctx(i):
            return "vendor" if vendor else ("test" if test_lines[i] else "prod")

        def safety_above(i, word="SAFETY:"):
            if word in com[i]:
                return True
            # Walk up over comments, attributes and the open lines of the same statement.
            for k in range(i - 1, max(-1, i - 12), -1):
                if word in com[k] or word in raw[k]:
                    return True
                c = code[k].strip()
                if not c:
                    if com[k].strip():
                        continue
                    if raw[k].strip() == "":
                        return False
                    continue
                if c.startswith("#["):
                    continue
                if c.endswith((";", "}", "{")) and k < i - 0:
                    # a previous statement or block opener ends here: the comment had to be after it
                    if c.endswith("{") and not c.startswith(("fn ", "pub ", "impl", "mod ")) and "unsafe" not in c:
                        continue
                    return False
            return False

        unsafe_impls = []

        def reason_attr_above(i):
            """An `#[expect(unsafe_code, reason = "...")]` right above the item states its why."""
            for k in range(i - 1, max(-1, i - 8), -1):
                if "reason" in raw[k]:
                    return True
                if not (code[k].strip().startswith(("#[", ")", "]")) or not code[k].strip()
                        or re.match(r"\s*[\w:]+,?\s*$", code[k])):
                    return False
            return False

        # Test function bodies for vacuous-test and sleep checks.
        paused = set()  # lines of `start_paused` tests: their sleeps run on virtual time
        i = 0
        while i < len(code):
            if TEST_ATTR.search(code[i]) and "#[test_case" not in code[i]:
                s = i + 1
                while s < len(code) and not re.search(r"\bfn\b", code[s]):
                    s += 1
                if s < len(code):
                    e, _ = item_end(code, s)
                    if any("start_paused" in code[k] for k in range(i, s)):
                        paused.update(range(s, e + 1))
                    body = "\n".join(code[s:e + 1])
                    name = re.search(r"fn\s+(\w+)", code[s])
                    should_panic = any("should_panic" in code[k] for k in range(i, s))
                    if not should_panic and not re.search(
                            r"assert|panic!|unwrap\(|\.expect(?:_err)?\(|\?|matches!|unreachable!|fail\w*\(|check\w*\(|verify\w*\(|expect_\w+\(|must_\w+\(|ensure\w*\(|bail!", body):
                        add("test-no-assert", ctx(i), f, s, name.group(1) if name else "")
                    i = e
            i += 1

        for i, line in enumerate(code):
            c = ctx(i)
            for m in re.finditer(r"\bunsafe\b", line):
                rest = line[m.end():].lstrip()
                before = line[:m.start()].rstrip()
                if before.endswith(("<", "(", "=", ",", ":", "&", ">")) and not rest.startswith("{"):
                    continue  # a function-pointer type such as `Option<unsafe extern "C" fn()>`, not a site
                if rest.startswith("{"):
                    kind = "unsafe-block"
                elif rest.startswith("impl"):
                    kind = "unsafe-impl"
                elif re.match(r'(extern\s+"[^"]*"\s+)?fn\b', rest) or re.match(r'extern\s*"\s*"\s*fn', rest):
                    kind = "unsafe-fn"
                elif rest.startswith("extern"):
                    kind = "unsafe-extern"
                elif rest.startswith("("):
                    kind = "unsafe-attr"  # #[unsafe(no_mangle)] etc.
                elif rest == "" and i + 1 < len(code) and code[i + 1].lstrip().startswith("{"):
                    kind = "unsafe-block"
                else:
                    continue
                add(kind, c, f, i, raw[i])
                if kind == "unsafe-impl":
                    impl_end = item_end(code, i)[0]
                    unsafe_impls.append((i, impl_end))
                if kind == "unsafe-fn":
                    # a method of an `unsafe impl` is covered by the impl's SAFETY comment
                    documented = (safety_above(i, "# Safety") or safety_above(i)
                                  or any(a < i <= b for a, b in unsafe_impls)
                                  or reason_attr_above(i))
                elif kind == "unsafe-extern":
                    documented = safety_above(i) or reason_attr_above(i)
                else:
                    documented = safety_above(i)
                if not documented and kind != "unsafe-attr":
                    add("noreason", c, f, i, kind + ": no `// SAFETY:` (or `# Safety` doc) | " + raw[i])
            for m in re.finditer(r"#!?\[\s*(?:cfg_attr\([^,]*,\s*)?(allow|expect)\s*\(", line):
                # gather the attribute text up to its closing bracket (may span lines)
                text, k, depth = line[m.start():], i, 0
                while True:
                    depth = text.count("[") - text.count("]")
                    if depth <= 0 or k + 1 >= len(code) or k > i + 8:
                        break
                    k += 1
                    text += " " + code[k].strip()
                lints = re.sub(r"\s+", "", text.split("(", 1)[1])
                lints = re.split(r",reason=|\)\]", lints)[0]
                has_reason = "reason" in text or "reason" in raw[i] or any("reason" in raw[x] for x in range(i, min(k + 1, len(raw))))
                add(f"{m.group(1)}-attr", c, f, i, lints + ("" if has_reason else "  [no reason]"))
                if not has_reason:
                    add("noreason", c, f, i, f"{m.group(1)}({lints}): no `reason = ` | " + raw[i])
            if re.search(r"#\[ignore\b", line):
                add("ignore-attr", c, f, i, raw[i])
                if not re.search(r"#\[ignore\s*=\s*\"[^\"]+\"", raw[i]):
                    add("noreason", c, f, i, "ignore: no `= \"reason\"` | " + raw[i])
            if re.search(r"\.unwrap\(\)", line):
                add("unwrap", c, f, i, raw[i])
                if c == "prod" and not (com[i].strip() or (i and com[i - 1].strip() and not code[i - 1].strip())):
                    add("noreason", c, f, i, "unwrap outside tests: no comment saying why it cannot fail | " + raw[i])
            if re.match(r"\s*let\s+_\s*(?::[^=]*)?=", line):
                add("let-underscore", c, f, i, raw[i])
                if (c == "prod" and not re.match(r"\s*let\s+_\s*=\s*[A-Za-z_][\w.]*\s*;", line)
                        and not any(com[k].strip() for k in range(max(0, i - 3), i + 1))):
                    add("let-underscore-uncommented", c, f, i, raw[i])
                    add("noreason", c, f, i, "`let _ =` outside tests: no comment saying why the value may be dropped (STANDARDS section 6) | " + raw[i])
            if re.search(r"\.expect\(", line):
                add("expect", c, f, i, raw[i])
                if c == "prod" and re.search(r"\.expect\(\s*\"\s*\"\s*\)", raw[i]):
                    add("noreason", c, f, i, "expect with an empty message | " + raw[i])
            for mac in ("panic", "unreachable", "todo", "unimplemented"):
                if re.search(rf"\b{mac}!\s*[\(\[{{]", line):
                    add(f"{mac}-macro", c, f, i, raw[i])
            if re.search(r"\btransmute\b", line):
                add("transmute", c, f, i, raw[i])
            if re.search(r"\bstatic\s+mut\b", line):
                add("static-mut", c, f, i, raw[i])
            if re.search(r"\bmem::forget\b|\bBox::leak\b|\.leak\(\)", line):
                add("leak", c, f, i, raw[i])
            if re.search(r"\bcfg!?\s*\(.*\b(windows|unix|target_os|target_family)\b", line):
                add("os-cfg", c, f, i, raw[i])
            if re.search(r"\b(?:std::)?thread::sleep\b|\btokio::time::sleep\b|\btime::sleep\(|\bsleep\(\s*Duration", line):
                add("sleep", c, f, i, raw[i])
                if c == "test" and i not in paused and not in_loop(code, i):
                    add("sleep-fixed", c, f, i, raw[i])
                    if not any(com[k].strip() for k in range(max(0, i - 3), i + 1)):
                        add("sleep-fixed-uncommented", c, f, i, raw[i])
                        add("noreason", c, f, i, "fixed sleep in a test: no comment saying why (an absence window, simulated latency, or an ordering that has no signal) | " + raw[i])

        file_ctx[f] = lambda i, v=vendor, tl=test_lines: "vendor" if v else ("test" if tl[i] else "prod")
        if not vendor:
            flake_pickers = flake_rust(f, raw, code, com, test_lines, lambda kind, i, detail: add(kind, ctx(i), f, i, detail))
            port_pickers.update({n: f for n in flake_pickers})
            flake_fixed_port(raw, code, com, test_lines, lambda kind, i, detail: add(kind, ctx(i), f, i, detail))
            ban_rows(raw, code, com, ctx, lambda kind, c, i, detail: add(kind, c, f, i, detail), f.split("/"), f in ALLOW_EXEMPT)

    # A `ban-allow` exemption whose file no longer has an `allow` is stale: delete the entry.
    held = {r[2].rsplit(":", 1)[0] for r in rows if r[0] == "ban-allow-exempt"}
    for exempt, why in ALLOW_EXEMPT.items():
        if exempt not in held and os.path.exists(os.path.join(root, exempt)):
            add("noreason", "any", exempt, 0, f"stale ban-allow exemption ({why}): the file has no `allow` any more; delete the entry from ALLOW_EXEMPT in scripts/quality-gate.py")

    # Rule p1: the callers of the port-picking functions found above, in any file.
    for f, (code, com) in stripped.items():
        if f.startswith("vendor/") or not port_pickers:
            continue
        flake_port_calls(code, com, texts[f], port_pickers, lambda kind, i, detail, f=f: add(kind, file_ctx[f](i), f, i, detail))

    # Markers in every text file (comments and strings alike, so docs inside scripts count).
    for f, lines in texts.items():
        vendor = f.startswith("vendor/")
        if vendor or f in SCANNER_FILES:
            continue
        for i, line in enumerate(lines):
            # A marker is the first word of a comment or is followed by `:` or `(`; prose that mentions TODO is not one.
            m = re.search(r"(?://+|#+|/\*+|<!--|^\s*\*)\s*(TODO|HACK|FIXME|XXX)\b|\b(TODO|HACK|FIXME|XXX)[:(]", line)
            if m:
                m = re.search(r"(TODO|HACK|FIXME|XXX)", m.group(0))
            if m and not re.search(r"`(TODO|HACK|FIXME|XXX)`|TODO\.md|todo!", line):
                add("marker-" + m.group(1).lower(), "any", f, i, line)
                if not re.search(r"\b(?:TODO|HACK|FIXME|XXX)\(\s*[\w@./-]+\s*\)|#\d+|https?://", line):
                    add("noreason", "any", f, i, m.group(1) + ": names no owner (`" + m.group(1) + "(who)`, `#123` or a URL) | " + line)
        ban_html_sink_rows(f, lines, lambda kind, c, i, detail, f=f: add(kind, c, f, i, detail))
        if f.startswith("ui/e2e/") and f.endswith(".ts"):
            flake_e2e(lines, lambda kind, i, detail, f=f: add(kind, "test", f, i, detail))
        if f.startswith("ui/") and os.path.splitext(f)[1] in (".ts", ".js", ".svelte"):
            is_test = ".test." in f or ".spec." in f or "/e2e/" in f
            for i, line in enumerate(lines):
                if re.search(r"\b(?:test|it|describe)\.(?:skip|fixme|todo|only)\b|\bx(?:it|test|describe)\(", line):
                    add("ui-skip", "test" if is_test else "prod", f, i, line)
                if re.search(r"waitForTimeout\(|new Promise\(\s*\(?\w*\)?\s*=>\s*setTimeout", line):
                    add("ui-sleep", "test" if is_test else "prod", f, i, line)
                m2 = re.search(r"(eslint-disable(?:-next-line|-line)?|@ts-ignore|@ts-expect-error|@ts-nocheck|svelte-ignore)(.*)", line)
                if m2:
                    uctx = "test" if is_test else "prod"
                    add("ui-suppression", uctx, f, i, line)
                    tail = m2.group(2)
                    why = (" -- " in tail) if m2.group(1).startswith("eslint") else bool(re.search(r"\w", tail))
                    if m2.group(1) == "svelte-ignore":
                        why = bool(re.search(r"--|//|:", tail.split(None, 1)[1] if len(tail.split(None, 1)) > 1 else "")) or (i and "//" in lines[i - 1] or "<!--" in lines[i - 1] and "svelte-ignore" not in lines[i - 1])
                    if not why:
                        add("noreason", uctx, f, i, m2.group(1) + ": no reason | " + line)
                if re.search(r"\bas any\b|: any\b", line):
                    add("ui-any", "test" if is_test else "prod", f, i, line)
        # .sh files and the `run:` blocks of workflow YAML (a line match, so the block need not be parsed).
        if f.endswith((".sh", ".yml", ".yaml")):
            for i, line in enumerate(lines):
                if "shellcheck disable" in line:
                    add("shellcheck-disable", "any", f, i, line)
                    after = line.split("shellcheck disable", 1)[1]
                    if not re.search(r"#\s*\S", after) and not (i and lines[i - 1].lstrip().startswith("#") and "shellcheck" not in lines[i - 1]):
                        add("noreason", "any", f, i, "shellcheck disable: no `# why` | " + line)

    # Config-level suppressions.
    def lines_of(p):
        return texts.get(p, [])
    for i, line in enumerate(lines_of("deny.toml")):
        if re.search(r'\{\s*id\s*=\s*"RUSTSEC', line):
            add("deny-advisory-ignore", "config", "deny.toml", i, line)
            if "reason" not in line:
                add("noreason", "config", "deny.toml", i, "advisory ignore: no reason | " + line)
        if re.search(r'\{\s*(crate|name)\s*=', line) and "skip" in "".join(lines_of("deny.toml")[max(0, i - 30):i]):
            add("deny-skip-or-exception", "config", "deny.toml", i, line)
    excl_dir = os.path.join(root, "scripts/diff-coverage-exclusions")
    excl_files = sorted(f for f in os.listdir(excl_dir) if f.endswith(".txt")) if os.path.isdir(excl_dir) else []
    for name in excl_files:
        rel = "scripts/diff-coverage-exclusions/" + name
        with open(os.path.join(excl_dir, name), encoding="utf-8") as h:
            for i, line in enumerate(h):
                if line.strip() and not line.lstrip().startswith("#"):
                    add("coverage-exclusion", "config", rel, i, line)
                    parts = line.rstrip("\n").split(" | ")
                    if len(parts) < 3 or not parts[-1].strip():
                        add("noreason", "config", rel, i, "coverage exclusion: no reason | " + line)
    for i, line in enumerate(lines_of("Cargo.toml")):
        if re.match(r"\s*[\w:]+\s*=\s*(\"allow\"|\{\s*level\s*=\s*\"allow\")", line):
            add("cargo-lint-allow", "config", "Cargo.toml", i, line)
    kit_dependency_rows(texts, add)
    crate_root_rows(texts, stripped, add)
    inline_test_rows(texts, stripped, add)
    return rows


# Unit tests live in a sibling file (`<module>/tests.rs`, declared `#[cfg(test)] mod tests;`), never in an inline
# `#[cfg(test)] mod X { }` block, whatever its size. A sibling declaration is never flagged.
TEST_MOD = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*\{")


def inline_test_rows(texts, stripped, add):
    for f, (code, _) in stripped.items():
        if f.startswith("vendor/"):
            continue
        raw = texts[f]
        for i, line in enumerate(code):
            if not (TEST_CFG.search(raw[i]) and line.lstrip().startswith("#[")):
                continue
            s = i + 1
            while s < len(code) and (not code[s].strip() or code[s].strip().startswith("#[")):
                s += 1
            m = s < len(code) and TEST_MOD.match(code[s])
            if not m:
                continue
            e, _ = item_end(code, s)
            size = e - s + 1
            add("inline-test-block", "test", f, s, f"{size} lines")
            add("noreason", "test", f, s,
                f"inline `mod {m.group(1)}` ({size} lines): move it to a sibling "
                f"file (`#[cfg(test)] mod {m.group(1)};` and `<module>/{m.group(1)}.rs`) | " + raw[s])


# Every crate root (`crates/<name>/src/lib.rs`, `main.rs`, each bin target `src/bin/*.rs` and `build.rs`, each its own compilation unit) says what it does
# about unsafe code: `#![forbid(unsafe_code)]` (also under `cfg_attr(<cfg>, forbid(unsafe_code))` for a crate
# that has FFI on one platform only) or `#![expect(unsafe_code, reason = "...")]`. The workspace lint is only
# `deny`, which a nested `#[allow]` lifts silently; `forbid` makes the absence of unsafe a compiler guarantee.
CRATE_ROOT = re.compile(r"^crates/[^/]+/(?:src/(?:lib|main|bin/[^/]+)|build)\.rs$")
INNER_ATTR = re.compile(r"#!\[")


def crate_root_rows(texts, stripped, add):
    for f, (code, _) in stripped.items():
        if not CRATE_ROOT.match(f):
            continue
        raw = texts[f]
        ok = False
        for i, line in enumerate(code):
            if not INNER_ATTR.search(line):
                continue
            # The attribute runs to its closing bracket, over as many lines as it takes.
            end = i
            while end + 1 < len(code) and code[end].count("[") > code[end].count("]") + sum(
                    code[k].count("]") - code[k].count("[") for k in range(i, end)):
                end += 1
            masked = " ".join(code[i:end + 1])
            text = " ".join(raw[i:end + 1])
            if re.search(r"\bforbid\(\s*unsafe_code\s*\)", masked):
                ok = True
            elif re.search(r"\bexpect\(\s*unsafe_code\b", masked) and re.search(r'reason\s*=\s*"[^"]*\w[^"]*"', text):
                ok = True
        if not ok:
            add("no-forbid", "prod", f, 0, "crate root")
            add("noreason", "prod", f, 0,
                "crate root: neither `#![forbid(unsafe_code)]` nor `#![expect(unsafe_code, reason = \"...\")]` | " + (raw[0] if raw else ""))


# The shared test kit (crates/puddle-test-kit) is a dev-dependency only: a product crate that lists it under
# [dependencies] or [build-dependencies] (also per target, or renamed) would ship test helpers. The test-only
# crates (TEST_CRATES) may list it as a normal dependency, since they are not product code.
TEST_KIT = ("puddle-test-kit", "puddle-proxy-test-support", "puddle-host-test-support",
            "puddle-compute-msb-test-support", "puddle-e2e",
            "puddle-vm-tests")


# Bans in the standards' own words (docs/adr/0002 "never `#[serde(untagged)]`", docs/STANDARDS.md section 5 "error messages
# are lower-case, no trailing period"). Each is a kind of its own and a failing one: a site adds a `noreason` row.
BAN_UNTAGGED = re.compile(r"#!?\[\s*serde\s*\([^\]]*\buntagged\b")
BAN_ERROR_MESSAGE = re.compile(r'#\[\s*error\(\s*"((?:[^"\\]|\\.)*)"')


# `#[allow]` and `#![allow]` are banned: `#[expect(lint, reason = "...")]` says the same and fails when the lint stops
# firing. clippy's own `allow_attributes` skips the inner `#![allow]` form, so this rule covers it. A file listed here
# keeps its `allow` until the reason names is gone; the entry fails once the file has none (stale). Empty since the
# last three test-support modules moved into library crates.
ALLOW_EXEMPT: dict[str, str] = {}
# Modules behind `feature = "contract"` or `"fake"` are test support the scanner reads as prod; their string errors are
# the contract tests' own verdicts, not an error type a caller matches on.
STRING_ERROR_TEST_DIRS = {"contract", "fake"}
BAN_ALLOW = re.compile(r"#!?\[\s*(?:cfg_attr\([^\]]*?\ballow\b|allow\s*\()")


def result_string_hits(line):
    """Count `Result<_, String>` (two type arguments, the second exactly `String`) in one code line."""
    hits = 0
    for m in re.finditer(r"(?<![\w:])(?:std::)?Result\s*<", line):
        depth, args, cur = 1, [], ""
        for ch in line[m.end():]:
            if ch == "<":
                depth += 1
            elif ch == ">" and depth == 1:
                args.append(cur)
                break
            elif ch == ">":
                depth -= 1
            elif ch == "," and depth == 1:
                args.append(cur)
                cur = ""
                continue
            cur += ch
        else:
            continue
        if len(args) == 2 and args[1].strip() == "String":
            hits += 1
    return hits


def ban_rows(raw, code, com, ctx, add, path_parts=(), allow_exempt=False):
    """Rows `ban-untagged`, `ban-error-capital`, `ban-error-period`, `ban-allow`, `ban-result-string` for one Rust file; `add(kind, ctx, i, detail)`."""
    for i, line in enumerate(code):
        if BAN_ALLOW.search(line):
            if allow_exempt:
                add("ban-allow-exempt", ctx(i), i, raw[i])
            else:
                add("ban-allow", ctx(i), i, raw[i])
                add("noreason", ctx(i), i, "allow attribute: use #[expect(lint, reason = \"...\")] (cfg_attr for a lint that fires under one cfg only); an `allow` hides a lint that stopped firing | " + raw[i])
        if ctx(i) == "prod" and not STRING_ERROR_TEST_DIRS.intersection(path_parts) and result_string_hits(line):
            add("ban-result-string", ctx(i), i, raw[i])
            add("noreason", ctx(i), i, "Result<_, String> in production code: return a typed error (thiserror); no comment excuses one (STANDARDS section 6) | " + raw[i])
        if BAN_UNTAGGED.search(line):
            add("ban-untagged", ctx(i), i, raw[i])
            add("noreason", ctx(i), i, "#[serde(untagged)]: a wrong branch is silent, use an internally or adjacently tagged enum (ADR 0002) | " + raw[i])
        if line.lstrip().startswith("#[") and "error(" in line:
            m = BAN_ERROR_MESSAGE.search(raw[i])
            if m and m.group(1)[:1].isupper():
                add("ban-error-capital", ctx(i), i, raw[i])
                add("noreason", ctx(i), i, "error message starts with a capital: lower-case it (STANDARDS section 5) | " + raw[i])
            if m and m.group(1).endswith("."):
                add("ban-error-period", ctx(i), i, raw[i])
                add("noreason", ctx(i), i, "error message ends with a period: drop it (STANDARDS section 5) | " + raw[i])


# The UI keeps zero HTML injection sinks: the style policy lets the terminal emulator inject `<style>` elements, which
# markup injected through a sink could reach. Tests, generated declarations and node_modules are out of scope.
BAN_HTML_SINK = re.compile(r"\{@html\b|\b(?:innerHTML|outerHTML|insertAdjacentHTML|setHTMLUnsafe|createContextualFragment|srcdoc)\b|\.parseFromString\s*\(|\bdocument\s*\.\s*write(?:ln)?\s*\(")
BAN_HTML_SINK_FIX = "render text with interpolation (`{value}`) or a component, never as markup"


def ban_html_sink_rows(f, lines, add):
    """Rows `ban-html-sink` for one UI source file; `add(kind, ctx, i, detail)`."""
    ext = os.path.splitext(f)[1]
    if not f.startswith("ui/src/") or ext not in (".ts", ".js", ".svelte") or f.endswith(".d.ts") or ".test." in f or ".spec." in f:
        return
    for i, line in enumerate(lines):
        if BAN_HTML_SINK.search(line) and not line.lstrip().startswith(("//", "*", "/*", "<!--")):
            add("ban-html-sink", "prod", i, line)
            add("noreason", "prod", i, f"HTML injection sink: {BAN_HTML_SINK_FIX} | " + line)


def kit_dependency_rows(texts, add):
    """A product manifest listing a test crate (TEST_KIT) under [dependencies] or [build-dependencies] fails, by key,
    by `package = ...` or through `{ workspace = true }` resolved against the root [workspace.dependencies]. A manifest
    that does not parse fails too: a guard that skips what it cannot read passes the files most worth reading."""
    try:
        root = tomllib.loads("\n".join(texts.get("Cargo.toml", [])))
    except tomllib.TOMLDecodeError:
        root = {}
    shared = {k: v.get("package", k) if isinstance(v, dict) else k for k, v in root.get("workspace", {}).get("dependencies", {}).items()}
    for f, lines in texts.items():
        if not (f.startswith("crates/") and f.endswith("/Cargo.toml")) or f.startswith(TEST_CRATES):
            continue
        try:
            doc = tomllib.loads("\n".join(lines))
        except tomllib.TOMLDecodeError as e:
            add("noreason", "config", f, 0, f"unparsable manifest, the test-kit dependency guard cannot read it: {e} | {lines[0] if lines else ''}")
            continue
        tables = [doc, *(t for t in doc.get("target", {}).values() if isinstance(t, dict))]
        for table in tables:
            for kind in ("dependencies", "build-dependencies"):
                for name, spec in table.get(kind, {}).items():
                    spec = spec if isinstance(spec, dict) else {}
                    package = shared.get(name, name) if spec.get("workspace") else spec.get("package", name)
                    kit = next((k for k in TEST_KIT if k in (name, package)), None)
                    if kit:
                        at = next((i for i, line in enumerate(lines) if re.match(rf"\s*{re.escape(name)}\b", line)), 0)
                        add("noreason", "config", f, at,
                            f"{kit} in [{kind}]: a shared test kit is a dev-dependency only; move it to [dev-dependencies] | {lines[at]}")


def counts(rows):
    c = Counter(f"{k}.{ctx}" for k, ctx, _, _ in rows)
    return dict(sorted(c.items()))

def self_test():
    return load_sibling("quality_gate_selftest").self_test(sys.modules[__name__])


def main(argv):
    if "--self-test" in argv:
        return self_test()
    args = [a for a in argv if not a.startswith("--")]
    root = args[0] if args else "puddle"
    kind = None
    if "--kind" in argv:
        kind = argv[argv.index("--kind") + 1]
        args = [a for a in args if a != kind]
        root = args[0] if args else "puddle"
    rows = scan(root)
    if "--flakes" in argv:
        for rule, sites, bare, mode, fix in flake_summary(rows):
            print(f"{rule}\t{sites}\t{bare}\t{mode}\t{fix}")
        return 0
    if "--missing" in argv:
        miss = sorted(r for r in rows if r[0] == "noreason" and r[1] != "vendor")
        for r in miss:
            print(f"{r[2]}\t{r[1]}\t{r[3]}")
        return 1 if miss else 0
    if "--list" in argv:
        for r in sorted(rows):
            if kind is None or r[0] == kind:
                print("\t".join(r))
    else:
        print(json.dumps(counts(rows), indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
