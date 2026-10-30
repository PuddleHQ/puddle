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
within three lines above a fixed sleep in a test (not in a polling loop, not in a `start_paused` test); it checks that a comment is there, not what it says. A TODO, HACK,
FIXME or XXX must name where it is tracked: `TODO(owner)`, an issue number (`#123`) or a URL. Those
gaps are the `noreason` rows (`scripts/check.sh quality` fails on any). There is no baseline file and
no count to hold: a new site with a good reason is fine, a site without one fails.

ROOT defaults to `puddle` (a checkout or a `git archive` export of it). Skips target/, vendor/,
node_modules/, fuzz/corpus and build output. `vendor/` is third-party code we patch, counted apart.

Flake rules (`flake-<id>` kinds; `--flakes` lists them with the fix each names) count test shapes that
raced in the past: a hand-rolled wait helper (every one outside the shared test kit counts; the row says
whether it lacks a name or a bound), `file.lock()`, `try_recv()`, a port picked
and dropped, a name built from the clock, a wall-clock bar, records matched by position, an awaited page read
asserted once. A site is reasoned by `flake-ok: <why>` on the line or within three lines above. A rule is
counted (`flake-<id>`, `flake-<id>-unreasoned`) until its sites are fixed or reasoned, then it is added to
FLAKE_FAILING and an unreasoned site becomes a `noreason` row. Their self-test replays the original code of
fixed flakes from `scripts/flake-fixtures/`.

Context per Rust line: `test` (under tests/, benches/, examples/; in a `#[cfg(test)]` or
`#[cfg(any(test, feature = "testing"))]` item or a file module so declared; or in the test-only
crates puddle-e2e and puddle-vm-tests), else `prod`. Rows: kind, context, file:line, detail.

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
from collections import Counter

SKIP_DIRS = {"target", "node_modules", ".git", ".svelte-kit", "build", "corpus", "dist", "test-results",
             "playwright-report", ".scratch"}
TEST_CRATES = ("crates/puddle-e2e/", "crates/puddle-vm-tests/")
TEST_CFG = re.compile(r"#\[cfg\((?:test|all\(test\b[^\]]*|any\(test\b[^\]]*|any\([^\]]*feature\s*=\s*\"testing\"[^\]]*|feature\s*=\s*\"testing\")\)\]")
TEST_ATTR = re.compile(r"#\[(?:tokio::|rstest|test_case|test_log::)?test\b[^\]]*\]|#\[tokio::test\b")
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


def item_end(code, start):
    """Line index where the item starting at `start` ends (brace-balanced), or `start` if it ends
    with `;` before any `{`. Returns (end, is_decl_without_body)."""
    depth, opened = 0, False
    for j in range(start, len(code)):
        for ch in code[j]:
            if ch == "{":
                depth += 1
                opened = True
            elif ch == "}":
                depth -= 1
            elif ch == ";" and not opened and depth == 0:
                return j, True
        if opened and depth <= 0:
            return j, False
    return len(code) - 1, False


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

    # Rule p1: the callers of the port-picking functions found above, in any file.
    for f, (code, com) in stripped.items():
        if f.startswith("vendor/") or not port_pickers:
            continue
        flake_port_calls(code, com, texts[f], port_pickers, lambda kind, i, detail, f=f: add(kind, file_ctx[f](i), f, i, detail))

    # Markers in every text file (comments and strings alike, so docs inside scripts count).
    for f, lines in texts.items():
        vendor = f.startswith("vendor/")
        if vendor or f == "scripts/quality-gate.py":
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
    excl = os.path.join(root, "scripts/diff-coverage-exclusions.txt")
    if os.path.exists(excl):
        with open(excl, encoding="utf-8") as h:
            for i, line in enumerate(h):
                if line.strip() and not line.lstrip().startswith("#"):
                    add("coverage-exclusion", "config", "scripts/diff-coverage-exclusions.txt", i, line)
                    parts = line.rstrip("\n").split(" | ")
                    if len(parts) < 3 or not parts[-1].strip():
                        add("noreason", "config", "scripts/diff-coverage-exclusions.txt", i, "coverage exclusion: no reason | " + line)
    for i, line in enumerate(lines_of("Cargo.toml")):
        if re.match(r"\s*[\w:]+\s*=\s*(\"allow\"|\{\s*level\s*=\s*\"allow\")", line):
            add("cargo-lint-allow", "config", "Cargo.toml", i, line)
    return rows


# Flake-prone test shapes (counted, never failing yet). Each rule has two kinds: `flake-<id>` for every
# site and `flake-<id>-unreasoned` for the sites with no `flake-ok: <why>` comment on the line or within
# three lines above (a UI line may carry `eslint-disable-next-line no-restricted-syntax -- <why>` instead).
# A rule in FLAKE_FAILING also adds a `noreason` row per unreasoned site, so `--missing` and the `quality`
# gate fail on it; flipping a rule from counted to failing is adding its id there, once its sites are fixed
# or reasoned. The scan reads shapes, not truth: a reason is read by a reviewer, and a shape it cannot see
# (a producer that is synchronous, a wait anchored on something already true) needs a run under load.
FLAKE_FAILING = {"o2"}
FLAKE_OK = re.compile(r"flake-ok:\s*\S")
FLAKE_FIX = {
    "w1": "a wait helper outside the shared test kit: use `eventually(what, within, probe)` (it names the wait and bounds it below nextest's slow-timeout), or `flake-ok: <why>`",
    "w2": "`file.lock()` in a test waits for ever when another process holds it: poll `try_lock` against the test's budget, or `flake-ok: <why>`",
    "o1": "`try_recv()` reads a channel once, before another task has sent: use `next_event_within(what, d)` to wait for an event or `no_event_for(d)` to show none comes, or `flake-ok: <why the producer is synchronous>`",
    "p1": "a port picked from a listener that is dropped can be taken before the caller binds it: bind port 0 and pass the listener, or let the child bind 0 and report the port, or `flake-ok: <why a retry is the only way>`",
    "n1": "a name built from the clock or the pid collides across parallel runs: create the file or folder with `tempfile`, or `flake-ok: <why>`",
    "t1": "a test that asserts real time stayed under a fixed Duration fails on a loaded machine: assert on virtual time, CPU time or a measured margin, or `flake-ok: <what the bar measures and why it holds under load>`",
    "a1": "records matched to expectations by position depend on an order the contract does not promise: find each record by its key, or `flake-ok: <why the order is promised>`",
    "o2": "an awaited page read asserted once runs before the page settles: use a web-first assertion (`await expect(locator).toHaveText(...)`) or `await expect.poll(() => read)`, or `flake-ok: <why a one-shot read is meant>`",
}
FLAKE_WAIT_NAME = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(until\w*|eventually\w*|wait_for\w*|wait_until\w*|poll_until\w*)\b")
FLAKE_WAIT_HOME = "crates/puddle-test-kit/"  # the shared kit's own helpers are the target, not a hit
FLAKE_CLOCK = re.compile(r"SystemTime::now\(\)|UNIX_EPOCH|as_nanos\(\)|subsec_nanos\(\)|\{nanos\}|\bnanos\b")
FLAKE_PAGE_READ = re.compile(r"\.(evaluate|evaluateAll|boundingBox|textContent|innerText|innerHTML|getAttribute|isVisible|isHidden|isChecked|isEnabled|isDisabled|isEditable|count|inputValue|allTextContents|allInnerTexts|all)\(|\b\w+\(\s*page\b")


def flake_row(add, rule, i, detail, reasoned):
    add(f"flake-{rule}", i, detail)
    if not reasoned:
        add(f"flake-{rule}-unreasoned", i, detail)
        if rule in FLAKE_FAILING:
            add("noreason", i, f"flake-{rule}: {FLAKE_FIX[rule]} | {detail}")


def flake_rust(f, raw, code, com, test, rows_add):
    """Per-file Rust rules. `rows_add(kind, i, detail)` records a row; returns the port-picking fns found."""
    def reasoned(i):
        return any(FLAKE_OK.search(com[k]) for k in range(max(0, i - 3), i + 1))

    def hit(rule, i, detail=None):
        flake_row(rows_add, rule, i, (detail if detail is not None else raw[i]).strip(), reasoned(i))

    pickers = {}
    for i, line in enumerate(code):
        t = test[i]
        if t and re.search(r"\.try_recv\(\)", line):
            hit("o1", i)
        m = re.search(r"\bfn\s+(\w+)[^{;]*->\s*u16\b", line)
        if m:
            e, _ = item_end(code, i)
            body = "\n".join(code[i:e + 1])
            if re.search(r"\bbind\(", body) and re.search(r"\.port\(\)", body) and not re.search(r"spawn|into_std|forget", body):
                pickers[m.group(1)] = i
                hit("p1", i)
        elif re.search(r"bind\([^;]*\)[^;]*\.local_addr\(\)[^;]*\.port\(\)", line) and not re.search(r"\blet\s+\w+\s*=\s*\w*Listener", line):
            hit("p1", i)
        if t and re.search(r"\b\w*file\w*\.lock(?:_exclusive|_shared)?\(\)", line, re.I):
            hit("w2", i)
        m = FLAKE_WAIT_NAME.match(line)
        if t and m and not f.startswith(FLAKE_WAIT_HOME):
            sig, k = line, i
            while "{" not in sig and k + 1 < len(code) and k < i + 6:
                k += 1
                sig += code[k]
            sig = sig.split("{")[0]
            named = bool(re.search(r":\s*&(?:'\w+\s+)?str\b|line:|what:|name:", sig))
            bounded = bool(re.search(r"Duration|Instant|within|limit|deadline|timeout", sig))
            hit("w1", i, ("no-name " if not named else "") + ("no-bound " if not bounded else "") + raw[i])
        if t and FLAKE_CLOCK.search(raw[i]) and re.search(r"format!|\.join\(|temp_dir", raw[i]):
            hit("n1", i)
        elif t and re.search(r"\benv::temp_dir\(\)", line):
            # the clock may be read a few lines above or below the path it names
            stmt = "".join(raw[max(0, i - 10):i + 1])
            k = i
            while k + 1 < len(code) and k < i + 8 and code[k + 1].strip() != "}":
                k += 1
                stmt += raw[k]
            if FLAKE_CLOCK.search(stmt) or "Instant::now()" in stmt:
                hit("n1", i)
        if t and re.search(r"\b(?:assert|debug_assert)\w*!\s*\(", line):
            stmt, k = line, i
            while ");" not in code[k] and k + 1 < len(code) and k < i + 6:
                k += 1
                stmt += code[k]
            if (re.search(r"<=?\s*Duration::from_|elapsed\(\)\s*<|Duration::from_\w+\([^)]*\)\s*>=?", stmt)
                    and not re.search(r"Instant::now\(\)\s*<=?\s*\w*deadline|now\(\)\s*<\s*\w+", stmt)):
                hit("t1", i)
        if t and re.search(r"\b\w*(record|event|row|entr|audit|line)\w*\.(?:iter\(\)\.)?zip\(", line, re.I):
            hit("a1", i)
    return pickers


def flake_port_calls(code, com, raw, pickers, rows_add):
    """Callers of a port-picking fn (rule p1): each one hands the caller a port another process can take."""
    for i, line in enumerate(code):
        for name in pickers:
            if re.search(rf"(?<!fn )\b{name}\(\)", line):
                ok = any(FLAKE_OK.search(com[k]) for k in range(max(0, i - 3), i + 1))
                flake_row(rows_add, "p1", i, raw[i].strip(), ok)


# Matchers that only a locator, page or response takes: `expect(dialog).toHaveCount(0)` on a variable is a
# web-first assertion that waits by itself, never a value read once.
FLAKE_WEB_FIRST = re.compile(
    r"\)\s*\.(?:not\.)?(?:toHave(?:Count|Text|Attribute|Class|CSS|Id|JSProperty|Value|Values|Title|URL|Role|AccessibleName|AccessibleDescription)"
    r"|toBe(?:Visible|Hidden|Checked|Enabled|Disabled|Editable|Focused|Attached|InViewport)|toContain(?:Text|Class)|toMatchAriaSnapshot|toBeOK)\b"
)


def flake_e2e(lines, rows_add):
    """Rule o2 in ui/e2e: an awaited page read asserted once (`expect(await read)`, or a read kept in a variable).
    A reason on the line that reads into a variable covers every `expect` of that variable."""
    awaited = {}  # variable -> (line of the read, whether that line carries a reason)

    def reasoned(i):
        for k in range(max(0, i - 3), i + 1):
            if FLAKE_OK.search(lines[k]):
                return True
        prev = lines[i - 1] if i else ""
        return bool(re.search(r"eslint-disable-next-line\s+no-restricted-syntax\s+--\s+\S", prev))

    for i, line in enumerate(lines):
        if line.strip().startswith("//"):
            continue
        m = re.search(r"\b(?:const|let)\s+(\w+)\s*=\s*await\s+(.*)", line)
        if m and FLAKE_PAGE_READ.search(m.group(2)):
            awaited[m.group(1)] = (i, reasoned(i))
        if re.search(r"^\s*(?:async\s+)?function\b|^\s*test\(", line):
            awaited.clear()
        for em in re.finditer(r"(?<![.\w])expect\((?!\s*\.)", line):
            inner = line[em.end():].lstrip()
            v = re.match(r"(\w+)\s*[,)]", inner)
            if inner.startswith("await ") and FLAKE_PAGE_READ.search(inner):
                flake_row(rows_add, "o2", i, line.strip(), reasoned(i))
            elif v and v.group(1) in awaited and not FLAKE_WEB_FIRST.search(line[em.end():]):
                flake_row(rows_add, "o2", i, line.strip(), reasoned(i) or awaited[v.group(1)][1])


def flake_summary(rows):
    """Per rule: sites, sites with no reason, the fix the error text names."""
    out = []
    for rule, fix in FLAKE_FIX.items():
        sites = sum(1 for r in rows if r[0] == f"flake-{rule}")
        bare = sum(1 for r in rows if r[0] == f"flake-{rule}-unreasoned")
        out.append((rule, sites, bare, "failing" if rule in FLAKE_FAILING else "counted", fix))
    return out


def counts(rows):
    c = Counter(f"{k}.{ctx}" for k, ctx, _, _ in rows)
    return dict(sorted(c.items()))


SELF_TEST = r'''
fn a() {
    // SAFETY: fine
    unsafe { x() };
    let s = "unsafe { not code }";
    unsafe { y() };
    let v = foo.unwrap(); // TODO later
    // FIXME(ana): tracked
    // HACK see #12
    // prose that mentions TODO in passing
    let p = r"C:\";
    unsafe { z() };
}
#[allow(clippy::foo)]
fn b() {}
#[expect(dead_code, reason = "kept")]
fn c() {}
#[cfg(test)]
mod tests {
    #[test]
    fn t() { let _ = 1; }
    #[test]
    fn u() { assert!(true); x.unwrap(); }
    #[tokio::test]
    async fn bare_sleep() {
        a().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(true);
    }
    #[tokio::test]
    async fn explained_sleep() {
        // Absence window: nothing may arrive.
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(true);
    }
    #[tokio::test(start_paused = true)]
    async fn virtual_sleep() {
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(true);
    }
    #[tokio::test]
    async fn multi_line_signature(
        x: u8,
    ) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(true);
    }
    #[tokio::test]
    async fn polled_sleep() {
        while !ids.iter().all(|p| {
            seen(p)
        }) {
            assert!(now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
'''


SELF_TEST_YAML = r'''
jobs:
  a:
    steps:
      - run: |
          # shellcheck disable=SC2086 # one word per browser
          npx playwright install-deps $browsers
          # a reason on the line above
          # shellcheck disable=SC2086
          npx playwright install $browsers
          # shellcheck disable=SC2086
          npx playwright install $browsers
'''


FLAKE_FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "flake-fixtures")
FLAKE_HOST = "crates/puddle-host/tests/fake_host.rs"
FLAKE_STORE = "crates/puddle-certs/tests/windows_store.rs"
FLAKE_LAB = "crates/puddle-e2e/src/lab/"
FLAKE_SPEC = "ui/e2e/x.spec.ts"


def flake_self_test():
    """Replay the original code of fixed flakes (scripts/flake-fixtures/, `.txt` so no gate scans them as
    code): each rule must flag the code that flaked, pass the fix where the fix leaves the shape
    behind, and accept a written reason. Returns the failures."""
    import tempfile
    failures = []

    def read(name):
        with open(os.path.join(FLAKE_FIXTURES, name), encoding="utf-8") as h:
            return h.read()

    def reasoned(text, needle):
        out = []
        for line in text.split("\n"):
            if needle in line:
                out.append("// flake-ok: the measured margin is 10x")
            out.append(line)
        return "\n".join(out)

    def run(files):
        with tempfile.TemporaryDirectory() as d:
            for rel, text in files.items():
                os.makedirs(os.path.dirname(os.path.join(d, rel)), exist_ok=True)
                with open(os.path.join(d, rel), "w", encoding="utf-8") as h:
                    h.write(text)
            return scan(d)

    def expect(label, rows, rule, sites, bare):
        got = (sum(1 for r in rows if r[0] == f"flake-{rule}"), sum(1 for r in rows if r[0] == f"flake-{rule}-unreasoned"))
        if got != (sites, bare):
            failures.append(f"{label}: flake-{rule} sites/unreasoned {got}, want {(sites, bare)}")

    def case(label, rule, files, sites, bare):
        expect(label, run(files), rule, sites, bare)

    case("o1 original", "o1", {FLAKE_HOST: read("o1-sign-in-event.bad.rs.txt")}, 1, 1)
    case("o1 fix", "o1", {FLAKE_HOST: read("o1-sign-in-event.good.rs.txt")}, 0, 0)
    case("o1 outside tests", "o1", {"crates/x/src/lib.rs": "fn f() { while let Ok(e) = rx.try_recv() { use_it(e); } }\n"}, 0, 0)
    case("o1 reasoned", "o1", {FLAKE_HOST: reasoned(read("o1-sign-in-event.bad.rs.txt"), "events.try_recv()")}, 1, 0)
    case("w1 original", "w1", {FLAKE_HOST: read("w1-wait-helper.bad.rs.txt")}, 1, 1)
    w1_rows = [r for r in run({FLAKE_HOST: read("w1-wait-helper.bad.rs.txt")}) if r[0] == "flake-w1"]
    if not (w1_rows and w1_rows[0][3].startswith("no-bound ")):
        failures.append(f"w1 original: the row must say the helper has no bound: {w1_rows}")
    case("w1 not a wait name", "w1", {FLAKE_HOST: "async fn settle(what: &str, within: Duration) {}\n"}, 0, 0)
    case("w1 in production code", "w1", {"crates/x/src/lib.rs": read("w1-wait-helper.bad.rs.txt")}, 0, 0)
    case("w1 reasoned", "w1", {FLAKE_HOST: reasoned(read("w1-wait-helper.bad.rs.txt"), "async fn eventually")}, 1, 0)
    case("w1 in the kit", "w1", {"crates/puddle-test-kit/tests/wait.rs": read("w1-wait-helper.bad.rs.txt")}, 0, 0)
    case("w2 original", "w2", {FLAKE_STORE: read("w2-store-lock.bad.rs.txt")}, 1, 1)
    case("w2 fix", "w2", {FLAKE_STORE: read("w2-store-lock.good.rs.txt")}, 0, 0)
    case("n1 original", "n1", {FLAKE_STORE: read("n1-clock-name.bad.rs.txt")}, 2, 2)
    case("n1 tempfile", "n1", {FLAKE_STORE: "fn t() { let dir = tempfile::tempdir().unwrap(); let file = dir.path().join(\"a.cer\"); }\n"}, 0, 0)
    mod_rs, call_bad, call_good = FLAKE_LAB + "mod.rs", FLAKE_LAB + "interceptor.rs", FLAKE_LAB + "interceptor.rs"
    case("p1 original", "p1", {mod_rs: read("p1-free-port.bad.rs.txt"), call_bad: read("p1-free-port-call.bad.rs.txt")}, 2, 2)
    case("p1 fix", "p1", {mod_rs: reasoned(read("p1-free-port.bad.rs.txt"), "pub fn free_port"), call_good: read("p1-free-port-call.good.rs.txt")}, 1, 0)
    case("a1 original", "a1", {"crates/puddle-e2e/tests/proxy_agent_store.rs": read("a1-positional-zip.bad.rs.txt")}, 1, 1)
    case("a1 fix", "a1", {"crates/puddle-e2e/tests/proxy_agent_store.rs": read("a1-positional-zip.good.rs.txt")}, 0, 0)
    store = "crates/puddle-store/tests/events_and_audit_query.rs"
    case("t1 original", "t1", {store: read("t1-wall-clock-bar.bad.rs.txt")}, 1, 1)
    case("t1 fix keeps the bar", "t1", {store: read("t1-wall-clock-bar.good.rs.txt")}, 1, 1)
    case("t1 a deadline loop is not a bar", "t1", {store: "fn t() { assert!(start.elapsed() < Duration::from_secs(1) || Instant::now() < deadline, \"late\"); }\n"}, 0, 0)
    case("t1 reasoned", "t1", {store: reasoned(read("t1-wall-clock-bar.good.rs.txt"), "assert!(best")}, 1, 0)
    case("o2 variable", "o2", {FLAKE_SPEC: read("o2-density-measure.bad.ts.txt")}, 1, 1)
    case("o2 poll", "o2", {FLAKE_SPEC: read("o2-density-measure.good.ts.txt")}, 0, 0)
    case("o2 focus", "o2", {FLAKE_SPEC: read("o2-focus-read.bad.ts.txt")}, 1, 1)
    one_shot = 'test("x", async ({ page }) => {\n  expect(await measure(page, "main select")).toBeNull();\n});\n'
    case("o2 inline", "o2", {FLAKE_SPEC: one_shot}, 1, 1)
    case("o2 reasoned", "o2", {FLAKE_SPEC: reasoned(one_shot, "expect(await")}, 1, 0)
    lint = one_shot.replace("  expect", "  // eslint-disable-next-line no-restricted-syntax -- the data is held\n  expect")
    case("o2 eslint-disable with a reason", "o2", {FLAKE_SPEC: lint}, 1, 0)
    held = 'test("x", async ({ page }) => {\n  const dialog = await openDialog(page);\n  await expect(dialog).toHaveCount(0);\n  await expect(dialog).not.toBeVisible();\n});\n'
    case("o2 a locator in a variable is a web-first assertion", "o2", {FLAKE_SPEC: held}, 0, 0)
    value = 'test("x", async ({ page }) => {\n  const n = await page.locator("li").count();\n  expect(n).toBeGreaterThan(0);\n  expect(n).toBeLessThan(9);\n});\n'
    case("o2 a count in a variable is read once per expect", "o2", {FLAKE_SPEC: value}, 2, 2)
    case("o2 a reason on the read covers its expects", "o2", {FLAKE_SPEC: value.replace("  const n", "  // flake-ok: the list is complete\n  const n")}, 2, 0)

    # A counted rule adds no `noreason` row; a rule flipped to failing adds one that opens with its fix.
    bad = {
        "o1": {FLAKE_HOST: read("o1-sign-in-event.bad.rs.txt")},
        "w1": {FLAKE_HOST: read("w1-wait-helper.bad.rs.txt")},
        "w2": {FLAKE_STORE: read("w2-store-lock.bad.rs.txt")},
        "n1": {FLAKE_STORE: read("n1-clock-name.bad.rs.txt")},
        "p1": {mod_rs: read("p1-free-port.bad.rs.txt")},
        "a1": {"crates/puddle-e2e/tests/proxy_agent_store.rs": read("a1-positional-zip.bad.rs.txt")},
        "t1": {store: read("t1-wall-clock-bar.bad.rs.txt")},
        "o2": {FLAKE_SPEC: read("o2-density-measure.bad.ts.txt")},
    }
    if sorted(bad) != sorted(FLAKE_FIX):
        failures.append(f"every rule needs a failing-flip case: {sorted(set(FLAKE_FIX) ^ set(bad))}")
    for rule, files in bad.items():
        live = rule in FLAKE_FAILING  # a rule already flipped stays flipped once its case is done
        FLAKE_FAILING.discard(rule)
        try:
            if any(r[0] == "noreason" for r in run(files)):
                failures.append(f"{rule}: a counted rule must not add a noreason row")
            FLAKE_FAILING.add(rule)
            miss = [r for r in run(files) if r[0] == "noreason"]
        finally:
            if not live:
                FLAKE_FAILING.discard(rule)
            else:
                FLAKE_FAILING.add(rule)
        if not miss or not all(r[3].startswith(f"flake-{rule}: {FLAKE_FIX[rule]} | ") for r in miss):
            failures.append(f"{rule}: a failing rule adds noreason rows that open with the whole fix text: {miss}")
    return failures


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "crates/x/src"))
        with open(os.path.join(d, "crates/x/src/lib.rs"), "w") as h:
            h.write(SELF_TEST)
        os.makedirs(os.path.join(d, ".github/workflows"))
        with open(os.path.join(d, ".github/workflows/ci.yml"), "w") as h:
            h.write(SELF_TEST_YAML)
        rows = scan(d)
        got = counts(rows)
        # The flagged directive is the bare one in the workflow, on its own line.
        flagged = [r[2] for r in rows if r[0] == "noreason" and r[2].startswith(".github/")]
        if flagged != [".github/workflows/ci.yml:11"]:
            print("self-test FAIL workflow noreason rows", flagged)
            return 1
    bad_flake = flake_self_test()
    if bad_flake:
        print("self-test FAIL flake rules:\n  " + "\n  ".join(bad_flake))
        return 1
    want = {
        "shellcheck-disable.any": 3, "noreason.any": 2,
        "unsafe-block.prod": 3, "unwrap.prod": 1, "unwrap.test": 1,
        "allow-attr.prod": 1, "expect-attr.prod": 1,
        "test-no-assert.test": 1, "marker-todo.any": 1, "marker-fixme.any": 1, "marker-hack.any": 1,
        "noreason.prod": 3,
        "sleep.test": 5, "sleep-fixed.test": 3, "sleep-fixed-uncommented.test": 2, "noreason.test": 2,
    }
    bad = {k: (got.get(k), v) for k, v in want.items() if got.get(k) != v}
    extra = {k: v for k, v in got.items() if k not in want}
    if bad or extra:
        print("self-test FAIL", bad, extra)
        return 1
    print("self-test pass")
    return 0


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
