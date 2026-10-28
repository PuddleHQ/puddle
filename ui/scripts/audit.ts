// SPDX-License-Identifier: GPL-3.0-or-later
// The `ui-audit` gate: `npm audit` on production dependencies at moderate and above, minus the
// reviewed exceptions in audit-exceptions.json (advisory id, reason, review-by date), like
// deny.toml's `ignore`. An exception past its date fails again, so it gets looked at.
// `--json` (the nightly issue script, ci/advisories-issue.sh) prints the failing advisories as a JSON array on
// stdout instead of text: exit 0 for none, 1 for some, 2 when npm could not answer.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { runCli } from "./cli.ts";

const root = resolve(import.meta.dirname, "..");

/** Advisory ids (GHSA-...) found in an `npm audit --json` report, with the package and severity. */
export interface Advisory {
  id: string;
  name: string;
  severity: string;
  title: string;
}

export interface Exception {
  id: string;
  review_by: string;
}

interface Report {
  vulnerabilities?: Record<
    string,
    { via?: Array<string | { url?: string; severity: string; title: string }> }
  >;
  error?: { summary?: string };
}

export function advisories(report: Report): Advisory[] {
  const found = new Map<string, Advisory>();
  for (const [name, vuln] of Object.entries(report.vulnerabilities ?? {})) {
    for (const via of vuln.via ?? []) {
      if (typeof via === "object" && via.url) {
        const id = via.url.split("/").pop() ?? via.url;
        found.set(id, { id, name, severity: via.severity, title: via.title });
      }
    }
  }
  return [...found.values()];
}

/** The findings that are not excepted, and the exceptions that expired or matched nothing. */
export function judge(
  found: Advisory[],
  exceptions: Exception[],
  today: string,
): { failures: string[]; unused: string[] } {
  const failures: string[] = [];
  const byId = new Map(exceptions.map((e) => [e.id, e]));
  for (const f of found) {
    const exception = byId.get(f.id);
    if (!exception)
      failures.push(`${f.id} (${f.severity}) in ${f.name}: ${f.title}`);
    else if (exception.review_by < today)
      failures.push(
        `${f.id}: exception expired on ${exception.review_by}; review it`,
      );
  }
  const unused = exceptions
    .filter((e) => !found.some((f) => f.id === e.id))
    .map((e) => e.id);
  return { failures, unused };
}

/** The findings that fail the gate: not excepted, or excepted with a review date in the past. */
export function failing(
  found: Advisory[],
  exceptions: Exception[],
  today: string,
): Advisory[] {
  const byId = new Map(exceptions.map((e) => [e.id, e]));
  return found.filter((f) => {
    const exception = byId.get(f.id);
    return !exception || exception.review_by < today;
  });
}

export function main(argv: string[] = []): number {
  const json = argv.includes("--json");
  const exceptions = JSON.parse(
    readFileSync(resolve(root, "audit-exceptions.json"), "utf8"),
  ).exceptions;
  let output: string;
  try {
    output = execFileSync(
      "npm",
      ["audit", "--omit=dev", "--audit-level=moderate", "--json"],
      {
        cwd: root,
        encoding: "utf8",
        shell: process.platform === "win32",
      },
    );
  } catch (error) {
    // npm audit exits 1 when it finds something, and prints the report on stdout.
    const stdout = (error as { stdout?: string }).stdout;
    if (!stdout) {
      if (!json) throw error;
      console.error(`ui-audit: npm audit gave no report: ${String(error)}`);
      return 2;
    }
    output = stdout;
  }
  const report = JSON.parse(output) as Report;
  if (report.error) {
    console.error(
      `ui-audit: npm audit failed: ${report.error.summary ?? JSON.stringify(report.error)}`,
    );
    return json ? 2 : 1;
  }
  const levels = new Set(["moderate", "high", "critical"]);
  const found = advisories(report).filter((a) => levels.has(a.severity));
  if (json) {
    const rows = failing(
      found,
      exceptions,
      new Date().toISOString().slice(0, 10),
    );
    console.log(JSON.stringify(rows));
    return rows.length > 0 ? 1 : 0;
  }
  const { failures, unused } = judge(
    found,
    exceptions,
    new Date().toISOString().slice(0, 10),
  );
  for (const id of unused)
    console.warn(`ui-audit: exception ${id} matches nothing now; remove it`);
  if (failures.length > 0) {
    console.error(
      `ui-audit: ${failures.length} problem(s)\n  ${failures.join("\n  ")}`,
    );
    return 1;
  }
  console.log(
    `ui-audit ok: no unaccepted advisories at moderate or above (${exceptions.length} exceptions)`,
  );
  return 0;
}

runCli(import.meta.filename, main);
