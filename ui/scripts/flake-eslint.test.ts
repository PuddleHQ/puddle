// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { ESLint } from "eslint";
import tsParser from "@typescript-eslint/parser";
import { describe, expect, it } from "vitest";
import { e2eExpectAwait } from "../eslint.e2e.js";

const original = readFileSync(
  resolve(
    import.meta.dirname,
    "../../scripts/flake-fixtures/o2-expect-await.bad.ts.txt",
  ),
  "utf8",
);

// One instance for the file, holding the e2e block `eslint.config.js` also uses and the TypeScript parser.
// The full config also loads the Svelte compiler and every plugin: 1 s idle, 14 s with the CPUs oversubscribed
// four times, and a 30 s hook timed out on the CI host. This one costs about half of that. The load is
// CPU work that no test is about, so it happens at import (the parser) and in the warm-up below, where the
// runner sets no bar; each test then times only the lint, 1 to 60 ms. The last test checks that the config
// still includes the block.
const eslint = new ESLint({
  cwd: resolve(import.meta.dirname, ".."),
  overrideConfigFile: true,
  overrideConfig: [{ languageOptions: { parser: tsParser } }, e2eExpectAwait],
});

// What `npm run lint` says about `source` as if it sat in ui/e2e.
async function lint(source: string) {
  const [result] = await eslint.lintText(
    `test("x", async ({ page }) => {\n${source}\n});\n`,
    { filePath: resolve(import.meta.dirname, "../e2e/probe.spec.ts") },
  );
  return result?.messages ?? [];
}

await lint("");

describe("the e2e rule against a read asserted once", () => {
  it("flags both awaited reads of the original code and names the fix", async () => {
    const found = (await lint(original)).filter(
      (m) => m.ruleId === "no-restricted-syntax",
    );
    expect(found.map((m) => m.severity)).toEqual([2, 2]); // errors: `npm run lint` fails
    expect(found[0]?.message).toContain("expect.poll");
    expect(found[0]?.message).toContain("web-first assertion");
  });

  it("passes the polling form and a one-shot with a written reason", async () => {
    const fixed = `await expect.poll(() => spaceFour(page)).toBeLessThan(roomy);
// eslint-disable-next-line no-restricted-syntax -- the data is held, so the read cannot move
expect(await measure(page, "main select")).toBeNull();`;
    expect(
      (await lint(fixed)).filter((m) => m.ruleId === "no-restricted-syntax"),
    ).toEqual([]);
  });

  it("does not apply outside the e2e specs", async () => {
    const [result] = await eslint.lintText(
      `expect(await read(page)).toBe(1);\n`,
      {
        filePath: resolve(import.meta.dirname, "../src/lib/probe.ts"),
      },
    );
    expect(
      result?.messages.filter((m) => m.ruleId === "no-restricted-syntax"),
    ).toEqual([]);
  });

  it("is the block that npm run lint uses", () => {
    const config = readFileSync(
      resolve(import.meta.dirname, "../eslint.config.js"),
      "utf8",
    );
    expect(config).toMatch(/^ {2}e2eExpectAwait,$/m);
  });
});
