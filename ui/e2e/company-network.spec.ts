// SPDX-License-Identifier: AGPL-3.0-or-later
// The company-network destination category on screen against the real API on the fixture backend:
// the switch in Settings, the message on an inbox request, the Network health entries and the
// chip and filter in Activity. Each test has a backend of its own. In the `company-network`
// scenario a private-access client is active, so 100.64.3.7 is the company's and 192.168.1.10 is
// an ordinary private address.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations, staysEmpty, watchCsp } from "./support";

async function open(page: Page, backend: Backend, path: string) {
  await backend.control.reset("company-network");
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto(path);
}

test("an inbox request on the company network says so and why, and opens that workspace's switch", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await open(page, backend, "/inbox");
  const row = page.locator("li.req", { hasText: "100.64.3.7" });
  await expect(row).toBeVisible();
  await expect(row).toContainText("on your company network");
  await expect(row).toContainText("private-access client");
  await expect(row.getByRole("button", { name: /^Allow/ })).toHaveCount(0);
  // A private address that is not the company's keeps the old words.
  const plain = page.locator("li.req", { hasText: "192.168.1.10" });
  await expect(plain).toContainText("Private networks");
  await expect(plain).not.toContainText("company network");

  await row
    .getByRole("link", { name: "Open the company network switch" })
    .click();
  await expect(page).toHaveURL(
    /\/workspaces\/[^/]+\/settings#local-destinations$/,
  );
  await expect(page.getByLabel("Company network")).toBeVisible();
  await staysEmpty(page, csp);
});

test("turning the company network on for a workspace makes its request approvable", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/inbox");
  const row = page.locator("li.req", { hasText: "100.64.3.7" });
  await row
    .getByRole("link", { name: "Open the company network switch" })
    .click();
  await page.getByLabel("Company network").selectOption({ label: "Allowed" });
  await expect(page.getByText(/saved\./)).toBeVisible();
  await page.goto("/inbox");
  const after = page.locator("li.req", { hasText: "100.64.3.7" });
  await expect(after.getByRole("button", { name: /^Allow/ })).toBeVisible();
  await expect(after).not.toContainText("can't be approved yet");
});

test("Settings shows the company network switch under private networks, off", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/settings");
  const toggles = page.locator('input[id^="set-local-"]');
  await expect(toggles).toHaveCount(6);
  await expect(toggles.nth(2)).toHaveAttribute(
    "id",
    "set-local-company_network",
  );
  const box = page.getByLabel("Company network");
  await expect(box).not.toBeChecked();
  await expect(
    page.getByText("mark as internal", { exact: false }),
  ).toBeVisible();
  await box.check();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByLabel("Company network")).toBeChecked();
  await expectNoAxeViolations(page);
});

test("Network health lists the private-access client and DNS suffixes behind the category", async ({
  page,
  backend,
}) => {
  await backend.control.reset("corporate-network");
  await backend.signIn(page);
  await page.goto("/settings/network-health");
  const card = page.getByRole("heading", { level: 2, name: "Company network" });
  await expect(card).toBeVisible();
  const section = card.locator("xpath=..");
  await expect(section).toContainText("One is installed and active");
  await expect(section).toContainText("corp.example, ad.corp.example");
});

test("Activity marks company-network records with a chip and filters on them", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/activity");
  const rows = page.locator("tbody tr.row");
  const chip = page.locator(".chip", { hasText: "Company network" });
  await expect(chip).toHaveCount(1);
  await expect(chip).toHaveAttribute("title", /private-access client/);
  await expect(rows.filter({ has: chip })).toContainText("100.64.3.7");
  await expect(rows.filter({ has: chip })).toContainText(
    "company network destinations are switched off",
  );

  await page.getByLabel("Destination").selectOption("company_network");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("100.64.3.7");
  await expect(page).toHaveURL(/category=company_network/);
});
