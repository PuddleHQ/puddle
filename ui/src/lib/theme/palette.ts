// SPDX-License-Identifier: GPL-3.0-or-later
// Palettes: the preset colour sets and the user's own, laid over the light or dark base. A palette
// sets the same custom properties tokens.css declares, on the root element, so components never
// know which one is active. Pond is tokens.css itself (a test keeps the copy below in step); the
// others are complete sets, and the custom palette starts from Pond and replaces what the user
// picked. Every pair that carries text, a control edge or a focus ring is held to WCAG 2.2 AA: the
// presets by a unit test, a custom set by a warning on the Settings screen.

export type PaletteChoice =
  "pond" | "dusk" | "meadow" | "ember" | "graphite" | "custom";
export type Preset = Exclude<PaletteChoice, "custom">;
export type Scheme = "light" | "dark";

export const PALETTE_CHOICES: readonly PaletteChoice[] = [
  "pond",
  "dusk",
  "meadow",
  "ember",
  "graphite",
  "custom",
];
export const PRESETS: readonly Preset[] = [
  "pond",
  "dusk",
  "meadow",
  "ember",
  "graphite",
];

export function isPaletteChoice(value: unknown): value is PaletteChoice {
  return (
    typeof value === "string" &&
    (PALETTE_CHOICES as readonly string[]).includes(value)
  );
}

/** The colours the user can set, named as the settings API names them (`custom_<name>`). */
export const CUSTOM_COLOURS = [
  "accent",
  "background",
  "surface",
  "text",
] as const;
export type CustomColour = (typeof CUSTOM_COLOURS)[number];
/** The user's own colours: `#rrggbb`, or null where the palette keeps Pond's. */
export type Custom = Record<CustomColour, string | null>;
export const NO_CUSTOM: Custom = {
  accent: null,
  background: null,
  surface: null,
  text: null,
};

export const TOKENS = [
  "bg",
  "surface",
  "surface-raised",
  "border",
  "border-subtle",
  "text",
  "text-muted",
  "accent",
  "accent-contrast",
  "accent-subtle",
  "danger",
  "success",
  "warning",
  "focus",
] as const;
export type Token = (typeof TOKENS)[number];
export type Tokens = Record<Token, string>;

export const HEX = /^#[0-9a-f]{6}$/i;

export const PALETTES: Record<Preset, Record<Scheme, Tokens>> = {
  pond: {
    light: {
      bg: "#f4f7f9",
      surface: "#ffffff",
      "surface-raised": "#edf2f5",
      border: "#6f8795",
      "border-subtle": "#d3dde3",
      text: "#12222b",
      "text-muted": "#4a6373",
      accent: "#0a6a85",
      "accent-contrast": "#ffffff",
      "accent-subtle": "#dceef4",
      danger: "#b42330",
      success: "#1c7a4b",
      warning: "#8a5a00",
      focus: "#0a6a85",
    },
    dark: {
      bg: "#0b171d",
      surface: "#11222a",
      "surface-raised": "#172c36",
      border: "#6a8795",
      "border-subtle": "#26404d",
      text: "#e2edf2",
      "text-muted": "#9db4c0",
      accent: "#52b6d4",
      "accent-contrast": "#03202a",
      "accent-subtle": "#133746",
      danger: "#ff8088",
      success: "#52c78f",
      warning: "#f2c063",
      focus: "#52b6d4",
    },
  },
  dusk: {
    light: {
      bg: "#f4f2f7",
      surface: "#ffffff",
      "surface-raised": "#eeecf4",
      border: "#766c93",
      "border-subtle": "#dbd7e4",
      text: "#18122b",
      "text-muted": "#4e4568",
      accent: "#2e1579",
      "accent-contrast": "#ffffff",
      "accent-subtle": "#e2ddf4",
      danger: "#b42330",
      success: "#1c7a4b",
      warning: "#8a5a00",
      focus: "#2e1579",
    },
    dark: {
      bg: "#0e0b19",
      surface: "#171226",
      "surface-raised": "#231d35",
      border: "#7b7297",
      "border-subtle": "#302749",
      text: "#e2deed",
      "text-muted": "#aba3c2",
      accent: "#a490e0",
      "accent-contrast": "#0c0425",
      "accent-subtle": "#221745",
      danger: "#ff8088",
      success: "#52c78f",
      warning: "#f2c063",
      focus: "#a490e0",
    },
  },
  meadow: {
    light: {
      bg: "#f2f7f4",
      surface: "#ffffff",
      "surface-raised": "#ecf4ee",
      border: "#6c9379",
      "border-subtle": "#d7e4dc",
      text: "#122b1b",
      "text-muted": "#456851",
      accent: "#127d3e",
      "accent-contrast": "#ffffff",
      "accent-subtle": "#ddf4e6",
      danger: "#b42330",
      success: "#1c7a4b",
      warning: "#8a5a00",
      focus: "#127d3e",
    },
    dark: {
      bg: "#0b190f",
      surface: "#122619",
      "surface-raised": "#1d3525",
      border: "#72977e",
      "border-subtle": "#274932",
      text: "#deede3",
      "text-muted": "#a3c2ad",
      accent: "#64d894",
      "accent-contrast": "#042512",
      "accent-subtle": "#17452a",
      danger: "#ff8088",
      success: "#52c78f",
      warning: "#f2c063",
      focus: "#64d894",
    },
  },
  ember: {
    light: {
      bg: "#f7f4f2",
      surface: "#ffffff",
      "surface-raised": "#f4efec",
      border: "#937a6c",
      "border-subtle": "#e4dcd7",
      text: "#2b1b12",
      "text-muted": "#685245",
      accent: "#842f0b",
      "accent-contrast": "#ffffff",
      "accent-subtle": "#f4e3dd",
      danger: "#b42330",
      success: "#1c7a4b",
      warning: "#8a5a00",
      focus: "#842f0b",
    },
    dark: {
      bg: "#19100b",
      surface: "#261912",
      "surface-raised": "#35261d",
      border: "#978072",
      "border-subtle": "#493427",
      text: "#ede3de",
      "text-muted": "#c2aea3",
      accent: "#e0845c",
      "accent-contrast": "#250e04",
      "accent-subtle": "#452517",
      danger: "#ff8088",
      success: "#52c78f",
      warning: "#f2c063",
      focus: "#e0845c",
    },
  },
  graphite: {
    light: {
      bg: "#f4f5f6",
      surface: "#ffffff",
      "surface-raised": "#eff0f1",
      border: "#7a7f85",
      "border-subtle": "#dcdee0",
      text: "#1b1e22",
      "text-muted": "#51565c",
      accent: "#154479",
      "accent-contrast": "#ffffff",
      "accent-subtle": "#e5e8eb",
      danger: "#b42330",
      success: "#1c7a4b",
      warning: "#8a5a00",
      focus: "#154479",
    },
    dark: {
      bg: "#101114",
      surface: "#191c1f",
      "surface-raised": "#25282c",
      border: "#7f848a",
      "border-subtle": "#33373d",
      text: "#e3e5e8",
      "text-muted": "#aeb2b7",
      accent: "#689ad4",
      "accent-contrast": "#041325",
      "accent-subtle": "#272d35",
      danger: "#ff8088",
      success: "#52c78f",
      warning: "#f2c063",
      focus: "#689ad4",
    },
  },
};

// --- colour arithmetic -------------------------------------------------------------------------

function channels(hex: string): [number, number, number] {
  return [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16)) as [
    number,
    number,
    number,
  ];
}

function luminance(hex: string): number {
  const [r, g, b] = channels(hex).map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** The WCAG contrast ratio of two `#rrggbb` colours, from 1 to 21. */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [
    number,
    number,
  ];
  return (hi + 0.05) / (lo + 0.05);
}

/** `a` moved `share` (0..1) of the way to `b`, channel by channel. */
export function mix(a: string, b: string, share: number): string {
  const from = channels(a);
  const to = channels(b);
  const hex = from.map((v, i) =>
    Math.round(v + ((to[i] as number) - v) * share)
      .toString(16)
      .padStart(2, "0"),
  );
  return `#${hex.join("")}`;
}

const WHITE = "#ffffff";
const BLACK = "#000000";

// --- what a palette resolves to ----------------------------------------------------------------

/** The tokens of a palette in one scheme; a custom set is Pond with the user's colours laid over it. */
export function resolve(
  choice: PaletteChoice,
  custom: Custom,
  scheme: Scheme,
): Tokens {
  if (choice !== "custom") return { ...PALETTES[choice][scheme] };
  const t = { ...PALETTES.pond[scheme] };
  const { accent, background, surface, text } = custom;
  if (background) t.bg = background;
  if (surface) t.surface = surface;
  if (text) t.text = text;
  if (accent) t.accent = accent;
  // What follows from the picked colours is derived from them, so one pick keeps the set together.
  if (accent) {
    t.focus = accent;
    t["accent-contrast"] =
      contrast(accent, WHITE) >= contrast(accent, BLACK) ? WHITE : BLACK;
  }
  if (accent || background) t["accent-subtle"] = mix(t.bg, t.accent, 0.14);
  if (background || surface || text) {
    t["surface-raised"] = mix(t.surface, t.text, 0.05);
    t["border-subtle"] = mix(t.bg, t.text, 0.16);
    t.border = mix(t.bg, t.text, 0.55);
    t["text-muted"] = mix(t.text, t.bg, 0.28);
  }
  return t;
}

// --- contrast ----------------------------------------------------------------------------------

const NAMES: Record<Token, string> = {
  bg: "the background",
  surface: "cards",
  "surface-raised": "raised panels",
  border: "borders",
  "border-subtle": "dividers",
  text: "text",
  "text-muted": "muted text",
  accent: "the accent",
  "accent-contrast": "text on the accent",
  "accent-subtle": "the accent's tint",
  danger: "error text",
  success: "success text",
  warning: "warning text",
  focus: "the focus ring",
};

const SURFACES = ["bg", "surface", "surface-raised"] as const;

/** [foreground, background, least ratio]: 4.5 for text, 3 for control edges and the focus ring. */
export const PAIRS: ReadonlyArray<readonly [Token, Token, number]> = [
  ...SURFACES.flatMap((s): Array<readonly [Token, Token, number]> => [
    ["text", s, 4.5],
    ["text-muted", s, 4.5],
    ["accent", s, 4.5],
    ["danger", s, 4.5],
    ["success", s, 4.5],
    ["warning", s, 4.5],
    ["border", s, 3],
    ["focus", s, 3],
  ]),
  ["accent-contrast", "accent", 4.5],
  ["text", "accent-subtle", 4.5],
  ["accent", "accent-subtle", 4.5],
];

export interface Failure {
  scheme: Scheme;
  /** A sentence fragment: "text on cards". */
  pair: string;
  ratio: number;
  least: number;
}

/** The pairs of a palette that fall short of AA, in the schemes the theme choice can show. */
export function failures(
  choice: PaletteChoice,
  custom: Custom,
  schemes: readonly Scheme[],
): Failure[] {
  return schemes.flatMap((scheme) => {
    const t = resolve(choice, custom, scheme);
    return PAIRS.flatMap(([fg, bg, least]) => {
      const ratio = contrast(t[fg], t[bg]);
      return ratio < least
        ? [{ scheme, pair: `${NAMES[fg]} on ${NAMES[bg]}`, ratio, least }]
        : [];
    });
  });
}

/** One line a person can act on, e.g. "Text on cards has 3.1:1 in the dark theme; it needs 4.5:1." */
export function describeFailure(f: Failure): string {
  const first = f.pair.charAt(0).toUpperCase() + f.pair.slice(1);
  return `${first} has ${f.ratio.toFixed(1)}:1 in the ${f.scheme} theme; it needs ${f.least}:1.`;
}

/** The schemes a theme choice can show; the system's own is whichever the computer asks for. */
export function schemesOf(theme: "system" | "light" | "dark"): Scheme[] {
  return theme === "system" ? ["light", "dark"] : [theme];
}

/** The scheme the window shows now: a fixed theme's own, else the one the computer asks for. */
export function activeScheme(theme: "system" | "light" | "dark"): Scheme {
  if (theme !== "system") return theme;
  return globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

// --- applying ----------------------------------------------------------------------------------

/** Sets the palette's tokens on the root element; Pond clears them, so tokens.css shows. */
export function applyPalette(
  root: HTMLElement,
  choice: PaletteChoice,
  custom: Custom,
): void {
  if (choice === "pond") {
    delete root.dataset["palette"];
    for (const token of TOKENS) root.style.removeProperty(`--color-${token}`);
    return;
  }
  root.dataset["palette"] = choice;
  const light = resolve(choice, custom, "light");
  const dark = resolve(choice, custom, "dark");
  for (const token of TOKENS) {
    root.style.setProperty(
      `--color-${token}`,
      `light-dark(${light[token]}, ${dark[token]})`,
    );
  }
}

// --- the per-viewer hint -----------------------------------------------------------------------

export const PALETTE_KEY = "puddle.palette";

export interface PaletteState {
  choice: PaletteChoice;
  custom: Custom;
}

type StorageLike = Pick<Storage, "getItem" | "setItem">;

/** Reads colours from untrusted data: a value that is not `#rrggbb` counts as unset. */
export function customFrom(data: unknown): Custom {
  const source = (data ?? {}) as Record<string, unknown>;
  const pick = (key: CustomColour) => {
    const value = source[key];
    return typeof value === "string" && HEX.test(value)
      ? value.toLowerCase()
      : null;
  };
  return {
    accent: pick("accent"),
    background: pick("background"),
    surface: pick("surface"),
    text: pick("text"),
  };
}

export function loadPalette(storage: StorageLike | undefined): PaletteState {
  try {
    const stored = storage?.getItem(PALETTE_KEY);
    const data = stored ? (JSON.parse(stored) as Record<string, unknown>) : {};
    const choice = data["choice"];
    return {
      choice: isPaletteChoice(choice) ? choice : "pond",
      custom: customFrom(data["custom"]),
    };
  } catch {
    return { choice: "pond", custom: { ...NO_CUSTOM } };
  }
}

export function savePalette(
  storage: StorageLike | undefined,
  state: PaletteState,
): void {
  try {
    storage?.setItem(PALETTE_KEY, JSON.stringify(state));
  } catch {
    // Not persisted; the palette still applies for this page.
  }
}
