// SPDX-License-Identifier: GPL-3.0-or-later
// What a scan and a slow page open may cost a test, as the test sees it: neither takes the 30 s a
// test has for what it checks.
import { slowdown } from "./cpu-throttle.ts";
import { expect, test } from "./fixture";
import { AXE_SCAN_ALLOWANCE_MS, expectNoAxeViolations } from "./support";

test("every axe scan adds its own time to the test's limit", async ({
  page,
}) => {
  await page.setContent(
    '<!doctype html><html lang="en"><title>t</title><main><h1>Hello</h1></main></html>',
  );
  const before = test.info().timeout;
  await expectNoAxeViolations(page);
  expect(test.info().timeout).toBe(before + AXE_SCAN_ALLOWANCE_MS * slowdown());
  await expectNoAxeViolations(page);
  expect(test.info().timeout).toBe(
    before + 2 * AXE_SCAN_ALLOWANCE_MS * slowdown(),
  );
});

const sluggish = test.extend({
  context: async ({ context }, use) => {
    const open = context.newPage.bind(context);
    context.newPage = async () => {
      await new Promise((done) => setTimeout(done, 4_000));
      return open();
    };
    await use(context);
  },
});

sluggish.describe("a page that is slow to open", () => {
  sluggish.describe.configure({ timeout: 3_000 });
  sluggish(
    "does not use up the time the test has for what it checks",
    async ({ page }) => {
      await page.goto("about:blank");
      expect(page.url()).toBe("about:blank");
    },
  );
});
