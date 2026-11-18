// SPDX-License-Identifier: AGPL-3.0-or-later
// Restore from a file on the settings screen, against the real API on the fixture backend (whose
// restore service accepts a file that starts with its magic words and answers with the facts a
// step sets). Each test has a backend of its own.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

async function openRestore(page: Page, backend: Backend) {
  await backend.signIn(page);
  await page.goto("/settings");
  await expect(
    page.getByRole("heading", { level: 2, name: "Back up and restore" }),
  ).toBeVisible();
}

const backupFile = {
  name: "puddle-backup.tar.gz",
  mimeType: "application/gzip",
  buffer: Buffer.from("Puddle fake backup: a file the fixture accepts"),
};

test("a file that is not a backup is refused with the reason and nothing can be confirmed", async ({
  page,
  backend,
}) => {
  await openRestore(page, backend);
  await page.getByTestId("restore-file").setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("just some notes"),
  });
  await expect(page.getByTestId("restore-problem")).toContainText(
    "not a gzip archive",
  );
  await expect(page.getByTestId("restore-preview")).toHaveCount(0);
  await expect(page.getByTestId("restore-continue")).toHaveCount(0);
});

test("a preview shows what would change, then two confirms apply it and the page says it restarts", async ({
  page,
  backend,
}) => {
  await openRestore(page, backend);
  await backend.control.step({
    do: "restore_preview",
    missing_disks: ["gone"],
    secrets: ["TOKEN"],
  });
  await page.getByTestId("restore-file").setInputFiles(backupFile);
  const preview = page.getByTestId("restore-preview");
  await expect(preview).toBeVisible();
  await expect(page.getByTestId("restore-part-rules")).toContainText("Rules");
  await expect(page.getByTestId("restore-missing-disks")).toContainText("gone");
  await expect(page.getByTestId("restore-secrets")).toContainText("TOKEN");
  // Add what is missing is the default, and nothing has been applied yet.
  await expect(page.getByLabel(/Add what is missing/)).toBeChecked();
  await expect(page.getByTestId("restore-waiting")).toHaveCount(0);

  await page.getByTestId("restore-continue").click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toContainText("Add what is missing from the file.");
  await dialog.getByRole("button", { name: "Add and restart" }).click();
  await expect(page.getByTestId("restore-waiting")).toContainText(
    "Puddle is restarting to apply the file",
  );
  await expect(preview).toHaveCount(0);
});

test("Replace says what is dropped, and a restore that cannot start says why and can be tried again", async ({
  page,
  backend,
}) => {
  await openRestore(page, backend);
  await backend.control.step({ do: "restore_preview" });
  await backend.control.step({
    do: "restore_apply_fails",
    reason:
      "the safety copy of the present state could not be made, so the restore did not start",
  });
  await page.getByTestId("restore-file").setInputFiles(backupFile);
  await page.getByRole("radio", { name: /Replace/ }).check();
  await page.getByTestId("restore-continue").click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toContainText(
    "1 entries here are replaced and 3 that are not in the file are dropped",
  );
  await dialog.getByRole("button", { name: "Replace and restart" }).click();
  await expect(dialog.getByRole("alert")).toContainText("did not start");
  await expect(page.getByTestId("restore-preview")).toBeVisible();

  await backend.control.step({ do: "restore_apply_fails" });
  await dialog.getByRole("button", { name: "Replace and restart" }).click();
  await expect(page.getByTestId("restore-waiting")).toBeVisible();
});

test("after a restart the result is shown until dismissed, and safety copies are listed and deleted", async ({
  page,
  backend,
}) => {
  await backend.control.step({
    do: "restore_result",
    applied: true,
    message: "Restored from a backup (add what is missing).",
  });
  await backend.control.step({
    do: "safety_copies",
    names: ["20231114T221320Z", "20231001T000000Z"],
  });
  await openRestore(page, backend);
  const result = page.getByTestId("restore-result");
  await expect(result).toContainText("Restored from a backup");
  await page.getByRole("button", { name: "Dismiss" }).click();
  await expect(result).toHaveCount(0);

  const copy = page.getByTestId("safety-copy-20231001T000000Z");
  await expect(copy).toContainText("2023-10-01 00:00 UTC");
  await expect(copy).toContainText("2 KB");
  await copy.getByRole("button", { name: "Delete" }).click();
  await page
    .getByRole("alertdialog")
    .getByRole("button", { name: "Delete the copy" })
    .click();
  await expect(copy).toHaveCount(0);
  await expect(page.getByTestId("safety-copy-20231114T221320Z")).toBeVisible();
});

test("the restore part has no accessibility violations with a preview showing", async ({
  page,
  backend,
}) => {
  await openRestore(page, backend);
  await backend.control.step({
    do: "restore_preview",
    missing_disks: ["gone"],
    secrets: ["TOKEN"],
  });
  await page.getByTestId("restore-file").setInputFiles(backupFile);
  await expect(page.getByTestId("restore-preview")).toBeVisible();
  await expectNoAxeViolations(page, "#s-backup");
});
