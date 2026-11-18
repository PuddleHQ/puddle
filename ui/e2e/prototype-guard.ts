// SPDX-License-Identifier: AGPL-3.0-or-later
// The script the desktop app's main window runs before the page's own scripts: it locks
// `Object.prototype`. Every signed-in page in these tests runs under it too, so a page that only
// breaks in the app (a library that assigns `toString` on a plain object did) fails here first.
import type { Page } from "@playwright/test";
import { resolve } from "node:path";

export const PROTOTYPE_GUARD = resolve(
  import.meta.dirname,
  "../../crates/puddle-app/src/window/prototype_guard.js",
);

/** Runs the app's prototype guard in `page` before any of its scripts. */
export async function guardPrototype(page: Page): Promise<void> {
  await page.addInitScript({ path: PROTOTYPE_GUARD });
}
