// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { keyAction, type KeyLike } from "./keys.ts";

const key = (key: string, mods: Partial<KeyLike> = {}): KeyLike => ({
  key,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...mods,
});

describe("keyAction", () => {
  it("keeps the terminal's own shortcuts", () => {
    expect(keyAction(key("C", { ctrlKey: true, shiftKey: true }))).toBe("copy");
    expect(keyAction(key("V", { ctrlKey: true, shiftKey: true }))).toBe(
      "paste",
    );
    expect(keyAction(key("Insert", { shiftKey: true }))).toBe("paste");
    expect(keyAction(key("F6"))).toBe("leave");
  });

  it("hands Ctrl+C, Ctrl+V, Ctrl+R, Ctrl+W, Ctrl+P, F5, F7 and Tab to the shell", () => {
    for (const k of ["c", "v", "r", "w", "p"]) {
      expect(keyAction(key(k, { ctrlKey: true }))).toBe("shell-cancel");
    }
    expect(keyAction(key("F5"))).toBe("shell-cancel");
    expect(keyAction(key("F7"))).toBe("shell-cancel");
    expect(keyAction(key("Tab"))).toBe("shell");
    expect(keyAction(key("a"))).toBe("shell");
    expect(keyAction(key("Insert"))).toBe("shell");
  });

  it("does not cancel a character typed with AltGr", () => {
    expect(
      keyAction(key("@", { ctrlKey: true, altKey: true, altGraph: true })),
    ).toBe("shell");
  });

  it("cancels Alt and Meta combinations and Ctrl+Shift others, and F6 with a modifier goes to the shell", () => {
    expect(keyAction(key("b", { altKey: true }))).toBe("shell-cancel");
    expect(keyAction(key("t", { metaKey: true }))).toBe("shell-cancel");
    expect(keyAction(key("i", { ctrlKey: true, shiftKey: true }))).toBe(
      "shell-cancel",
    );
    expect(keyAction(key("F6", { shiftKey: true }))).toBe("shell-cancel");
  });
});
