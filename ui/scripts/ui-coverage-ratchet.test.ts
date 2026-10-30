// SPDX-License-Identifier: GPL-3.0-or-later
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  afterAll,
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import {
  judge,
  main,
  parseBaseline,
  readTotals,
} from "./ui-coverage-ratchet.ts";

const t = (
  lines: number,
  branches = lines,
  functions = lines,
  statements = lines,
) => ({
  lines,
  branches,
  functions,
  statements,
});
const summary = (x: ReturnType<typeof t>) =>
  JSON.stringify({
    total: Object.fromEntries(
      Object.entries(x).map(([k, pct]) => [k, { pct }]),
    ),
  });

describe("readTotals", () => {
  it("reads the four percentages of vitest's json-summary", () => {
    expect(readTotals(summary(t(99.5, 90.2, 98, 97)))).toEqual(
      t(99.5, 90.2, 98, 97),
    );
  });
  it("names the metric a summary lacks", () => {
    expect(() => readTotals('{"total":{"lines":{"pct":1}}}')).toThrow(
      "summary has no branches total",
    );
    expect(() => readTotals("{}")).toThrow("summary has no lines total");
  });
});

describe("parseBaseline", () => {
  it("reads the four numbers", () => {
    expect(parseBaseline(JSON.stringify(t(90, 80, 85, 88)))).toEqual(
      t(90, 80, 85, 88),
    );
  });
  it("names the number a baseline lacks", () => {
    expect(() => parseBaseline('{"lines":1}')).toThrow(
      "baseline needs a numeric branches",
    );
  });
});

describe("judge", () => {
  it("passes at the baseline and proposes nothing", () => {
    expect(judge(t(90), t(90), undefined, false)).toEqual({
      failures: [],
      raised: undefined,
    });
  });

  it("fails each metric below its baseline, with no wobble", () => {
    const v = judge(
      t(89.99, 90, 85.5, 90),
      t(90, 90, 85.6, 90),
      undefined,
      false,
    );
    expect(v.failures).toEqual([
      "lines coverage 89.99 % is below the baseline 90 %",
      "functions coverage 85.50 % is below the baseline 85.6 %",
    ]);
  });

  it("proposes the measured values rounded down to 0.1 for the metrics that rose only", () => {
    const v = judge(
      t(91.29, 90, 95.99, 90),
      t(90, 90, 90, 90),
      undefined,
      false,
    );
    expect(v.raised).toEqual(t(91.2, 90, 95.9, 90));
  });

  it("fails a lowered baseline unless the owner's OK is given", () => {
    expect(judge(t(90), t(90, 80, 90, 90), t(90), false).failures).toEqual([
      "the branches baseline was lowered (90 to 80); that needs the owner's OK",
    ]);
    expect(judge(t(90), t(90, 80, 90, 90), t(90), true).failures).toEqual([]);
  });
});

describe("main", () => {
  const dir = mkdtempSync(join(tmpdir(), "ui-ratchet-"));
  afterAll(() => rmSync(dir, { recursive: true, force: true }));
  let out: ReturnType<typeof vi.spyOn>;
  let err: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    out = vi.spyOn(console, "log").mockImplementation(() => {});
    err = vi.spyOn(console, "error").mockImplementation(() => {});
  });
  afterEach(() => {
    vi.restoreAllMocks();
    delete process.env["COVERAGE_BASELINE_LOWER_OK"];
  });

  const files = (
    measured: ReturnType<typeof t>,
    baseline: ReturnType<typeof t>,
    previous?: ReturnType<typeof t>,
  ) => {
    const s = join(dir, "summary.json");
    const b = join(dir, "baseline.json");
    writeFileSync(s, summary(measured));
    writeFileSync(b, JSON.stringify(baseline));
    const args = ["--summary", s, "--baseline", b];
    if (previous) {
      const p = join(dir, "previous.json");
      writeFileSync(p, JSON.stringify(previous));
      args.push("--previous", p);
    }
    return { args, b };
  };

  it("prints the totals and returns 0 at the baseline", () => {
    expect(main(files(t(90), t(90)).args)).toBe(0);
    expect(out).toHaveBeenCalledWith(
      "ui coverage: lines 90.00 % (baseline 90), branches 90.00 % (baseline 90), functions 90.00 % (baseline 90), statements 90.00 % (baseline 90)",
    );
  });

  it("returns 1 and says which metric fell below", () => {
    expect(main(files(t(80), t(90)).args)).toBe(1);
    expect(err.mock.calls[0]).toEqual([
      "ui coverage ratchet: lines coverage 80.00 % is below the baseline 90 %",
    ]);
  });

  it("tells how to raise the baseline without writing it", () => {
    const { args, b } = files(t(95), t(90));
    expect(main(args)).toBe(0);
    expect(out).toHaveBeenLastCalledWith(
      "ui coverage ratchet: coverage is above the baseline (would raise it to lines 95, branches 95, functions 95, statements 95); the nightly coverage-baseline workflow raises it",
    );
    expect(JSON.parse(readFileSync(b, "utf8"))).toEqual(t(90));
  });

  it("writes the raised baseline with --write", () => {
    const { args, b } = files(t(95), t(90));
    expect(main([...args, "--write"])).toBe(0);
    expect(JSON.parse(readFileSync(b, "utf8"))).toEqual(t(95));
    expect(out).toHaveBeenLastCalledWith(
      `ui coverage ratchet: baseline raised to lines 95, branches 95, functions 95, statements 95 in ${b}; do not commit it, the nightly coverage-baseline workflow raises it`,
    );
  });

  it("fails a lowered baseline, and accepts it with the owner's OK in the environment", () => {
    const { args } = files(t(90), t(80), t(90));
    expect(main(args)).toBe(1);
    process.env["COVERAGE_BASELINE_LOWER_OK"] = "1";
    expect(main(args)).toBe(0);
  });

  it("refuses a call without a summary and a baseline", () => {
    expect(() => main(["--summary", "x"])).toThrow(
      "usage: ui-coverage-ratchet.ts --summary f --baseline f [--previous f] [--write]",
    );
  });
});
