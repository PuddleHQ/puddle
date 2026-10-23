// SPDX-License-Identifier: GPL-3.0-or-later
// The create form with a list of repositories, on the fixture backend (scenario `lived-in`): rows
// added and removed by keyboard, a directory clash flagged before the submit, the progress line
// naming each clone, and the notice for a clone that failed while the create still finished.
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeStops, watchCsp } from "./support";

const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

interface Made {
  name: string;
  start_folder: string | null;
  repositories: { url: string; directory: string }[];
}

async function workspace(
  request: APIRequestContext,
  backend: Backend,
  name: string,
): Promise<Made> {
  const response = await request.get(`/api/workspaces/${name}`, {
    headers: auth(backend),
  });
  return (await response.json()) as Made;
}

test.beforeEach(async ({ backend }) => {
  await backend.control.reset("lived-in");
});

async function openList(page: Page, backend: Backend) {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto("/workspaces");
  await expect(
    page.getByRole("heading", { level: 1, name: "Workspaces" }),
  ).toBeVisible();
}

const card = (page: Page, name: string): Locator =>
  page.getByRole("article", { name });
const dialogOf = (page: Page): Locator =>
  page.getByRole("dialog", { name: "New workspace" });
const rowOf = (page: Page, n: number): Locator =>
  dialogOf(page).getByRole("group", { name: `Repository ${n}` });
const addrOf = (page: Page, n: number): Locator =>
  rowOf(page, n).getByLabel("Git repository (HTTPS)");
const addButton = (page: Page): Locator =>
  dialogOf(page).getByRole("button", { name: /^Add (another|a) repository$/ });

async function openCreate(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: "New workspace" }).first().click();
  await expect(dialogOf(page)).toBeVisible();
  return dialogOf(page);
}

const API = "https://github.com/acme/api.git";
const WEB = "https://github.com/acme/web.git";
const CLI = "https://github.com/acme/cli.git";
const DOCS = "https://gitlab.com/acme/docs.git";

test.describe("the list of repositories", () => {
  test("keyboard only: three repositories with a row added, one removed and focus following", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({ do: "hold_workspaces" });
    await page.getByRole("button", { name: "New workspace" }).first().focus();
    await page.keyboard.press("Enter");
    await expect(addrOf(page, 1)).toBeFocused();
    await page.keyboard.type(API);

    // Add another repository: focus lands on the new row's address.
    await addButton(page).focus();
    await page.keyboard.press("Enter");
    await expect(addrOf(page, 2)).toBeFocused();
    await page.keyboard.type(WEB);
    await addButton(page).focus();
    await page.keyboard.press("Space");
    await expect(addrOf(page, 3)).toBeFocused();
    await page.keyboard.type(CLI);
    // Tab: the picker's summary, then the directory.
    await page.keyboard.press("Tab");
    await expect(
      rowOf(page, 3).getByText("Choose from your repositories"),
    ).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(rowOf(page, 3).getByLabel("Directory")).toBeFocused();
    await page.keyboard.type("cli-tool");

    // Remove the middle row: focus moves to the next row's address, which is row 2 now.
    await rowOf(page, 2)
      .getByRole("button", { name: "Remove repository 2" })
      .focus();
    await page.keyboard.press("Enter");
    await expect(rowOf(page, 3)).toHaveCount(0);
    await expect(addrOf(page, 2)).toBeFocused();
    await expect(addrOf(page, 2)).toHaveValue(CLI);

    // And one more, so the list has three again.
    await addButton(page).focus();
    await page.keyboard.press("Enter");
    await expect(addrOf(page, 3)).toBeFocused();
    await page.keyboard.type(DOCS);
    await expect(dialogOf(page).getByLabel("Name")).toHaveValue("api");
    await page.keyboard.press("Enter");
    await expect(dialogOf(page)).toHaveCount(0);

    const created = card(page, "api");
    await expect(created).toContainText("Creating");
    await expect(created).toContainText(`${API} and 2 more`);
    // The service names each clone as it goes.
    for (const [n, dir] of [
      [1, "api"],
      [2, "cli-tool"],
      [3, "docs"],
    ] as const) {
      await backend.control.emit({
        type: "workspace_progress",
        workspace: "api",
        step: "cloning",
        detail: [API, CLI, DOCS][n - 1],
        repository: dir,
      });
      await expect(created.getByRole("status")).toContainText(
        `Cloning ${dir}, ${n} of 3`,
      );
    }
    await backend.control.step({ do: "release_workspaces" });
    await expect(created).toContainText("Not started");
    const made = await workspace(request, backend, "api");
    expect(made.repositories).toEqual([
      { url: API, directory: "api" },
      { url: CLI, directory: "cli-tool" },
      { url: DOCS, directory: "docs" },
    ]);
  });

  test("an empty list makes an ordinary workspace, and the card says so", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await dialog.getByRole("button", { name: "Remove repository 1" }).click();
    await expect(dialog).toContainText(
      "No repository: this makes an ordinary workspace",
    );
    await expect(addButton(page)).toBeFocused();
    await expect(addButton(page)).toHaveText("Add a repository");
    await dialog.getByLabel("Name").fill("scratch");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog).toHaveCount(0);
    await expect(card(page, "scratch")).toContainText("No repository");
    expect((await workspace(request, backend, "scratch")).repositories).toEqual(
      [],
    );
  });

  test("a directory two rows share is flagged before the submit, with a free name, and clears when one changes", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await addrOf(page, 1).fill(API);
    await addButton(page).click();
    await addrOf(page, 2).fill("https://gitlab.com/other/api.git");
    for (const n of [1, 2]) {
      await expect(rowOf(page, n).getByLabel("Directory")).toHaveAttribute(
        "aria-invalid",
        "true",
      );
    }
    await expect(rowOf(page, 2).getByRole("alert")).toHaveText(
      "Repository 1, the earlier one, also goes into api. Use another directory, such as api-2.",
    );
    // Computed styles: the flagged field and its message carry the danger colour, a clean one does not.
    const colours = await rowOf(page, 2).evaluate((row) => {
      const bad = row.querySelector<HTMLInputElement>(
        'input[aria-invalid="true"]',
      )!;
      const good = row.querySelector<HTMLInputElement>(
        "input:not([aria-invalid])",
      )!;
      const message = row.querySelector<HTMLElement>("p.error")!;
      return {
        bad: getComputedStyle(bad).borderTopColor,
        good: getComputedStyle(good).borderTopColor,
        message: getComputedStyle(message).color,
      };
    });
    expect(colours.bad).not.toBe(colours.good);
    expect(colours.message).toBe(colours.bad);

    // Submitting changes nothing and puts focus on the first flagged field.
    const before = (
      await (
        await request.get("/api/workspaces", { headers: auth(backend) })
      ).json()
    ).workspaces.length as number;
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(rowOf(page, 1).getByLabel("Directory")).toBeFocused();
    expect(
      (
        await (
          await request.get("/api/workspaces", { headers: auth(backend) })
        ).json()
      ).workspaces.length,
    ).toBe(before);

    await rowOf(page, 2).getByLabel("Directory").fill("api-2");
    await expect(rowOf(page, 1).getByLabel("Directory")).not.toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await expect(rowOf(page, 2).getByLabel("Directory")).not.toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog).toHaveCount(0);
    const made = await workspace(request, backend, "api");
    expect(made.repositories.map((r) => r.directory)).toEqual(["api", "api-2"]);
  });

  test("the same repository twice and a start folder that leaves the disk are refused before anything is made", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await addrOf(page, 1).fill("https://github.com/Acme/API.git");
    await addButton(page).click();
    await addrOf(page, 2).fill("https://github.com/acme/api/");
    await rowOf(page, 2).getByLabel("Directory").fill("again");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(addrOf(page, 2)).toBeFocused();
    await expect(rowOf(page, 2).getByRole("alert")).toHaveText(
      "This is the same repository as repository 1. List it once.",
    );
    await dialog.getByRole("button", { name: "Remove repository 2" }).click();
    await dialog.getByText("Start folder, image and memory").click();
    await dialog.getByLabel("Start folder").fill("../outside");
    await expect(dialog.getByLabel("Start folder")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog.getByLabel("Start folder")).toBeFocused();
    await expect(dialog).toBeVisible();
  });

  test("the identity of a row, its switches and the start folder go to the service", async ({
    page,
    request,
    backend,
  }) => {
    await backend.control.reset("git-identities");
    await openList(page, backend);
    const dialog = await openCreate(page);
    await addrOf(page, 1).fill("https://github.com/acme/ledger.git");
    await expect(
      rowOf(page, 1).getByLabel("Git identity").locator("option:checked"),
    ).toHaveText("Work");
    await addButton(page).click();
    await addrOf(page, 2).fill("https://github.com/someone/notes.git");
    await expect(
      rowOf(page, 2).getByLabel("Git identity").locator("option:checked"),
    ).toHaveText("Personal");
    await rowOf(page, 2).getByRole("checkbox", { name: "Push" }).uncheck();
    await dialog.getByText("Start folder, image and memory").click();
    await dialog.getByLabel("Start folder").fill("ledger");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog).toHaveCount(0);
    const made = await workspace(request, backend, "ledger");
    expect(made.start_folder).toBe("ledger");
    await page.goto("/workspaces/ledger/git");
    await expect(
      page.getByRole("checkbox", { name: "Push github.com/someone/notes" }),
    ).not.toBeChecked();
    await expect(
      page.getByRole("checkbox", { name: "Pull github.com/someone/notes" }),
    ).toBeChecked();
    await expect(
      page.getByRole("list", { name: "Identities of ledger, in order" }),
    ).toContainText("Work");
    await expect(
      page.getByRole("list", { name: "Identities of ledger, in order" }),
    ).toContainText("Personal");
  });

  test("the controls are big enough to hit (WCAG 2.5.8) and the row's fields do not overflow the dialog", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await addButton(page).click();
    const edge = (await dialog.boundingBox())!;
    const right = edge.x + edge.width;
    const controls = [
      addButton(page),
      rowOf(page, 1).getByRole("button", { name: "Remove repository 1" }),
      rowOf(page, 2).getByRole("button", { name: "Remove repository 2" }),
      rowOf(page, 2).getByLabel("Git identity"),
      rowOf(page, 2).getByLabel("Directory"),
      rowOf(page, 2).getByLabel("Git repository (HTTPS)"),
      // A checkbox is small by itself; its label is the target.
      rowOf(page, 2).locator("label.choice", { hasText: "Pull" }),
      rowOf(page, 2).locator("label.choice", { hasText: "Push" }),
    ];
    for (const control of controls) {
      await control.scrollIntoViewIfNeeded();
      const box = (await control.boundingBox())!;
      expect(box.width).toBeGreaterThanOrEqual(24);
      expect(box.height).toBeGreaterThanOrEqual(24);
      expect(box.x + box.width).toBeLessThanOrEqual(right);
    }
  });
});

test.describe("a clone that fails", () => {
  test("is a notice naming the repository and git's reason as plain text, with the workspace as the way out, and goes away with its cause", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await addrOf(page, 1).fill(API);
    await addButton(page).click();
    await addrOf(page, 2).fill(WEB);
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog).toHaveCount(0);
    // Both clones ran in the fake; the host would have raised this for the one that failed.
    await expect(card(page, "api")).toContainText("Not started");
    await backend.control.step({
      do: "problem",
      key: "clone:api:web",
      workspace: "api",
      title: `Could not clone ${WEB}`,
      detail: "fatal: repository not found <img src=x onerror=alert(1)>",
    });
    const notices = page.getByRole("region", { name: "Notices" });
    await expect(notices).toContainText(`Could not clone ${WEB}`);
    await expect(notices).toContainText("fatal: repository not found");
    // Guest text is text: no element came out of it.
    await expect(notices.locator("img")).toHaveCount(0);
    await notices.getByRole("link", { name: "Open workspace" }).click();
    await expect(page).toHaveURL(/\/workspaces\/api$/);
    // The create itself finished: the workspace is not busy and keeps both entries.
    await expect(
      page.getByRole("heading", { level: 1, name: "api", exact: true }),
    ).toBeVisible();
    await backend.control.step({ do: "clear_problem", key: "clone:api:web" });
    await expect(notices).not.toContainText("Could not clone");
  });
});

test.describe("the card", () => {
  test("shows the first repository and how many more, or that there is none", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await expect(card(page, "web-shop")).toContainText(
      "https://github.com/acme/web-shop.git",
    );
    await expect(card(page, "web-shop")).not.toContainText("more");
    const dialog = await openCreate(page);
    await addrOf(page, 1).fill(API);
    await addButton(page).click();
    await addrOf(page, 2).fill(WEB);
    await addButton(page).click();
    await addrOf(page, 3).fill(CLI);
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(card(page, "api")).toContainText(`${API} and 2 more`);
  });
});

const stops = axeStops([
  ["two rows with a clash"],
  ["no repository", "refused"],
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
      await openList(page, backend);
      const dialog = await openCreate(page);
      await addrOf(page, 1).fill(API);
      await addButton(page).click();
      await addrOf(page, 2).fill("https://gitlab.com/o/api.git");
      await rowOf(page, 2).getByText("Choose from your repositories").click();
      await expect(rowOf(page, 2).getByLabel("Directory")).toHaveAttribute(
        "aria-invalid",
        "true",
      );
      await scan(page, "two rows with a clash");

      await dialog.getByRole("button", { name: "Create workspace" }).click();
      await expect(rowOf(page, 1).getByLabel("Directory")).toBeFocused();
      await scan(page, "refused");
      await rowOf(page, 2)
        .getByRole("button", { name: /^Remove/ })
        .click();
      await rowOf(page, 1)
        .getByRole("button", { name: /^Remove/ })
        .click();
      await expect(dialog).toContainText("No repository");
      await scan(page, "no repository");
      scan.done();
      expect(await csp()).toEqual([]);
    });
  }
}
