// SPDX-License-Identifier: GPL-3.0-or-later
import { execFileSync } from "node:child_process";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { main } from "./audit.ts";

vi.mock("node:child_process", () => ({ execFileSync: vi.fn() }));
vi.mock("node:fs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs")>();
  return {
    ...actual,
    readFileSync: (path: string, ...rest: unknown[]) =>
      String(path).endsWith("audit-exceptions.json")
        ? mocked.exceptions
        : (actual.readFileSync as (...a: unknown[]) => unknown)(path, ...rest),
  };
});
const mocked = vi.hoisted(() => ({ exceptions: "" }));

const npm = vi.mocked(execFileSync);

const report = (...ids: Array<[string, string]>) =>
  JSON.stringify({
    vulnerabilities: {
      pkg: {
        via: ids.map(([id, severity]) => ({
          url: `https://github.com/advisories/${id}`,
          severity,
          title: `title of ${id}`,
        })),
      },
    },
  });

/** npm audit exits 1 with the report on stdout when it finds something. */
const npmFindings = (stdout: string) => {
  npm.mockImplementation(() => {
    throw Object.assign(new Error("exit 1"), { stdout });
  });
};

describe("ui-audit main", () => {
  let out: string[];
  let err: string[];
  let warn: string[];

  beforeEach(() => {
    mocked.exceptions = JSON.stringify({ exceptions: [] });
    out = [];
    err = [];
    warn = [];
    vi.spyOn(console, "log").mockImplementation(
      (m: string) => void out.push(m),
    );
    vi.spyOn(console, "error").mockImplementation(
      (m: string) => void err.push(m),
    );
    vi.spyOn(console, "warn").mockImplementation(
      (m: string) => void warn.push(m),
    );
  });
  afterEach(() => {
    vi.restoreAllMocks();
    npm.mockReset();
  });

  it("passes and counts the exceptions when npm reports nothing", () => {
    mocked.exceptions = JSON.stringify({
      exceptions: [{ id: "GHSA-old", review_by: "2999-01-01" }],
    });
    npm.mockReturnValue("{}");
    expect(main([])).toBe(0);
    expect(out).toEqual([
      "ui-audit ok: no unaccepted advisories at moderate or above (1 exceptions)",
    ]);
    expect(warn).toEqual([
      "ui-audit: exception GHSA-old matches nothing now; remove it",
    ]);
  });

  it("fails and lists an advisory that has no exception, read from npm's stdout on exit 1", () => {
    npmFindings(report(["GHSA-aaaa", "high"]));
    expect(main([])).toBe(1);
    expect(err).toEqual([
      "ui-audit: 1 problem(s)\n  GHSA-aaaa (high) in pkg: title of GHSA-aaaa",
    ]);
  });

  it("ignores advisories below moderate", () => {
    npm.mockReturnValue(report(["GHSA-low", "low"]));
    expect(main([])).toBe(0);
    expect(err).toEqual([]);
  });

  it("accepts an advisory with a current exception", () => {
    mocked.exceptions = JSON.stringify({
      exceptions: [{ id: "GHSA-aaaa", review_by: "2999-01-01" }],
    });
    npm.mockReturnValue(report(["GHSA-aaaa", "moderate"]));
    expect(main([])).toBe(0);
    expect(warn).toEqual([]);
  });

  it("fails an advisory whose exception expired", () => {
    mocked.exceptions = JSON.stringify({
      exceptions: [{ id: "GHSA-aaaa", review_by: "2000-01-01" }],
    });
    npm.mockReturnValue(report(["GHSA-aaaa", "moderate"]));
    expect(main([])).toBe(1);
    expect(err).toEqual([
      "ui-audit: 1 problem(s)\n  GHSA-aaaa: exception expired on 2000-01-01; review it",
    ]);
  });

  it("fails with npm's own summary when the report is an error", () => {
    npm.mockReturnValue(JSON.stringify({ error: { summary: "offline" } }));
    expect(main([])).toBe(1);
    expect(err).toEqual(["ui-audit: npm audit failed: offline"]);
  });

  it("prints the error object when the error report has no summary, and exits 2 with --json", () => {
    npm.mockReturnValue(JSON.stringify({ error: { code: "E404" } }));
    expect(main(["--json"])).toBe(2);
    expect(err).toEqual(['ui-audit: npm audit failed: {"code":"E404"}']);
  });

  it("rethrows npm's failure when it gave no report", () => {
    npm.mockImplementation(() => {
      throw new Error("spawn npm ENOENT");
    });
    expect(() => main([])).toThrow("spawn npm ENOENT");
  });

  it("exits 2 with a message when npm gave no report under --json", () => {
    npm.mockImplementation(() => {
      throw new Error("spawn npm ENOENT");
    });
    expect(main(["--json"])).toBe(2);
    expect(err).toEqual([
      "ui-audit: npm audit gave no report: Error: spawn npm ENOENT",
    ]);
  });

  it("prints the failing advisories as a JSON array and exits 1 under --json", () => {
    npmFindings(report(["GHSA-aaaa", "critical"]));
    expect(main(["--json"])).toBe(1);
    expect(out).toEqual([
      JSON.stringify([
        {
          id: "GHSA-aaaa",
          name: "pkg",
          severity: "critical",
          title: "title of GHSA-aaaa",
        },
      ]),
    ]);
  });

  it("prints an empty JSON array and exits 0 under --json when nothing fails", () => {
    npm.mockReturnValue("{}");
    expect(main(["--json"])).toBe(0);
    expect(out).toEqual(["[]"]);
  });

  it("asks npm for production dependencies at moderate and above", () => {
    npm.mockReturnValue("{}");
    main([]);
    expect(npm.mock.calls[0]?.[1]).toEqual([
      "audit",
      "--omit=dev",
      "--audit-level=moderate",
      "--json",
    ]);
  });
});
