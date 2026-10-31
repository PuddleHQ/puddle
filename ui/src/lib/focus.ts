// SPDX-License-Identifier: GPL-3.0-or-later
import { tick } from "svelte";

/**
 * Puts focus somewhere sensible after the row it was on is gone: the first of `selectors` that
 * finds an element, else `fallback`. A dialog gives focus back to its trigger as it closes, so
 * this waits for the dialog to go first, and leaves focus alone when it already sits on something.
 */
export async function settleFocus(
  selectors: string[],
  fallback: HTMLElement | undefined,
): Promise<void> {
  for (let frame = 0; frame < 30; frame += 1) {
    await tick();
    if (!document.querySelector('[role="alertdialog"], [role="dialog"]')) break;
    await new Promise((resolve) => requestAnimationFrame(resolve));
  }
  await tick();
  const active = document.activeElement;
  if (active && active !== document.body && document.contains(active)) return;
  for (const selector of selectors) {
    const element = document.querySelector<HTMLElement>(selector);
    if (element) {
      element.focus();
      return;
    }
  }
  fallback?.focus();
}
