// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFileSync, readdirSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { PALETTES, PRESETS } from "./palette.ts";

// A status colour (the green of a finished check, the red of an error, the amber of a warning)
// comes from --color-success, --color-danger and --color-warning, which every palette sets. A
// literal one in a component or stylesheet is the same colour on every theme, and on a theme
// whose palette it does not match it looks wrong (the completed checks of the first-run
// screens stayed green).
const SRC = join(import.meta.dirname, "..", "..");
/** The files that define colours. */
const TOKEN_FILES = new Set(["lib/theme/tokens.css"]);

const NAMED =
  "green|lime|limegreen|seagreen|forestgreen|red|crimson|orange|gold|yellow|amber";
const LITERAL = new RegExp(
  [
    "#[0-9a-fA-F]{3,8}\\b",
    "\\b(?:rgba?|hsla?|hwb|oklch|oklab|lab|lch)\\(",
    `:\\s*(?:${NAMED})\\b`,
    `\\b(?:text|bg|border|fill|stroke)-(?:${NAMED})(?:-\\d+)?\\b`,
  ].join("|"),
);
/** A black or white scrim is no status colour. */
const SCRIM = /\brgba?\(\s*(?:0\s+0\s+0|255\s+255\s+255)\s*\//;

function styled(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return styled(path);
    return /\.(svelte|css)$/.test(entry.name) ? [path] : [];
  });
}

/** Lines of CSS or markup that hold a literal colour, as `file:line: text`. */
export function literalColours(name: string, text: string): string[] {
  return text.split("\n").flatMap((line, i) => {
    const code = line.replace(/\/\*.*?\*\//g, "");
    if (!LITERAL.test(code) || SCRIM.test(code)) return [];
    return [`${name}:${i + 1}: ${line.trim()}`];
  });
}

describe("status colours", () => {
  it("no component or stylesheet writes a literal colour outside tokens.css", () => {
    const found = styled(SRC).flatMap((path) => {
      const name = relative(SRC, path).split(sep).join("/");
      return TOKEN_FILES.has(name)
        ? []
        : literalColours(name, readFileSync(path, "utf8"));
    });
    expect(found).toEqual([]);
  });

  it("the check catches a hard-coded green and lets a token and a scrim through", () => {
    expect(literalColours("a.svelte", ".ok { color: #1c7a4b; }")).toHaveLength(
      1,
    );
    expect(literalColours("a.svelte", ".ok { color: green; }")).toHaveLength(1);
    expect(
      literalColours("a.svelte", ".ok { color: rgb(20 120 70); }"),
    ).toHaveLength(1);
    expect(
      literalColours("a.svelte", ".ok { color: var(--color-success); }"),
    ).toEqual([]);
    expect(
      literalColours("a.css", ".m { background: rgb(0 0 0 / 0.5); }"),
    ).toEqual([]);
  });

  it("every named theme has a success colour of its own, apart from Pond's", () => {
    for (const preset of PRESETS.filter((p) => p !== "pond")) {
      for (const scheme of ["light", "dark"] as const) {
        expect(
          PALETTES[preset][scheme].success,
          `${preset} ${scheme}`,
        ).not.toBe(PALETTES.pond[scheme].success);
      }
    }
  });
});
