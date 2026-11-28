"""Flake-prone test shapes for quality-gate.py, moved out of it to keep that file under 1000 lines.

quality-gate.py loads this file by path (it runs with `python3 -I`, so a sibling is not importable)
and uses `item_end`, `flake_rust`, `flake_e2e`, `flake_fixed_port`, `flake_port_calls`,
`flake_summary`, `load_flake_rules` and the `FLAKE_*` tables from it.
"""
import os
import re


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


# Flake-prone test shapes (counted; failing once the rule's file says so). Each rule has two kinds: `flake-<id>` for every
# site and `flake-<id>-unreasoned` for the sites with no `flake-ok: <why>` comment on the line or within
# three lines above (a UI line may carry `eslint-disable-next-line no-restricted-syntax -- <why>` instead).
# A failing rule also adds a `noreason` row per unreasoned site, so `--missing` and the `quality`
# gate fail on it. Each rule is one file, `scripts/flake-rules/<id>.rule` (`status: counted|failing`, `fix:`, and the
# `fixture:` and `as:` the self-test replays). Flipping a rule is a one-line edit of its own file and a new rule is a
# new file, so two branches that each add or flip a rule never edit the same line (`scripts/flake-rules-test.sh`).
# The scan reads shapes, not truth: a reason is read by a reviewer, and a shape it cannot see
# (a producer that is synchronous, a wait anchored on something already true) needs a run under load.
FLAKE_RULES_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "flake-rules")
FLAKE_RULE_KEYS = ("status", "fix", "fixture", "as")


def load_flake_rules(directory):
    """Read `<id>.rule` files: one `key: value` per line, keys in FLAKE_RULE_KEYS, status counted or failing."""
    rules = {}
    for name in sorted(os.listdir(directory)):
        if not name.endswith(".rule"):
            continue
        fields = {}
        with open(os.path.join(directory, name), encoding="utf-8") as h:
            for n, line in enumerate(h, 1):
                key, sep, value = line.rstrip("\n").partition(": ")
                if not sep or key not in FLAKE_RULE_KEYS or key in fields:
                    raise SystemExit(f"{name}:{n}: want one `key: value` per line with keys {FLAKE_RULE_KEYS}, each once")
                fields[key] = value
        if sorted(fields) != sorted(FLAKE_RULE_KEYS) or fields["status"] not in ("counted", "failing"):
            raise SystemExit(f"{name}: needs {FLAKE_RULE_KEYS} and `status: counted` or `status: failing`")
        rules[name[:-len(".rule")]] = fields
    return rules


FLAKE_RULES = load_flake_rules(FLAKE_RULES_DIR)
FLAKE_FAILING = {r for r, v in FLAKE_RULES.items() if v["status"] == "failing"}
FLAKE_FIX = {r: v["fix"] for r, v in FLAKE_RULES.items()}
FLAKE_OK = re.compile(r"flake-ok:\s*\S")
FLAKE_POSIX_LIT = re.compile(r'"/(?:tmp|home|no|nonexistent)\b')  # a literal Unix path; `/none` (a URL path) is not `/no`
FLAKE_PATH_SINK = re.compile(r"\bPath(?:Buf)?::(?:new|from)\(|\.join\(|\b\w*(?:dir|path|file|root)\w*\b.*\.(?:into|to_owned)\(\)")
FLAKE_PATH_FORMAT = re.compile(r'format!\(\s*"\{[^}"]*\}/\{')
FLAKE_PATH_WORD = re.compile(r"\b\w*(?:dir|path|root|tmp|home|base)\w*\b", re.I)
FLAKE_WAIT_NAME = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(until\w*|eventually\w*|wait_for\w*|wait_until\w*|poll_until\w*)\b")
FLAKE_WAIT_HOME = "crates/puddle-test-kit/"  # the kit's own helpers (the crate is that path) are the target, not a hit
FLAKE_RESERVED = re.compile(r'"[^"]*(?<![@\w.-])(?:[a-z0-9-]+\.)*(?:[a-z0-9-]+\.(?:test|example)|example\.(?:com|net|org))(?![\w-]|\.[a-z0-9])', re.I)  # not `x.test.invalid` (a `.invalid` name) or `x.test.rs` (a file)
FLAKE_REAL_DNS = re.compile(r"\b(?:web_internet|through_company_proxy|SystemResolver)\b")  # a test file where a connect can reach the OS resolver
FLAKE_LOOP_HEAD = re.compile(r"^\s*(?:'\w+:\s*)?(?:(?:let\b[^=]*=\s*|break\s+)?loop\b|for\b.*\bin\b|while\b)")
FLAKE_PAUSE = re.compile(r"\bsleep\(|\byield_now\(\)|\.tick\(\)")
FLAKE_KIT_CALL = re.compile(r"\b(?:eventually\w*|next_event_\w+|no_event_\w+)\s*\(")
FLAKE_EXEC_MODE = re.compile(r"from_mode\(\s*0o0*[1357][0-7]{2}\b")  # an owner-executable mode; 0o644, 0o600 and friends stay out
FLAKE_CHMOD_X = re.compile(r'Command::new\(\s*"chmod"\s*\)')
FLAKE_FILE_WRITE = re.compile(r"\bfs::write\(|\bFile::create\(|\.write_all\(|\bfs::copy\(|\bwrite\(\s*&?\w*(?:path|file)")
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

    # Rule w3: a loop that pauses between looks is a wait written by hand, whatever it is called. Only the
    # innermost loop of a nest counts, and a loop on tokio's paused clock (`advance`) steps virtual time.
    loops = []
    wait_fns = [(i, item_end(code, i)[0]) for i, line in enumerate(code) if FLAKE_WAIT_NAME.match(line)]  # rule w1 owns these
    for i, line in enumerate(code):
        if test[i] and not f.startswith(FLAKE_WAIT_HOME) and FLAKE_LOOP_HEAD.match(line) and not any(a < i <= b for a, b in wait_fns):
            end, _ = item_end(code, i)
            body = code[i:end + 1]
            if any(FLAKE_PAUSE.search(b) for b in body) and not any(re.search(r"\badvance\(", b) for b in body):
                loops.append((i, end))
    for i, end in loops:
        if not any(i < j and e <= end for j, e in loops):
            hit("w3", i)

    pickers = {}
    picker_ranges = [(i, item_end(code, i)[0]) for i, line in enumerate(code)
                     if re.search(r"\bfn\s+\w+[^{;]*->\s*u16\b", line)]
    stdin_pipes = set()  # names bound to a child's `stdin.take()` earlier in the file (rule s1)
    real_dns = f.startswith("crates/puddle-vm-tests/") or (("/tests/" in f or f.startswith("tests/")) and any(FLAKE_REAL_DNS.search(c) for c in code))
    for i, line in enumerate(code):
        t = test[i]
        if t and real_dns and FLAKE_RESERVED.search(raw[i]) and not raw[i].lstrip().startswith("//"):
            hit("d1", i)
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
        if t:
            flake_dropped_port(code, raw, i, hit, picker_ranges)
            flake_refused_after_stop(code, i, hit)
        if t and re.search(r"\b\w*file\w*\.lock(?:_exclusive|_shared)?\(\)", line, re.I):
            hit("w2", i)
        m = FLAKE_WAIT_NAME.match(line)
        if m and t and not f.startswith(FLAKE_WAIT_HOME) and FLAKE_KIT_CALL.search("\n".join(code[i:item_end(code, i)[0] + 1]).partition("{")[2]):
            m = None  # a helper that only fills in the probe of a kit wait is named and bounded by the kit
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
                    and not re.search(r"Instant::now\(\)\s*<=?\s*\w*deadline|now\(\)\s*<\s*\w+", stmt)
                    and not (re.search(r"\b\w*cpu\w*\s*<=?\s*Duration::from_", stmt) and not re.search(r"elapsed\(\)|Instant", stmt)
                             and any("cpu_time(" in c for c in code[max(0, i - 15):i]))):  # a bar on `puddle_test_kit::cpu_time`
                hit("t1", i)
        # Rule e1: a file this test wrote in the last few lines, then made executable. The write leaves a writable
        # descriptor open in this process, a sibling test's fork copies it, and the exec fails with `Text file busy`.
        if t and ("set_permissions(" in line or FLAKE_CHMOD_X.search(raw[i])):
            ahead = "".join(raw[i:i + 5])
            if (FLAKE_EXEC_MODE.search(ahead) or ("chmod" in raw[i] and "+x" in ahead)) and any(FLAKE_FILE_WRITE.search(code[k]) for k in range(max(0, i - 12), i + 1)):
                hit("e1", i)
        # Rule s1: a test that writes a child's piped stdin and unwraps the result. A child that exits before it reads
        # (the script failed, the hook refused the plan) closes the pipe, the write returns `BrokenPipe`, and the test panics
        # on that instead of reading the child's own outcome. `puddle_test_kit::feed_stdin` treats that as the child's result.
        if t and "write_all(" in line:
            k = i
            while k > 0 and i - k < 6 and code[k - 1].strip() and not code[k - 1].rstrip().endswith((";", "{", "}")):
                k -= 1
            stmt = "".join(code[k:min(len(code), i + 4)]).partition(";")[0]
            head, _, tail = stmt.partition("write_all(")
            taken = re.search(r"\.stdin\s*\.take\(\)", head) or any(re.search(rf"(?<![\w.]){v}\s*\.\s*$", head) for v in stdin_pipes)
            checked = re.search(r"\)\s*(?:\.await\s*)?(?:\.unwrap\(\)|\.expect\(|\?)", tail) and not re.match(r"\s*let\s+_\b", stmt)
            if taken and checked:
                hit("s1", k)
        mp = re.search(r"\blet\s+(?:mut\s+)?(\w+)\s*=\s*\w+(?:\.\w+)*\.stdin\s*\.take\(\)", line)
        if mp:
            stdin_pipes.add(mp.group(1))
        if t and re.search(r"\b\w*(record|event|row|entr|audit|line)\w*\.(?:iter\(\)\.)?zip\(", line, re.I):
            hit("a1", i)
        # Rule f1: a Unix path literal that becomes a host path, or two parts joined with a `/` by hand, is a test that
        # passes on Linux and fails on Windows (`/tmp` has no drive, `a/b` mixes separators). Guest paths stay strings.
        if t and not raw[i].lstrip().startswith("//") and "://" not in raw[i]:
            if (FLAKE_POSIX_LIT.search(raw[i]) and FLAKE_PATH_SINK.search(line)) or (
                    FLAKE_PATH_FORMAT.search(raw[i]) and FLAKE_PATH_WORD.search(raw[i])):
                hit("f1", i)
    return pickers


FLAKE_SOCKET_BIND = re.compile(r"\b(?:TcpListener|UdpSocket)::bind\(")
FLAKE_HELD_PORT = re.compile(r"\b(?:quiet_port|ReservedPort|refused_port|reserved)\b")
FLAKE_FN_HEAD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s")


def statement_from(code, i, span=8):
    """The text of the statement that starts on line i: up to the first `;`, or a closing brace line, within `span` lines."""
    out = []
    for k in range(i, min(len(code), i + span)):
        out.append(code[k])
        if ";" in code[k] or code[k].strip() == "}":
            break
    return "\n".join(out).split(";")[0]


def flake_dropped_port(code, raw, i, hit, picker_ranges):
    """Rule p1 beyond a picker fn: a listener bound to port 0 whose port is read and then given up (a chain
    that drops the listener in the same statement, or `drop(listener)` after `listener.local_addr()`)."""
    line = code[i]
    if not FLAKE_SOCKET_BIND.search(line) or re.search(r"\blet\s*\(", line) or any(a <= i <= b for a, b in picker_ranges):
        return
    stmt = statement_from(code, i)
    after_bind = FLAKE_SOCKET_BIND.split(stmt, maxsplit=1)[-1]
    if "local_addr(" in after_bind:
        hit("p1", i)
        return
    m = re.search(r"\blet\s+(?:mut\s+)?(\w+)\s*=", line)
    if not m:
        return
    name = m.group(1)
    end = item_end(code, max(range(i + 1), key=lambda k: k if FLAKE_FN_HEAD.match(code[k]) else -1))[0]
    ports = set()  # variables that took the listener's address or port
    for j in range(i + 1, min(end, i + 60) + 1):
        if re.search(rf"\b{name}\.local_addr\(\)", code[j]):
            taken = re.match(r"\s*let\s+(?:mut\s+)?(\w+)\s*=", code[j])
            if taken:
                ports.add(taken.group(1))
        if re.search(rf"\bdrop\(\s*{name}\s*\)", code[j]):
            if any(re.search(rf"\b{v}\b", c) for v in ports for c in raw[j + 1:end + 1]):  # the number is used after the listener is gone
                hit("p1", i)
            return


def flake_refused_after_stop(code, i, hit):
    """Rule p3: a connect that must fail because a server stopped or never was. The port is a stranger's by then
    unless the test holds it (a port from `quiet_port` or `ReservedPort`)."""
    line = code[i]
    if not re.search(r"\bTcpStream::connect\(", line):
        return
    stmt = statement_from(code, i)
    if not re.search(r"\.is_err\(\)|\.unwrap_err\(\)|\.expect_err\(", stmt.partition("connect(")[2]):
        return
    start = max((k for k in range(i + 1) if FLAKE_FN_HEAD.match(code[k])), default=0)
    if any(FLAKE_HELD_PORT.search(code[k]) for k in range(start, i + 1)):
        return
    hit("p3", i)


FLAKE_NET_CALL = re.compile(r"\b(?:bind|connect)\w*\s*\(|\blisten\s*\(|\b(?:Tcp(?:Listener|Stream)|UdpSocket)\b")
# A port after an address of this machine (an IP, `localhost`, `[::1]`) or a bare `:port`. A name (`api.test:8443`) is an
# authority handed to a fake, not a socket, and a URL (`://`) is a fake site: both stay out.
FLAKE_LOCAL_ADDR = re.compile(r'(?:(?<=\d)|(?<=localhost)|(?<=\])|(?<![\w.-])):[1-9]\d{3,4}(?!\d)', re.I)
FLAKE_BIND_LITERAL = re.compile(r'bind\(\s*(?:"[^"]*:[1-9]\d*"|\(\s*[^,()]+,\s*[1-9]\d*\s*\))')


def flake_fixed_port(raw, code, com, test, rows_add):
    """Rule p2: a test that binds or connects a literal port. Another test or process may hold it, and it stays held
    after a run that died. A literal 4 or 5 digit port in a string within two lines of `bind`, `listen` or `connect`
    (the string is read from `raw`; `code` has them blanked), or a `bind` of any nonzero literal."""
    for i, line in enumerate(code):
        if not test[i]:
            continue
        near = any(FLAKE_NET_CALL.search(code[k]) for k in range(max(0, i - 2), min(len(code), i + 3)))
        strings = [m for m in re.findall(r'"(?:[^"\\]|\\.)*"', raw[i]) if "://" not in m]
        if FLAKE_BIND_LITERAL.search(raw[i]) or (near and any(FLAKE_LOCAL_ADDR.search(m) for m in strings)):
            ok = any(FLAKE_OK.search(com[k]) for k in range(max(0, i - 3), i + 1))
            flake_row(rows_add, "p2", i, raw[i].strip(), ok)


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


def expect_argument(text):
    """The text of an `expect(...)` argument, up to its closing parenthesis (or the end of the line)."""
    depth = 1
    for k, ch in enumerate(text):
        depth += (ch == "(") - (ch == ")")
        if depth == 0:
            return text[:k]
    return text


def flake_e2e(lines, rows_add):
    """Rule o2 in ui/e2e: a read of the page kept in a variable and asserted once (`const box = (await loc.boundingBox())!`,
    then `expect(box.width)`: a non-null assertion says "there is a box now"; a plain `const n = await read` only counts for `expect(n)` itself). A reason on the line that reads into a variable covers every `expect` of that variable.
    The inline `expect(await read)` is ESLint's (`ui/eslint.config.js`, an error), not this scan's."""
    awaited = {}  # variable -> (whether that line carries a reason, whether an expect on any use of it counts)

    def reasoned(i):
        for k in range(max(0, i - 3), i + 1):
            if FLAKE_OK.search(lines[k]):
                return True
        prev = lines[i - 1] if i else ""
        return bool(re.search(r"eslint-disable-next-line\s+no-restricted-syntax\s+--\s+\S", prev))

    for i, line in enumerate(lines):
        if line.strip().startswith("//"):
            continue
        m = re.search(r"\b(?:const|let)\s+(\w+)\s*=\s*\(?\s*await\s+(.*)", line)
        if m and FLAKE_PAGE_READ.search(m.group(2)):
            awaited[m.group(1)] = (reasoned(i), bool(re.search(r"\)\s*!\s*;?\s*$", line)))
        if re.search(r"^\s*(?:async\s+)?function\b|^\s*test\(", line):
            awaited.clear()
        for em in re.finditer(r"(?<![.\w])expect\((?!\s*\.)", line):
            argument = expect_argument(line[em.end():])
            first = re.match(r"\s*(\w+)\s*(?:,|$)", argument)  # `expect(n)` or `expect(n, "message")`
            used = [first.group(1)] if first and first.group(1) in awaited else []
            if not used:  # `expect(box.width)`: only a read of the non-null form counts
                used = [v for v in re.findall(r"(?<![.\w])(\w+)", re.sub(r"`[^`]*`|\"[^\"]*\"|'[^']*'", "", argument)) if v in awaited and awaited[v][1]]
            if used and not FLAKE_WEB_FIRST.search(line[em.end():]):
                flake_row(rows_add, "o2", i, line.strip(), reasoned(i) or awaited[used[0]][0])


def flake_summary(rows):
    """Per rule: sites, sites with no reason, the fix the error text names."""
    out = []
    for rule, fix in FLAKE_FIX.items():
        sites = sum(1 for r in rows if r[0] == f"flake-{rule}")
        bare = sum(1 for r in rows if r[0] == f"flake-{rule}-unreasoned")
        out.append((rule, sites, bare, "failing" if rule in FLAKE_FAILING else "counted", fix))
    return out
