// SPDX-License-Identifier: GPL-3.0-or-later
// The never-decrypt names as one editable text: a name per line, or separated by commas or
// spaces. The API normalises and judges each name; this only splits and removes repeats.

/** Splits typed text into names: lower case, no empties, no repeats, in the order typed. */
export function parseNames(text: string): string[] {
  const seen = new Set<string>();
  for (const part of text.split(/[\s,;]+/)) {
    const name = part.trim().toLowerCase();
    if (name) seen.add(name);
  }
  return [...seen];
}

/** The names as the text a person edits. */
export function formatNames(names: readonly string[]): string {
  return names.join("\n");
}

/** Whether saving would change nothing. */
export function sameNames(a: readonly string[], b: readonly string[]): boolean {
  const left = [...a].sort();
  const right = [...b].sort();
  return left.length === right.length && left.every((n, i) => n === right[i]);
}
