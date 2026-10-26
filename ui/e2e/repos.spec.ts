// SPDX-License-Identifier: GPL-3.0-or-later
// The repository lists against the real API on the fixture backend, scenario `repo-lists`: five
// identities, each with a list in a different state (Work: GitHub with a note, plus an Azure
// DevOps organisation with a project that has a space; Personal: the rest of github.com;
// Contractor: a fine-grained token; Fresh: an account that reaches nothing; Busy: a rate-limited
// host showing an old list). Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeStops, watchCsp } from "./support";

const WORK = 1;
const PERSONAL = 2;
const CONTRACTOR = 3;
const FRESH = 4;
const BUSY = 5;

// The page's clock starts this much before the fixture's, so "in 10 minutes" holds while the test runs.
const BEHIND_MS = 30_000;

async function open(page: Page, backend: Backend, identity: number) {
  await backend.control.reset("repo-lists");
  await backend.installClock(page, BEHIND_MS);
  await backend.signIn(page);
  await page.goto(`/identities/${identity}`);
  await expect(
    page.getByRole("heading", { level: 2, name: "Repos it can reach" }),
  ).toBeVisible();
}

const repos = (page: Page) => page.getByTestId("identity-repos");
const rowOf = (page: Page, name: string) =>
  repos(page).getByRole("row", { name: new RegExp(`^${name} `) });
const create = (page: Page, name: string) =>
  page.getByRole("button", {
    name: `Create a workspace for this: ${name}`,
    exact: true,
  });
const dialog = (page: Page) =>
  page.getByRole("dialog", { name: "New workspace" });

test("lists what the identity reaches on every host, with role and traits, and says what the list leaves out", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await open(page, backend, WORK);
  await expect(rowOf(page, "acme/web-shop")).toContainText("admin");
  await expect(rowOf(page, "acme/design-tokens")).toContainText("public");
  await expect(rowOf(page, "acme/legacy-portal")).toContainText("archived");
  await expect(rowOf(page, "acme/web-shop-fork")).toContainText("fork");
  await expect(rowOf(page, "acme-labs/experiments")).toBeVisible();
  await expect(rowOf(page, "contoso/Platform/api")).toBeVisible();
  await expect(repos(page)).toContainText(
    "Organisations that restrict third-party apps",
  );
  await expect(repos(page)).toContainText("9 repositories.");
  const sources = repos(page).getByRole("list", {
    name: "How current each list is",
  });
  await expect(sources).toContainText("gh · tijs-work on github.com");
  await expect(sources).toContainText("dev.azure.com/contoso");
  await expect(sources).toContainText(/6 repositories, read [23] minutes ago/);
  expect(await csp()).toEqual([]);
});

test("a slow list shows progress, then the repositories when the host answers", async ({
  page,
  backend,
}) => {
  await backend.control.reset("repo-lists");
  await backend.control.script("hold_repos");
  await backend.installClock(page, BEHIND_MS);
  await backend.signIn(page);
  await page.goto(`/identities/${WORK}`);
  const summary = repos(page).getByTestId("repo-summary");
  await expect(summary).toContainText("Reading the lists from your Git hosts");
  await expect(summary).toContainText("Still asking your Git hosts");
  await expect(rowOf(page, "acme/web-shop")).toHaveCount(0);
  await backend.control.script("release_repos");
  await expect(rowOf(page, "acme/web-shop")).toBeVisible();
  await expect(summary).toContainText("9 repositories.");
});

test("a rate-limited host shows the old list, why, and when Puddle asks again", async ({
  page,
  backend,
}) => {
  await open(page, backend, BUSY);
  const sources = repos(page).getByRole("list", {
    name: "How current each list is",
  });
  await expect(sources).toContainText("Old list");
  await expect(sources).toContainText(/1[45] minutes ago/);
  await expect(sources).toContainText("Puddle asks again in 10 minutes");
  await expect(sources).toContainText(
    "GitHub says this account asked too often",
  );
  await expect(sources).toContainText(
    "This hour's request budget on github.com is used up",
  );
  await expect(rowOf(page, "busy-org/queue")).toBeVisible();
  await expect(create(page, "busy-org/queue")).toBeEnabled();
});

test("a fine-grained token's list says it is limited to what was granted", async ({
  page,
  backend,
}) => {
  await open(page, backend, CONTRACTOR);
  await expect(repos(page)).toContainText(
    "lists only the repositories it was granted",
  );
  await expect(rowOf(page, "northwind/ledger")).toBeVisible();
});

test("an account that reaches nothing says so", async ({ page, backend }) => {
  await open(page, backend, FRESH);
  await expect(repos(page).getByTestId("repo-empty")).toContainText(
    "reaches no repository",
  );
  await expect(repos(page).getByRole("table")).toHaveCount(0);
});

test("an Azure DevOps project with a space is listed, with what that costs and the way out", async ({
  page,
  backend,
}) => {
  await open(page, backend, WORK);
  const row = rowOf(page, "contoso/Shop Floor/scanner");
  await expect(row.getByTestId("repo-table-warning")).toContainText(
    "turn that off on the workspace's Git tab",
  );
  await create(page, "contoso/Shop Floor/scanner").click();
  await expect(dialog(page).getByLabel("Git repository (HTTPS)")).toHaveValue(
    "https://dev.azure.com/contoso/Shop%20Floor/_git/scanner",
  );
  await expect(dialog(page).getByTestId("table-cannot-hold")).toBeVisible();
});

test("Refresh asks the host again and shows what changed", async ({
  page,
  backend,
}) => {
  await open(page, backend, PERSONAL);
  await expect(rowOf(page, "tijs-demo/docs-site")).toBeVisible();
  await expect(rowOf(page, "tijs-demo/new-idea")).toHaveCount(0);
  await backend.control.script("personal_gains_a_repository");
  await repos(page).getByRole("button", { name: "Refresh" }).click();
  await expect(rowOf(page, "tijs-demo/new-idea")).toBeVisible();
});

test("searches the identity's repositories by words", async ({
  page,
  backend,
}) => {
  await open(page, backend, WORK);
  await repos(page).getByLabel("Search these repositories").fill("acme web");
  await expect(repos(page).getByTestId("repo-summary")).toContainText(
    "2 repositories match",
  );
  await expect(rowOf(page, "acme/billing")).toHaveCount(0);
  await repos(page)
    .getByLabel("Search these repositories")
    .fill("nothing like it");
  await expect(repos(page).getByTestId("repo-summary")).toContainText(
    "No repository matches",
  );
});

test("Create a workspace for this opens the form filled in, with the identity that listed it and the others that cover it, and makes the workspace with them", async ({
  page,
  backend,
}) => {
  await open(page, backend, WORK);
  await create(page, "acme/billing").click();
  const form = dialog(page);
  await expect(form.getByLabel("Git repository (HTTPS)")).toHaveValue(
    "https://github.com/acme/billing",
  );
  await expect(form.getByLabel("Name")).toHaveValue("billing");
  const chosen = form.getByLabel("Git identity").locator("option:checked");
  await expect(chosen).toHaveText("Work (listed this repository)");
  await expect(
    form.locator('optgroup[label="Cover this repository"]'),
  ).toContainText("Personal");
  await form.getByRole("button", { name: "Create workspace" }).click();
  await expect(page).toHaveURL(/\/workspaces\/billing$/);
  await page.goto("/workspaces/billing/git");
  const ids = page.getByRole("list", {
    name: "Identities of billing, in order",
  });
  await expect(ids.getByRole("listitem")).toHaveCount(1);
  await expect(ids.getByRole("listitem").first()).toContainText("Work");
});

test("the create form's picker searches every identity's repositories and fills the form", async ({
  page,
  backend,
}) => {
  await backend.control.reset("repo-lists");
  await backend.installClock(page, BEHIND_MS);
  await backend.signIn(page);
  await page.goto("/workspaces");
  await page.getByRole("button", { name: "New workspace" }).first().click();
  const form = dialog(page);
  await form.getByText("Choose from your repositories").click();
  await expect(
    form.getByRole("button", { name: /northwind\/ledger/ }),
  ).toBeVisible();
  await form.getByLabel("Search your repositories").fill("tijs-demo dot");
  await expect(
    form.getByRole("button", { name: /tijs-demo\/docs-site/ }),
  ).toHaveCount(0);
  await form.getByRole("button", { name: /tijs-demo\/dotfiles/ }).click();
  await expect(form.getByLabel("Git repository (HTTPS)")).toHaveValue(
    "https://github.com/tijs-demo/dotfiles",
  );
  await expect(form.getByLabel("Name")).toBeFocused();
  await expect(
    form.getByLabel("Git identity").locator("option:checked"),
  ).toHaveText(/Personal/);
  // A typed address still works, and the identities follow it.
  await form
    .getByLabel("Git repository (HTTPS)")
    .fill("https://github.com/acme/web-shop");
  await expect(
    form.getByLabel("Git identity").locator("option:checked"),
  ).toHaveText("Work");
});

test("the create form's picker says which lists failed or are limited", async ({
  page,
  backend,
}) => {
  await backend.control.reset("repo-lists");
  await backend.installClock(page, BEHIND_MS);
  await backend.signIn(page);
  await page.goto("/workspaces");
  await page.getByRole("button", { name: "New workspace" }).first().click();
  const form = dialog(page);
  await form.getByText("Choose from your repositories").click();
  await expect(
    form.getByText("GitHub says this account asked too often"),
  ).toBeVisible();
  await expect(
    form.getByText(/lists only the repositories it was granted/),
  ).toBeVisible();
});

test("keyboard only: from the repository row to the finished form", async ({
  page,
  backend,
}) => {
  await open(page, backend, PERSONAL);
  await create(page, "tijs-demo/dotfiles").focus();
  await page.keyboard.press("Enter");
  const form = dialog(page);
  await expect(form.getByLabel("Git repository (HTTPS)")).toBeFocused();
  await expect(form.getByLabel("Git repository (HTTPS)")).toHaveValue(
    "https://github.com/tijs-demo/dotfiles",
  );
  await page.keyboard.press("Escape");
  await expect(form).toBeHidden();
  await expect(create(page, "tijs-demo/dotfiles")).toBeFocused();

  // Open the picker with the keyboard from the workspaces list.
  await page.goto("/workspaces");
  await page.getByRole("button", { name: "New workspace" }).first().focus();
  await page.keyboard.press("Enter");
  await form.getByText("Choose from your repositories").focus();
  await page.keyboard.press("Enter");
  const search = form.getByLabel("Search your repositories");
  await expect(search).toBeVisible();
  await search.focus();
  await page.keyboard.type("dotfiles");
  // The list is read after the picker opens: until it arrives "no docs-site" is true of an empty
  // list too, and a Tab then leaves the search for the next control. Wait for the match itself.
  const dotfiles = form.getByRole("button", { name: /tijs-demo\/dotfiles/ });
  await expect(dotfiles).toBeVisible();
  await expect(
    form.getByRole("button", { name: /tijs-demo\/docs-site/ }),
  ).toHaveCount(0);
  await page.keyboard.press("Tab");
  await expect(dotfiles).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(form.getByLabel("Name")).toBeFocused();
  await expect(form.getByLabel("Git repository (HTTPS)")).toHaveValue(
    "https://github.com/tijs-demo/dotfiles",
  );
});

// The states are scanned at most two to a test: a scan costs seconds on a loaded host.
const stops = axeStops([
  ["work", "rate limited"],
  ["empty", "form with identities"],
  ["form with the picker open"],
]);
for (const scheme of ["light", "dark"] as const) {
  for (const group of stops.groups) {
    test(`axe finds nothing in ${scheme}: ${group.join(", ")}`, async ({
      page,
      backend,
    }) => {
      const scan = stops.scanner(group);
      await page.emulateMedia({ colorScheme: scheme });
      await open(page, backend, WORK);
      await expect(rowOf(page, "acme/web-shop")).toBeVisible();
      await scan(page, "work");

      await page.goto(`/identities/${BUSY}`);
      await expect(rowOf(page, "busy-org/queue")).toBeVisible();
      await scan(page, "rate limited");

      await page.goto(`/identities/${FRESH}`);
      await expect(repos(page).getByTestId("repo-empty")).toBeVisible();
      await scan(page, "empty");

      await page.goto(`/identities/${WORK}`);
      await create(page, "acme/web-shop").click();
      await expect(
        dialog(page).getByLabel("Git identity").locator("option:checked"),
      ).toHaveText(/Work/);
      await scan(page, "form with identities");
      await dialog(page).getByText("Choose from your repositories").click();
      await expect(
        dialog(page).getByRole("button", { name: /northwind\/ledger/ }),
      ).toBeVisible();
      await scan(page, "form with the picker open");
      scan.done();
    });
  }
}
