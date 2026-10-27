// SPDX-License-Identifier: GPL-3.0-or-later
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
import { execFileSync } from "node:child_process";
import {
  cleanGitEnv,
  parseExclusions,
  type Coverage,
  type Source,
} from "./diff-coverage.ts";
import { judge, main, parseCrates, report } from "./full-coverage.ts";

const cov = (files: Record<string, Record<number, number>>): Coverage =>
  new Map(
    Object.entries(files).map(([p, lines]) => [
      p,
      new Map(Object.entries(lines).map(([n, h]) => [Number(n), h])),
    ]),
  );
const text: Source = (path) =>
  Array.from({ length: 20 }, (_, i) => `${path} line ${i + 1}`);

describe("parseCrates", () => {
  it("reads one folder per line and skips comments, blanks and trailing slashes", () => {
    expect(
      parseCrates("# why\n\ncrates/a\r\n  crates/b/  \n#crates/c\n"),
    ).toEqual(["crates/a", "crates/b"]);
  });
});

describe("judge", () => {
  it("passes a crate whose every executable line ran", () => {
    const v = judge(
      cov({ "crates/a/src/lib.rs": { 1: 3, 2: 1 } }),
      ["crates/a"],
      [],
      text,
    );
    expect(v).toMatchObject({
      missed: [],
      absent: [],
      stale: [],
      executable: 2,
      excluded: 0,
    });
  });

  it("names each line that never ran, in file and line order, with its text", () => {
    const v = judge(
      cov({
        "crates/a/src/z.rs": { 4: 0 },
        "crates/a/src/b.rs": { 9: 0, 2: 0, 3: 5 },
      }),
      ["crates/a"],
      [],
      text,
    );
    expect(v.missed).toEqual([
      { path: "crates/a/src/b.rs", line: 2, text: "crates/a/src/b.rs line 2" },
      { path: "crates/a/src/b.rs", line: 9, text: "crates/a/src/b.rs line 9" },
      { path: "crates/a/src/z.rs", line: 4, text: "crates/a/src/z.rs line 4" },
    ]);
  });

  it("does not count a sibling crate whose name starts the same, nor files outside src", () => {
    const v = judge(
      cov({
        "crates/a-extra/src/lib.rs": { 1: 0 },
        "crates/a/tests/t.rs": { 1: 0 },
        "crates/a/src/lib.rs": { 1: 1 },
      }),
      ["crates/a"],
      [],
      text,
    );
    expect(v.missed).toEqual([]);
    expect(v.executable).toBe(1);
  });

  it("fails a listed crate that has no entry in the report", () => {
    const v = judge(
      cov({ "crates/a/src/lib.rs": { 1: 1 } }),
      ["crates/a", "crates/typo"],
      [],
      text,
    );
    expect(v.absent).toEqual(["crates/typo"]);
  });

  it("accepts a line an exclusion names whole, and by `*` for the whole file, and reports an exclusion that matched nothing", () => {
    const ex = parseExclusions(
      [
        "crates/a/src/x.rs | unreachable_fallback_arm!() | fallback arm",
        "crates/a/**/y.rs | * | whole file",
        "crates/a/src/x.rs | a line that is gone now | stale",
      ].join("\n"),
    );
    const v = judge(
      cov({
        "crates/a/src/x.rs": { 1: 0, 2: 0 },
        "crates/a/src/sub/y.rs": { 1: 0 },
      }),
      ["crates/a"],
      ex,
      () => ["  unreachable_fallback_arm!()  ", "other"],
    );
    expect(v.excluded).toBe(2);
    expect(v.missed).toEqual([
      { path: "crates/a/src/x.rs", line: 2, text: "other" },
    ]);
    expect(v.stale.map((e) => e.needle)).toEqual(["a line that is gone now"]);
  });

  it("does not excuse part of a line, and not a line that shows up more often than the entry allows", () => {
    const ex = parseExclusions(
      "crates/a/src/x.rs | unreachable_fallback_arm!() | fallback arm",
    );
    const same = ["unreachable_fallback_arm!()", "unreachable_fallback_arm!()"];
    const v = judge(
      cov({ "crates/a/src/x.rs": { 1: 0, 2: 0 } }),
      ["crates/a"],
      ex,
      () => same,
    );
    expect(v.excluded).toBe(0);
    expect(v.missed.map((m) => m.line)).toEqual([1, 2]);
    expect(v.notes).toHaveLength(2);
    const part = judge(
      cov({ "crates/a/src/x.rs": { 1: 0 } }),
      ["crates/a"],
      ex,
      () => ["unreachable_fallback_arm!() // more"],
    );
    expect(part.excluded).toBe(0);
  });
});

describe("report", () => {
  it("says what passed", () => {
    expect(
      report({ missed: [], absent: [], stale: [], executable: 7, excluded: 1 }),
    ).toEqual({
      code: 0,
      lines: ["full-coverage ok: 7 executable lines, 1 excluded"],
    });
  });

  it("lists absent crates, missed lines and stale exclusions", () => {
    const r = report({
      missed: [{ path: "crates/a/src/x.rs", line: 3, text: "  let x = 1;" }],
      absent: ["crates/typo"],
      stale: parseExclusions(
        "crates/a/src/x.rs | a line that is gone now | why",
      ),
      executable: 1,
      excluded: 0,
    });
    expect(r.code).toBe(1);
    expect(r.lines).toEqual([
      "full-coverage: crates/typo has no entry in the coverage report",
      "full-coverage: 1 line(s) never ran in a test:",
      "  crates/a/src/x.rs:3: let x = 1;",
      'full-coverage: exclusion "crates/a/src/x.rs | a line that is gone now" matches no unrun line; remove it',
    ]);
  });
});

describe("main", () => {
  let dir = "";
  const files: Record<string, string> = {
    "crates/a/src/lib.rs": "fn one() {}\nfn two() {}\n",
    "crates.txt": "crates/a\n",
    "none.txt": "",
  };
  beforeAll(() => {
    dir = realpathSync.native(mkdtempSync(join(tmpdir(), "full-coverage-")));
    execFileSync("git", ["init", "-q"], { cwd: dir, env: cleanGitEnv() });
    for (const [name, body] of Object.entries(files)) {
      mkdirSync(join(dir, name, ".."), { recursive: true });
      writeFileSync(join(dir, name), body);
    }
  });
  afterAll(() => rmSync(dir, { recursive: true, force: true }));
  afterEach(() => vi.restoreAllMocks());

  const lcov = (second: number) => {
    const file = join(dir, "report.lcov");
    writeFileSync(
      file,
      `SF:${join(dir, "crates/a/src/lib.rs")}\nDA:1,1\nDA:2,${second}\nend_of_record\n`,
    );
    return file;
  };
  const args = (l: string) => [
    "--lcov",
    l,
    "--crates",
    join(dir, "crates.txt"),
    "--exclusions",
    join(dir, "none.txt"),
  ];

  it("prints ok on stdout and returns 0 when every line ran", () => {
    const out = vi.spyOn(console, "log").mockImplementation(() => {});
    expect(main(args(lcov(2)), dir)).toBe(0);
    expect(out).toHaveBeenCalledWith(
      "full-coverage ok: 2 executable lines, 0 excluded",
    );
  });

  it("prints the unrun line with its source text on stderr and returns 1", () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(main(args(lcov(0)), dir)).toBe(1);
    expect(err.mock.calls.map((c) => c[0])).toEqual([
      "full-coverage: 1 line(s) never ran in a test:",
      "  crates/a/src/lib.rs:2: fn two() {}",
    ]);
  });

  it("refuses a call without its three arguments", () => {
    expect(() => main(["--lcov", "x"], dir)).toThrow(
      "usage: full-coverage.ts --lcov <file> --crates <list> --exclusions <file>",
    );
  });

  it("reads an empty string for a line past the end of the source", () => {
    const file = join(dir, "past.lcov");
    writeFileSync(
      file,
      `SF:${join(dir, "crates/a/src/lib.rs")}\nDA:99,0\nend_of_record\n`,
    );
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    expect(main(args(file), dir)).toBe(1);
    expect(err.mock.calls.map((c) => c[0])).toContain(
      "  crates/a/src/lib.rs:99: ",
    );
  });
});
