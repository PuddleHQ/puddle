// SPDX-License-Identifier: AGPL-3.0-or-later
// The activity log card against the real API on the fixture backend: a single owner reaches it on
// the settings screen, sees the log's size, changes the days it is kept, and reads the server's own
// refusal for a number outside 1 to 3650. Each test has a backend of its own.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { staysEmpty, watchCsp } from "./support";

// The group on the Settings page: a section with the "Activity log" heading.
function activityLog(page: Page) {
  return page.locator("section.group").filter({
    has: page.getByRole("heading", { level: 2, name: "Activity log" }),
  });
}

async function openCard(page: Page, backend: Backend) {
  await backend.signIn(page);
  await page.goto("/settings/this-computer#set-audit");
  const card = activityLog(page);
  await expect(card.getByLabel(/Keep the activity log for/)).toBeVisible();
  return card;
}

test("a single owner sees the log's size and the days it is kept", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  const card = await openCard(page, backend);
  await expect(card.getByLabel(/Keep the activity log for/)).toHaveValue("90");
  await expect(card.getByText("Oldest record")).toBeVisible();
  await expect(
    card.getByText(/records?, [\d.]+ (B|KB|MB)/).first(),
  ).toBeVisible();
  await expect(card.getByText(/Database file/)).toBeVisible();
  await expect(card.getByText(/getting large/)).toHaveCount(0);
  await staysEmpty(page, csp);
});

test("saving the days keeps them across a reload", async ({
  page,
  backend,
}) => {
  const card = await openCard(page, backend);
  const days = card.getByLabel(/Keep the activity log for/);
  await days.fill("30");
  await card
    .getByRole("button", { name: "Save how long the activity log is kept" })
    .click();
  await expect(card.getByText("Saved.")).toBeVisible();
  await page.reload();
  await expect(
    activityLog(page).getByLabel(/Keep the activity log for/),
  ).toHaveValue("30");
});

for (const bad of ["0", "3651"]) {
  test(`${bad} days shows the server's refusal and changes nothing`, async ({
    page,
    backend,
  }) => {
    const card = await openCard(page, backend);
    const days = card.getByLabel(/Keep the activity log for/);
    await days.fill(bad);
    await card
      .getByRole("button", { name: "Save how long the activity log is kept" })
      .click();
    await expect(card.getByRole("alert")).toContainText(
      `The log is kept for 1 to 3650 days (${bad} given).`,
    );
    await page.reload();
    await expect(
      activityLog(page).getByLabel(/Keep the activity log for/),
    ).toHaveValue("90");
  });
}

test("the owner sees the same card on the admin screen", async ({
  page,
  backend,
}) => {
  await backend.control.reset("multi-user");
  await backend.signIn(page);
  await page.goto("/admin");
  // The owner alone sees the totals and the log behind this toggle.
  await page.getByRole("button", { name: "Show server details" }).click();
  const card = page.getByRole("region", { name: "Activity log" });
  await expect(card.getByLabel(/Keep the activity log for/)).toHaveValue("90");
});
