// SPDX-License-Identifier: GPL-3.0-or-later
// The diff-coverage gate (docs/STANDARDS.md, "Coverage"): every line a change adds or changes in
// code that a coverage run measured must be executed by a test, or sit in the reviewed exclusion
// file (scripts/diff-coverage-exclusions.txt: `path glob | whole line, or * | [xN |] reason`).
//
// How an exclusion matches (a pass must not hide a later untested line of the same shape):
// - The text is a whole line, compared trimmed, never a part of one. Several lines are joined with
//   the two characters `\n`; the entry then needs those lines directly above the last one as well,
//   which is how a bare `)?;` or `}` is pinned to the statement it ends. A backslash escapes
//   the next character, since `|` separates the fields: `\|` is a bar and `\\` a backslash in the code.
// - The file may hold at most N places where the text matches (`xN`, default 1). A copy added later
//   makes the count N+1 and the entry stops excusing anything in that file, visibly.
// - A text under 20 characters is refused when the file is parsed. `*` (a whole file, for code that
//   only another OS or a VM runs) is the one wildcard and stays a reviewed whole-file choice.
//
// Usage: node scripts/diff-coverage.ts --base <rev> --lcov <file> --exts .rs[,.ts] [--prefix ui/]
//          [--exclusions <file>] [--label <name>]
// `--base` is the commit the change is measured from (merge base locally, PR or push base in CI);
// the diff is that commit against the working tree. `--lcov` is an lcov report whose `SF:` paths
// are absolute or relative to `--prefix` (a directory under the repository root, default "").
//
// What it can't see (a pass is not proof of tested behaviour):
// - Lines without coverage data: comments, blank lines, declarations, and code compiled out of the
//   measured build (`cfg(windows)` lines inside a Linux run). Only executable lines can fail.
// - Whole files absent from the report (types-only files, files not compiled here): listed as
//   "not measured", never failed.
// - Line coverage, not branch or region: a line where one branch ran counts as covered.
// - Untracked files: `git diff` only sees tracked ones, so `git add -N <file>` before running by hand.
// - A covered line is not an asserted line: it says a test ran it, not that a test checked it.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { relative, resolve } from "node:path";
import { runCli } from "./cli.ts";

/** Execution counts per line, per repository-relative path. */
export type Coverage = Map<string, Map<number, number>>;

/** One line a change adds: where, and its text (for exclusion matching and the report). */
export interface AddedLine {
  path: string;
  line: number;
  text: string;
}

export interface Exclusion {
  glob: string;
  /** `*`, or the whole trimmed lines (in file order) the excused line ends. */
  lines: string[] | "*";
  /** The text as written, for messages. */
  needle: string;
  /** Most places in one file where the text may match. */
  max: number;
  reason: string;
  used: boolean;
}

/** The shortest text an entry may have; shorter ones are one token that fits any file's lines. */
export const MIN_TEXT = 20;
/** Written between lines in an entry that needs the lines above its last one. */
export const LINE_JOIN = "\\n";

/** The lines of a repository file as it is on disk now, or undefined when there is none. */
export type Source = (path: string) => string[] | undefined;

const posix = (p: string) => p.split("\\").join("/");

/** Parses an lcov report; a line seen twice (generic instantiations) keeps its highest count. */
export function parseLcov(
  text: string,
  repoRoot: string,
  prefix: string,
): Coverage {
  const out: Coverage = new Map();
  let current: Map<number, number> | undefined;
  for (const raw of text.split(/\r?\n/)) {
    if (raw.startsWith("SF:")) {
      const file = raw.slice(3);
      const abs = resolve(repoRoot, prefix, file);
      const path = posix(relative(repoRoot, abs));
      current = out.get(path) ?? new Map();
      out.set(path, current);
    } else if (raw.startsWith("DA:") && current) {
      const [no, count] = raw.slice(3).split(",");
      const line = Number(no);
      const hits = Number(count);
      if (Number.isInteger(line) && Number.isFinite(hits)) {
        current.set(line, Math.max(current.get(line) ?? 0, hits));
      }
    } else if (raw === "end_of_record") {
      current = undefined;
    }
  }
  return out;
}

/** Parses `git diff -U0` output into the added lines (new-file numbering). */
export function parseDiff(diff: string): AddedLine[] {
  const out: AddedLine[] = [];
  let path: string | undefined;
  let next = 0;
  for (const raw of diff.split("\n")) {
    if (raw.startsWith("+++ ")) {
      path =
        raw === "+++ /dev/null"
          ? undefined
          : posix(raw.slice(4).replace(/^b\//, ""));
    } else if (raw.startsWith("@@")) {
      const m = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      if (m) next = Number(m[1]);
    } else if (path && raw.startsWith("+") && !raw.startsWith("+++")) {
      out.push({ path, line: next, text: raw.slice(1) });
      next += 1;
    }
  }
  return out;
}

/**
 * Index of the next field separator `|` at or after `from`, or -1. A backslash escapes the
 * character after it, so `\|` and `\\` never separate.
 */
function bar(line: string, from: number): number {
  for (let i = from; i < line.length; i += 1) {
    if (line[i] === "\\") i += 1;
    else if (line[i] === "|") return i;
  }
  return -1;
}

/** The lines an entry's text stands for: `\n` ends a line, `\|` is a bar, `\\` is a backslash. */
function decodeLines(text: string): string[] {
  const lines: string[] = [""];
  const put = (t: string) => lines.splice(-1, 1, (lines.at(-1) ?? "") + t);
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i] as string;
    if (c !== "\\") put(c);
    else if (text[i + 1] === "n") {
      lines.push("");
      i += 1;
    } else if (text[i + 1] === "|" || text[i + 1] === "\\") {
      put(text[i + 1] as string);
      i += 1;
    } else put(c);
  }
  return lines.map((l) => l.trim());
}

/** Exclusion file: `path glob | whole line (or *) | [xN |] reason`, `#` comments, blanks ignored. */
export function parseExclusions(text: string): Exclusion[] {
  const out: Exclusion[] = [];
  for (const [i, raw] of text.split(/\r?\n/).entries()) {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) continue;
    const first = bar(line, 0);
    const second = first < 0 ? -1 : bar(line, first + 1);
    const glob = first < 0 ? "" : line.slice(0, first).trim();
    const needle = second < 0 ? "" : line.slice(first + 1, second).trim();
    let reason = second < 0 ? "" : line.slice(second + 1).trim();
    let max = 1;
    const count = /^x([1-9]\d*)\s*\|\s*/.exec(reason);
    if (count) {
      max = Number(count[1]);
      reason = reason.slice(count[0].length).trim();
    }
    if (!glob || !needle || !reason) {
      throw new Error(
        `exclusions line ${i + 1}: want "path glob | whole line, or * | [xN |] reason" (a reason is required)`,
      );
    }
    if (needle === "*") {
      out.push({ glob, lines: "*", needle, max, reason, used: false });
      continue;
    }
    const lines = decodeLines(needle);
    if (lines.some((l) => l === "") || lines.join(" ").length < MIN_TEXT) {
      throw new Error(
        `exclusions line ${i + 1}: "${needle}" is too short to name one line (under ${MIN_TEXT} characters, or an empty line). Write the whole line, or put the line above it first, joined with ${LINE_JOIN}; use * for a whole file`,
      );
    }
    out.push({ glob, lines, needle, max, reason, used: false });
  }
  return out;
}

export function globToRegExp(glob: string): RegExp {
  let re = "";
  for (let i = 0; i < glob.length; i += 1) {
    const c = glob[i] as string;
    if (c === "*" && glob[i + 1] === "*") {
      re += ".*";
      i += 1;
    } else if (c === "*") re += "[^/]*";
    else re += c.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${re}$`);
}

export interface Report {
  /** Added lines that are executable and never ran, and match no exclusion. */
  uncovered: AddedLine[];
  /** Added lines that are executable and ran. */
  covered: number;
  /** Added lines an exclusion accepted (only those that would have failed). */
  excluded: number;
  /** Changed files with no entry in the coverage report. */
  notMeasured: string[];
}

/** Whether `lines` end at 1-based line `at` of `file`, compared trimmed. */
export function endsAt(file: string[], at: number, lines: string[]): boolean {
  const start = at - lines.length;
  if (start < 0) return false;
  return lines.every((l, k) => (file[start + k] ?? "").trim() === l);
}

/** How many places in `file` the entry's lines end at. */
export function occurrences(file: string[], lines: string[]): number {
  let n = 0;
  for (let at = lines.length; at <= file.length; at += 1) {
    if (endsAt(file, at, lines)) n += 1;
  }
  return n;
}

/**
 * Splits the added lines into covered, excused and failed. `source` gives a file's lines as
 * they are now (`run` reads the working tree), which a line-text entry is compared against.
 *
 * What it can't see: a line is excused by its own text and the lines above it, so an uncovered
 * line that is an exact copy of an excused one (same text, same lines above) in a file that still
 * holds at most N of them is accepted; and an entry kept after its line was rewritten into a
 * different line is simply stale, reported as unused. Neither a covered copy nor a moved line
 * changes the count.
 */
export function judge(
  added: AddedLine[],
  coverage: Coverage,
  exclusions: Exclusion[],
  source: Source = () => undefined,
  notes: string[] = [],
): Report {
  const matchers = exclusions.map((e) => ({ e, re: globToRegExp(e.glob) }));
  const report: Report = {
    uncovered: [],
    covered: 0,
    excluded: 0,
    notMeasured: [],
  };
  const unmeasured = new Set<string>();
  for (const a of added) {
    const lines = coverage.get(a.path);
    if (!lines) {
      unmeasured.add(a.path);
      continue;
    }
    const hits = lines.get(a.line);
    if (hits === undefined) continue; // not an executable line
    if (hits > 0) {
      report.covered += 1;
      continue;
    }
    const file = source(a.path);
    const hit = matchers.find(({ e, re }) => {
      if (!re.test(a.path)) return false;
      if (e.lines === "*") return true;
      const lines = file ?? (e.lines.length === 1 ? [a.text] : undefined);
      // Without the file only a one-line entry can be checked, against the added line itself.
      const at = file ? a.line : 1;
      if (!lines || !endsAt(lines, at, e.lines)) return false;
      const n = occurrences(lines, e.lines);
      if (n > e.max) {
        notes.push(
          `exclusion "${e.glob} | ${e.needle}" allows ${e.max} place(s) in a file, ${a.path} has ${n}: it excuses nothing there until the count is checked and raised`,
        );
        return false;
      }
      return true;
    });
    if (hit) {
      hit.e.used = true;
      report.excluded += 1;
    } else report.uncovered.push(a);
  }
  report.notMeasured = [...unmeasured].sort();
  return report;
}

/** The environment without the variables a git hook exports, which would override `cwd`. */
export function cleanGitEnv(
  env: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv {
  const out = { ...env };
  for (const k of ["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE", "GIT_PREFIX"])
    delete out[k];
  return out;
}

export function git(args: string[], cwd: string): string {
  return execFileSync("git", args, {
    env: cleanGitEnv(),
    cwd,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });
}

export interface Options {
  base: string;
  lcov: string;
  exts: string[];
  prefix: string;
  exclusions?: string | undefined;
  label: string;
}

function parseArgs(argv: string[]): Options {
  const get = (name: string) => {
    const i = argv.indexOf(`--${name}`);
    return i >= 0 ? argv[i + 1] : undefined;
  };
  const base = get("base");
  const lcov = get("lcov");
  const exts = get("exts");
  if (!base || !lcov || !exts) {
    throw new Error(
      "usage: diff-coverage.ts --base <rev> --lcov <file> --exts .rs[,.ts] [--prefix dir/] [--exclusions file] [--label name]",
    );
  }
  return {
    base,
    lcov,
    exts: exts.split(","),
    prefix: get("prefix") ?? "",
    exclusions: get("exclusions"),
    label: get("label") ?? "diff-coverage",
  };
}

export function run(opts: Options, cwd: string = process.cwd()): number {
  const repoRoot = git(["rev-parse", "--show-toplevel"], cwd).trim();
  if (!existsSync(opts.lcov)) {
    console.error(
      `${opts.label}: ${opts.lcov} not found; run the coverage gate first`,
    );
    return 1;
  }
  const diff = git(
    [
      "diff",
      "-U0",
      "--no-color",
      "--no-ext-diff",
      "--no-renames",
      opts.base,
      "--",
    ],
    repoRoot,
  );
  const added = parseDiff(diff).filter(
    (a) =>
      a.path.startsWith(opts.prefix) &&
      opts.exts.some((x) => a.path.endsWith(x)),
  );
  const coverage = parseLcov(
    readFileSync(opts.lcov, "utf8"),
    repoRoot,
    opts.prefix,
  );
  const exclusions = opts.exclusions
    ? parseExclusions(readFileSync(opts.exclusions, "utf8"))
    : [];
  const cache = new Map<string, string[] | undefined>();
  const source: Source = (path) => {
    if (!cache.has(path)) {
      const file = resolve(repoRoot, path);
      cache.set(
        path,
        existsSync(file)
          ? readFileSync(file, "utf8").split(/\r?\n/)
          : undefined,
      );
    }
    return cache.get(path);
  };
  const notes: string[] = [];
  const report = judge(added, coverage, exclusions, source, notes);
  for (const n of new Set(notes)) console.log(`${opts.label}: note: ${n}`);
  if (report.notMeasured.length > 0) {
    console.log(
      `${opts.label}: not in the coverage report (not measured): ${report.notMeasured.join(", ")}`,
    );
  }
  for (const e of exclusions.filter((x) => !x.used)) {
    console.log(
      `${opts.label}: note: exclusion "${e.glob} | ${e.needle}" matched nothing in this diff`,
    );
  }
  if (report.uncovered.length > 0) {
    console.error(
      `${opts.label}: ${report.uncovered.length} added or changed line(s) never ran in a test:`,
    );
    for (const u of report.uncovered)
      console.error(`  ${u.path}:${u.line}: ${u.text.trim()}`);
    console.error(
      `  Cover them, or list them with a reason in the exclusion file (reviewed like code).`,
    );
    return 1;
  }
  console.log(
    `${opts.label} ok: ${report.covered} changed executable line(s) covered, ${report.excluded} excluded, since ${opts.base.slice(0, 10)}`,
  );
  return 0;
}

/** The gate as the command line calls it: the arguments in, the exit code out. */
export function runFromArgs(argv: string[]): number {
  return run(parseArgs(argv));
}

runCli(import.meta.filename, runFromArgs, "diff-coverage");
