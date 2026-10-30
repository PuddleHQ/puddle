// SPDX-License-Identifier: GPL-3.0-or-later
// The network-health screen and the notices against the real API on the fixture backend. Each
// test has a backend of its own. Waits are on conditions (text on the page, a response), never on
// time or key order.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { watchCsp, expectNoAxeViolations, staysEmpty } from "./support";

async function open(page: Page, backend: Backend, scenario: string) {
  await backend.control.reset(scenario);
  await backend.signIn(page);
  await page.goto("/settings/network-health");
  await expect(
    page.getByRole("heading", { level: 1, name: "Network health" }),
  ).toBeVisible();
}

const leftOutHeading = (page: Page) =>
  page.getByRole("heading", { name: "Left out of Puddle's own TLS checks" });

const notices = (page: Page) => page.getByRole("region", { name: "Notices" });

test("a corporate network: the setup, sign-in, roots and routes, no notice, no CSP violation", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await open(page, backend, "corporate-network");
  await expect(page.getByText("Automatic proxy script (PAC)")).toBeVisible();
  await expect(
    page.getByText("http://wpad.corp.example/proxy.pac"),
  ).toBeVisible();
  await expect(page.getByText("Signed in")).toBeVisible();
  await expect(page.getByRole("cell", { name: /^Corp Root CA/ })).toBeVisible();
  await expect(page.getByText("Corp Issuing CA 01")).toBeVisible();
  await expect(page.getByText(/Old Corp Root CA/)).toBeVisible();
  await expect(page.getByText("https://github.com:443")).toBeVisible();
  // A proxy marked as not answering is a note, with what to do about it.
  await expect(
    page.getByText(/proxy-b.corp.example:8080 is marked as not answering/),
  ).toBeVisible();
  await expect(page.getByText("No problems found.")).toBeVisible();
  await expect(notices(page).getByRole("button")).toHaveCount(0);
  await staysEmpty(page, csp);
});

test("a network change updates the open page, and trouble raises a notice that clears again", async ({
  page,
  backend,
}) => {
  await open(page, backend, "corporate-network");
  await expect(page.getByText(/change number 3/)).toBeVisible();

  // The change script moves to epoch 4 and clears the dead proxy.
  await backend.control.script("network-change");
  await expect(page.getByText(/change number 4/)).toBeVisible();
  await expect(page.getByText(/is marked as not answering/)).toHaveCount(0);

  await backend.control.script("network-trouble");
  await expect(page.getByText("4 problems found.")).toBeVisible();
  const notice = notices(page).getByText(/^Network trouble:/);
  await expect(notice).toBeVisible();
  // Nothing moved the focus to the notice.
  await expect(notice).not.toBeFocused();
  await expect(page.getByText(/What to do:/).first()).toBeVisible();

  await backend.control.script("network-change");
  await expect(page.getByText("No problems found.")).toBeVisible();
  await expect(notices(page).getByText(/^Network trouble:/)).toHaveCount(0);
});

test("trouble on a fresh start: the notice links to the screen, which says what to do", async ({
  page,
  backend,
}) => {
  await backend.control.reset("network-trouble");
  await backend.signIn(page);
  await page.goto("/workspaces");
  const notice = notices(page).getByText(/^Network trouble:/);
  await expect(notice).toBeVisible();
  await notices(page).getByRole("link", { name: "Network health" }).click();
  await expect(page).toHaveURL(/\/settings\/network-health$/);
  await expect(
    page.locator("main").getByText(/The proxy script at .* isn't answering/),
  ).toBeVisible();
  await expect(
    page.getByText(/couldn't read a certificate store/),
  ).toBeVisible();
  await expect(
    page.getByText(/Signing in to fallback.corp.example:3128 failed/),
  ).toBeVisible();
  await page.getByRole("button", { name: "Check again" }).click();
  await expect(page.getByRole("button", { name: "Check again" })).toBeEnabled();
});

test("a company certificate Puddle's own TLS checks can't use is listed with why, and is a problem", async ({
  page,
  backend,
}) => {
  await open(page, backend, "network-trouble");
  await expect(leftOutHeading(page)).toBeVisible();
  const item = page.getByRole("listitem").filter({
    hasText: "Corp Inspection Root",
  });
  await expect(item).toContainText("e5e5e5e5e5e5e5e5");
  await expect(item).toContainText(
    "its extended key usage is not valid as a trust anchor",
  );
  // The summary says so too, with what to do.
  await expect(
    page.getByText(/Puddle can't use a company certificate when it checks/),
  ).toBeVisible();
  await expect(page.getByText(/Ask IT for it again/)).toBeVisible();
});

test("with nothing left out the list says so", async ({ page, backend }) => {
  await open(page, backend, "corporate-network");
  await expect(leftOutHeading(page)).toBeVisible();
  await expect(page.getByText("No certificate was left out.")).toBeVisible();
});

test("settings link to the details", async ({ page, backend }) => {
  await backend.signIn(page);
  await page.goto("/settings");
  await page.getByRole("link", { name: /Details/ }).click();
  await expect(
    page.getByRole("heading", { level: 1, name: "Network health" }),
  ).toBeVisible();
});

for (const scheme of ["light", "dark"] as const) {
  test(`axe finds nothing in ${scheme}, healthy or in trouble`, async ({
    page,
    backend,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await open(page, backend, "corporate-network");
    await expect(page.getByText("No problems found.")).toBeVisible();
    await expectNoAxeViolations(page, "healthy");
    await backend.control.script("network-trouble");
    await expect(notices(page).getByText(/^Network trouble:/)).toBeVisible();
    await expectNoAxeViolations(page, "trouble");
  });
}
