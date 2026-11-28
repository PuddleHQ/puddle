// SPDX-License-Identifier: AGPL-3.0-or-later
// What every workspace screen spec shares against the fixture backend (fake workspace service):
// the API reads, opening the list or a detail page, and the lookups for a row, a card and a toast.
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";

export const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

export interface Ws {
  id: string;
  name: string;
  status: string;
  busy: string | null;
  direct_ssh: boolean;
  memory_mib: number;
  repositories: { url: string }[];
}

export async function workspaceList(
  request: APIRequestContext,
  backend: Backend,
): Promise<Ws[]> {
  const response = await request.get("/api/workspaces", {
    headers: auth(backend),
  });
  return ((await response.json()) as { workspaces: Ws[] }).workspaces;
}

export async function pendingCount(
  request: APIRequestContext,
  backend: Backend,
): Promise<number> {
  const response = await request.get("/api/pending", {
    headers: auth(backend),
  });
  return ((await response.json()) as { requests: unknown[] }).requests.length;
}

/** Every test starts from the `lived-in` scenario on its worker's own backend. */
export function installReset() {
  test.beforeEach(async ({ backend }) => {
    await backend.control.reset("lived-in");
  });
}

export async function openList(
  page: Page,
  backend: Backend,
  path = "/workspaces",
) {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto(path);
  await expect(
    page.getByRole("heading", { level: 1, name: "Workspaces" }),
  ).toBeVisible();
}

export const card = (page: Page, name: string): Locator =>
  page.getByRole("article", { name });

/** A row's name link: the way into the workspace (the row has no separate "Details"). */
export const nameLink = (row: Locator, name: string): Locator =>
  row.getByRole("link", { name, exact: true });

/** Opens the "more" menu of a row and chooses Stop: the row's one action is Start or Connect. */
export async function stopFromRow(page: Page, row: Locator, name: string) {
  await row.getByRole("button", { name: `More about ${name}` }).click();
  await page.getByRole("menuitem", { name: `Stop ${name}` }).click();
}

export const toast = (page: Page, text: string | RegExp): Locator =>
  page.getByRole("status").filter({ hasText: text });

export async function openDetail(
  page: Page,
  backend: Backend,
  name: string,
  tab = "",
): Promise<void> {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto(`/workspaces/${name}${tab ? `/${tab}` : ""}`);
  await expect(
    page.getByRole("heading", { level: 1, name, exact: true }),
  ).toBeVisible();
}
