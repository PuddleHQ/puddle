// SPDX-License-Identifier: AGPL-3.0-or-later
// Signing in to a company proxy that asks for a user name and password, against the real API on
// the fixture backend: store it in Settings, see it on the network-health screen, never see the
// password again, remove it. Waits are on conditions, never on time.
import { expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

const PASSWORD = "e2e-proxy-password-1f3a";

test("a stored proxy sign-in shows by proxy and user in Settings and on the network-health screen, never as a password, and goes when removed", async ({
  page,
  backend,
}) => {
  await backend.control.reset("corporate-network");
  await backend.signIn(page);
  await page.goto("/settings");
  await expect(page.getByText("No proxy sign-in is stored.")).toBeVisible();

  await page
    .getByLabel("Proxy", { exact: true })
    .fill("proxy-a.corp.example:8080");
  await page.getByLabel("User name").fill("svc-alice");
  await page.getByLabel("Password").fill(PASSWORD);
  await expectNoAxeViolations(page);
  await page.getByRole("button", { name: "Keep sign-in" }).click();

  const stored = page.getByRole("list", { name: "Stored proxy sign-ins" });
  await expect(stored).toContainText("proxy-a.corp.example:8080");
  await expect(stored).toContainText("signs in as svc-alice");
  await expect(page.getByLabel("Password")).toHaveValue("");
  await expect(page.locator("body")).not.toContainText(PASSWORD);

  await page.goto("/settings/network-health");
  const sign = page.getByRole("region", { name: "Signing in to the proxy" });
  await expect(sign).toContainText("proxy-a.corp.example:8080");
  await expect(sign).toContainText("svc-alice");
  await expect(page.locator("body")).not.toContainText(PASSWORD);

  await page.goto("/settings");
  await page
    .getByRole("button", {
      name: "Remove the sign-in for proxy-a.corp.example:8080",
    })
    .click();
  await expect(page.getByText("No proxy sign-in is stored.")).toBeVisible();
});
