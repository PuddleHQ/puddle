// SPDX-License-Identifier: AGPL-3.0-or-later
// The full-page "New workspace" route, scanned by axe empty and opened from a repository, on the
// fixture backend (the dialog on /workspaces is the same form).
import { expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

test.describe("new workspace page", () => {
  test("has no axe violations, empty and with a repository filled in", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("git-identities");
    await backend.signIn(page);

    await page.goto("/workspaces/new");
    await expect(
      page.getByRole("region", { name: "New workspace" }),
    ).toBeVisible();
    await expectNoAxeViolations(page, "empty form");

    await page.goto(
      `/workspaces/new?url=${encodeURIComponent("https://github.com/acme/fresh.git")}`,
    );
    await expect(page.getByLabel("Git repository (HTTPS)")).toHaveValue(
      "https://github.com/acme/fresh.git",
    );
    await expectNoAxeViolations(page, "form opened from a repository");
  });
});
