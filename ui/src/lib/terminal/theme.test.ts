// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from "vitest";
import { isDark, parseRgb, readLook } from "./theme.ts";

afterEach(() => vi.restoreAllMocks());

describe("parseRgb", () => {
  it("reads the forms a browser reports", () => {
    expect(parseRgb("rgb(10, 20, 30)")).toEqual({ r: 10, g: 20, b: 30 });
    expect(parseRgb("rgba(10 20 30 / 0.5)")).toEqual({ r: 10, g: 20, b: 30 });
    expect(parseRgb("var(--x)")).toBeNull();
    expect(parseRgb("")).toBeNull();
  });
});

describe("isDark", () => {
  it("goes by luminance", () => {
    expect(isDark({ r: 11, g: 23, b: 29 })).toBe(true);
    expect(isDark({ r: 244, g: 247, b: 249 })).toBe(false);
  });
});

/** A probe whose computed colour is what `colors` says for the token it was last given. */
function probeWith(colors: Record<string, string>, family = "JetBrains Mono") {
  const probe = document.createElement("span");
  vi.spyOn(globalThis, "getComputedStyle").mockImplementation(
    () =>
      ({
        get color() {
          const token = /var\((--[a-z-]+)\)/.exec(probe.style.color)?.[1] ?? "";
          return colors[token] ?? "";
        },
        get fontFamily() {
          return probe.style.fontFamily === "var(--font-mono)" ? family : "";
        },
      }) as CSSStyleDeclaration,
  );
  return probe;
}

describe("readLook", () => {
  it("takes the colours and the font from the tokens", () => {
    const probe = probeWith({
      "--color-surface": "rgb(255, 255, 255)",
      "--color-text": "rgb(18, 34, 43)",
      "--color-accent": "rgb(10, 106, 133)",
      "--color-danger": "rgb(180, 35, 48)",
      "--color-success": "rgb(28, 122, 75)",
      "--color-warning": "rgb(138, 90, 0)",
      "--color-accent-subtle": "rgb(220, 238, 244)",
    });
    const { theme, fontFamily } = readLook(probe);
    expect(theme.background).toBe("#ffffff");
    expect(theme.foreground).toBe("#12222b");
    expect(theme.cursor).toBe("#0a6a85");
    expect(theme.red).toBe("#b42330");
    expect(theme.black).toBe("#12222b");
    expect(fontFamily).toBe("JetBrains Mono");
    expect(probe.style.color).toBe("");
  });

  it("uses the dark set on a dark background", () => {
    const { theme } = readLook(
      probeWith({ "--color-surface": "rgb(17, 34, 42)" }),
    );
    expect(theme.background).toBe("#11222a");
    expect(theme.black).toBe("#26404d");
  });

  it("falls back to a dark look and a plain monospace font when the tokens do not resolve", () => {
    const { theme, fontFamily } = readLook(probeWith({}, ""));
    expect(theme.background).toBe("#0b171d");
    expect(theme.foreground).toBe("#e2edf2");
    expect(fontFamily).toBe("monospace");
  });
});
