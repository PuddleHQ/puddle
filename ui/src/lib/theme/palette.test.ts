// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import {
  activeScheme,
  applyPalette,
  contrast,
  customFrom,
  describeFailure,
  failures,
  isPaletteChoice,
  loadPalette,
  mix,
  NO_CUSTOM,
  PALETTES,
  PALETTE_KEY,
  PAIRS,
  PRESETS,
  resolve,
  savePalette,
  schemesOf,
  TOKENS,
  type Custom,
} from "./palette.ts";

const css = readFileSync(join(import.meta.dirname, "tokens.css"), "utf8");

/** The two colours of `--color-<token>: light-dark(a, b)` in tokens.css. */
function cssPair(token: string): [string, string] {
  const rule = new RegExp(`--color-${token}:\\s*([^;]+);`).exec(css);
  const pair = /light-dark\(\s*(#[0-9a-f]{6})\s*,\s*(#[0-9a-f]{6})\s*\)/i.exec(
    rule?.[1] ?? "",
  );
  if (!pair) throw new Error(`token ${token} is not a light-dark pair`);
  return [pair[1] as string, pair[2] as string];
}

const custom = (over: Partial<Custom>): Custom => ({ ...NO_CUSTOM, ...over });

describe("the preset palettes", () => {
  it("Pond is the colours of tokens.css", () => {
    for (const token of TOKENS) {
      const [light, dark] = cssPair(token);
      expect(PALETTES.pond.light[token], token).toBe(light);
      expect(PALETTES.pond.dark[token], token).toBe(dark);
    }
  });

  for (const preset of PRESETS) {
    for (const scheme of ["light", "dark"] as const) {
      it(`${preset} in ${scheme} passes AA on every pair`, () => {
        const tokens = PALETTES[preset][scheme];
        for (const [fg, bg, least] of PAIRS) {
          expect(
            contrast(tokens[fg], tokens[bg]),
            `${fg} on ${bg}`,
          ).toBeGreaterThanOrEqual(least);
        }
        expect(failures(preset, NO_CUSTOM, [scheme])).toEqual([]);
      });
    }
  }

  it("each preset is a different look", () => {
    const accents = PRESETS.map((p) => PALETTES[p].light.accent);
    expect(new Set(accents).size).toBe(PRESETS.length);
  });
});

describe("a custom palette", () => {
  it("with nothing picked is Pond", () => {
    for (const scheme of ["light", "dark"] as const) {
      expect(resolve("custom", NO_CUSTOM, scheme)).toEqual(
        PALETTES.pond[scheme],
      );
    }
  });

  it("derives what follows from the accent and leaves the rest as Pond has it", () => {
    const t = resolve("custom", custom({ accent: "#7a1fa2" }), "light");
    expect(t.accent).toBe("#7a1fa2");
    expect(t.focus).toBe("#7a1fa2");
    expect(t["accent-contrast"]).toBe("#ffffff");
    expect(t["accent-subtle"]).not.toBe(PALETTES.pond.light["accent-subtle"]);
    expect(t.bg).toBe(PALETTES.pond.light.bg);
    expect(t.border).toBe(PALETTES.pond.light.border);
  });

  it("picks black text on a light accent", () => {
    expect(
      resolve("custom", custom({ accent: "#ffd84d" }), "dark")[
        "accent-contrast"
      ],
    ).toBe("#000000");
  });

  it("derives the neutral tokens when the background, surface or text is picked", () => {
    const t = resolve(
      "custom",
      custom({ background: "#101820", text: "#f0f4f8", surface: "#18222c" }),
      "dark",
    );
    expect(t.bg).toBe("#101820");
    expect(t.surface).toBe("#18222c");
    expect(t["surface-raised"]).toBe(mix("#18222c", "#f0f4f8", 0.05));
    expect(t["text-muted"]).toBe(mix("#f0f4f8", "#101820", 0.28));
    expect(
      failures(
        "custom",
        custom({ background: "#101820", text: "#f0f4f8", surface: "#18222c" }),
        ["dark"],
      ),
    ).toEqual([]);
  });

  it("names the pairs that fail, per scheme, and says how far", () => {
    // Pale text on Pond's light surfaces, in both schemes.
    const found = failures("custom", custom({ text: "#cccccc" }), ["light"]);
    expect(found.length).toBeGreaterThan(0);
    const first = found.find((f) => f.pair === "text on cards");
    expect(first).toBeDefined();
    expect(describeFailure(first!)).toMatch(
      /^Text on cards has \d\.\d:1 in the light theme; it needs 4\.5:1\.$/,
    );
    expect(failures("custom", custom({ text: "#cccccc" }), [])).toEqual([]);
  });

  it("reports a failing accent against the surfaces it sits on", () => {
    const found = failures("custom", custom({ accent: "#cfe3ea" }), ["light"]);
    expect(found.map((f) => f.pair)).toContain("the accent on the background");
  });
});

describe("colour arithmetic and schemes", () => {
  it("mixes channel by channel", () => {
    expect(mix("#000000", "#ffffff", 0.5)).toBe("#808080");
    expect(mix("#102030", "#102030", 0.7)).toBe("#102030");
  });

  it("shows both schemes for the system theme and one for a fixed theme", () => {
    expect(schemesOf("system")).toEqual(["light", "dark"]);
    expect(schemesOf("dark")).toEqual(["dark"]);
  });

  it("takes the computer's scheme for the system theme", () => {
    const media = (dark: boolean) =>
      vi.stubGlobal("matchMedia", () => ({ matches: dark }));
    media(true);
    expect(activeScheme("system")).toBe("dark");
    media(false);
    expect(activeScheme("system")).toBe("light");
    expect(activeScheme("dark")).toBe("dark");
    vi.unstubAllGlobals();
  });

  it("knows its palette names", () => {
    expect(isPaletteChoice("custom")).toBe(true);
    expect(isPaletteChoice("neon")).toBe(false);
    expect(isPaletteChoice(3)).toBe(false);
  });
});

describe("applying a palette", () => {
  const root = () => document.createElement("html");

  it("sets every token as a light-dark pair and marks the root", () => {
    const el = root();
    applyPalette(el, "meadow", NO_CUSTOM);
    expect(el.dataset["palette"]).toBe("meadow");
    for (const token of TOKENS) {
      expect(el.style.getPropertyValue(`--color-${token}`)).toBe(
        `light-dark(${PALETTES.meadow.light[token]}, ${PALETTES.meadow.dark[token]})`,
      );
    }
  });

  it("clears them for Pond, so the stylesheet shows", () => {
    const el = root();
    applyPalette(el, "ember", NO_CUSTOM);
    applyPalette(el, "pond", NO_CUSTOM);
    expect(el.dataset["palette"]).toBeUndefined();
    expect(el.style.getPropertyValue("--color-accent")).toBe("");
  });

  it("applies the user's colours", () => {
    const el = root();
    applyPalette(el, "custom", custom({ accent: "#7a1fa2" }));
    expect(el.style.getPropertyValue("--color-accent")).toContain("#7a1fa2");
  });
});

describe("the hint kept in the page", () => {
  const store = () => {
    const data = new Map<string, string>();
    return {
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
    };
  };

  it("round-trips a choice and colours", () => {
    const s = store();
    savePalette(s, { choice: "custom", custom: custom({ text: "#102030" }) });
    expect(loadPalette(s)).toEqual({
      choice: "custom",
      custom: custom({ text: "#102030" }),
    });
  });

  it("falls back to Pond on nothing, damage or an unusable store", () => {
    expect(loadPalette(undefined).choice).toBe("pond");
    const s = store();
    expect(loadPalette(s).choice).toBe("pond");
    s.setItem(PALETTE_KEY, "{not json");
    expect(loadPalette(s)).toEqual({ choice: "pond", custom: NO_CUSTOM });
    s.setItem(PALETTE_KEY, JSON.stringify({ choice: "neon", custom: 4 }));
    expect(loadPalette(s)).toEqual({ choice: "pond", custom: NO_CUSTOM });
    const broken = {
      getItem: () => {
        throw new Error("no storage");
      },
      setItem: () => {
        throw new Error("no storage");
      },
    };
    expect(loadPalette(broken).choice).toBe("pond");
    expect(() =>
      savePalette(broken, { choice: "pond", custom: NO_CUSTOM }),
    ).not.toThrow();
  });

  it("counts only #rrggbb as a colour, in lower case", () => {
    expect(
      customFrom({
        accent: "#ABCDEF",
        text: "red",
        surface: 4,
        background: "#12345",
      }),
    ).toEqual(custom({ accent: "#abcdef" }));
    expect(customFrom(null)).toEqual(NO_CUSTOM);
  });
});
