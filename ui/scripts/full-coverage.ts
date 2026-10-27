// SPDX-License-Identifier: GPL-3.0-or-later
// The full-coverage gate (docs/STANDARDS.md, "Coverage"): in every crate listed in
// scripts/full-coverage-crates.txt, every executable line under src/ must have run in a test of the
// workspace run, except the lines scripts/full-coverage-exclusions.txt names with a reason. The
// workspace ratchet reads two totals and the diff gate only the lines a change touches, so a crate
// that was complete can't slip under either; this one fails on the first line that stops running.
//
// Usage: node scripts/full-coverage.ts --lcov <llvm-cov lcov file> --crates <list> --exclusions <file>
//
// What it can't see: "ran" is not "asserted" (a test may execute a line and check nothing), and a
// line in a file the report doesn't list at all (a crate with no test run) is only noticed through
// the crate itself having no entry: it fails then, so a typo in the list can't turn the gate off.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { runCli } from "./cli.ts";
import {
  git,
  globToRegExp,
  parseExclusions,
  parseLcov,
  type Coverage,
  type Exclusion,
} from "./diff-coverage.ts";

export interface Missed {
  path: string;
  line: number;
  text: string;
}

export interface Verdict {
  /** Lines that never ran and no exclusion accepts. */
  missed: Missed[];
  /** Crates with no entry in the report at all. */
  absent: string[];
  /** Exclusions that matched no unrun line. */
  stale: Exclusion[];
  /** Executable lines seen, and how many an exclusion accepted. */
  executable: number;
  excluded: number;
}

/** Parses the crate list: one folder per line, `#` comments and blank lines skipped. */
export function parseCrates(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim().replace(/\/+$/, ""))
    .filter((l) => l !== "" && !l.startsWith("#"));
}

/** `source(path, line)` returns the text of one source line, for matching exclusions. */
export function judge(
  coverage: Coverage,
  crates: string[],
  exclusions: Exclusion[],
  source: (path: string, line: number) => string,
): Verdict {
  const verdict: Verdict = {
    missed: [],
    absent: [],
    stale: [],
    executable: 0,
    excluded: 0,
  };
  for (const crate of crates) {
    const files = [...coverage.keys()].filter((p) =>
      p.startsWith(`${crate}/src/`),
    );
    if (files.length === 0) verdict.absent.push(crate);
    for (const path of files.sort()) {
      const lines = coverage.get(path) as Map<number, number>;
      for (const [line, hits] of [...lines].sort((a, b) => a[0] - b[0])) {
        verdict.executable += 1;
        if (hits > 0) continue;
        const text = source(path, line);
        const hit = exclusions.find(
          (e) =>
            globToRegExp(e.glob).test(path) &&
            (e.needle === "*" || text.includes(e.needle)),
        );
        if (hit) {
          hit.used = true;
          verdict.excluded += 1;
        } else verdict.missed.push({ path, line, text });
      }
    }
  }
  verdict.stale = exclusions.filter((e) => !e.used);
  return verdict;
}

/** The report lines, and the exit code: 1 when anything failed. */
export function report(v: Verdict): { code: number; lines: string[] } {
  const lines: string[] = [];
  for (const crate of v.absent)
    lines.push(`full-coverage: ${crate} has no entry in the coverage report`);
  if (v.missed.length > 0) {
    lines.push(
      `full-coverage: ${v.missed.length} line(s) never ran in a test:`,
    );
    for (const m of v.missed)
      lines.push(`  ${m.path}:${m.line}: ${m.text.trim()}`);
  }
  for (const e of v.stale)
    lines.push(
      `full-coverage: exclusion "${e.glob} | ${e.needle}" matches no unrun line; remove it`,
    );
  if (lines.length > 0) return { code: 1, lines };
  return {
    code: 0,
    lines: [
      `full-coverage ok: ${v.executable} executable lines, ${v.excluded} excluded`,
    ],
  };
}

export function main(argv: string[], cwd: string = process.cwd()): number {
  const get = (n: string) => {
    const i = argv.indexOf(`--${n}`);
    return i < 0 ? undefined : argv[i + 1];
  };
  const lcov = get("lcov");
  const crates = get("crates");
  const exclusions = get("exclusions");
  if (!lcov || !crates || !exclusions) {
    throw new Error(
      "usage: full-coverage.ts --lcov <file> --crates <list> --exclusions <file>",
    );
  }
  const root = git(["rev-parse", "--show-toplevel"], cwd).trim();
  const cache = new Map<string, string[]>();
  const source = (path: string, line: number) => {
    if (!cache.has(path))
      cache.set(path, readFileSync(resolve(root, path), "utf8").split("\n"));
    return (cache.get(path) as string[])[line - 1] ?? "";
  };
  const { code, lines } = report(
    judge(
      parseLcov(readFileSync(lcov, "utf8"), root, ""),
      parseCrates(readFileSync(crates, "utf8")),
      parseExclusions(readFileSync(exclusions, "utf8")),
      source,
    ),
  );
  for (const l of lines) (code === 0 ? console.log : console.error)(l);
  return code;
}

runCli(import.meta.filename, main, "full-coverage");
