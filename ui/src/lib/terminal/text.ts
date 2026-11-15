// SPDX-License-Identifier: AGPL-3.0-or-later
// Text that comes from the workspace (a window title, an end reason) and the lines Puddle says
// when a terminal is over.
import type { Ended } from "./wire.ts";

// Control characters, bidi controls and zero-width characters: they can reorder or hide text.
const HIDDEN_RANGES: readonly (readonly [number, number])[] = [
  [0x061c, 0x061c],
  [0x200b, 0x200f],
  [0x202a, 0x202e],
  [0x2060, 0x2064],
  [0x2066, 0x2069],
  [0xfeff, 0xfeff],
];

function hidden(char: string): boolean {
  const code = char.codePointAt(0) ?? 0;
  return (
    /\p{Cc}/u.test(char) ||
    HIDDEN_RANGES.some(([from, to]) => code >= from && code <= to)
  );
}

/** `text` without control, bidi and zero-width characters. */
export function cleanText(text: string): string {
  return Array.from(text)
    .filter((char) => !hidden(char))
    .join("");
}

export const TITLE_LIMIT = 40;

/** A window title a program set, as a sub-tab label: cleaned, trimmed and cut at 40 characters. `""` when nothing is left. */
export function cleanTitle(title: string): string {
  return Array.from(cleanText(title).trim()).slice(0, TITLE_LIMIT).join("");
}

/** What the end line offers. */
export type EndAction = "restart" | "start" | "overview" | "new";

export interface EndLine {
  line: string;
  /** More text the guest or the host gave, cleaned; drawn as text. */
  detail: string | null;
  action: EndAction;
}

/** The terminal's connection broke or Puddle is quitting: no `ended` message came. */
export const BROKE: EndLine = {
  line: "The connection to Puddle broke; the terminal ended.",
  detail: null,
  action: "new",
};

/** The line under an ended terminal, by cause. */
export function endLine(end: Ended): EndLine {
  const detail = end.message === null ? null : cleanText(end.message);
  switch (end.reason) {
    case "exited":
      return {
        line:
          end.status === null
            ? "The shell exited."
            : `The shell exited with status ${end.status}.`,
        detail: null,
        action: "restart",
      };
    case "stopped":
      return {
        line: "The workspace stopped; the terminal ended.",
        detail: null,
        action: "start",
      };
    case "crashed":
      return {
        line: "The workspace crashed; the terminal ended.",
        detail,
        action: "start",
      };
    case "boot_failed":
      return {
        line: detail ?? "The workspace did not finish starting.",
        detail: null,
        action: "overview",
      };
    case "quitting":
      return BROKE;
  }
}
