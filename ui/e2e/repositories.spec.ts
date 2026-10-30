// SPDX-License-Identifier: GPL-3.0-or-later
// The workspace page's Repositories section and start folder, on the fixture backend. Scenario
// `unmanaged-checkouts`: dev is running with a managed checkout (changes in it), a hostile one,
// one without a remote, one over ssh, an unreadable one, a personal one and a scratch one, and a
// listed entry (docs) that is not on the disk; parked is stopped. Scenario `start-folder-missing`:
// dev's start folder does not exist.
import type { Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeStops, watchCsp } from "./support";

async function open(page: Page, backend: Backend, name = "dev", tab = "") {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto(`/workspaces/${name}${tab}`);
}

const section = (page: Page): Locator =>
  page.getByRole("region", { name: "Repositories" });
const rowOf = (page: Page, directory: string): Locator =>
  section(page)
    .getByRole("listitem")
    .filter({ has: page.getByText(directory, { exact: true }) });

test.beforeEach(async ({ backend }) => {
  await backend.control.reset("unmanaged-checkouts");
});

test.describe("the checkouts on the disk", () => {
  test("lists each one with what Puddle knows, and gives a reason where it cannot manage it", async ({
    page,
    backend,
  }) => {
    await open(page, backend);
    await expect(section(page)).toBeVisible();
    const api = rowOf(page, "api");
    await expect(api).toContainText("On the list");
    await expect(api).toContainText("changes");
    await expect(api).toContainText("main");
    await expect(
      api.getByRole("link", { name: "Work", exact: true }),
    ).toBeVisible();
    await expect(rowOf(page, "scratch")).toContainText(
      "Not managed by Puddle: works as is",
    );
    await expect(rowOf(page, "over-ssh")).toContainText(
      "SSH is not supported yet",
    );
    await expect(rowOf(page, "no-remote")).toContainText("no remote");
    await expect(rowOf(page, "broken")).toContainText("dubious ownership");
    await expect(
      rowOf(page, "over-ssh").getByRole("button", { name: /Manage/ }),
    ).toHaveCount(0);
    // A listed entry with no folder on the disk.
    await expect(rowOf(page, "docs")).toContainText("Not on the disk");
    await expect(
      rowOf(page, "docs").getByRole("button", { name: "Clone again: docs" }),
    ).toBeEnabled();
  });

  test("shows what the guest says as text and runs none of it", async ({
    page,
    backend,
  }) => {
    const csp = await watchCsp(page);
    await open(page, backend);
    const row = rowOf(page, "hostile");
    await expect(row).toContainText("<img src=x onerror=window.__owned=true>");
    await expect(section(page).locator("img")).toHaveCount(0);
    await expect(
      rowOf(page, "broken").locator("b", { hasText: "dubious" }),
    ).toHaveCount(0);
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { __owned?: boolean }).__owned,
        ),
      )
      .toBeUndefined();
    await expect.poll(() => csp()).toEqual([]);
  });

  test("keyboard only: Manage this puts a checkout on the list and focus stays on the section", async ({
    page,
    backend,
  }) => {
    await open(page, backend);
    const manage = rowOf(page, "scratch").getByRole("button", {
      name: "Manage this: scratch",
    });
    await manage.focus();
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("dialog", { name: "Manage scratch" });
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("https://github.com/acme/scratch.git");
    await expect(dialog).toContainText("Nothing in the checkout changes");
    // The dialog opens on the identity; Tab goes to the Pull box, Space clears it.
    await expect(dialog.getByLabel("Git identity")).toBeFocused();
    await page.keyboard.press("Tab");
    await page.keyboard.press("Space");
    await expect(dialog.getByLabel("Pull", { exact: true })).not.toBeChecked();
    await dialog.getByRole("button", { name: "Manage this" }).focus();
    await page.keyboard.press("Enter");
    await expect(dialog).toHaveCount(0);
    await expect(rowOf(page, "scratch")).toContainText("On the list");
    await expect(
      rowOf(page, "scratch").getByRole("button", { name: /Manage/ }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("heading", { level: 2, name: "Repositories" }),
    ).toBeFocused();
  });

  test("Manage this sends the address it showed, and a changed remote is refused with both addresses", async ({
    page,
    backend,
  }) => {
    await open(page, backend);
    const sent: string[] = [];
    // The remote moves between the listing and the click: the host sees another address than the one
    // the dialog sent.
    await page.route("**/api/workspaces/dev/repositories", async (route) => {
      const body = route.request().postDataJSON() as { url: string };
      sent.push(body.url);
      await route.continue({
        postData: JSON.stringify({
          ...body,
          url: "https://github.com/acme/elsewhere.git",
        }),
      });
    });
    await rowOf(page, "scratch")
      .getByRole("button", { name: "Manage this: scratch" })
      .click();
    const dialog = page.getByRole("dialog", { name: "Manage scratch" });
    await dialog.getByRole("button", { name: "Manage this" }).click();
    await expect(dialog.getByRole("alert")).toContainText("elsewhere");
    await expect(dialog.getByRole("alert")).toContainText("scratch");
    expect(sent).toEqual(["https://github.com/acme/scratch.git"]);
    await expect(dialog).toBeVisible();
    await expect(rowOf(page, "scratch")).toContainText("Not managed by Puddle");
  });

  test("removing from the list says the files stay, and the folder stays on the disk", async ({
    page,
    backend,
  }) => {
    await open(page, backend);
    await rowOf(page, "api")
      .getByRole("button", { name: "Remove from the list: api" })
      .click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("the folder and its files stay");
    await dialog.getByRole("button", { name: "Remove from the list" }).click();
    await expect(dialog).toHaveCount(0);
    await expect(rowOf(page, "api")).toContainText(
      "Not managed by Puddle: works as is",
    );
  });

  test("Add repository clones a new one, refuses a directory that exists, and is off while stopped", async ({
    page,
    backend,
  }) => {
    await open(page, backend);
    await section(page).getByRole("button", { name: "Add repository" }).click();
    const dialog = page.getByRole("dialog");
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/scratch");
    await dialog.getByRole("button", { name: "Clone" }).click();
    await expect(dialog.getByRole("alert")).toContainText(
      "scratch already exists in the workspace",
    );
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/web.git");
    await dialog.getByRole("button", { name: "Clone" }).click();
    await expect(dialog).toHaveCount(0);
    await expect(rowOf(page, "web")).toBeVisible();

    await open(page, backend, "parked");
    const add = section(page).getByRole("button", { name: "Add repository" });
    await expect(add).toBeDisabled();
    await expect(section(page)).toContainText(
      "Start the workspace to add a repository.",
    );
  });
});

test.describe("the start folder", () => {
  test.beforeEach(async ({ backend }) => {
    await backend.control.reset("start-folder-missing");
  });

  test("is checked as typed, and a missing folder is said on the setting and in the notice", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "dev", "/settings");
    await expect(folder(page)).toHaveValue("api/services/web");
    await folder(page).fill("../up");
    await expect(page.getByRole("alert")).toContainText("Leave out . and ..");
    await folder(page).fill("/abs");
    await expect(page.getByRole("alert")).toContainText("leading /");
    await folder(page).fill("api/services/web");
    await backend.control.step({
      do: "problem",
      key: "start-folder:dev",
      workspace: "dev",
      title: "The start folder of dev does not exist",
      detail: "api/services/web is not in the workspace <b>bold</b>",
    });
    const notices = page.getByRole("region", { name: "Notices" });
    await expect(notices).toContainText("does not exist");
    await expect(notices.locator("b")).toHaveCount(0);
    await expect(page.getByText("The folder does not exist.")).toBeVisible();
    await notices.getByRole("link", { name: "Change start folder" }).click();
    await expect(page).toHaveURL(/\/workspaces\/dev\/settings#start-folder$/);
    // Saving another folder clears the warning once the host looks again.
    await folder(page).fill("api");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Start folder saved.")).toBeVisible();
  });
});

const stops = axeStops([
  ["the section", "the manage dialog"],
  ["the add dialog", "the start folder"],
]);
for (const scheme of ["light", "dark"] as const) {
  for (const group of stops.groups) {
    test(`axe finds nothing in ${scheme}: ${group.join(", ")}`, async ({
      page,
      backend,
    }) => {
      const scan = stops.scanner(group);
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      await open(page, backend);
      await expect(rowOf(page, "scratch")).toBeVisible();
      await scan(page, "the section");
      await rowOf(page, "scratch")
        .getByRole("button", { name: "Manage this: scratch" })
        .click();
      await expect(
        page.getByRole("dialog", { name: "Manage scratch" }),
      ).toBeVisible();
      await scan(page, "the manage dialog");
      await page.keyboard.press("Escape");
      await section(page)
        .getByRole("button", { name: "Add repository" })
        .click();
      await expect(page.getByRole("dialog")).toBeVisible();
      await scan(page, "the add dialog");
      await page.keyboard.press("Escape");
      await open(page, backend, "dev", "/settings");
      await expect(folder(page)).toBeVisible();
      await scan(page, "the start folder");
      await expect.poll(() => csp()).toEqual([]);
      scan.done();
    });
  }
}

function folder(page: Page): Locator {
  return page.getByRole("textbox", { name: "Start folder" });
}
