// SPDX-License-Identifier: GPL-3.0-or-later
// The options of the "Look" step, with the words that go with them.
import type { DensityChoice } from "#lib/theme/density.ts";
import { PALETTES, PRESETS, type Preset } from "#lib/theme/palette.ts";
import type { ThemeChoice } from "#lib/theme/theme.ts";
import { PALETTE_INFO } from "#lib/theme/palette-options.ts";

export interface Option<T extends string> {
  value: T;
  label: string;
  hint: string;
}

export const THEME_OPTIONS: readonly Option<ThemeChoice>[] = [
  {
    value: "system",
    label: "Follow my system",
    hint: "Light or dark as your computer is set.",
  },
  { value: "light", label: "Light", hint: "Always light." },
  { value: "dark", label: "Dark", hint: "Always dark." },
];

export const DENSITY_OPTIONS: readonly Option<DensityChoice>[] = [
  {
    value: "comfortable",
    label: "Comfortable",
    hint: "More room around things.",
  },
  {
    value: "compact",
    label: "Compact",
    hint: "Tighter spacing, so more fits on screen. Buttons stay easy to hit.",
  },
];

/** The presets the step offers (Custom is for Settings), each with the colours its swatches show. */
export const PALETTE_PRESET_OPTIONS: readonly (Option<Preset> & {
  swatch: readonly [string, string][];
})[] = PRESETS.map((value) => {
  const info = PALETTE_INFO[value];
  const pair = (
    token: "bg" | "surface" | "accent" | "text",
  ): [string, string] => [
    PALETTES[value].light[token],
    PALETTES[value].dark[token],
  ];
  return {
    value,
    ...info,
    swatch: [pair("bg"), pair("surface"), pair("accent"), pair("text")],
  };
});
