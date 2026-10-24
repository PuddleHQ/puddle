// SPDX-License-Identifier: GPL-3.0-or-later
// A phone-width window (390 px) must show every screen without a horizontal scroll bar. The
// check reads box sizes, not pixels: the page's scroll width against its client width, and on a
// failure the elements that stick out, so the message names the culprit.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";

const ROUTES = [
  "/workspaces",
  "/workspaces/web-shop",
  "/workspaces/web-shop/settings",
  "/workspaces/web-shop/network",
  "/workspaces/web-shop/ports",
  "/workspaces/web-shop/git",
  "/workspaces/web-shop/environment",
  "/workspaces/web-shop/shell-init",
  "/inbox",
  "/rules",
  "/activity",
  "/identities",
  "/identities/1",
  "/settings",
  "/settings/network-health",
  "/welcome",
  "/welcome/check",
  "/welcome/certificates",
  "/welcome/connect",
  "/welcome/look",
  "/welcome/workspace",
] as const;

/** The page's overflow in px, and the elements whose right edge is past the window. */
async function overflow(page: Page): Promise<{ by: number; culprits: string[] }> {
  return page.evaluate(() => {
    const root = document.documentElement;
    const width = root.clientWidth;
    const culprits: string[] = [];
    for (const el of document.body.querySelectorAll("*")) {
      const right = el.getBoundingClientRect().right;
      if (right > width + 0.5 && culprits.length < 8) {
        const id = el.id ? `#${el.id}` : "";
        const cls = typeof el.className === "string" && el.className
          ? `.${el.className.trim().split(/\s+/).join(".")}`
          : "";
        culprits.push(`${el.tagName.toLowerCase()}${id}${cls} right=${Math.round(right)}`);
      }
    }
    return { by: root.scrollWidth - width, culprits };
  });
}

test.describe("at 390 px wide", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  for (const route of ROUTES) {
    test(`${route} has no horizontal scroll`, async ({ page, backend }) => {
      await backend.installClock(page);
      await backend.signIn(page);
      await page.goto(route);
      await expect(page.locator("h1").first()).toBeVisible();
      const { by, culprits } = await overflow(page);
      expect(by, `overflow by ${by} px; sticking out: ${culprits.join(", ")}`).toBeLessThanOrEqual(0);
    });
  }
});
