// SPDX-License-Identifier: AGPL-3.0-or-later
// The installed-app walk (crates/puddle-app/e2e/usable.mjs) plays the first-run flow in the real
// window, and only the windows msi job runs it. A change to a first-run page therefore also runs
// the cheap part of that job here: the script still parses, and it reads the list of pages and
// controls (walk.json) instead of keeping a copy that can drift.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const walker = resolve(
  import.meta.dirname,
  "../../../../crates/puddle-app/e2e/usable.mjs",
);

describe("the installed-app walk", () => {
  it("parses", () => {
    expect(() =>
      execFileSync(process.execPath, ["--check", walker], { stdio: "pipe" }),
    ).not.toThrow();
  });

  it("reads the first-run list, and names none of the pages' headings or buttons itself", () => {
    const text = readFileSync(walker, "utf8");
    expect(text).toContain("walk.json");
    // Controls behind a closed details are opened through the list, not by a name kept in the script.
    expect(text).toContain("behindDetails");
    const source = text
      .split("\n")
      .filter((line) => !/^\s*(\/\/|\/?\*)/.test(line))
      .join("\n");
    for (const words of [
      "Get started",
      "Start system check",
      "Copy report",
      "Which hosts should Puddle decrypt?",
    ]) {
      expect(source, words).not.toContain(`"${words}"`);
    }
  });
});
