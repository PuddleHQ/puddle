// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  editorOf,
  extensionsText,
  parseExtensions,
  parseFontSize,
  parseKeybindings,
  parseTheme,
} from "./editor.ts";

describe("the colour theme", () => {
  it("trims, treats empty as unset and refuses a name over 100 characters", () => {
    expect(parseTheme("  Solarized Dark ")).toEqual({
      ok: true,
      value: "Solarized Dark",
    });
    expect(parseTheme("  ")).toEqual({ ok: true, value: null });
    expect(parseTheme("x".repeat(100)).ok).toBe(true);
    expect(parseTheme("x".repeat(101)).ok).toBe(false);
    expect(parseTheme("Dark\u0007").ok).toBe(false);
  });
});

describe("the font size", () => {
  it("takes a whole number from 8 to 40 and refuses the rest with the range", () => {
    expect(parseFontSize("14")).toEqual({ ok: true, value: 14 });
    expect(parseFontSize("8").ok && parseFontSize("40").ok).toBe(true);
    expect(parseFontSize("")).toEqual({ ok: true, value: null });
    for (const text of ["7", "41", "12.5", "-3", "big"]) {
      const parsed = parseFontSize(text);
      expect(parsed.ok, text).toBe(false);
      if (!parsed.ok) expect(parsed.message).toContain("8 to 40");
    }
  });
});

describe("the extension list", () => {
  it("lower-cases, sorts and drops repeats, from lines, commas or spaces", () => {
    expect(
      parseExtensions(
        "Rust-Lang.Rust-Analyzer, esbenp.prettier-vscode\nrust-lang.rust-analyzer",
      ),
    ).toEqual({
      ok: true,
      value: ["esbenp.prettier-vscode", "rust-lang.rust-analyzer"],
    });
    expect(parseExtensions("")).toEqual({ ok: true, value: [] });
  });

  it("names the id that is not publisher.name", () => {
    for (const text of ["nopublisher", "a.b --install", ".name", "pub."]) {
      const parsed = parseExtensions(text);
      expect(parsed.ok, text).toBe(false);
    }
    const parsed = parseExtensions("good.one nopublisher");
    expect(!parsed.ok && parsed.message).toContain("nopublisher");
  });

  it("refuses more than 50 ids", () => {
    const many = Array.from({ length: 51 }, (_, i) => `p.e${i}`).join(" ");
    expect(parseExtensions(many).ok).toBe(false);
  });

  it("shows a list one id per line", () => {
    expect(extensionsText(["a.b", "c.d"])).toBe("a.b\nc.d");
    expect(extensionsText(null)).toBe("");
  });
});

describe("the key bindings", () => {
  it("accepts a JSON array of key and command objects and keeps the text as typed", () => {
    const text = '[{"key":"ctrl+k","command":"a.b","when":"editorFocus"}]';
    expect(parseKeybindings(` ${text} `)).toEqual({ ok: true, value: text });
    expect(parseKeybindings("")).toEqual({ ok: true, value: null });
    expect(parseKeybindings("[]")).toEqual({ ok: true, value: "[]" });
  });

  it("refuses what the API refuses", () => {
    for (const text of [
      "{",
      "{}",
      '[{"key":"a"}]',
      '[{"key":"a","command":" "}]',
      "[1]",
    ]) {
      expect(parseKeybindings(text).ok, text).toBe(false);
    }
    expect(parseKeybindings(`["${"x".repeat(70_000)}"]`).ok).toBe(false);
  });
});

describe("a stored layer", () => {
  it("is read as unset when it predates the editor group", () => {
    expect(editorOf({})).toEqual({
      colour_theme: null,
      font_size: null,
      extensions: null,
      keybindings: null,
    });
    expect(
      editorOf({
        editor: {
          colour_theme: "X",
          font_size: null,
          extensions: null,
          keybindings: null,
        },
      }).colour_theme,
    ).toBe("X");
  });
});
