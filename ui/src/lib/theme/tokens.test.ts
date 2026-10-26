// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { contrast } from "./palette.ts";

const css = readFileSync(join(import.meta.dirname, "tokens.css"), "utf8");

type Mode = "light" | "dark";

/** `--color-x: light-dark(a, b)` or `--color-x: a` from the first `:root` block. */
function token(name: string, mode: Mode): string {
  const rule = new RegExp(`${name}:\\s*([^;]+);`).exec(css);
  if (!rule?.[1]) throw new Error(`token ${name} missing`);
  const value = rule[1].trim();
  const pair =
    /^light-dark\(\s*(#[0-9a-f]{6})\s*,\s*(#[0-9a-f]{6})\s*\)$/i.exec(value);
  if (pair) return (mode === "light" ? pair[1] : pair[2]) as string;
  if (/^#[0-9a-f]{6}$/i.test(value)) return value;
  throw new Error(`token ${name}: unexpected value ${value}`);
}

const surfaces = [
  "--color-bg",
  "--color-surface",
  "--color-surface-raised",
] as const;

/** [foreground, background, minimum ratio]: 4.5 for text, 3 for UI parts and focus rings. */
const pairs: Array<[string, string, number]> = [
  ...surfaces.flatMap((s): Array<[string, string, number]> => [
    ["--color-text", s, 4.5],
    ["--color-text-muted", s, 4.5],
    ["--color-accent", s, 4.5],
    ["--color-danger", s, 4.5],
    ["--color-success", s, 4.5],
    ["--color-warning", s, 4.5],
    ["--color-border", s, 3],
    ["--color-focus", s, 3],
  ]),
  ["--color-accent-contrast", "--color-accent", 4.5],
  ["--color-text", "--color-accent-subtle", 4.5],
  ["--color-accent", "--color-accent-subtle", 4.5],
  ["--color-text", "--color-duck-soft", 4.5],
  ["--color-duck-ink", "--color-duck", 4.5],
  ["--color-duck", "--color-bg", 1],
];

describe("default theme tokens", () => {
  for (const mode of ["light", "dark"] as const) {
    describe(mode, () => {
      for (const [fg, bg, min] of pairs) {
        it(`${fg} on ${bg} is at least ${min}:1`, () => {
          expect(
            contrast(token(fg, mode), token(bg, mode)),
          ).toBeGreaterThanOrEqual(min);
        });
      }
    });
  }

  it("computes the WCAG ratio (black on white is 21:1)", () => {
    expect(contrast("#000000", "#ffffff")).toBeCloseTo(21, 5);
    expect(contrast("#777777", "#777777")).toBe(1);
  });

  it("rejects a token it cannot read", () => {
    expect(() => token("--color-missing", "light")).toThrow("missing");
    expect(() => token("--font-sans", "light")).toThrow("unexpected");
  });
});
