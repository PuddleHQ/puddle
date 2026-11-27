// SPDX-License-Identifier: AGPL-3.0-or-later
// A phone-width window (390 px) must show every screen without a horizontal scroll bar. The
// check reads box sizes, not pixels: the page's scroll width against its client width, and on a
// failure the elements that stick out, so the message names the culprit.
import { expect, test } from "./fixture";
import { lengthenUserStrings, overflow } from "./support";

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
  "/settings/app-errors",
  "/welcome",
  "/welcome/check",
  "/welcome/certificates",
  "/welcome/connect",
  "/welcome/interception",
  "/welcome/look",
  "/welcome/done",
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

// The seeded data is short, but a repository URL, a host or a path is as long as its user made it.
// `lengthenUserStrings` makes every such field in the API's answers long and unbroken, so a row
// that cannot wrap fails on its route (the global `.meta` never wraps, #1293).
for (const scenario of ["lived-in", "git-identities", "unmanaged-checkouts"]) {
  test.describe(`at 390 px wide, scenario ${scenario}, every host, URL and path 120 characters longer`, () => {
    test.use({ viewport: { width: 390, height: 844 } });

    for (const route of ROUTES) {
      test(`${route} has no horizontal scroll`, async ({ page, backend }) => {
        await backend.control.reset(scenario);
        await lengthenUserStrings(page);
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
}
