// SPDX-License-Identifier: AGPL-3.0-or-later
// The workspace disks the workspace list does not name (fixture scenario `lived-in`): the notice
// on the workspace list, restoring one by keyboard, and the question for a repository address when
// the disk gave none. The notice never says a disk holds files and offers no way to delete one.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

const notice = (page: Page) =>
  page.getByRole("region", { name: /workspace disks? (is|are) not in your/ });
const restore = (page: Page, volume: string) =>
  notice(page).getByRole("button", {
    name: `Restore as workspace ${volume}`,
  });

async function open(page: Page, backend: Backend) {
  await backend.control.reset("lived-in");
  await backend.control.step({
    do: "orphan_volume",
    id: "lost",
    remote: "https://github.com/acme/lost.git",
  });
  await backend.control.step({ do: "orphan_volume", id: "bare" });
  await backend.signIn(page);
  await page.goto("/workspaces");
  await expect(notice(page)).toBeVisible();
}

test("the notice names each disk, claims nothing about its files and offers no delete", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await expect(
    notice(page).getByRole("heading", {
      name: "2 workspace disks are not in your workspace list",
    }),
  ).toBeVisible();
  await expect(notice(page)).toContainText("ws-lost");
  await expect(notice(page)).toContainText("ws-bare");
  await expect(notice(page)).toContainText("cannot tell from outside");
  await expect(
    notice(page).getByRole("button", { name: /delete|remove/i }),
  ).toHaveCount(0);
  await expectNoAxeViolations(page);
});

test("a disk is restored by keyboard, becomes a stopped workspace, and leaves the notice", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await restore(page, "ws-lost").focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("status").filter({ hasText: "Restored lost" }),
  ).toHaveText(
    "Restored lost as a workspace. It is stopped; start it to use it.",
  );
  await expect(notice(page).getByText("ws-lost")).toHaveCount(0);
  await expect(notice(page).getByText("ws-bare")).toBeVisible();
  await expect(
    notice(page).getByRole("heading", {
      name: "A workspace disk is not in your workspace list",
    }),
  ).toBeFocused();
  await expect(page.getByRole("list", { name: "Workspaces" })).toContainText(
    "lost",
  );
  await expectNoAxeViolations(page);
});

test("a disk with no readable address asks for one, and the last restore ends the notice", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await restore(page, "ws-lost").click();
  await expect(notice(page).getByText("ws-lost")).toHaveCount(0);
  await restore(page, "ws-bare").click();
  await expect(page.getByRole("alert")).toContainText("enter the address");
  const field = page.getByLabel("Repository address for bare");
  await expect(field).toBeVisible();
  await expectNoAxeViolations(page);
  await field.fill("https://github.com/acme/web.git");
  await restore(page, "ws-bare").click();
  await expect(notice(page)).toHaveCount(0);
  await expect(page.getByRole("list", { name: "Workspaces" })).toContainText(
    "bare",
  );
});
