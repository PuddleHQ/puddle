// SPDX-License-Identifier: AGPL-3.0-or-later
// The owner's admin screen against the real API on the fixture backend with several users on
// (scenario `multi-user`): making a user with a one-time invite link, the person choosing their own
// password with it, the list, and a typed-name delete. Where several users are not on there is no
// screen at all.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import {
  expectNoAxeViolations,
  overflow,
  staysEmpty,
  watchCsp,
} from "./support";

const PUBLIC_HOST = "puddle.example.org";
const PASSWORD = "an e2e password, long enough";

async function openAdmin(page: Page, backend: Backend): Promise<void> {
  await backend.signIn(page);
  await page.goto("/admin");
  await expect(
    page.getByRole("heading", { level: 1, name: "Admin" }),
  ).toBeVisible();
}

/** Makes `name` through the screen and returns the secret in the link the dialog showed once. */
async function addUser(page: Page, name: string): Promise<string> {
  await page.getByRole("button", { name: "Add user" }).click();
  await page.getByLabel("Name").fill(name);
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Add user" })
    .click();
  const link = page.getByRole("textbox", { name: "Invite link" });
  await expect(link).toBeVisible();
  await expect(link).toHaveValue(/\/invite#.+/);
  const value = await link.inputValue();
  return value.slice(value.indexOf("#") + 1);
}

async function acceptInvite(
  backend: Backend,
  page: Page,
  secret: string,
): Promise<void> {
  // The person opens the link on the public name; the browser is not the owner's.
  const answer = await page.request.post(`${backend.url}/api/auth/invite`, {
    headers: {
      Host: PUBLIC_HOST,
      Origin: `https://${PUBLIC_HOST}`,
      "Sec-Fetch-Site": "same-origin",
    },
    data: { secret, password: PASSWORD, device_name: "e2e browser" },
  });
  const said = await answer.text();
  await expect(answer, said).toBeOK();
}

test.describe("with several users on", () => {
  test.beforeEach(async ({ backend }) => {
    await backend.control.reset("multi-user");
  });

  test("makes a user with a one-time link, who chooses their own password, then deletes them", async ({
    page,
    backend,
  }) => {
    const csp = await watchCsp(page);
    await openAdmin(page, backend);
    await expect(page.getByRole("link", { name: "Admin" })).toBeVisible();
    await expect(
      page.getByText(/Only one person uses this Puddle/),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "This server" }),
    ).toHaveCount(0);
    await page.getByRole("button", { name: "Show server details" }).click();
    await expect(
      page.getByRole("heading", { name: "This server" }),
    ).toBeVisible();

    const secret = await addUser(page, "Sam");
    // The dialog never asks for a password, and its link is the only place the secret shows.
    await expect(page.locator('input[type="password"]')).toHaveCount(0);
    await expectNoAxeViolations(page, "the invite link dialog");
    await page.getByRole("button", { name: "Done" }).click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(page.locator("body")).not.toContainText(secret);

    const row = page
      .getByRole("list", { name: "Users" })
      .getByRole("listitem")
      .filter({ hasText: "Sam" });
    await expect(row).toContainText("Invited");
    await expectNoAxeViolations(page, "admin with an invited user");

    // The link works once, for the person who opens it.
    await acceptInvite(backend, page, secret);
    await page.reload();
    await expect(row).toContainText("Active");
    await expect(row).toContainText("Devices 1");
    // Nothing the owner can read carries the secret or the password.
    await expect(page.locator("body")).not.toContainText(PASSWORD);

    await row.getByRole("button", { name: "Delete Sam" }).click();
    const dialog = page.getByRole("alertdialog", { name: "Delete Sam?" });
    const confirm = dialog.getByRole("button", { name: "Delete user" });
    await expect(confirm).toBeDisabled();
    await dialog.getByLabel(/Type/).fill("sam");
    await expect(confirm).toBeDisabled();
    await expectNoAxeViolations(page, "the delete dialog");
    await dialog.getByLabel(/Type/).fill("Sam");
    await confirm.click();
    await expect(dialog).toHaveCount(0);
    await expect(row).toHaveCount(0, { timeout: 30_000 });
    await expect(page.getByText("Sam was deleted")).toBeVisible();
    await staysEmpty(page, csp);
  });

  test("a second invite link replaces the first, and the audit says who did what", async ({
    page,
    backend,
  }) => {
    await openAdmin(page, backend);
    const first = await addUser(page, "Kim");
    await page.getByRole("button", { name: "Done" }).click();
    await page.getByRole("button", { name: "New invite link for Kim" }).click();
    const link = page.getByRole("textbox", { name: "Invite link" });
    await expect(link).toHaveValue(/\/invite#.+/);
    await expect(link).not.toHaveValue(
      new RegExp(first.replace(/[^\w-]/g, "\\$&")),
    );
    await page.getByRole("button", { name: "Done" }).click();
    const audit = page.getByRole("list", { name: /Admin audit/ });
    await expect(audit).toContainText("made the account Kim");
    await expect(audit).toContainText("gave Kim a new invite link");
  });

  test("a name that is taken is said in the dialog", async ({
    page,
    backend,
  }) => {
    await openAdmin(page, backend);
    await addUser(page, "Lee");
    await page.getByRole("button", { name: "Done" }).click();
    await page.getByRole("button", { name: "Add user" }).click();
    await page.getByLabel("Name").fill("lee");
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "Add user" })
      .click();
    await expect(page.getByRole("alert")).toBeVisible();
    await expect(
      page.getByRole("dialog", { name: "Add a user" }),
    ).toBeVisible();
  });

  test.describe("at 390 px wide", () => {
    test.use({ viewport: { width: 390, height: 844 } });

    test("the list, the totals and the dialogs have no horizontal scroll", async ({
      page,
      backend,
    }) => {
      await openAdmin(page, backend);
      await addUser(
        page,
        "A-very-long-user-name-that-must-wrap-somewhere-sensible",
      );
      expect((await overflow(page)).by, "invite dialog").toBeLessThanOrEqual(0);
      await page.getByRole("button", { name: "Done" }).click();
      await expect(page.getByRole("dialog")).toHaveCount(0);
      expect((await overflow(page)).by, "list").toBeLessThanOrEqual(0);
      await page.getByRole("button", { name: /^Delete A-very/ }).click();
      await expect(page.getByRole("alertdialog")).toBeVisible();
      expect((await overflow(page)).by, "delete dialog").toBeLessThanOrEqual(0);
    });
  });
});

test.describe("with one user", () => {
  test("has no Admin link, and the page says there is no admin screen", async ({
    page,
    backend,
  }) => {
    await backend.signIn(page);
    await page.goto("/rules");
    await expect(page.getByRole("heading", { level: 1 }).first()).toBeVisible();
    await expect(page.getByRole("link", { name: "Admin" })).toHaveCount(0);
    await page.goto("/admin");
    await expect(
      page.getByText(/Only one person uses this Puddle/),
    ).toBeVisible();
    await expect(page.getByText(/Couldn't read the admin data/)).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Try again" })).toHaveCount(
      0,
    );
    await expect(page.getByRole("button", { name: "Add user" })).toHaveCount(0);
    await expectNoAxeViolations(page, "admin where there is none");
  });
});
