// SPDX-License-Identifier: AGPL-3.0-or-later
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
import { cleanGitEnv, run, type Options } from "./diff-coverage.ts";

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
  afterAll(() => rmSync(dir, { recursive: true, force: true }), HOOK_MS);
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
      [
        "t: 1 stale exclusion(s); an excuse that excuses nothing fails the gate:",
      ],
      [
        `  ${join(dir, "ex.txt")}:2: stale exclusion "src/zzz.rs | *": no file matches its path glob; delete the entry`,
      ],
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

describe("run fails a stale exclusion", () => {
  let dir = "";
  beforeAll(() => {
    dir = realpathSync.native(
      mkdtempSync(join(tmpdir(), "diff-coverage-stale-")),
    );
    const g = (...a: string[]) =>
      execFileSync(
        "git",
        ["-c", "user.email=t@example.com", "-c", "user.name=t", ...a],
        { cwd: dir, env: cleanGitEnv() },
      );
    g("init", "-q");
    mkdirSync(join(dir, "src"));
    writeFileSync(
      join(dir, "src/a.rs"),
      "fn a() {\n    let excused_line = compute_one()?;\n}\n",
    );
    g("add", ".");
    g("commit", "-q", "-m", "base");
  }, HOOK_MS);
  afterAll(() => rmSync(dir, { recursive: true, force: true }), HOOK_MS);
  afterEach(() => vi.restoreAllMocks());

  const gate = (lcov: string, exclusions: string): [number, string] => {
    writeFileSync(join(dir, "l.info"), lcov);
    writeFileSync(join(dir, "ex.txt"), exclusions);
    vi.spyOn(console, "log").mockImplementation(() => {});
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const code = run(
      {
        base: "HEAD",
        lcov: join(dir, "l.info"),
        exts: [".rs"],
        prefix: "",
        label: "t",
        exclusions: join(dir, "ex.txt"),
      },
      dir,
    );
    return [code, err.mock.calls.flat().join("\n")];
  };
  const lcov = (hits: number) =>
    `SF:${dir}/src/a.rs\nDA:2,${hits}\nend_of_record\n`;
  const live = "src/a.rs | let excused_line = compute_one()?; | why\n";

  it("passes while the excused line is in the file and never runs, though the diff is empty", () => {
    expect(gate(lcov(0), live)).toEqual([0, ""]);
  });

  it("fails with the exclusion file and line when the text is gone", () => {
    const [code, out] = gate(
      lcov(0),
      "# header\nsrc/a.rs | let long_gone_line = compute_one()?; | why\n",
    );
    expect(code).toBe(1);
    expect(out).toContain(
      `${join(dir, "ex.txt")}:2: stale exclusion "src/a.rs | let long_gone_line = compute_one()?;"`,
    );
  });

  const gateMany = (lcovs: string[], exclusions: string): [number, string] => {
    const files = lcovs.map((text, i) => {
      const f = join(dir, `l${i}.info`);
      writeFileSync(f, text);
      return f;
    });
    writeFileSync(join(dir, "ex.txt"), exclusions);
    vi.spyOn(console, "log").mockImplementation(() => {});
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const code = run(
      {
        base: "HEAD",
        lcov: files.join(","),
        exts: [".rs"],
        prefix: "",
        label: "t",
        exclusions: join(dir, "ex.txt"),
      },
      dir,
    );
    return [code, err.mock.calls.flat().join("\n")];
  };

  it("gives the same verdict on two reports that disagree about a racy line, in either order", () => {
    expect(gateMany([lcov(4), lcov(0)], live)).toEqual([0, ""]);
    expect(gateMany([lcov(0), lcov(4)], live)).toEqual([0, ""]);
  });

  it("fails an entry that every one of several reports ran, naming the entry", () => {
    const [code, out] = gateMany([lcov(4), lcov(1)], live);
    expect(code).toBe(1);
    expect(out).toContain("a test ran every line it excuses in all 2 reports");
    expect(out).toContain(`${join(dir, "ex.txt")}:1: stale exclusion`);
  });

  it("names the report that is missing", () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    writeFileSync(join(dir, "l.info"), lcov(0));
    writeFileSync(join(dir, "ex.txt"), live);
    const code = run(
      {
        base: "HEAD",
        lcov: `${join(dir, "l.info")},${join(dir, "gone.info")}`,
        exts: [".rs"],
        prefix: "",
        label: "t",
        exclusions: join(dir, "ex.txt"),
      },
      dir,
    );
    expect(code).toBe(1);
    expect(err.mock.calls.flat().join("\n")).toContain("gone.info not found");
  });

  it("only reports, and passes, when a test now runs the excused line", () => {
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    try {
      expect(gate(lcov(4), live)).toEqual([0, ""]);
      expect(log.mock.calls.flat().join("\n")).toContain(
        "a test now runs every line it excuses",
      );
    } finally {
      log.mockRestore();
    }
  });
});

describe("run on a file split", () => {
  it("does not judge lines that only moved to another file, and still fails a new line beside them", () => {
    const dir = realpathSync.native(mkdtempSync(join(tmpdir(), "dc-moved-")));
    try {
      const g = (...a: string[]) =>
        execFileSync(
          "git",
          ["-c", "user.email=t@example.com", "-c", "user.name=t", ...a],
          { cwd: dir, env: cleanGitEnv() },
        );
      const moved =
        "    let first = compute_the_first_value();\n    let second = compute_the_second_value();\n";
      g("init", "-q");
      mkdirSync(join(dir, "src"));
      writeFileSync(join(dir, "src/a.rs"), `fn a() {\n${moved}}\n`);
      g("add", ".");
      g("commit", "-q", "-m", "base");
      writeFileSync(join(dir, "src/a.rs"), "fn a() {\n    b();\n}\n");
      writeFileSync(
        join(dir, "src/b.rs"),
        `fn b() {\n${moved}    let third = a_brand_new_uncovered_line();\n}\n`,
      );
      g("add", "-N", "src/b.rs");
      writeFileSync(
        join(dir, "l.info"),
        `SF:${dir}/src/a.rs\nDA:2,1\nend_of_record\nSF:${dir}/src/b.rs\nDA:2,0\nDA:3,0\nDA:4,0\nend_of_record\n`,
      );
      const log = vi.spyOn(console, "log").mockImplementation(() => {});
      const err = vi.spyOn(console, "error").mockImplementation(() => {});
      const code = run(
        {
          base: "HEAD",
          lcov: join(dir, "l.info"),
          exts: [".rs"],
          prefix: "",
          label: "t",
        },
        dir,
      );
      vi.restoreAllMocks();
      expect(code).toBe(1);
      expect(log.mock.calls.flat().join("\n")).toContain(
        "2 added line(s) only moved",
      );
      expect(err.mock.calls.flat().join("\n")).toContain(
        "src/b.rs:4: let third = a_brand_new_uncovered_line();",
      );
      expect(err.mock.calls.flat().join("\n")).toContain(
        "1 added or changed line(s)",
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
