// SPDX-License-Identifier: AGPL-3.0-or-later
// An error no code expected reaches the server: one thrown in the page, outside any handler, is
// reported, scrubbed, and listed under Settings, App errors. Each test has a backend of its own.
import { expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

const SECRET =
  "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

test("an uncaught error reaches the server scrubbed and Settings lists it", async ({
  page,
  backend,
}) => {
  await backend.control.reset("empty");
  await backend.signIn(page);
  await page.goto("/settings");
  await expect(
    page.getByRole("heading", { level: 1, name: "Settings" }),
  ).toBeVisible();

  const reported = page.waitForResponse(
    (r) =>
      r.url().endsWith("/api/ui-errors") && r.request().method() === "POST",
  );
  // Thrown from a timer, as a click handler's bug would be: no page code catches it.
  await page.evaluate((secret) => {
    setTimeout(() => {
      throw new TypeError(`token ${secret} was refused`);
    }, 0);
  }, SECRET);
  expect((await reported).status()).toBe(204);

  await page
    .getByRole("link", { name: /^Details\s*about app errors$/ })
    .click();
  await expect(
    page.getByRole("heading", { level: 1, name: "App errors" }),
  ).toBeVisible();
  const item = page
    .getByRole("list", { name: "App errors" })
    .getByRole("listitem");
  await expect(item).toHaveCount(1);
  await expect(item).toContainText("Uncaught error");
  await expect(item).toContainText("on /settings");
  await expect(item).toContainText("TypeError: token [redacted] was refused");
  await expect(page.getByText(SECRET)).toHaveCount(0);
  await expectNoAxeViolations(page);
});
