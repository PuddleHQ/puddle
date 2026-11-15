// SPDX-License-Identifier: AGPL-3.0-or-later
// A tab that runs another build than the server serves reloads once into the server's build. The
// server's answer is rewritten in the browser to name a newer build, as it would after an upgrade;
// everything else is the real app against the fixture backend. Waits are on conditions.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";

const RELOADED_FOR = "puddle.reloaded-for-build";

/** Counts the page's loads and, when `served` is given, makes `/api/health` name that build. */
async function watch(page: Page, served: string | null) {
  const counts = { loads: 0, health: 0 };
  page.on("load", () => (counts.loads += 1));
  await page.route("**/api/health", async (route) => {
    counts.health += 1;
    if (served === null) return route.continue();
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    await route.fulfill({
      response,
      json: { ...body, ui_build_id: served },
    });
  });
  return counts;
}

async function open(page: Page, backend: Backend) {
  await backend.control.reset("lived-in");
  await backend.signIn(page);
  await page.goto("/inbox");
}

test("a tab on another build than the server's reloads once and stays on the page", async ({
  page,
  backend,
}) => {
  const counts = await watch(page, "a-newer-build");
  await open(page, backend);
  await expect.poll(() => counts.loads).toBe(2);
  await expect(
    page.getByRole("link", { name: "Skip to content" }),
  ).toBeAttached();
  // The second load asked the server again and, still not on that build, did not reload a third time.
  await expect.poll(() => counts.health).toBeGreaterThanOrEqual(2);
  await expect
    .poll(() =>
      page.evaluate((key) => sessionStorage.getItem(key), RELOADED_FOR),
    )
    .toBe("a-newer-build");
  expect(counts.loads).toBe(2);
});

test("a server that serves no app build leaves the tab alone", async ({
  page,
  backend,
}) => {
  const counts = await watch(page, null);
  await open(page, backend);
  await expect(
    page.getByRole("link", { name: "Skip to content" }),
  ).toBeAttached();
  await expect.poll(() => counts.health).toBeGreaterThanOrEqual(1);
  expect(counts.loads).toBe(1);
});
