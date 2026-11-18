// SPDX-License-Identifier: AGPL-3.0-or-later
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const srcDir = join(import.meta.dirname, "..", "..");

function sources(dir: string, pattern: RegExp): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path, pattern);
    return pattern.test(entry.name) ? [path] : [];
  });
}

const read = (path: string) => ({ path, text: readFileSync(path, "utf8") });
const files = sources(srcDir, /\.(svelte|css|html)$/).map(read);
// Scripts can build a style string (fillVars), so they count as declarations too.
const scripts = sources(srcDir, /\.ts$/).map(read);

/** A name counts as declared when some stylesheet, component or script sets it. */
const declared = new Set(
  [...files, ...scripts].flatMap(({ text }) => [
    ...Array.from(text.matchAll(/(--[a-z0-9-]+)\s*:/g), (m) => m[1] as string),
    ...Array.from(text.matchAll(/style:(--[a-z0-9-]+)\s*=/g), (m) => m[1]),
  ]),
);

describe("style variables", () => {
  it("every var(--name) used in the UI is declared somewhere", () => {
    const missing = files.flatMap(({ path, text }) =>
      Array.from(text.matchAll(/var\(\s*(--[a-z0-9-]+)/g))
        .map((m) => m[1] as string)
        .filter((name) => !declared.has(name))
        .map((name) => `${name} in ${path.slice(srcDir.length + 1)}`),
    );
    // An undeclared name makes the browser drop the whole property, so a gap or size silently becomes zero.
    expect([...new Set(missing)]).toEqual([]);
  });
});
