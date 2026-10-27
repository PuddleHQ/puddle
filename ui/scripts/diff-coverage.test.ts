// SPDX-License-Identifier: GPL-3.0-or-later
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
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
  parseExclusions,
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
      'exclusions line 1: want "path glob | text in the line, or * | reason" (a reason is required)',
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
      needle: "*",
      reason: "OS glue | tier W",
    });
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

  it("accepts a listed exclusion by file and line text, and marks it used", () => {
    const ex = parseExclusions(
      "src/*.rs | new | reason one\nsrc/a.rs | let b | reason two",
    );
    const r = judge(added, cov, ex);
    expect(r.uncovered).toEqual([]);
    expect(r.excluded).toBe(2);
    expect(ex.every((e) => e.used)).toBe(true);
  });

  it("does not let an exclusion for another file or other text through", () => {
    const ex = parseExclusions(
      "src/b.rs | * | other file\nsrc/a.rs | nothing here | other text",
    );
    expect(judge(added, cov, ex).uncovered).toHaveLength(2);
    expect(ex.some((e) => e.used)).toBe(false);
  });
});

describe("run on a real git repository", () => {
  // One repository for all three tests: `run` only reads it, and every git
  // start-up is a process spawn that a loaded host schedules slowly, so
  // building it per test put six spawns inside each 5 s test body.
  let dir = "";
  beforeAll(() => {
    // `.native` expands Windows 8.3 short names (RUNNER~1), which git reports in long form; the
    // JS realpath leaves them short, so the lcov paths would fall outside the repository.
    dir = realpathSync.native(mkdtempSync(join(tmpdir(), "diff-coverage-")));
    const g = (...a: string[]) =>
      execFileSync(
        "git",
        ["-c", "user.email=t@example.com", "-c", "user.name=t", ...a],
        { cwd: dir, env: cleanGitEnv() },
      );
    g("init", "-q");
    mkdirSync(join(dir, "src"));
    writeFileSync(join(dir, "src/a.rs"), "fn a() {\n    1;\n}\n");
    g("add", ".");
    g("commit", "-q", "-m", "base");
    writeFileSync(
      join(dir, "src/a.rs"),
      "fn a() {\n    1;\n    2;\n    3;\n}\n",
    );
  });
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
    expect(err.mock.calls.flat().join("\n")).toContain("src/a.rs:4: 3;");
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
      "src/a.rs | 2; | reason\nsrc/zzz.rs | * | reason\n",
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
      ["  src/a.rs:4: 3;"],
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
    writeFileSync(join(dir, "ex.txt"), "src/a.rs | 2; | reason\n");
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
