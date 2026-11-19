// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect } from "vitest";

/**
 * Fails when a generated id is something a person can read or hear in `root`: its text, or an
 * attribute that is spoken or shown (aria-label, title, placeholder, alt, an option's label). A
 * value or href that carries the id for the program is fine; the id may stay behind those.
 */
const SHOWN = ["aria-label", "title", "placeholder", "alt"] as const;

export function expectNoIdsShown(root: HTMLElement, ids: readonly string[]) {
  const shown: string[] = [root.textContent ?? ""];
  for (const el of root.querySelectorAll("*")) {
    for (const attribute of SHOWN) {
      const value = el.getAttribute(attribute);
      if (value) shown.push(value);
    }
  }
  const text = shown.join("\n");
  for (const id of ids) {
    expect(text, `the id ${id} is shown to the person`).not.toContain(id);
  }
}
