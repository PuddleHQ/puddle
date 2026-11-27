// SPDX-License-Identifier: AGPL-3.0-or-later
// Every page under `src/routes` is scanned by axe in some e2e spec (arc42 8.10). A spec "scans" a
// route when it calls `expectNoAxeViolations` or `axeStops` and names the route's path; that is the
// most a static check can know, so it catches a route nobody scans, not a scan that skips a state.
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const ui = resolve(import.meta.dirname, "..");

/** Routes with no scan yet. A route leaves this list when a spec scans it, and the test says so. */
const NOT_SCANNED_YET: string[] = [];

function files(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? files(join(dir, e.name)) : [join(dir, e.name)],
  );
}

/** `src/routes/settings/(pages)/git/+page.svelte` is the route `/settings/git`. */
export function routesOf(paths: string[]): string[] {
  return paths
    .map((p) => relative(join(ui, "src/routes"), p).replaceAll("\\", "/"))
    .filter((p) => p === "+page.svelte" || p.endsWith("/+page.svelte"))
    .map((p) => "/" + p.replace(/\+page\.svelte$/, "").replace(/\/$/, ""))
    .map((p) => p.replace(/\/\([^/)]*\)/g, ""))
    .map((p) => (p === "/" ? p : p.replace(/\/$/, "")))
    .sort();
}

/** Whether a spec's text names the route: its path, with any `[id]` standing for one segment. */
export function names(route: string, spec: string): boolean {
  const pattern = route
    .split("/")
    .map((s) =>
      s === "[id]" ? "[^/\"'`\\s]+" : s.replace(/[.*+?^${}()|\\]/g, "\\$&"),
    )
    .join("/");
  if (new RegExp(`${pattern}(["'\`#?]|\\s|$)`).test(spec)) return true;
  // A settings page opened through `/settings/${where}` is named by its section, quoted.
  const section = /^\/settings\/([^/]+)$/.exec(route)?.[1];
  return section !== undefined && spec.includes(`"${section}"`);
}

const scans = files(join(ui, "e2e"))
  .filter((f) => f.endsWith(".spec.ts"))
  .map((f) => readFileSync(f, "utf8"))
  .filter((s) => /expectNoAxeViolations|axeStops/.test(s));
const routes = routesOf(files(join(ui, "src/routes")));

describe("the axe scan of every route", () => {
  it("finds the routes", () => {
    expect(routes).toContain("/inbox");
    expect(routes).toContain("/settings/git");
    expect(routes).toContain("/workspaces/[id]/terminal");
    expect(routes).not.toContain("/settings/(pages)/git");
  });

  it("has a spec that scans each route, except the ones listed", () => {
    const unscanned = routes.filter((r) => !scans.some((s) => names(r, s)));
    expect(unscanned).toEqual(NOT_SCANNED_YET);
  });

  it("lists only routes that exist", () => {
    for (const r of NOT_SCANNED_YET) expect(routes).toContain(r);
  });

  it("reads a route's name in a spec", () => {
    expect(names("/inbox", 'goto("/inbox")')).toBe(true);
    expect(names("/inbox", 'goto("/inbox-old")')).toBe(false);
    expect(names("/workspaces/[id]/git", "goto(`/workspaces/${id}/git`)")).toBe(
      true,
    );
    expect(
      names("/workspaces/[id]/git", 'goto("/workspaces/web-shop/git")'),
    ).toBe(true);
    expect(names("/settings/git", 'where: "git"')).toBe(true);
  });
});
