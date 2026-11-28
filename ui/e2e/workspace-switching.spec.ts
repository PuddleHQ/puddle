// SPDX-License-Identifier: AGPL-3.0-or-later
// Switching between two running workspaces: A to B to A. Each stop shows the workspace it is for:
// the heading, the terminal (the fixture's shell answers `hostname` with the id of the workspace it
// belongs to) and the editor request (the browser editor's attach call goes to that workspace's
// id). The names differ from the ids, so a page that mixed the two up shows the wrong one.
import type { Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";

const alpha = { name: "Alpha Shop", id: "alpha-shop" };
const beta = { name: "Beta Docs", id: "beta-docs" };
type Workspace = typeof alpha;

const screen = (page: Page): Locator => page.locator(".xterm-rows");
const input = (page: Page): Locator => page.locator(".xterm-helper-textarea");

async function openList(page: Page, backend: Backend) {
  await backend.control.reset("two-running");
  await backend.signIn(page);
  await page.goto("/workspaces");
  await expect(
    page.getByRole("heading", { level: 1, name: "Workspaces" }),
  ).toBeVisible();
}

/** Back to the list from a workspace page without a reload, so the page's stores live on. */
async function toList(page: Page) {
  await page
    .getByRole("link", { name: "Workspaces", exact: true })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { level: 1, name: "Workspaces" }),
  ).toBeVisible();
}

/** Into a workspace from the list, by its name link, then to its terminal tab. */
async function enterTerminal(page: Page, w: Workspace) {
  await page.getByRole("link", { name: w.name, exact: true }).click();
  await expect(
    page.getByRole("heading", { level: 1, name: w.name, exact: true }),
  ).toBeVisible();
  await page
    .getByRole("navigation", { name: "Workspace sections" })
    .getByRole("link", { name: "Terminal" })
    .click();
  await expect(page).toHaveURL(new RegExp(`/workspaces/${w.id}/terminal$`));
}

/** Opens a terminal if the workspace has none yet, and asks it which workspace it belongs to. */
async function askTerminal(page: Page, w: Workspace) {
  const first = page.getByRole("tab", { name: "Terminal 1" });
  if ((await first.count()) === 0) {
    await page.getByRole("button", { name: "New terminal" }).click();
    await expect(first).toBeVisible();
  }
  await expect(screen(page)).toContainText("$");
  await input(page).focus();
  await page.keyboard.type("hostname");
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText(w.id);
}

/** What the page shows for the workspace it is on: heading, and a terminal that is that one's. */
async function expectShows(page: Page, w: Workspace, other: Workspace) {
  await expect(
    page.getByRole("heading", { level: 1, name: w.name, exact: true }),
  ).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`/workspaces/${w.id}/terminal$`));
  await expect(screen(page)).toContainText(w.id);
  await expect(screen(page)).not.toContainText(other.id);
}

/** From the list, the connect step's browser editor: the request names the workspace's id. */
async function openEditor(page: Page, w: Workspace) {
  await page.getByRole("button", { name: `Connect to ${w.name}` }).click();
  const attach = page.waitForRequest(
    (r) => r.method() === "POST" && r.url().includes("/attach"),
  );
  await page
    .getByRole("dialog", { name: `Connect to ${w.name}` })
    .getByRole("button", { name: "Open isolated VS Code" })
    .click();
  const request = await attach;
  expect(new URL(request.url()).pathname).toBe(
    `/api/workspaces/${w.id}/attach`,
  );
  expect(request.postDataJSON()).toEqual({ mode: "browser" });
  await expect(
    page.getByRole("status").filter({ hasText: `Opening ${w.name} in` }),
  ).toBeVisible();
}

test("switching from one running workspace to the other and back shows the right workspace each time: heading, terminal and editor request", async ({
  page,
  backend,
}) => {
  await openList(page, backend);

  await enterTerminal(page, alpha);
  await askTerminal(page, alpha);
  await expectShows(page, alpha, beta);

  await toList(page);
  await enterTerminal(page, beta);
  await askTerminal(page, beta);
  await expectShows(page, beta, alpha);

  // Back to the first: its own session is still there, and nothing of the second shows.
  await toList(page);
  await enterTerminal(page, alpha);
  await expectShows(page, alpha, beta);
  await expect(page.getByRole("tab", { name: "Terminal 1" })).toBeVisible();

  await toList(page);
  await openEditor(page, alpha);
  await openEditor(page, beta);
  await openEditor(page, alpha);
});
