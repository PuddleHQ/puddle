// SPDX-License-Identifier: GPL-3.0-or-later
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  readFileSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  afterAll,
  afterEach,
  beforeAll,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import {
  cleanGitEnv,
  judge,
  parseDiff,
  exclusionOrderError,
  parseExclusions,
  readExclusions,
  parseLcov,
  run,
  runFromArgs,
  type Options,
} from "./diff-coverage.ts";

const diff = `diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -2,0 +3,2 @@ fn x
+let a = 1;
+let b = 2;
@@ -9 +11 @@
-old
+new
diff --git a/gone.rs b/gone.rs
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
`;

describe("parseDiff", () => {
  it("numbers added lines in the new file and skips deletions", () => {
    expect(parseDiff(diff)).toEqual([
      { path: "src/a.rs", line: 3, text: "let a = 1;" },
      { path: "src/a.rs", line: 4, text: "let b = 2;" },
      { path: "src/a.rs", line: 11, text: "new" },
    ]);
  });
});

describe("parseDiff details", () => {
  it("skips a malformed hunk header and files whose diff header has no path", () => {
    expect(
      parseDiff("+++ b/a.rs\n@@ nonsense @@\n+x\n+++ /dev/null\n+y\n"),
    ).toEqual([{ path: "a.rs", line: 0, text: "x" }]);
  });
});

describe("parseLcov", () => {
  it("makes paths repository-relative and keeps the highest count of a repeated line", () => {
    const cov = parseLcov(
      "SF:/repo/crates/x/src/a.rs\nDA:3,0\nDA:3,2\nDA:4,0\nend_of_record\nSF:src/b.ts\nDA:1,1\nend_of_record\n",
      "/repo",
      "ui/",
    );
    expect(cov.get("crates/x/src/a.rs")?.get(3)).toBe(2);
    expect(cov.get("crates/x/src/a.rs")?.get(4)).toBe(0);
    expect(cov.get("ui/src/b.ts")?.get(1)).toBe(1);
  });
});

describe("parseLcov details", () => {
  it("merges two records of one file, reads CRLF reports and skips malformed or orphan DA lines", () => {
    const cov = parseLcov(
      "DA:1,1\r\nSF:/repo/a.ts\r\nDA:1,0\r\nDA:x,1\r\nDA:2,y\r\nend_of_record\r\nDA:9,9\r\nSF:/repo/a.ts\r\nDA:3,4\r\nend_of_record\r\n",
      "/repo",
      "",
    );
    expect([...cov.keys()]).toEqual(["a.ts"]);
    expect([...(cov.get("a.ts") ?? [])]).toEqual([
      [1, 0],
      [3, 4],
    ]);
  });
});

describe("exclusions file", () => {
  it("refuses a line without any | and one without a closing |", () => {
    expect(() => parseExclusions("a.rs")).toThrow(
      'exclusions line 1: want "path glob | whole line, or * | [xN |] reason" (a reason is required)',
    );
    expect(() => parseExclusions("\n# c\na.rs | x")).toThrow(/line 3/);
  });

  it("needs a reason on every entry", () => {
    expect(() => parseExclusions("a.rs | * |")).toThrow(/reason is required/);
    expect(() => parseExclusions("a.rs | *")).toThrow(/reason is required/);
  });
  it("ignores comments and blank lines and keeps a | inside the reason", () => {
    const [e] = parseExclusions(
      "# c\n\ncrates/**/win.rs | * | OS glue | tier W\n",
    );
    expect(e).toMatchObject({
      glob: "crates/**/win.rs",
      lines: "*",
      max: 1,
      reason: "OS glue | tier W",
    });
  });
  it("reads a count, and several lines joined with a backslash n", () => {
    const [e] = parseExclusions(
      "a.rs | let value = load(path)?; | x3 | three copies, none reachable",
    );
    expect(e).toMatchObject({ max: 3, reason: "three copies, none reachable" });
    const [f] = parseExclusions(
      "a.rs | let value = load(path) \\n )?; | the error arm",
    );
    expect(f?.lines).toEqual(["let value = load(path)", ")?;"]);
  });
  it("reads a backslash n in the code as part of the line, and \\\\n as a line break after a backslash", () => {
    const [e] = parseExclusions(
      'a.rs | return Err(format!("exited early\\\\n{}", why())); | a string with a newline escape',
    );
    expect(e?.lines).toEqual([
      'return Err(format!("exited early\\n{}", why()));',
    ]);
  });
  it("keeps a lone backslash that escapes nothing", () => {
    const [e] = parseExclusions(
      'a.rs | let pattern = r"\\d+"; | a regex with a backslash',
    );
    expect(e?.lines).toEqual(['let pattern = r"\\d+";']);
  });
  it("names the first entry that stands before the one above it", () => {
    const text =
      "# h\na.rs | one one one one one one | r\n\n# c\nb.rs | x | r\na.rs | two two two two two two | r";
    expect(exclusionOrderError(text)).toMatch(/line 6 belongs before line 5/);
  });
  it("orders by path glob, then by the whole line, and lets comments sit anywhere", () => {
    expect(exclusionOrderError("")).toBeUndefined();
    expect(
      exclusionOrderError(
        "# h\na.rs | b | r\n# c\na.rs | c | r\na.rs/x | a | r\n",
      ),
    ).toBeUndefined();
    expect(exclusionOrderError("a.rs | c | r\na.rs | b | r")).toMatch(/line 2/);
  });
  it("readExclusions refuses an unsorted file and reads a sorted one", () => {
    const dir = mkdtempSync(join(tmpdir(), "excl-"));
    try {
      const file = join(dir, "ex.txt");
      writeFileSync(file, "b.rs | * | r\na.rs | * | r\n");
      expect(() => readExclusions(file)).toThrow(/belongs before line 1/);
      writeFileSync(file, "a.rs | * | r\nb.rs | * | r\n");
      expect(readExclusions(file).map((e) => e.glob)).toEqual(["a.rs", "b.rs"]);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
  it("keeps the committed files sorted", () => {
    for (const name of ["diff", "full"]) {
      const file = join(
        import.meta.dirname,
        `../../scripts/${name}-coverage-exclusions.txt`,
      );
      expect(exclusionOrderError(readFileSync(file, "utf8"))).toBeUndefined();
    }
  });
  it("accepts every entry of the committed file", () => {
    const file = join(
      import.meta.dirname,
      "../../scripts/diff-coverage-exclusions.txt",
    );
    const entries = parseExclusions(readFileSync(file, "utf8"));
    expect(entries.length).toBeGreaterThan(100);
    // A whole-file `*` is a deliberate, reviewed choice; keep the list short.
    expect(entries.filter((e) => e.lines === "*").length).toBeLessThan(15);
  });
  it.each([
    ")?;",
    "}",
    "return;",
    ".await",
    "format!(",
    "host_port,",
    "Ok(())",
  ])("refuses the one-token text %s", (text) => {
    expect(() => parseExclusions(`a.rs | ${text} | reason`)).toThrow(
      /too short to name one line/,
    );
  });
  it("refuses a short text that only looks long once its lines are joined by a gap", () => {
    expect(() => parseExclusions("a.rs | } \\n | reason")).toThrow(
      /too short|empty line/,
    );
    expect(() => parseExclusions("a.rs | x \\n )?; | reason")).toThrow(
      /too short/,
    );
  });
});

describe("judge", () => {
  const cov = new Map([
    [
      "src/a.rs",
      new Map([
        [3, 1],
        [4, 0],
        [11, 0],
      ]),
    ],
  ]);
  const added = parseDiff(diff);
  // The file as it is on disk: lines 3, 4 and 11 are the added ones.
  const file = [
    "fn a() {",
    "    start();",
    "    let a = 1;",
    "    let b = compute_the_value()?;",
    ...Array.from({ length: 6 }, () => "    pad();"),
    "    new_line_of_the_change();",
    "}",
  ];
  const source = (path: string) => (path === "src/a.rs" ? file : undefined);

  it("fails an executable line that never ran, ignores lines with no data, and says so for unmeasured files", () => {
    const r = judge(
      [...added, { path: "src/new.rs", line: 1, text: "x" }],
      cov,
      [],
    );
    expect(r.covered).toBe(1);
    expect(r.uncovered.map((u) => u.line)).toEqual([4, 11]);
    expect(r.notMeasured).toEqual(["src/new.rs"]);
  });

  it("skips an added line that has no coverage data", () => {
    const r = judge([{ path: "src/a.rs", line: 99, text: "// note" }], cov, []);
    expect(r).toEqual({
      uncovered: [],
      covered: 0,
      excluded: 0,
      notMeasured: [],
    });
  });

  it("accepts a listed exclusion by file and whole line text, and marks it used", () => {
    const ex = parseExclusions(
      "src/*.rs | new_line_of_the_change(); | reason one\nsrc/a.rs | let b = compute_the_value()?; | reason two",
    );
    const r = judge(added, cov, ex, source);
    expect(r.uncovered).toEqual([]);
    expect(r.excluded).toBe(2);
    expect(ex.every((e) => e.used)).toBe(true);
  });

  it("does not let a part of a line, another file or other text through", () => {
    const ex = parseExclusions(
      [
        "src/b.rs | * | other file",
        "src/a.rs | nothing here at all, no line | other text",
        // A part of line 4 and of line 11: the old matcher took these.
        "src/a.rs | let b = compute_the_value() | a prefix",
        "src/a.rs | new_line_of_the_chan | a part",
      ].join("\n"),
    );
    expect(judge(added, cov, ex, source).uncovered).toHaveLength(2);
    expect(ex.some((e) => e.used)).toBe(false);
  });

  it("fails a new uncovered line of the same shape as an excused one", () => {
    // `let b = compute_the_value()?;` is excused; a later change adds `let c = other_value()?;`.
    const ex = parseExclusions(
      "src/a.rs | let b = compute_the_value()?; | reason",
    );
    const later = [...file];
    later[4] = "    let c = other_value()?;";
    const r = judge(
      [
        {
          path: "src/a.rs",
          line: 4,
          text: "    let b = compute_the_value()?;",
        },
        { path: "src/a.rs", line: 5, text: "    let c = other_value()?;" },
      ],
      new Map([
        [
          "src/a.rs",
          new Map([
            [4, 0],
            [5, 0],
          ]),
        ],
      ]),
      ex,
      () => later,
    );
    expect(r.excluded).toBe(1);
    expect(r.uncovered.map((u) => u.line)).toEqual([5]);
  });

  it("stops excusing a text once the file holds more copies than the count allows", () => {
    const twice = [...file, "    let b = compute_the_value()?;"];
    const ex = parseExclusions(
      "src/a.rs | let b = compute_the_value()?; | reason",
    );
    const notes: string[] = [];
    const r = judge(added, cov, ex, () => twice, notes);
    expect(r.uncovered.map((u) => u.line)).toEqual([4, 11]);
    expect(notes[0]).toMatch(/allows 1 place\(s\) in a file, src\/a.rs has 2/);
    const counted = parseExclusions(
      "src/a.rs | let b = compute_the_value()?; | x2 | two copies, neither reachable",
    );
    expect(
      judge(added, cov, counted, () => twice).uncovered.map((u) => u.line),
    ).toEqual([11]);
  });

  it("pins a short closing line to the line above it", () => {
    const f = [
      "call_the_thing(",
      "    with_an_argument",
      ")?;",
      "other(",
      ")?;",
    ];
    const ex = parseExclusions(
      "a.rs | call_the_thing( \\n with_an_argument \\n )?; | reason",
    );
    const c = new Map([
      [
        "a.rs",
        new Map([
          [3, 0],
          [5, 0],
        ]),
      ],
    ]);
    const r = judge(
      [
        { path: "a.rs", line: 3, text: ")?;" },
        { path: "a.rs", line: 5, text: ")?;" },
      ],
      c,
      ex,
      () => f,
    );
    expect(r.excluded).toBe(1);
    expect(r.uncovered.map((u) => u.line)).toEqual([5]);
  });

  it("compares trimmed, so an indent change keeps an entry matching", () => {
    const ex = parseExclusions("a.rs | let b = compute_the_value()?; | reason");
    const r = judge(
      [{ path: "a.rs", line: 1, text: "\t\tlet b = compute_the_value()?;" }],
      new Map([["a.rs", new Map([[1, 0]])]]),
      ex,
      () => ["        let b = compute_the_value()?;"],
    );
    expect(r.excluded).toBe(1);
  });

  it("lets * excuse every uncovered line of a file", () => {
    const ex = parseExclusions("src/a.rs | * | whole-file OS glue");
    expect(judge(added, cov, ex, source).uncovered).toEqual([]);
  });
});

// Idle the fixture takes 25 ms; node starts git with fork, whose cost grows with the memory of the
// whole vitest process (3 spawns: 9 ms at 0 GB, 110 ms at 3 GB, 115 ms at 12 GB under 3x CPU load,
// 183 ms for the hook in a full coverage run). A host at load 90 on 40 cores has stalled this hook
// past vitest's 10 s default, which nothing here reproduces (best: 183 ms), so the bar is 60 s: a git
// that hangs still fails the file here, long before the CI job limit.
const HOOK_MS = 60_000;
const SLOW_HOOK_MS = 1_000;

describe("run on a real git repository", () => {
  // One repository for all three tests: `run` only reads it, and every git
  // start-up is a process spawn that a loaded host schedules slowly, so
  // building it per test put six spawns inside each 5 s test body.
  let dir = "";
  beforeAll(() => {
    const started = performance.now();
    const spawnMs: number[] = [];
    // `.native` expands Windows 8.3 short names (RUNNER~1), which git reports in long form; the
    // JS realpath leaves them short, so the lcov paths would fall outside the repository.
    dir = realpathSync.native(mkdtempSync(join(tmpdir(), "diff-coverage-")));
    const g = (...a: string[]) => {
      const t = performance.now();
      execFileSync(
        "git",
        ["-c", "user.email=t@example.com", "-c", "user.name=t", ...a],
        { cwd: dir, env: cleanGitEnv() },
      );
      spawnMs.push(Math.round(performance.now() - t));
    };
    g("init", "-q");
    mkdirSync(join(dir, "src"));
    writeFileSync(join(dir, "src/a.rs"), "fn a() {\n    1;\n}\n");
    g("add", ".");
    g("commit", "-q", "-m", "base");
    writeFileSync(
      join(dir, "src/a.rs"),
      "fn a() {\n    1;\n    let two = compute_two();\n    let three = compute_three();\n}\n",
    );
    // A slow hook leaves its per-spawn times in the log, so the next occurrence names its step.
    const total = Math.round(performance.now() - started);
    if (total > SLOW_HOOK_MS) {
      console.warn(
        `diff-coverage fixture: ${total} ms, git spawns ${spawnMs.join(", ")} ms`,
      );
    }
  }, HOOK_MS);
  afterAll(() => rmSync(dir, { recursive: true, force: true }));
  afterEach(() => vi.restoreAllMocks());

  const opts = (lcov: string): Options => ({
    base: "HEAD",
    lcov,
    exts: [".rs"],
    prefix: "",
    label: "t",
  });

  it("passes when every added executable line ran", () => {
    writeFileSync(
      join(dir, "l.info"),
      `SF:${dir}/src/a.rs\nDA:2,1\nDA:3,1\nDA:4,5\nend_of_record\n`,
    );
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    expect(run(opts(join(dir, "l.info")), dir)).toBe(0);
    // A report whose paths miss the repository also exits 0, as "not measured"; rule that out.
    const out = log.mock.calls.flat().join("\n");
    expect(out).not.toContain("not measured");
    expect(out).toContain("2 changed executable line(s) covered");
  });

  it("fails and names the file and line when one added line never ran", () => {
    writeFileSync(
      join(dir, "l.info"),
      `SF:${dir}/src/a.rs\nDA:2,1\nDA:3,1\nDA:4,0\nend_of_record\n`,
    );
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(run(opts(join(dir, "l.info")), dir)).toBe(1);
    expect(err.mock.calls.flat().join("\n")).toContain(
      "src/a.rs:4: let three = compute_three();",
    );
  });

  it("reads the file on disk: an excused line passes, a new uncovered one of the same shape fails, a one-token entry is refused", () => {
    const a = join(dir, "src/a.rs");
    const before = readFileSync(a, "utf8");
    try {
      writeFileSync(
        a,
        "fn a() {\n    1;\n    excused_call(first_argument)?;\n    later_call(second_argument)?;\n}\n",
      );
      writeFileSync(
        join(dir, "l.info"),
        `SF:${dir}/src/a.rs\nDA:2,1\nDA:3,0\nDA:4,0\nend_of_record\n`,
      );
      const ex = join(dir, "ex.txt");
      const log = vi.spyOn(console, "log").mockImplementation(() => {});
      const err = vi.spyOn(console, "error").mockImplementation(() => {});
      const withEx = () =>
        run({ ...opts(join(dir, "l.info")), exclusions: ex }, dir);
      writeFileSync(ex, "src/a.rs | )?; | one token\n");
      expect(withEx).toThrow(/too short to name one line/);
      writeFileSync(
        ex,
        "src/a.rs | excused_call(first_argument)?; | the one excused line\n",
      );
      expect(withEx()).toBe(1);
      expect(err.mock.calls.flat().join("\n")).toContain(
        "src/a.rs:4: later_call(second_argument)?;",
      );
      expect(err.mock.calls.flat().join("\n")).not.toContain("src/a.rs:3:");
      writeFileSync(
        ex,
        "src/a.rs | excused_call(first_argument)?; | one\nsrc/a.rs | later_call(second_argument)?; | two\n",
      );
      expect(withEx()).toBe(0);
      expect(log.mock.calls.flat().join("\n")).toContain("2 excluded");
    } finally {
      writeFileSync(a, before);
    }
  });

  it("fails when the coverage report is missing", () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(run(opts(join(dir, "nope.info")), dir)).toBe(1);
    expect(err.mock.calls).toEqual([
      [`t: ${join(dir, "nope.info")} not found; run the coverage gate first`],
    ]);
  });

  it("names a changed file the report does not know as not measured, and still exits 0", () => {
    writeFileSync(
      join(dir, "l.info"),
      "SF:/elsewhere/b.rs\nDA:1,1\nend_of_record\n",
    );
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    expect(run(opts(join(dir, "l.info")), dir)).toBe(0);
    expect(log.mock.calls).toEqual([
      ["t: not in the coverage report (not measured): src/a.rs"],
      ["t ok: 0 changed executable line(s) covered, 0 excluded, since HEAD"],
    ]);
  });

  it("accepts a listed exclusion, reports one that matched nothing, and prints the whole failure for the rest", () => {
    writeFileSync(
      join(dir, "l.info"),
      `SF:${dir}/src/a.rs\nDA:3,0\nDA:4,0\nend_of_record\n`,
    );
    writeFileSync(
      join(dir, "ex.txt"),
      "src/a.rs | let two = compute_two(); | reason\nsrc/zzz.rs | * | reason\n",
    );
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const code = run(
      { ...opts(join(dir, "l.info")), exclusions: join(dir, "ex.txt") },
      dir,
    );
    expect(code).toBe(1);
    expect(log.mock.calls).toEqual([
      ['t: note: exclusion "src/zzz.rs | *" matched nothing in this diff'],
    ]);
    expect(err.mock.calls).toEqual([
      ["t: 1 added or changed line(s) never ran in a test:"],
      ["  src/a.rs:4: let three = compute_three();"],
      [
        "  Cover them, or list them with a reason in the exclusion file (reviewed like code).",
      ],
    ]);
  });

  it("counts an excluded line in the success line", () => {
    writeFileSync(
      join(dir, "l.info"),
      `SF:${dir}/src/a.rs\nDA:3,0\nDA:4,1\nend_of_record\n`,
    );
    writeFileSync(
      join(dir, "ex.txt"),
      "src/a.rs | let two = compute_two(); | reason\n",
    );
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const code = run(
      { ...opts(join(dir, "l.info")), exclusions: join(dir, "ex.txt") },
      dir,
    );
    expect(code).toBe(0);
    expect(log.mock.calls).toEqual([
      ["t ok: 1 changed executable line(s) covered, 1 excluded, since HEAD"],
    ]);
  });

  it("only looks at files under the prefix with a listed extension", () => {
    writeFileSync(
      join(dir, "l.info"),
      `SF:${dir}/src/a.rs\nDA:3,0\nend_of_record\n`,
    );
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    for (const change of [{ prefix: "other/" }, { exts: [".ts"] }]) {
      log.mockClear();
      expect(run({ ...opts(join(dir, "l.info")), ...change }, dir)).toBe(0);
      expect(log.mock.calls).toEqual([
        ["t ok: 0 changed executable line(s) covered, 0 excluded, since HEAD"],
      ]);
    }
  });

  it("runs in the current directory by default", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    expect(run(opts(join(dir, "nope.info")))).toBe(1);
  });
});

describe("runFromArgs", () => {
  it("refuses a call without --base, --lcov and --exts, naming the usage", () => {
    expect(() => runFromArgs(["--base", "HEAD", "--lcov", "x"])).toThrow(
      /^usage: diff-coverage\.ts --base <rev> --lcov <file> --exts/,
    );
  });

  it("labels its messages diff-coverage by default", () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const code = runFromArgs([
      ...["--base", "HEAD", "--lcov", "/nonexistent/l.info", "--exts", ".rs"],
    ]);
    vi.restoreAllMocks();
    expect(code).toBe(1);
    expect(err.mock.calls).toEqual([
      [
        "diff-coverage: /nonexistent/l.info not found; run the coverage gate first",
      ],
    ]);
  });

  it("reads the options from the command line and takes the label from --label", () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const code = runFromArgs([
      ...["--base", "HEAD", "--lcov", "/nonexistent/l.info", "--exts", ".rs"],
      ...["--label", "mine", "--prefix", "ui/", "--exclusions", "/x"],
    ]);
    vi.restoreAllMocks();
    expect(code).toBe(1);
    expect(err.mock.calls).toEqual([
      ["mine: /nonexistent/l.info not found; run the coverage gate first"],
    ]);
  });
});

describe("cleanGitEnv", () => {
  it("cleans the process environment when none is given", () => {
    vi.stubEnv("GIT_DIR", "x");
    vi.stubEnv("PUDDLE_KEEP", "1");
    const env = cleanGitEnv();
    vi.unstubAllEnvs();
    expect(env["GIT_DIR"]).toBeUndefined();
    expect(env["PUDDLE_KEEP"]).toBe("1");
  });

  it("drops the variables a git hook exports and keeps the rest", () => {
    const env = cleanGitEnv({
      GIT_DIR: "x",
      GIT_INDEX_FILE: "y",
      GIT_WORK_TREE: "z",
      PATH: "p",
    });
    expect(env).toEqual({ PATH: "p" });
  });
});
