// SPDX-License-Identifier: AGPL-3.0-or-later
// A phone-width window (390 px) must show every screen without a horizontal scroll bar. The
// check reads box sizes, not pixels: the page's scroll width against its client width, and on a
// failure the elements that stick out, so the message names the culprit.
import { expect, test } from "./fixture";
import { overflow } from "./support";

const ROUTES = [
  "/workspaces",
  "/workspaces/web-shop",
  "/workspaces/web-shop/settings",
  "/workspaces/web-shop/network",
  "/workspaces/web-shop/settings/ports",
  "/workspaces/web-shop/terminal",
  "/workspaces/web-shop/git",
  "/workspaces/web-shop/settings/environment",
  "/workspaces/web-shop/settings/shell-init",
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
  "/welcome/interception",
  "/welcome/look",
  "/welcome/workspace",
] as const;

test.describe("at 390 px wide", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  for (const route of ROUTES) {
    test(`${route} has no horizontal scroll`, async ({ page, backend }) => {
      await backend.installClock(page);
      await backend.signIn(page);
      await page.goto(route);
      await expect(page.locator("h1").first()).toBeVisible();
      const { by, culprits } = await overflow(page);
      expect(
        by,
        `overflow by ${by} px; sticking out: ${culprits.join(", ")}`,
      ).toBeLessThanOrEqual(0);
    });
  }
});
