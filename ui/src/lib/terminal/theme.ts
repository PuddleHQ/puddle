// SPDX-License-Identifier: AGPL-3.0-or-later
// The emulator's colours and font, read from the UI's tokens, so both themes and every
// palette reach the terminal. The emulator itself raises any foreground below 4.5:1.
import type { ITheme } from "@xterm/xterm";

interface Rgb {
  r: number;
  g: number;
  b: number;
}

/** `rgb(...)` or `rgba(...)` as a browser reports a computed colour; `null` for anything else. */
export function parseRgb(text: string): Rgb | null {
  const m =
    /^rgba?\(\s*(\d+(?:\.\d+)?)[ ,]+(\d+(?:\.\d+)?)[ ,]+(\d+(?:\.\d+)?)/.exec(
      text.trim(),
    );
  if (!m) return null;
  return { r: Number(m[1]), g: Number(m[2]), b: Number(m[3]) };
}

const hex = (c: Rgb): string =>
  `#${[c.r, c.g, c.b]
    .map((v) => Math.round(v).toString(16).padStart(2, "0"))
    .join("")}`;

/** Whether a background is dark, by its relative luminance. */
export const isDark = (c: Rgb): boolean =>
  (0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b) / 255 < 0.5;

// The ANSI colours the tokens do not carry, one set per scheme. The emulator lifts any that is too
// dim for its background.
const LIGHT = {
  black: "#12222b",
  blue: "#1f5fbf",
  magenta: "#8a3f9e",
  white: "#d3dde3",
  brightBlack: "#4a6373",
  brightWhite: "#ffffff",
};
const DARK = {
  black: "#26404d",
  blue: "#6aa4ff",
  magenta: "#d58cf0",
  white: "#c5d6df",
  brightBlack: "#9db4c0",
  brightWhite: "#ffffff",
};

const FALLBACK_BG = "#0b171d";
const FALLBACK_FG = "#e2edf2";

export interface TerminalLook {
  theme: ITheme;
  fontFamily: string;
}

/** Reads the look from the tokens as `probe` (an element in the page) resolves them now. */
export function readLook(probe: HTMLElement): TerminalLook {
  const style = globalThis.getComputedStyle(probe);
  const color = (token: string, fallback: string): Rgb | string => {
    probe.style.color = "";
    probe.style.color = `var(${token})`;
    return parseRgb(style.color) ?? fallback;
  };
  const css = (value: Rgb | string): string =>
    typeof value === "string" ? value : hex(value);
  const bg = color("--color-surface", FALLBACK_BG);
  const dark = typeof bg === "string" ? true : isDark(bg);
  const base = dark ? DARK : LIGHT;
  const fg = color("--color-text", FALLBACK_FG);
  const accent = color("--color-accent", "#52b6d4");
  const red = color("--color-danger", "#ff8088");
  const green = color("--color-success", "#52c78f");
  const yellow = color("--color-warning", "#f2c063");
  const selection = color("--color-accent-subtle", "#133746");
  probe.style.color = "";
  probe.style.fontFamily = "var(--font-mono)";
  const fontFamily = style.fontFamily || "monospace";
  probe.style.fontFamily = "";
  return {
    fontFamily,
    theme: {
      background: css(bg),
      foreground: css(fg),
      cursor: css(accent),
      cursorAccent: css(bg),
      selectionBackground: css(selection),
      selectionForeground: css(fg),
      black: base.black,
      red: css(red),
      green: css(green),
      yellow: css(yellow),
      blue: base.blue,
      magenta: base.magenta,
      cyan: css(accent),
      white: base.white,
      brightBlack: base.brightBlack,
      brightRed: css(red),
      brightGreen: css(green),
      brightYellow: css(yellow),
      brightBlue: base.blue,
      brightMagenta: base.magenta,
      brightCyan: css(accent),
      brightWhite: base.brightWhite,
    },
  };
}
