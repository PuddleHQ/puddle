// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { ESLint } from "eslint";
import { describe, expect, it, vi } from "vitest";

// The first lint builds the whole type-aware ESLint program, which took over the 5 s default
// on a loaded host.
vi.setConfig({ testTimeout: 60_000 });

const original = readFileSync(
  resolve(
    import.meta.dirname,
    "../../scripts/flake-fixtures/o2-expect-await.bad.ts.txt",
  ),
  "utf8",
);

// What `npm run lint` says about `source` as if it sat in ui/e2e.
async function lint(source: string) {
  const eslint = new ESLint({ cwd: resolve(import.meta.dirname, "..") });
  const [result] = await eslint.lintText(
    `test("x", async ({ page }) => {\n${source}\n});\n`,
    { filePath: resolve(import.meta.dirname, "../e2e/probe.spec.ts") },
  );
  return result?.messages ?? [];
}

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
    const eslint = new ESLint({ cwd: resolve(import.meta.dirname, "..") });
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
});
