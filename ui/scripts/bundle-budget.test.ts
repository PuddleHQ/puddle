// SPDX-License-Identifier: GPL-3.0-or-later
import { randomBytes } from "node:crypto";
import { describe, expect, it } from "vitest";
import { assetsOf, measure, overBudget } from "./bundle-budget.ts";

const page = `<link href="/favicon.svg" rel="icon">
<link href="/_app/immutable/entry/start.A.js" rel="modulepreload">
<link href="/_app/immutable/assets/0.B.css" rel="stylesheet">
<link href="/_app/immutable/entry/start.A.js" rel="modulepreload">
<script>import("/_app/immutable/entry/app.C.js?x=1")</script>
<script type="module" src="/_app/immutable/entry/app.C.js"></script>`;

describe("bundle budget", () => {
  it("lists each script and stylesheet the page names once, and nothing else", () => {
    expect(assetsOf(page)).toEqual([
      { path: "/_app/immutable/entry/start.A.js", kind: "js" },
      { path: "/_app/immutable/assets/0.B.css", kind: "css" },
      { path: "/_app/immutable/entry/app.C.js", kind: "js" },
    ]);
  });

  it("sums gzipped sizes per kind, so repeated text counts for little", () => {
    const sizes = measure(page, (path) =>
      path.endsWith(".css")
        ? new TextEncoder().encode("a".repeat(10_000))
        : randomBytes(1000),
    );
    expect(sizes.js).toBeGreaterThan(2000);
    expect(sizes.css).toBeLessThan(100);
    expect(sizes.files).toHaveLength(3);
  });

  it("refuses a page that names no script", () => {
    expect(() => measure("<p>hi</p>", () => new Uint8Array())).toThrow(
      /names no script/,
    );
  });

  it("passes at the budget and names the biggest files one byte over", () => {
    const sizes = {
      js: 250_000,
      css: 50_000,
      files: [{ path: "/_app/a.js", gzip: 250_000 }],
    };
    expect(overBudget(sizes)).toEqual([]);
    expect(overBudget({ ...sizes, js: 250_001 })).toEqual([
      "initial js: 250001 bytes gzipped, budget 250000",
      "  250000 /_app/a.js",
    ]);
    expect(overBudget({ ...sizes, css: 50_001 })[0]).toMatch(/initial css/);
  });
});
