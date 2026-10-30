// SPDX-License-Identifier: GPL-3.0-or-later
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type Keep, parseKeep, rewrite, run } from "./expect-poll-codemod.ts";

const go = (body: string, keeps: Keep[] = []) =>
  rewrite(
    `test("x", async ({ page }) => {\n${body}\n});\n`,
    "a.spec.ts",
    keeps,
  );

describe("rewrite", () => {
  it("turns an awaited read into a poll, keeping the matcher chain", () => {
    const out = go(`  expect(await page.title()).not.toBe("owned");`);
    expect(out.text).toContain(
      `  await expect.poll(() => page.title()).not.toBe("owned");`,
    );
    expect(out).toMatchObject({ converted: 1, kept: 0 });
  });

  it("moves the message into the poll options", () => {
    const out = go(
      "  expect(await axe(page), `${path} (${scheme})`).toEqual([]);",
    );
    expect(out.text).toContain(
      "await expect.poll(() => axe(page), { message: `${path} (${scheme})` }).toEqual([]);",
    );
  });

  it("makes the poll function async when the read awaits inside itself", () => {
    const out = go(`  expect(await (await get()).json()).toEqual({});`);
    expect(out.text).toContain(
      `await expect.poll(async () => (await get()).json()).toEqual({});`,
    );
  });

  it("rewrites a multi-line statement and every site in the file", () => {
    const out = go(
      `  expect(await rows(page).count()).toBeGreaterThanOrEqual(\n    1,\n  );\n  expect(await bg()).toBe(dark);`,
    );
    expect(out.text).toContain(
      `await expect.poll(() => rows(page).count()).toBeGreaterThanOrEqual(\n    1,\n  );`,
    );
    expect(out.text).toContain(`await expect.poll(() => bg()).toBe(dark);`);
    expect(out.converted).toBe(2);
  });

  it("leaves a plain expect, a web-first assertion and a non-statement alone", () => {
    const body = `  expect(used.body).toBe(1);\n  await expect(page).toHaveURL(/x/);\n  const f = async () => expect(await read()).toBe(1);`;
    const out = go(body);
    expect(out.text).toContain(body);
    expect(out).toMatchObject({ converted: 0, kept: 0 });
  });

  it("keeps a matching site as a one-shot under a written reason, once", () => {
    const keeps = [parseKeep("response\\.text\\(\\)=the body is complete")];
    const first = go(
      `  expect(await response.text()).toContain("<svg");`,
      keeps,
    );
    expect(first.text).toContain(
      `  // eslint-disable-next-line no-restricted-syntax -- the body is complete\n  expect(await response.text()).toContain("<svg");`,
    );
    expect(first).toMatchObject({ converted: 0, kept: 1 });
    const again = rewrite(first.text, "a.spec.ts", keeps);
    expect(again.text).toBe(first.text);
  });

  it("is idempotent on converted code", () => {
    const first = go(`  expect(await bg()).toBe(dark);`);
    expect(rewrite(first.text, "a.spec.ts", [])).toMatchObject({
      text: first.text,
      converted: 0,
    });
  });
});

describe("parseKeep", () => {
  it("splits the pattern from the reason at the last equals sign", () => {
    expect(parseKeep("a=b=why")).toMatchObject({
      pattern: /a=b/,
      reason: "why",
    });
  });

  it.each(["nothing", "=reason", "pattern="])("refuses %j", (arg) => {
    expect(() => parseKeep(arg)).toThrow(/pattern=reason/);
  });
});

describe("run", () => {
  let dir = "";
  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), "codemod-"));
    vi.spyOn(console, "log").mockImplementation(() => {});
  });
  afterEach(() => {
    rmSync(dir, { recursive: true, force: true });
    vi.restoreAllMocks();
  });
  const spec = (name: string, body: string) => {
    const path = join(dir, name);
    writeFileSync(path, body);
    return path;
  };

  it("writes the files it changed and reports them", () => {
    const path = spec("a.spec.ts", `expect(await bg()).toBe(1);\n`);
    expect(run([path])).toBe(0);
    expect(readFileSync(path, "utf8")).toBe(
      `await expect.poll(() => bg()).toBe(1);\n`,
    );
    expect(console.log).toHaveBeenCalledWith(`${path}: 1 converted, 0 kept`);
  });

  it("with --check writes nothing and fails while a file would change", () => {
    const path = spec("a.spec.ts", `expect(await bg()).toBe(1);\n`);
    expect(run(["--check", path])).toBe(1);
    expect(readFileSync(path, "utf8")).toBe(`expect(await bg()).toBe(1);\n`);
    const clean = spec("b.spec.ts", `await expect(page).toHaveURL(/x/);\n`);
    expect(run(["--check", clean])).toBe(0);
  });

  it("takes --keep pairs", () => {
    const path = spec("a.spec.ts", `expect(await bg()).toBe(1);\n`);
    run(["--keep", "bg=a reason", path]);
    expect(readFileSync(path, "utf8")).toContain(
      "-- a reason\nexpect(await bg())",
    );
  });
});
