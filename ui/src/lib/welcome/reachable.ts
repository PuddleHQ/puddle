// SPDX-License-Identifier: AGPL-3.0-or-later
// A control inside a closed <details> is not in the accessibility tree that UI Automation reads, so the
// installed-app walk cannot find it until the person (or the walk) opens the summary. Tests use this to
// say which summary a control sits behind.

/** The text of the summary of the closed <details> around the element, or null when none is closed. */
export function closedDetailsAround(el: Element): string | null {
  const details = el.closest("details:not([open])");
  if (!details) return null;
  return details.querySelector(":scope > summary")?.textContent?.trim() ?? "";
}
