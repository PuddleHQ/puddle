// SPDX-License-Identifier: GPL-3.0-or-later
// The UI's coverage ratchet (docs/STANDARDS.md, "Coverage"): ui/coverage-baseline.json holds the best
// lines, branches, functions and statements percentages reached so far (rounded down to 0.1), and
// vite.config.ts uses them as vitest's thresholds, so a run below any of them fails in `npm test`.
// This script is the other half: a change may not lower the committed baseline (unless
// COVERAGE_BASELINE_LOWER_OK=1, which scripts/check.sh sets only for a commit carrying the trailer
// `Owner-OK: coverage-baseline`), and when a run measures above it, `--write` raises it in the file
// (a local run only: do not commit that, the nightly `coverage-baseline` workflow raises the baseline in its own pull request;
// CI never writes). v8's counts are the same on every machine for the same code, so unlike the Rust
// ratchet there is no wobble allowance.
//
// Usage: node scripts/ui-coverage-ratchet.ts --summary coverage/coverage-summary.json
//          --baseline coverage-baseline.json [--previous <baseline json of the base commit>] [--write]
//
// What it can't see: percentages of a whole run, so a drop in one folder hidden by a gain in another
// passes it (the folder thresholds in vite.config.ts and the diff-coverage gate hold those), and a
// green run says nothing about how well the tests assert.
import { readFileSync, writeFileSync } from "node:fs";
import { runCli } from "./cli.ts";

export const METRICS = [
  "lines",
  "branches",
  "functions",
  "statements",
] as const;
export type Metric = (typeof METRICS)[number];
export type Totals = Record<Metric, number>;

/** Reads the totals out of vitest's `json-summary` report. */
export function readTotals(json: string): Totals {
  const total = (
    JSON.parse(json) as { total?: Partial<Record<Metric, { pct?: number }>> }
  ).total;
  const out = {} as Totals;
  for (const m of METRICS) {
    const pct = total?.[m]?.pct;
    if (typeof pct !== "number") throw new Error(`summary has no ${m} total`);
    out[m] = pct;
  }
  return out;
}

/** Reads a baseline file: the four numbers, nothing else required of it. */
export function parseBaseline(text: string): Totals {
  const b = JSON.parse(text) as Partial<Totals>;
  const out = {} as Totals;
  for (const m of METRICS) {
    const v = b[m];
    if (typeof v !== "number") throw new Error(`baseline needs a numeric ${m}`);
    out[m] = v;
  }
  return out;
}

const down = (n: number) => Math.floor(n * 10 + 1e-9) / 10;

export interface Verdict {
  failures: string[];
  /** The baseline to commit when a metric went up, else undefined. */
  raised: Totals | undefined;
}

export function judge(
  measured: Totals,
  baseline: Totals,
  previous: Totals | undefined,
  lowerOk: boolean,
): Verdict {
  const failures: string[] = [];
  const next = { ...baseline };
  let up = false;
  for (const m of METRICS) {
    if (measured[m] < baseline[m]) {
      failures.push(
        `${m} coverage ${measured[m].toFixed(2)} % is below the baseline ${baseline[m]} %`,
      );
    }
    if (previous && baseline[m] < previous[m] && !lowerOk) {
      failures.push(
        `the ${m} baseline was lowered (${previous[m]} to ${baseline[m]}); that needs the owner's OK`,
      );
    }
    const best = down(measured[m]);
    if (best > baseline[m]) {
      next[m] = best;
      up = true;
    }
  }
  return { failures, raised: up ? next : undefined };
}

export function main(argv: string[]): number {
  const get = (n: string) => {
    const i = argv.indexOf(`--${n}`);
    return i < 0 ? undefined : argv[i + 1];
  };
  const summary = get("summary");
  const file = get("baseline");
  if (!summary || !file) {
    throw new Error(
      "usage: ui-coverage-ratchet.ts --summary f --baseline f [--previous f] [--write]",
    );
  }
  const measured = readTotals(readFileSync(summary, "utf8"));
  const baseline = parseBaseline(readFileSync(file, "utf8"));
  const prev = get("previous");
  const previous = prev ? parseBaseline(readFileSync(prev, "utf8")) : undefined;
  const v = judge(
    measured,
    baseline,
    previous,
    process.env["COVERAGE_BASELINE_LOWER_OK"] === "1",
  );
  console.log(
    `ui coverage: ${METRICS.map((m) => `${m} ${measured[m].toFixed(2)} % (baseline ${baseline[m]})`).join(", ")}`,
  );
  if (v.failures.length > 0) {
    for (const f of v.failures) console.error(`ui coverage ratchet: ${f}`);
    return 1;
  }
  if (v.raised) {
    const shown = METRICS.map((m) => `${m} ${v.raised?.[m]}`).join(", ");
    if (argv.includes("--write")) {
      writeFileSync(file, `${JSON.stringify(v.raised, null, 2)}\n`);
      console.log(
        `ui coverage ratchet: baseline raised to ${shown} in ${file}; do not commit it, the nightly coverage-baseline workflow raises it`,
      );
    } else {
      console.log(
        `ui coverage ratchet: coverage is above the baseline (would raise it to ${shown}); the nightly coverage-baseline workflow raises it`,
      );
    }
  }
  return 0;
}

runCli(import.meta.filename, main, "ui-coverage-ratchet");
