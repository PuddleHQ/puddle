"""The scanner's self-test, moved out of quality-gate.py to keep that file under 1000 lines.

`quality-gate.py --self-test` loads this file by path and calls `self_test(gate)`, `gate` being
the running scanner module (it is run with `python3 -I`, so a sibling is not importable).
"""
import os

gate = None  # the scanner module, set by self_test

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
fn d(tx: Tx, unused: u8) {
    let _ = tx.send(1);
    // the receiver may be gone
    let _ = tx.send(2);
    let _ = unused;
    let _guard = tx.lock();
}
#[allow(clippy::foo)]
fn b() {}
fn e() -> Result<(), String> { Ok(()) }
fn h() -> Result<Vec<String>, String> { Ok(vec![]) }
fn m() -> Result<BTreeMap<String, String>, Error> { todo() }
fn n() -> io::Result<String> { todo() }
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
        // flake-ok: the fixture of a polled sleep
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


def kit_self_test():
    """The kit may be a dev-dependency (also per target) and a normal dependency of a test-only crate; a
    product crate listing it under [dependencies] or [build-dependencies], renamed or per target, fails."""
    import tempfile
    manifests = {
        "crates/dev/Cargo.toml": '[dev-dependencies]\npuddle-test-kit = { workspace = true }\n[target.\'cfg(unix)\'.dev-dependencies]\npuddle-test-kit = { workspace = true }\n',
        "crates/puddle-e2e/Cargo.toml": '[dependencies]\npuddle-test-kit = { workspace = true }\n',
        "crates/prod/Cargo.toml": '[dependencies]\npuddle-test-kit = { workspace = true }\n',
        "crates/buildscript/Cargo.toml": '[build-dependencies]\nkit = { package = "puddle-test-kit", path = "../x" }\n',
        "crates/pertarget/Cargo.toml": '[target.\'cfg(windows)\'.dependencies]\npuddle-test-kit = { workspace = true }\n',
    }
    with tempfile.TemporaryDirectory() as d:
        for name, text in manifests.items():
            os.makedirs(os.path.dirname(os.path.join(d, name)))
            with open(os.path.join(d, name), "w", encoding="utf-8") as h:
                h.write(text)
        flagged = sorted(r[2].split(":")[0] for r in gate.scan(d) if r[0] == "noreason")
    want = ["crates/buildscript/Cargo.toml", "crates/pertarget/Cargo.toml", "crates/prod/Cargo.toml"]
    return [] if flagged == want else [f"flagged {flagged}, wanted {want}"]


ROOT_CASES = {
    "forbid": "#![forbid(unsafe_code)]\n",
    "cfg_forbid": "#![cfg_attr(not(windows), forbid(unsafe_code))]\n",
    "expect": '#![expect(\n    unsafe_code,\n    reason = "FFI to libc"\n)]\n',
    "expect_empty_reason": '#![expect(unsafe_code, reason = "")]\n',
    "expect_no_reason": "#![expect(unsafe_code)]\n",
    "deny_only": "#![deny(unsafe_code)]\n",
    "commented": "// #![forbid(unsafe_code)]\n",
    "in_string": 'const S: &str = "#![forbid(unsafe_code)]";\n',
    "other_attr": "#![warn(missing_docs)]\n",
    "bare": "pub fn f() {}\n",
}
ROOT_BAD = {"expect_empty_reason", "expect_no_reason", "deny_only", "commented", "in_string", "other_attr", "bare"}


def root_self_test():
    """The rule fails for a root without the attribute and passes once the attribute is there: each bad
    case is flagged, then the same crate with `#![forbid(unsafe_code)]` added is not; a main.rs and a
    nested module file are read as the rule says."""
    import tempfile
    problems = []
    with tempfile.TemporaryDirectory() as d:
        for name, text in ROOT_CASES.items():
            os.makedirs(os.path.join(d, f"crates/{name}/src"))
            with open(os.path.join(d, f"crates/{name}/src/lib.rs"), "w", encoding="utf-8") as h:
                h.write(text + "pub fn f() {}\n")
        # Bin targets and build scripts: a bad and a good one each (a crate whose lib.rs is fine).
        for crate, attr in (("binbad", ""), ("bingood", "#![forbid(unsafe_code)]\n")):
            os.makedirs(os.path.join(d, f"crates/{crate}/src/bin"))
            for rel in ("src/lib.rs", "src/bin/tool.rs", "build.rs"):
                with open(os.path.join(d, f"crates/{crate}/{rel}"), "w", encoding="utf-8") as h:
                    h.write("#![forbid(unsafe_code)]\nfn main() {}\n" if rel == "src/lib.rs" else attr + "fn main() {}\n")
        flagged_files = {r[2].split(":")[0] for r in gate.scan(d) if r[0] == "no-forbid"}
        if {f for f in flagged_files if f.startswith(("crates/binbad/", "crates/bingood/"))} != {
                "crates/binbad/src/bin/tool.rs", "crates/binbad/build.rs"}:
            problems.append(f"bin/build.rs rule flagged {sorted(flagged_files)}")
        os.makedirs(os.path.join(d, "crates/bin/src"))
        with open(os.path.join(d, "crates/bin/src/main.rs"), "w", encoding="utf-8") as h:
            h.write("fn main() {}\n")
        with open(os.path.join(d, "crates/bin/src/other.rs"), "w", encoding="utf-8") as h:
            h.write("pub fn g() {}\n")
        flagged = {r[2].split("/")[1] for r in gate.scan(d) if r[0] == "no-forbid"}
        want = ROOT_BAD | {"bin", "binbad"}
        if flagged != want:
            problems.append(f"crate root rule flagged {sorted(flagged)}, want {sorted(want)}")
        for name in want:
            for fname in ("lib.rs", "main.rs"):
                path = os.path.join(d, f"crates/{name}/src/{fname}")
                if os.path.exists(path):
                    with open(path, encoding="utf-8") as h:
                        body = h.read()
                    with open(path, "w", encoding="utf-8") as h:
                        h.write("#![forbid(unsafe_code)]\n" + body)
        for rel in ("src/bin/tool.rs", "build.rs"):
            path = os.path.join(d, f"crates/binbad/{rel}")
            with open(path, encoding="utf-8") as h:
                body = h.read()
            with open(path, "w", encoding="utf-8") as h:
                h.write("#![forbid(unsafe_code)]\n" + body)
        after = [r for r in gate.scan(d) if r[0] == "no-forbid" or "crate root" in r[3]]
        if after:
            problems.append(f"crate root rule still flags after the fix: {after}")
    return problems


def inline_test_self_test():
    """Every inline `#[cfg(test)] mod` block is flagged, even a 3 line one, and the message names the
    sibling form; a sibling declaration, its file of any size and a module without `#[cfg(test)]` are not."""
    import tempfile

    def inline(name, n):
        body = "".join(f"    fn t{k}() {{}}\n" for k in range(n))
        return f"pub fn f() {{}}\n\n#[cfg(test)]\nmod {name} {{\n{body}}}\n"

    cases = {
        "big": inline("tests", 900), "small": inline("tests", 1),
        "helper": inline("memory", 3).replace("mod memory", "pub(crate) mod memory"),
        "plain": inline("tests", 5).replace("#[cfg(test)]\n", ""),
        "sibling": "pub fn f() {}\n\n#[cfg(test)]\nmod tests;\n",
    }
    problems = []
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "crates/x/src/sibling"))
        for name, text in cases.items():
            with open(os.path.join(d, f"crates/x/src/{name}.rs"), "w", encoding="utf-8") as h:
                h.write(text)
        with open(os.path.join(d, "crates/x/src/lib.rs"), "w", encoding="utf-8") as h:
            h.write("#![forbid(unsafe_code)]\n")
        with open(os.path.join(d, "crates/x/src/sibling/tests.rs"), "w", encoding="utf-8") as h:
            h.write("#[test]\nfn t() { assert!(true); }\n" + "".join(f"fn h{k}() {{}}\n" for k in range(900)))
        rows = [r for r in gate.scan(d) if r[0] == "noreason" and "inline `mod" in r[3]]
        files = sorted(r[2].split(":")[0] for r in rows)
        want = ["crates/x/src/big.rs", "crates/x/src/helper.rs", "crates/x/src/small.rs"]
        if files != want:
            problems.append(f"inline test block rule flagged {files}, want {want}")
        elif any("sibling" not in r[3] or ".rs`" not in r[3] for r in rows):
            problems.append(f"the message does not name the sibling form: {rows[0][3]}")
    return problems


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
            return gate.scan(d)

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
    kit_wait = ("async fn wait_for_rows(store: &Store, count: usize) -> Vec<Row> {\n"
                "    eventually(\"rows\", Duration::from_secs(5), || store.rows().filter(|r| r.len() >= count)).await\n}\n")
    case("w1 delegates to the kit", "w1", {FLAKE_HOST: kit_wait}, 0, 0)
    case("w1 names the kit only in a comment", "w1", {FLAKE_HOST: kit_wait.replace("    eventually(", "    // eventually(\n    loop_until(")}, 1, 1)
    case("w1 in the kit", "w1", {"crates/puddle-test-kit/tests/wait.rs": read("w1-wait-helper.bad.rs.txt")}, 0, 0)
    spin = read("w3-sleep-loop.bad.rs.txt")
    case("w3 original", "w3", {FLAKE_HOST: spin}, 1, 1)
    case("w3 reasoned", "w3", {FLAKE_HOST: reasoned(spin, "for _ in")}, 1, 0)
    case("w3 in production code", "w3", {"crates/x/src/lib.rs": spin}, 0, 0)
    case("w3 in the kit", "w3", {"crates/puddle-test-kit/src/wait.rs": spin}, 0, 0)
    case("w3 a loop that never pauses", "w3", {FLAKE_HOST: spin.replace("tokio::time::sleep(Duration::from_millis(10)).await;", "step();")}, 0, 0)
    case("w3 a while loop that yields", "w3", {FLAKE_HOST: "async fn t() {\n    while !done() {\n        tokio::task::yield_now().await;\n    }\n}\n"}, 1, 1)
    case("w3 virtual time", "w3", {FLAKE_HOST: spin.replace("tokio::time::sleep(Duration::from_millis(10))", "tokio::time::advance(STEP)")}, 0, 0)
    case("w3 a loop that yields a value", "w3", {FLAKE_HOST: "async fn t() {\n    let link = loop {\n        if let Some(l) = get() {\n            break l;\n        }\n        sleep(D).await;\n    };\n}\n"}, 1, 1)
    case("w3 only the inner loop counts", "w3", {FLAKE_HOST: "async fn t() {\n    for _ in 0..3 {\n        loop {\n            sleep(D).await;\n        }\n    }\n}\n"}, 1, 1)
    case("w2 original", "w2", {FLAKE_STORE: read("w2-store-lock.bad.rs.txt")}, 1, 1)
    case("w2 fix", "w2", {FLAKE_STORE: read("w2-store-lock.good.rs.txt")}, 0, 0)
    case("n1 original", "n1", {FLAKE_STORE: read("n1-clock-name.bad.rs.txt")}, 2, 2)
    case("n1 tempfile", "n1", {FLAKE_STORE: "fn t() { let dir = tempfile::tempdir().unwrap(); let file = dir.path().join(\"a.cer\"); }\n"}, 0, 0)
    mod_rs, call_bad, call_good = FLAKE_LAB + "mod.rs", FLAKE_LAB + "interceptor.rs", FLAKE_LAB + "interceptor.rs"
    case("p1 original", "p1", {mod_rs: read("p1-free-port.bad.rs.txt"), call_bad: read("p1-free-port-call.bad.rs.txt")}, 2, 2)
    case("p1 fix", "p1", {mod_rs: reasoned(read("p1-free-port.bad.rs.txt"), "pub fn free_port"), call_good: read("p1-free-port-call.good.rs.txt")}, 1, 0)
    case("p1 a picker that keeps its listener is not one", "p1", {mod_rs: "pub fn serve() -> u16 {\n    let l = TcpListener::bind((LOCAL, 0)).unwrap();\n    let port = l.local_addr().unwrap().port();\n    tokio::spawn(async move { l.accept().await });\n    port\n}\n"}, 0, 0)
    case("p1 a held listener is not one", "p1", {mod_rs: "fn t() { let listener = TcpListener::bind((LOCAL, 0)).unwrap(); let port = listener.local_addr().unwrap().port(); }\n"}, 0, 0)
    dropped, held = read("p1-dropped-listener.bad.rs.txt"), read("p1-dropped-listener.good.rs.txt")
    case("p1 a dead port made by a dropped listener", "p1", {FLAKE_HOST: dropped}, 2, 2)
    case("p1 a dead port held for the test", "p1", {FLAKE_HOST: held}, 0, 0)
    case("p1 a dropped listener reasoned", "p1", {FLAKE_HOST: reasoned(reasoned(dropped, "let closed"), "let gone")}, 2, 0)
    case("p3 original", "p3", {FLAKE_HOST: read("p3-refused-connect.bad.rs.txt")}, 2, 2)
    case("p3 fix", "p3", {FLAKE_HOST: read("p3-refused-connect.good.rs.txt")}, 0, 0)
    case("p3 reasoned", "p3", {FLAKE_HOST: reasoned(read("p3-refused-connect.bad.rs.txt"), "TcpStream::connect")}, 2, 0)
    case("p3 in production code", "p3", {"crates/x/src/lib.rs": read("p3-refused-connect.bad.rs.txt")}, 0, 0)
    case("a1 original", "a1", {"crates/puddle-e2e/tests/proxy_agent_store.rs": read("a1-positional-zip.bad.rs.txt")}, 1, 1)
    case("a1 fix", "a1", {"crates/puddle-e2e/tests/proxy_agent_store.rs": read("a1-positional-zip.good.rs.txt")}, 0, 0)
    store = "crates/puddle-store/tests/events_and_audit_query.rs"
    case("t1 original", "t1", {store: read("t1-wall-clock-bar.bad.rs.txt")}, 1, 1)
    case("t1 fix keeps the bar", "t1", {store: read("t1-wall-clock-bar.good.rs.txt")}, 1, 1)
    case("t1 a deadline loop is not a bar", "t1", {store: "fn t() { assert!(start.elapsed() < Duration::from_secs(1) || Instant::now() < deadline, \"late\"); }\n"}, 0, 0)
    case("t1 a CPU-time bar is not a wall-clock bar", "t1", {store: "fn t() { let (_, cpu) = cpu_time(|| work());\nassert!(cpu < Duration::from_millis(100), \"{cpu:?}\"); }\n"}, 0, 0)
    case("t1 a cpu-named variable not from cpu_time stays", "t1", {store: "fn t() { let cpu = start.elapsed(); assert!(cpu < Duration::from_millis(100), \"{cpu:?}\"); }\n"}, 1, 1)
    case("t1 a cpu bar does not excuse a wall-clock half", "t1", {store: "fn t() { let ((), cpu) = cpu_time(|| w()); assert!(start.elapsed() < Duration::from_secs(1) && cpu < Duration::from_millis(100)); }\n"}, 1, 1)
    case("t1 a bar on a variable not named cpu stays", "t1", {store: "fn t() { let took = start.elapsed(); assert!(took < Duration::from_millis(100), \"{took:?}\"); }\n"}, 1, 1)
    case("t1 reasoned", "t1", {store: reasoned(read("t1-wall-clock-bar.good.rs.txt"), "assert!(best")}, 1, 0)
    vm_test = "crates/puddle-host/tests/vm_x.rs"
    case("d1 original", "d1", {vm_test: read("d1-fake-site.bad.rs.txt")}, 1, 1)
    case("d1 fix", "d1", {vm_test: read("d1-fake-site.good.rs.txt")}, 0, 0)
    case("d1 reasoned", "d1", {vm_test: reasoned(read("d1-fake-site.bad.rs.txt"), "const SITE")}, 1, 0)
    case("d1 example.com", "d1", {vm_test: 'use vm_support::web_internet;\nfn t() { run("curl https://api.example.com/"); }\n'}, 1, 1)
    case("d1 not a reserved name", "d1", {vm_test: 'use vm_support::web_internet;\nfn t() { run("curl https://notexample.com/ https://contest.org/ me@example.org"); }\n'}, 0, 0)
    case("d1 a .invalid name that has test as a label is not reserved", "d1", {vm_test: 'use vm_support::web_internet;\nfn t() { run("curl https://api.test.invalid/ x.example.invalid"); }\n'}, 0, 0)
    case("d1 a file name that ends in .test.rs is not a name", "d1", {vm_test: 'use vm_support::web_internet;\nconst F: &str = "tests/site.test.rs";\n'}, 0, 0)
    case("d1 a name with a port or a trailing dot is one", "d1", {vm_test: 'use vm_support::web_internet;\nfn t() { run("curl http://site.test:8443/ http://other.example./"); }\n'}, 1, 1)
    case("d1 a test with a fake resolver", "d1", {"crates/puddle-proxy/tests/x.rs": 'fn t() { let n = "site.test"; }\n'}, 0, 0)
    case("d1 in production code", "d1", {"crates/x/src/lib.rs": 'use SystemResolver;\nconst DOC: &str = "site.test";\n'}, 0, 0)
    case("o2 variable", "o2", {FLAKE_SPEC: read("o2-density-measure.bad.ts.txt")}, 1, 1)
    case("o2 poll", "o2", {FLAKE_SPEC: read("o2-density-measure.good.ts.txt")}, 0, 0)
    case("o2 focus", "o2", {FLAKE_SPEC: read("o2-focus-read.bad.ts.txt")}, 1, 1)
    # `expect(await read)` is ESLint's (an error in ui/eslint.config.js, proved by ui/scripts/flake-eslint.test.ts): the scan skips it.
    one_shot = 'test("x", async ({ page }) => {\n  expect(await measure(page, "main select")).toBeNull();\n});\n'
    case("o2 inline is ESLint's, not the scan's", "o2", {FLAKE_SPEC: one_shot}, 0, 0)
    held = 'test("x", async ({ page }) => {\n  const dialog = await openDialog(page);\n  await expect(dialog).toHaveCount(0);\n  await expect(dialog).not.toBeVisible();\n});\n'
    case("o2 a locator in a variable is a web-first assertion", "o2", {FLAKE_SPEC: held}, 0, 0)
    value = 'test("x", async ({ page }) => {\n  const n = await page.locator("li").count();\n  expect(n).toBeGreaterThan(0);\n  expect(n).toBeLessThan(9);\n});\n'
    case("o2 a count in a variable is read once per expect", "o2", {FLAKE_SPEC: value}, 2, 2)
    kept = 'test("x", async ({ page }) => {\n  const box = (await page.locator("li").boundingBox())!;\n  expect(box.width).toBeGreaterThanOrEqual(24);\n  expect(box.x + box.width).toBeLessThanOrEqual(90);\n});\n'
    case("o2 a non-null read in parentheses, asserted by property", "o2", {FLAKE_SPEC: kept}, 2, 2)
    case("o2 a reason on a parenthesised read covers its expects", "o2", {FLAKE_SPEC: kept.replace("  const box", "  // flake-ok: laid out by the time the dialog opened\n  const box")}, 2, 0)
    case("o2 a poll of the box passes", "o2", {FLAKE_SPEC: 'test("x", async ({ page }) => {\n  await expect.poll(async () => (await page.locator("li").boundingBox())?.width ?? 0).toBeGreaterThanOrEqual(24);\n});\n'}, 0, 0)
    case("o2 a parenthesised read inside a retry block is not kept", "o2", {FLAKE_SPEC: 'test("x", async ({ page }) => {\n  await expect(async () => {\n    const t = (await page.locator("li").textContent()) ?? "";\n    expect(t.length).toBe(3);\n  }).toPass();\n});\n'}, 0, 0)
    case("o2 a variable named only in the message is not read", "o2", {FLAKE_SPEC: kept.replace("expect(box.width)", 'expect(24, "the box width")')}, 1, 1)
    case("o2 a plain read asserted by property is not counted", "o2", {FLAKE_SPEC: 'test("x", async ({ page }) => {\n  const box = await page.locator("li").boundingBox();\n  expect(box?.width).toBe(3);\n});\n'}, 0, 0)
    case("o2 a reason on the read covers its expects", "o2", {FLAKE_SPEC: value.replace("  const n", "  // flake-ok: the list is complete\n  const n")}, 2, 0)

    f1 = "crates/puddle-store/tests/store_paths.rs"
    f1_bad = read("f1-posix-path.bad.rs.txt")
    case("f1 original", "f1", {f1: f1_bad}, 2, 2)
    case("f1 reasoned", "f1", {f1: reasoned(reasoned(f1_bad, "PathBuf::from"), "Path::new")}, 2, 0)
    case("f1 a tempdir joined portably", "f1", {f1: 'fn t() { let dir = tempfile::tempdir().unwrap(); let file = dir.path().join("config.toml"); }\n'}, 0, 0)
    case("f1 a guest path stays a string", "f1", {f1: 'fn t() { sandbox.write("/tmp/leaf.pem", bytes); run("cat /home/dev/.rc"); }\n'}, 0, 0)
    case("f1 a URL path is not a host path", "f1", {f1: 'fn t() { let u = Url::parse("https://x.test/tmp/a").unwrap(); let p = PathBuf::from(u.path()); }\n'}, 0, 0)
    case("f1 a hand-joined path", "f1", {f1: 'fn t() { let file = format!("{dir}/{name}.toml"); }\n'}, 1, 1)
    case("f1 a hand-joined image name is not a path", "f1", {f1: 'fn t() { let image = format!("{REGISTRY}/{REPO}:1"); }\n'}, 0, 0)
    case("f1 in production code", "f1", {"crates/x/src/lib.rs": f1_bad}, 0, 0)
    case("f1 a comment", "f1", {f1: '// Path::new("/tmp/x")\nfn t() {}\n'}, 0, 0)

    def bans(src):
        return sorted({r[0] for r in run({"crates/x/src/lib.rs": "#![forbid(unsafe_code)]\n" + src}) if r[0].startswith("ban-")})
    if bans('#[derive(Deserialize)]\n#[serde(untagged)]\nenum E { A(u8), B(String) }\n') != ["ban-untagged"]:
        failures.append("ban-untagged: a bare #[serde(untagged)] must be flagged")
    if bans('#[serde(rename_all = "snake_case", untagged)]\nenum E { A }\n') != ["ban-untagged"]:
        failures.append("ban-untagged: untagged among other serde arguments must be flagged")
    if bans('#[serde(tag = "kind")]\nenum E { A }\n// #[serde(untagged)]\nconst S: &str = "#[serde(untagged)]";\n'):
        failures.append("ban-untagged: a tagged enum, a comment and a string are not sites")
    if bans('enum E {\n    #[error("Bad {0}")]\n    A(u8),\n}\n') != ["ban-error-capital"]:
        failures.append("ban-error-capital: a capital first letter must be flagged")
    if bans('enum E {\n    #[error("bad {0}.")]\n    A(u8),\n}\n') != ["ban-error-period"]:
        failures.append("ban-error-period: a trailing period must be flagged")
    if bans('enum E {\n    #[error("bad {0}: {1}")]\n    A(u8, String),\n    #[error("{0}")]\n    B(String),\n    #[error("see a.b. later")]\n    C,\n}\n'):
        failures.append("ban-error: lower-case messages, `{0}` and a mid-text period are not sites")
    if not any(r[0] == "noreason" for r in run({"crates/x/src/lib.rs": "#![forbid(unsafe_code)]\n#[serde(untagged)]\nenum E { A }\n"})):
        failures.append("ban-untagged: a site must add a noreason row so --missing fails")
    def sinks(files):
        return sorted(r[2] for r in run(files) if r[0] == "ban-html-sink")
    sink_svelte, sink_ts = read("ban-html-sink.bad.svelte.txt"), read("ban-html-sink.bad.ts.txt")
    if len(sinks({"ui/src/lib/A.svelte": sink_svelte})) != 1:
        failures.append("ban-html-sink: `{@html` in a .svelte file must be flagged")
    all_sinks = ("innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "setHTMLUnsafe",
                 "createContextualFragment", "parseFromString", "srcdoc")
    for sink in all_sinks:
        hits = [ln for ln in sink_ts.split("\n") if sink in ln]
        if len(hits) != 1 or len(sinks({"ui/src/lib/a.ts": sink_ts})) != len(all_sinks):
            failures.append(f"ban-html-sink: the fixture must trip {sink} once and all {len(all_sinks)} sinks in all: {hits}")
        only = "\n".join(ln for ln in sink_ts.split("\n") if sink not in ln or ln.lstrip().startswith("//"))
        if len(sinks({"ui/src/lib/a.ts": only})) != len(all_sinks) - 1:
            failures.append(f"ban-html-sink: {sink} was not flagged on its own")
    if sinks({"ui/src/lib/A.svelte": read("ban-html-sink.good.svelte.txt")}):
        failures.append("ban-html-sink: text interpolation, a component and a comment are not sinks")
    for rel in ("ui/src/lib/a.test.ts", "ui/src/lib/a.spec.ts", "ui/src/lib/api/schema.d.ts", "ui/e2e/a.spec.ts", "crates/x/src/lib.rs"):
        if sinks({rel: sink_ts}):
            failures.append(f"ban-html-sink: {rel} is out of scope")
    if not any(r[0] == "noreason" and "HTML injection sink" in r[3] for r in run({"ui/src/lib/A.svelte": sink_svelte})):
        failures.append("ban-html-sink: a site must add a noreason row that names the fix so --missing fails")
    p2_bad, p2_good = read("p2-fixed-port.bad.rs.txt"), read("p2-fixed-port.good.rs.txt")
    case("p2 original", "p2", {"crates/x/tests/p.rs": p2_bad}, 3, 3)
    case("p2 fix: port 0, a fake authority and URL, a log line stay out; a reasoned literal counts once", "p2", {"crates/x/tests/p.rs": p2_good}, 1, 0)
    case("p2 in production code", "p2", {"crates/x/src/lib.rs": p2_bad}, 0, 0)
    e1_bad, e1_good = read("e1-script-write.bad.rs.txt"), read("e1-script-write.good.rs.txt")
    case("e1 original: a write then set_permissions or chmod", "e1", {"crates/x/tests/e.rs": e1_bad}, 2, 2)
    case("e1 fix: the kit helper, a folder, a 0o644 file stay out; a reasoned site counts once", "e1", {"crates/x/tests/e.rs": e1_good}, 1, 0)
    case("e1 in production code", "e1", {"crates/x/src/lib.rs": e1_bad}, 0, 0)
    s1_bad, s1_good = read("s1-child-stdin-write.bad.rs.txt"), read("s1-child-stdin-write.good.rs.txt")
    case("s1 original: a one-line chain, a split chain, a variable taken from stdin", "s1", {"crates/x/tests/s.rs": s1_bad}, 3, 3)
    case("s1 fix: feed_stdin, a discarded result, a socket or file stay out; a reasoned site counts once", "s1", {"crates/x/tests/s.rs": s1_good}, 1, 0)
    case("s1 in production code", "s1", {"crates/x/src/lib.rs": s1_bad}, 0, 0)
    case("p2 in a cfg(test) module", "p2", {"crates/x/src/lib.rs": "#[cfg(test)]\nmod t {\n    fn f() { let l = TcpListener::bind(\"127.0.0.1:8080\"); }\n}\n"}, 1, 1)

    # A counted rule adds no `noreason` row; a rule flipped to failing adds one that opens with its fix.
    bad = {rule: {v["as"]: read(v["fixture"])} for rule, v in gate.FLAKE_RULES.items()}
    for rule, files in bad.items():
        was_failing = rule in gate.FLAKE_FAILING
        gate.FLAKE_FAILING.discard(rule)
        if any(r[0] == "noreason" and r[3].startswith(f"flake-{rule}:") for r in run(files)):
            failures.append(f"{rule}: a counted rule must not add a noreason row")
        gate.FLAKE_FAILING.add(rule)
        try:
            miss = [r for r in run(files) if r[0] == "noreason" and r[3].startswith(f"flake-{rule}:")]
        finally:
            if not was_failing:
                gate.FLAKE_FAILING.discard(rule)
        if not miss or not all(r[3].startswith(f"flake-{rule}: {gate.FLAKE_FIX[rule]} | ") for r in miss):
            failures.append(f"{rule}: a failing rule adds noreason rows that open with the whole fix text: {miss}")
    for label, text in (("an unknown status", "status: maybe\nfixture: a\nas: b\nfix: c\n"), ("a missing key", "status: failing\n"), ("a repeated key", "status: failing\nstatus: counted\nfixture: a\nas: b\nfix: c\n")):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "q1.rule"), "w", encoding="utf-8") as h:
                h.write(text)
            try:
                gate.load_flake_rules(d)
                failures.append(f"rule file with {label} was accepted")
            except SystemExit:
                pass
    return failures


def allow_exempt_self_test():
    """A `ban-allow` exemption keeps its file's `allow` quiet, and fails once the file has none."""
    import tempfile
    path = "crates/example/tests/support/mod.rs"
    gate.ALLOW_EXEMPT[path] = "self-test"
    failures = []
    for body, want in (('#![allow(dead_code, reason = "x")]\n', []), ('#![expect(dead_code, reason = "x")]\n', ["noreason"])):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.dirname(os.path.join(d, path)))
            with open(os.path.join(d, path), "w") as h:
                h.write(body)
            got = sorted(r[0] for r in gate.scan(d) if r[0] in ("ban-allow", "noreason"))
            if got != want:
                failures.append(f"{body.strip()}: want {want}, got {got}")
    del gate.ALLOW_EXEMPT[path]
    return failures


def test_cfg_self_test():
    """Only test-only cfgs make test context: `cfg(any(windows, test))` (either order) and
    `any(windows, feature = "testing")` stay prod and their unsafe block without a reason is flagged as prod."""
    import tempfile
    cases = {
        "a_windows_or_test": ("#[cfg(any(windows, test))]", "prod"),
        "b_test_or_windows": ("#[cfg(any(test, windows))]", "prod"),
        "c_windows_or_testing": ('#[cfg(any(windows, feature = "testing"))]', "prod"),
        "d_test": ("#[cfg(test)]", "test"),
        "e_test_or_testing": ('#[cfg(any(test, feature = "testing"))]', "test"),
        "f_all_test_unix": ("#[cfg(all(test, unix))]", "test"),
    }
    problems = []
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "crates/x/src"))
        with open(os.path.join(d, "crates/x/src/lib.rs"), "w", encoding="utf-8") as h:
            h.write("#![forbid(unsafe_code)]\n")
        for name, (attr, _) in cases.items():
            with open(os.path.join(d, f"crates/x/src/{name}.rs"), "w", encoding="utf-8") as h:
                h.write(f"{attr}\nfn f() {{ unsafe {{ g() }} }}\n")
        got = {r[2].split(":")[0].rsplit("/", 1)[1][:-3]: r[1] for r in gate.scan(d) if r[0] == "unsafe-block"}
        want = {name: ctx for name, (_, ctx) in cases.items()}
        if got != want:
            problems.append(f"unsafe block context {got}, want {want}")
    return problems


def self_test(scanner):
    global gate
    gate = scanner
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        os.makedirs(os.path.join(d, "crates/x/src"))
        with open(os.path.join(d, "crates/x/src/lib.rs"), "w") as h:
            h.write("#![forbid(unsafe_code)]\n" + SELF_TEST)
        os.makedirs(os.path.join(d, ".github/workflows"))
        with open(os.path.join(d, ".github/workflows/ci.yml"), "w") as h:
            h.write(SELF_TEST_YAML)
        # A local `npm test` leaves ui/coverage/ (gitignored); its HTML report carries bare eslint-disable lines.
        os.makedirs(os.path.join(d, "ui/coverage/lcov-report"))
        with open(os.path.join(d, "ui/coverage/lcov-report/prettify.js"), "w") as h:
            h.write("/* eslint-disable */\n")
        rows = gate.scan(d)
        if any(r[2].startswith("ui/coverage/") for r in rows):
            print("self-test FAIL generated ui/coverage was scanned")
            return 1
        got = gate.counts(rows)
        # The flagged directive is the bare one in the workflow, on its own line.
        flagged = [r[2] for r in rows if r[0] == "noreason" and r[2].startswith(".github/")]
        if flagged != [".github/workflows/ci.yml:11"]:
            print("self-test FAIL workflow noreason rows", flagged)
            return 1
    bad_exempt = allow_exempt_self_test()
    if bad_exempt:
        print("self-test FAIL ban-allow exemptions:\n  " + "\n  ".join(bad_exempt))
        return 1
    bad_kit = kit_self_test()
    if bad_kit:
        print("self-test FAIL test kit dependency:\n  " + "\n  ".join(bad_kit))
        return 1
    bad_root = root_self_test()
    if bad_root:
        print("self-test FAIL crate roots:\n  " + "\n  ".join(bad_root))
        return 1
    bad_inline = inline_test_self_test()
    if bad_inline:
        print("self-test FAIL inline test blocks:\n  " + "\n  ".join(bad_inline))
        return 1
    bad_cfg = test_cfg_self_test()
    if bad_cfg:
        print("self-test FAIL test cfg:\n  " + "\n  ".join(bad_cfg))
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
        "noreason.prod": 7, "ban-allow.prod": 1, "ban-result-string.prod": 2, "let-underscore.prod": 3, "let-underscore-uncommented.prod": 1,
        "sleep.test": 5, "sleep-fixed.test": 3, "sleep-fixed-uncommented.test": 2, "noreason.test": 3,
        "inline-test-block.test": 1,
        "flake-w3.test": 1,
    }
    bad = {k: (got.get(k), v) for k, v in want.items() if got.get(k) != v}
    extra = {k: v for k, v in got.items() if k not in want}
    if bad or extra:
        print("self-test FAIL", bad, extra)
        return 1
    print("self-test pass")
    return 0
