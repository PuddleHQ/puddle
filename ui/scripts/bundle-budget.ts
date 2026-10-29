// SPDX-License-Identifier: GPL-3.0-or-later
// The bundle budget gate (part of the `ui` gate, after `vite build`). What the first page load
// fetches (the scripts and stylesheets index.html names, gzipped as a server would send them) must
// stay under a fixed size, so a heavy dependency or an accidental import into the root layout
// fails here instead of making every start slower. Routes load their own chunks later and are not
// counted; the budget is for what blocks the first screen.
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { gzipSync } from "node:zlib";

/** Gzipped bytes of the initial scripts. 1 kB is 1000 bytes. */
export const INITIAL_JS_BUDGET = 250_000;
/** Gzipped bytes of the initial stylesheets. */
export const INITIAL_CSS_BUDGET = 50_000;

const root = resolve(import.meta.dirname, "..");

export interface Asset {
  path: string;
  kind: "js" | "css";
}

/**
 * The scripts and stylesheets a page names: `href`/`src` attributes on `/_app/` paths, once each.
 * An unquoted attribute or a path built by script is not seen, so `measure` also refuses a page
 * that names no script at all.
 */
export function assetsOf(html: string): Asset[] {
  const seen = new Set<string>();
  const found: Asset[] = [];
  for (const match of html.matchAll(/(?:href|src)="(\/_app\/[^"?#]+)"/g)) {
    const path = match[1] as string;
    if (seen.has(path)) continue;
    seen.add(path);
    if (path.endsWith(".js")) found.push({ path, kind: "js" });
    else if (path.endsWith(".css")) found.push({ path, kind: "css" });
  }
  return found;
}

export interface Sizes {
  js: number;
  css: number;
  files: { path: string; gzip: number }[];
}

/** Gzip size of each initial asset, `read` giving a file's bytes by its URL path. */
export function measure(
  html: string,
  read: (path: string) => Uint8Array,
): Sizes {
  const assets = assetsOf(html);
  if (!assets.some((a) => a.kind === "js"))
    throw new Error("index.html names no script: is it the built page?");
  const sizes: Sizes = { js: 0, css: 0, files: [] };
  for (const { path, kind } of assets) {
    const gzip = gzipSync(read(path), { level: 9 }).length;
    sizes[kind] += gzip;
    sizes.files.push({ path, gzip });
  }
  return sizes;
}

/** The lines to print when a budget is exceeded (empty when both hold), biggest files first. */
export function overBudget(
  sizes: Sizes,
  budgets = { js: INITIAL_JS_BUDGET, css: INITIAL_CSS_BUDGET },
): string[] {
  const lines: string[] = [];
  for (const kind of ["js", "css"] as const) {
    if (sizes[kind] > budgets[kind])
      lines.push(
        `initial ${kind}: ${sizes[kind]} bytes gzipped, budget ${budgets[kind]}`,
      );
  }
  if (lines.length > 0)
    for (const f of [...sizes.files]
      .sort((a, b) => b.gzip - a.gzip)
      .slice(0, 5))
      lines.push(`  ${f.gzip} ${f.path}`);
  return lines;
}

/** Check the built page in `build`; true when both budgets hold. */
export function main(build = join(root, "build")): boolean {
  const html = readFileSync(join(build, "index.html"), "utf8");
  const sizes = measure(html, (path) => readFileSync(join(build, path)));
  const failures = overBudget(sizes);
  console.log(
    `bundle budget: initial js ${sizes.js} of ${INITIAL_JS_BUDGET} bytes gzipped, css ${sizes.css} of ${INITIAL_CSS_BUDGET}`,
  );
  if (failures.length > 0) console.error(failures.join("\n"));
  return failures.length === 0;
}

if (import.meta.main && !main()) process.exit(1);
