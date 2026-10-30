// SPDX-License-Identifier: GPL-3.0-or-later
// The kept logins of a workspace on its settings tab (fixture scenario `lived-in`): the list, its
// empty and unreadable-store states, and forgetting one by keyboard. A name from a hostile source
// is drawn as text and wraps inside its row. Layout is checked by computed styles and box sizes,
// never by pictures.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

const section = (page: Page) =>
  page.getByRole("region", { name: "Kept logins" });
const dialog = (page: Page) => page.getByRole("alertdialog");
const forget = (page: Page, name: string) =>
  section(page).getByRole("button", { name: `Forget the ${name} login` });

const HOSTILE = `<img src=x onerror="document.title='owned'"> ${"long-".repeat(40)}`;

async function open(
  page: Page,
  backend: Backend,
  logins: { service: string; name: string; since?: number }[] = [
    { service: "claude", name: "Claude Code", since: 1_761_480_000_000 },
    { service: "github", name: "GitHub" },
  ],
  storeReadable = true,
) {
  await backend.control.reset("lived-in");
  await backend.control.step({
    do: "kept_logins",
    workspace: "web-shop",
    logins,
    store_readable: storeReadable,
  });
  await backend.signIn(page);
  await page.goto("/workspaces/web-shop/settings");
  await expect(
    page.getByRole("heading", { name: "Kept logins" }),
  ).toBeVisible();
}

test("the kept logins are listed with names and times, and nothing else", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  const rows = section(page).getByRole("listitem");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("Claude Code");
  await expect(rows.nth(0)).toContainText("Kept since");
  await expect(rows.nth(1)).toContainText(
    "Kept since a time Puddle did not record.",
  );
  await expectNoAxeViolations(page);
});

test("a kept login is forgotten by keyboard after a confirmation, and focus stays in the section", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await forget(page, "Claude Code").focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toBeVisible();
  await expect(dialog(page)).toHaveAccessibleName(
    "Forget the Claude Code login in web-shop?",
  );
  await expect(dialog(page)).toContainText("credential store");
  await expect(
    dialog(page).getByRole("button", { name: "Cancel" }),
  ).toBeFocused();
  await expectNoAxeViolations(page);
  await page.keyboard.press("Tab");
  await expect(
    dialog(page).getByRole("button", { name: "Forget login" }),
  ).toBeFocused();
  await page.keyboard.press("Enter");

  await expect(dialog(page)).toHaveCount(0);
  await expect(section(page).getByRole("listitem")).toHaveCount(1);
  await expect(section(page)).toContainText("Forgot the Claude Code login.");
  await expect(
    section(page).getByRole("heading", { name: "Kept logins" }),
  ).toBeFocused();
  await expectNoAxeViolations(page);

  // The forgotten login stays off the list after a reload: the host agrees.
  await page.reload();
  await expect(section(page).getByRole("listitem")).toHaveCount(1);
  await expect(section(page)).not.toContainText("Claude Code");
});

test("Escape and Cancel leave the login, and focus returns to its button", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await forget(page, "GitHub").focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog(page)).toHaveCount(0);
  await expect(forget(page, "GitHub")).toBeFocused();
  await expect(section(page).getByRole("listitem")).toHaveCount(2);
});

test("a store that refuses shows its way out in the dialog and keeps the row, and the next try works", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await backend.control.step({ do: "login_store_refuses", refuses: true });
  await forget(page, "Claude Code").focus();
  await page.keyboard.press("Enter");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");
  await expect(dialog(page).getByRole("alert")).toContainText(
    "credential store did not delete a saved login; unlock it or start it and ask again",
  );
  await expect(section(page).getByRole("listitem")).toHaveCount(2);
  await expectNoAxeViolations(page);

  await backend.control.step({ do: "login_store_refuses", refuses: false });
  await dialog(page).getByRole("button", { name: "Forget login" }).focus();
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toHaveCount(0);
  await expect(section(page).getByRole("listitem")).toHaveCount(1);
});

test("an empty list and an unreadable credential store each say so", async ({
  page,
  backend,
}) => {
  await open(page, backend, []);
  await expect(section(page)).toContainText(
    "Puddle keeps no logins for this workspace.",
  );
  await expect(section(page).getByRole("listitem")).toHaveCount(0);
  await expectNoAxeViolations(page);

  await open(page, backend, [], false);
  await expect(section(page).getByRole("alert")).toContainText(
    "couldn't read this computer's credential store",
  );
  await expect(
    section(page).getByRole("button", { name: "Try again" }),
  ).toBeVisible();
  await expectNoAxeViolations(page);

  // Trying again after the store answers shows the list.
  await backend.control.step({
    do: "kept_logins",
    workspace: "web-shop",
    logins: [{ service: "claude", name: "Claude Code" }],
  });
  await section(page).getByRole("button", { name: "Try again" }).click();
  await expect(section(page).getByRole("listitem")).toHaveCount(1);
});

test("a hostile name is text that wraps inside its row, and the buttons are big enough to hit", async ({
  page,
  backend,
}) => {
  await open(page, backend, [{ service: "claude", name: HOSTILE }]);
  // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
  expect(await page.title()).not.toBe("owned");
  await expect(section(page).locator("img")).toHaveCount(0);
  await expect(section(page)).toContainText("<img src=x");
  const edge = (await section(page).boundingBox())!;
  const row = (await section(page).getByRole("listitem").boundingBox())!;
  expect(row.x + row.width).toBeLessThanOrEqual(edge.x + edge.width + 1);
  const button = (await section(page).getByRole("button").boundingBox())!;
  expect(button.width).toBeGreaterThanOrEqual(24);
  expect(button.height).toBeGreaterThanOrEqual(24);
  expect(button.x + button.width).toBeLessThanOrEqual(edge.x + edge.width + 1);
  await expect
    .poll(() =>
      page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    )
    .toBe(true);
  await expectNoAxeViolations(page);
});
