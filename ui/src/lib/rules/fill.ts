// SPDX-License-Identifier: AGPL-3.0-or-later
// The colour of a rule set (`Fill`: one solid colour today, a tagged value so a gradient can be
// another kind later) and what drawing it takes: the ink that stays readable on it, the swatches
// the picker offers and the check of a typed value. The colour is never the only carrier of
// meaning: it always sits beside the set's name.
import type { components } from "#lib/api/schema.d.ts";
import { contrast } from "#lib/theme/palette.ts";

export type Fill = components["schemas"]["Fill"];

/** The swatches the picker offers (the same colours the service hands out by default). */
export const SWATCHES: readonly string[] = [
  "#2f80c9",
  "#2ea06e",
  "#d98a1f",
  "#c04f7a",
  "#7a5bc7",
  "#2aa1a8",
  "#cf5b3c",
  "#6f8f2e",
];

const HEX = /^#[0-9a-fA-F]{6}$/;

/** `#rrggbb` in either case, nothing else: the one spelling the service accepts. */
export function isHexColour(text: string): boolean {
  return HEX.test(text);
}

/** The solid fill of a `#rrggbb` colour, written the way the service stores it. */
export function solid(hex: string): Fill {
  return { kind: "solid", color: hex.toLowerCase() };
}

/** The one colour a dot or a chip is drawn from (a gradient would give its first stop). */
export function primaryColour(fill: Fill): string {
  return fill.color;
}

/**
 * Black or white, whichever reads better on `hex`: always at least 4.5:1 (WCAG 2.2 AA for text),
 * because the larger of the two ratios against black and white is never below 4.58.
 */
export function inkOn(hex: string): "#000000" | "#ffffff" {
  return contrast(hex, "#000000") >= contrast(hex, "#ffffff")
    ? "#000000"
    : "#ffffff";
}

/** Inline custom properties for a coloured element: `--set-fill` and the ink on it. */
export function fillVars(fill: Fill): string {
  const colour = primaryColour(fill);
  return `--set-fill: ${colour}; --set-ink: ${inkOn(colour)}`;
}

/** What a screen reader hears for a colour: the swatch's hex, since it has no name. */
export function describeFill(fill: Fill): string {
  return `colour ${primaryColour(fill)}`;
}
