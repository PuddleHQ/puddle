// SPDX-License-Identifier: AGPL-3.0-or-later
// The sweep behind "one frame and one title style per route": the frame (a width) is chosen once,
// in the root layout from `frameFor`, and the title style is one global h1 rule. A route that sets
// its own title size or a rem width on its content would break both, so none may.
import { describe, expect, it } from "vitest";
import { frameFor } from "#lib/nav.ts";
import { readFileSync } from "node:fs";

const base = readFileSync("src/lib/theme/base.css", "utf8");

const sources = import.meta.glob<string>(
  [
    "./**/*.svelte",
    "../lib/welcome/*.svelte",
    "../lib/components/EmptyState.svelte",
  ],
  { query: "?raw", import: "default", eager: true },
);

/** `./workspaces/[id]/git/+page.svelte` to `/workspaces/x/git`. */
function pathOf(file: string): string {
  return (
    file
      .replace(/^\.\//, "/")
      .replace(/\/\+(page|layout|error)\.svelte$/, "")
      .replace(/\/\([^)]*\)/g, "")
      .replace(/\[[^\]]*\]/g, "x") || "/"
  );
}

describe("every route", () => {
  const routes = Object.keys(sources).filter((f) => f.startsWith("./"));

  it("has a frame", () => {
    expect(routes.length).toBeGreaterThan(20);
    for (const file of routes) {
      expect(["reading", "full"], file).toContain(frameFor(pathOf(file)));
    }
  });

  it("sets no title size and no page-sized width (30 rem or more) of its own", () => {
    for (const [file, source] of Object.entries(sources)) {
      const style = source.slice(source.indexOf("<style>"));
      expect(style, `${file} sizes its own h1`).not.toMatch(
        /(^|\n)\s*h1[^{]*\{[^}]*font-size/,
      );
      expect(style, `${file} sets its own page width`).not.toMatch(
        /(^|[^(])max-width:\s*([3-9]\d|\d{3,})(\.\d+)?rem/,
      );
    }
  });
});

describe("the title style", () => {
  it("is one global h1 rule: 24 px (--text-xl), bold, for every route", () => {
    expect(base).toMatch(
      /\nh1 \{[^}]*font-size: var\(--text-xl\);[^}]*font-weight: 600/,
    );
  });

  it("is used by every page title and no route renders its own heading element for it", () => {
    const titled = Object.entries(sources).filter(([, s]) =>
      /<h1[\s>]/.test(s),
    );
    expect(titled.length).toBeGreaterThan(10);
  });
});
