// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { ESLint } from "eslint";
import { beforeAll, describe, expect, it } from "vitest";

const original = readFileSync(
  resolve(
    import.meta.dirname,
    "../../scripts/flake-fixtures/o2-expect-await.bad.ts.txt",
  ),
  "utf8",
);

// One instance for the file: the config and its plugins load once, so only `beforeAll` pays for that.
const eslint = new ESLint({ cwd: resolve(import.meta.dirname, "..") });

// Loading typescript-eslint, the Svelte plugin and the TypeScript compiler is CPU work of about 1 s on an
// idle host (the lint itself is 5 to 30 ms once they are loaded). With coverage on and the CPUs
// oversubscribed twice over it took 5.3 s, over the 5 s a test gets. The hook gets about five times
// that; the tests keep the default bar, so a slow lint still fails them.
const COLD_LOAD_MS = 30_000;

beforeAll(async () => {
  await lint("");
}, COLD_LOAD_MS);

// What `npm run lint` says about `source` as if it sat in ui/e2e.
async function lint(source: string) {
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
