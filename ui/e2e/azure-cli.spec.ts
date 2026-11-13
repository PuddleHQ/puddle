// SPDX-License-Identifier: AGPL-3.0-or-later
// The Azure CLI credential against the real API on the fixture backend, scenario `azure-cli`: one
// identity (Contoso) that reads the Azure CLI's token for contoso.onmicrosoft.com and whose `az`
// is not signed in. Puddle starts no sign-in for it: the screens say what is missing and the exact
// step to take in a terminal. Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeStops, staysEmpty, watchCsp } from "./support";

const LINE = "Azure CLI account in tenant contoso.onmicrosoft.com for contoso";
const STEP =
  "Run `az login --tenant contoso.onmicrosoft.com` in a terminal and finish the sign-in";

async function open(page: Page, backend: Backend, path: string) {
  await backend.control.reset("azure-cli");
  await backend.signIn(page);
  await page.goto(path);
}

test("a credential that reads the Azure CLI says what to do and offers no sign-in", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await open(page, backend, "/identities/1");
  await expect(
    page.getByRole("heading", { level: 1, name: "Contoso" }),
  ).toBeVisible();
  const azure = page.locator(`[data-credential="${LINE}"]`);
  await expect(azure).toContainText("Azure CLI · contoso.onmicrosoft.com");
  await expect(azure.getByTestId("az-note")).toContainText(
    "does not sign in for you",
  );
  await expect(azure.getByTestId("az-note")).toContainText(
    "az login --tenant contoso.onmicrosoft.com",
  );
  await azure.getByRole("button", { name: /^Test / }).click();
  await expect(azure).toContainText("not signed in");
  await expect(azure.getByTestId("fix-step")).toContainText(STEP);
  await expect(azure.getByRole("button", { name: /Sign in/ })).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await staysEmpty(page, csp);
});

test("a workspace that could not read it leads to the same step, not a sign-in button", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/identities/1");
  await expect(
    page.getByRole("heading", { level: 1, name: "Contoso" }),
  ).toBeVisible();
  const azure = page.locator(`[data-credential="${LINE}"]`);
  await expect(async () => {
    await backend.control.emit({
      type: "credential_sign_in_needed",
      host: "dev.azure.com",
      source: LINE,
    });
    await expect(azure.getByTestId("fix-step")).toContainText(STEP, {
      timeout: 500,
    });
  }).toPass();
  await expect(azure.getByRole("button", { name: /Sign in/ })).toHaveCount(0);
});

test("adds the Azure CLI to an identity by tenant and organisation, and checks it first", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/identities");
  await page.getByRole("button", { name: "Add identity" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Add identity" });
  await dialog.getByLabel("Identity name").fill("Fabrikam");
  await dialog.getByLabel("Author name").fill("Tijs F");
  await dialog.getByLabel("Author email").fill("tijs@fabrikam.example");
  await dialog.getByRole("button", { name: "Add a credential" }).click();
  await dialog.getByLabel("Use the Azure CLI (Azure DevOps)").check();
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "Microsoft Entra tenant",
  );
  await expect(dialog.getByLabel("Microsoft Entra tenant")).toBeFocused();
  await dialog.getByLabel("Microsoft Entra tenant").fill("fabrikam.com");
  await dialog.getByLabel(/Azure DevOps organisation/).fill("fabrikam");
  await expect(dialog).toContainText("az login --tenant fabrikam.com");
  await dialog.getByRole("button", { name: "Check the Azure CLI" }).click();
  await expect(dialog.getByTestId("az-check")).toContainText(
    "Puddle can read a token from the Azure CLI",
  );
  await dialog.getByRole("button", { name: "Add credential" }).click();
  await expect(dialog).toContainText(
    "Azure CLI · fabrikam.com · dev.azure.com: fabrikam",
  );
  await dialog.getByRole("button", { name: "Add identity" }).click();
  await expect(dialog).toBeHidden();
  await expect(
    page
      .locator("[data-identity-id]")
      .filter({ has: page.getByRole("heading", { name: "Fabrikam" }) }),
  ).toContainText("Azure CLI · fabrikam.com");
});

const stops = axeStops([["identity with the step", "editor with the step"]]);
for (const scheme of ["light", "dark"] as const) {
  test(`axe finds nothing in ${scheme}: the Azure CLI step`, async ({
    page,
    backend,
  }) => {
    const scan = stops.scanner(stops.groups[0] ?? []);
    await page.emulateMedia({ colorScheme: scheme });
    await open(page, backend, "/identities/1");
    const azure = page.locator(`[data-credential="${LINE}"]`);
    await azure.getByRole("button", { name: /^Test / }).click();
    await expect(azure.getByTestId("fix-step")).toBeVisible();
    await scan(page, "identity with the step");

    await page.goto("/identities");
    await page.getByRole("button", { name: "Add identity" }).first().click();
    const dialog = page.getByRole("dialog", { name: "Add identity" });
    await dialog.getByRole("button", { name: "Add a credential" }).click();
    await dialog.getByLabel("Use the Azure CLI (Azure DevOps)").check();
    await dialog.getByLabel("Microsoft Entra tenant").fill("contoso.com");
    await dialog.getByRole("button", { name: "Check the Azure CLI" }).click();
    await expect(dialog.getByTestId("az-check")).toBeVisible();
    await scan(page, "editor with the step");
    scan.done();
  });
}
