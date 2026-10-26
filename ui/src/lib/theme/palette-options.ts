// SPDX-License-Identifier: GPL-3.0-or-later
// The palettes as people choose them: a name and a line about each.
import { PALETTE_CHOICES, type PaletteChoice } from "./palette.ts";

export interface PaletteOption {
  value: PaletteChoice;
  label: string;
  hint: string;
}

export const PALETTE_INFO: Record<
  PaletteChoice,
  { label: string; hint: string }
> = {
  pond: {
    label: "Pond",
    hint: "Calm water blues, with duck yellow for what needs you.",
  },
  dusk: { label: "Dusk", hint: "Quiet violets." },
  meadow: { label: "Meadow", hint: "Soft greens." },
  ember: { label: "Ember", hint: "Warm reds and ambers." },
  graphite: { label: "Graphite", hint: "Neutral greys with a blue accent." },
  custom: {
    label: "Custom",
    hint: "Your own accent, background, panels and text.",
  },
};

export const PALETTE_OPTIONS: readonly PaletteOption[] = PALETTE_CHOICES.map(
  (value) => ({ value, ...PALETTE_INFO[value] }),
);
