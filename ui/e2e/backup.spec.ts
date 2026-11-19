// SPDX-License-Identifier: AGPL-3.0-or-later
// Back up now on the settings screen, against the real API on the fixture backend (whose backup
// service serves a stand-in file). Each test has a backend of its own.
import { readFileSync } from "node:fs";
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

async function openBackup(page: Page, backend: Backend) {
  await backend.signIn(page);
  await page.goto("/settings/this-computer");
  await expect(
    page.getByRole("heading", { level: 2, name: "Back up and restore" }),
  ).toBeVisible();
}

test("Back up now downloads a named file and says it is saved", async ({
  page,
  backend,
}) => {
  await openBackup(page, backend);
  const download = page.waitForEvent("download");
  await page.getByTestId("back-up-now").click();
  const file = await download;
  expect(file.suggestedFilename()).toMatch(
    /^puddle-backup-\d{8}T\d{6}Z\.tar\.gz$/,
  );
  const path = await file.path();
  expect(readFileSync(path, "utf8")).toContain("fake backup");
  const done = page.getByTestId("backup-done");
  await expect(done).toContainText(file.suggestedFilename());
  await expect(done).toContainText("bytes");
  await expect(page.getByTestId("backup-problem")).toHaveCount(0);
});

test("a backup that cannot be made says why, saves nothing and works again after", async ({
  page,
  backend,
}) => {
  await openBackup(page, backend);
  await backend.control.step({
    do: "backup_fails",
    reason: "no database at /data/puddle.db",
  });
  let downloads = 0;
  page.on("download", () => (downloads += 1));
  await page.getByTestId("back-up-now").click();
  await expect(page.getByTestId("backup-problem")).toContainText(
    "No database at /data/puddle.db.",
  );
  expect(downloads).toBe(0);
  await expect(page.getByTestId("backup-done")).toHaveCount(0);

  await backend.control.step({ do: "backup_fails" });
  const download = page.waitForEvent("download");
  await page.getByTestId("back-up-now").click();
  await download;
  await expect(page.getByTestId("backup-done")).toBeVisible();
  await expect(page.getByTestId("backup-problem")).toHaveCount(0);
});

test("the backup card has no accessibility violations, with a result showing", async ({
  page,
  backend,
}) => {
  await openBackup(page, backend);
  const download = page.waitForEvent("download");
  await page.getByTestId("back-up-now").click();
  await download;
  await expect(page.getByTestId("backup-done")).toBeVisible();
  await expectNoAxeViolations(page, "#s-backup");
});
