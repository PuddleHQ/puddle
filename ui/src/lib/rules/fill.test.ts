// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { contrast } from "#lib/theme/palette.ts";
import {
  SWATCHES,
  describeFill,
  fillVars,
  inkOn,
  isHexColour,
  primaryColour,
  solid,
} from "./fill.ts";

const tokens = readFileSync(
  join(import.meta.dirname, "../theme/tokens.css"),
  "utf8",
);

/** A `--color-*` token of the default theme in one mode. */
function token(name: string, mode: "light" | "dark"): string {
  const rule = new RegExp(`${name}:\\s*light-dark\\(([^,]+),([^)]+)\\)`).exec(
    tokens,
  );
  if (!rule?.[1] || !rule[2]) throw new Error(`token ${name} missing`);
  return (mode === "light" ? rule[1] : rule[2]).trim();
}

describe("a typed fill", () => {
  it("is written the way the service stores it, in lower case", () => {
    expect(solid("#0A6A85")).toEqual({ kind: "solid", color: "#0a6a85" });
    expect(primaryColour(solid("#0A6A85"))).toBe("#0a6a85");
    expect(describeFill(solid("#0A6A85"))).toBe("colour #0a6a85");
  });

  it("accepts #rrggbb only", () => {
    for (const ok of ["#000000", "#FFFFFF", "#0a6a85"]) {
      expect(isHexColour(ok)).toBe(true);
    }
    for (const bad of ["", "#abc", "abcdef", "#abcdeg", "#abcdef0", "teal"]) {
      expect(isHexColour(bad)).toBe(false);
    }
  });

  it("offers swatches that are valid, distinct colours", () => {
    expect(SWATCHES.every(isHexColour)).toBe(true);
    expect(new Set(SWATCHES).size).toBe(SWATCHES.length);
  });
});

describe("contrast of a set's colour", () => {
  // Every colour on the grid, not only the swatches: a user can pick anything.
  const grid: string[] = [];
  for (const r of [0, 64, 128, 192, 255]) {
    for (const g of [0, 64, 128, 192, 255]) {
      for (const b of [0, 64, 128, 192, 255]) {
        grid.push(
          `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`,
        );
      }
    }
  }

  it("keeps the ink readable (WCAG AA text, 4.5:1) on any colour", () => {
    for (const colour of [...grid, ...SWATCHES]) {
      expect(contrast(colour, inkOn(colour)), colour).toBeGreaterThanOrEqual(
        4.5,
      );
    }
  });

  it("picks dark ink on a light colour and light ink on a dark one", () => {
    expect(inkOn("#ffffff")).toBe("#000000");
    expect(inkOn("#fff4cf")).toBe("#000000");
    expect(inkOn("#000000")).toBe("#ffffff");
    expect(inkOn("#0a3a50")).toBe("#ffffff");
  });

  for (const mode of ["light", "dark"] as const) {
    it(`shows the edge of a dot or chip against every ${mode} surface (3:1)`, () => {
      // The fill itself may be near the background; the edge is the theme's border colour.
      for (const surface of [
        "--color-bg",
        "--color-surface",
        "--color-surface-raised",
      ]) {
        expect(
          contrast(token("--color-border", mode), token(surface, mode)),
          `${surface} ${mode}`,
        ).toBeGreaterThanOrEqual(3);
      }
    });

    it(`keeps the name beside a dot readable on every ${mode} surface (4.5:1)`, () => {
      for (const surface of [
        "--color-bg",
        "--color-surface",
        "--color-surface-raised",
      ]) {
        expect(
          contrast(token("--color-text", mode), token(surface, mode)),
        ).toBeGreaterThanOrEqual(4.5);
      }
    });
  }

  it("hands the colour and its ink to the element as custom properties", () => {
    expect(fillVars(solid("#2f80c9"))).toBe(
      `--set-fill: #2f80c9; --set-ink: ${inkOn("#2f80c9")}`,
    );
  });
});
