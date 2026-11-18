// SPDX-License-Identifier: AGPL-3.0-or-later
// A tab that runs another build than the server serves reloads once into the server's build. The
// server's answer is rewritten in the browser to name a newer build, as it would after an upgrade;
// everything else is the real app against the fixture backend. Waits are on conditions.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { staysEmpty } from "./support";

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

test("a tab on another build waits while the settings form has unsaved edits, then reloads", async ({
  page,
  backend,
}) => {
  // The page's 60 s poll runs on a clock the test moves, so no wait is a sleep.
  await page.clock.install();
  const server = { build: null as string | null };
  const counts = { loads: 0, health: 0, newer: 0 };
  page.on("load", () => (counts.loads += 1));
  await page.route("**/api/health", async (route) => {
    counts.health += 1;
    if (server.build === null) return route.continue();
    counts.newer += 1;
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    await route.fulfill({
      response,
      json: { ...body, ui_build_id: server.build },
    });
  });
  await backend.control.reset("lived-in");
  await backend.signIn(page);
  await page.goto("/settings/workspaces");
  const line = page.getByLabel("Warn when free space is under (GB)");
  await expect(line).toBeVisible();
  const before = await line.inputValue();
  await line.fill("33");
  await expect(
    page.getByRole("button", {
      name: "Save Warn when free space is under (GB)",
    }),
  ).toBeVisible();
  const loaded = counts.loads;

  // The server now names a newer build and the poll asks: the edit is held, not reloaded over.
  server.build = "a-newer-build";
  await page.clock.fastForward(61_000);
  await expect.poll(() => counts.newer).toBeGreaterThanOrEqual(1);
  expect(counts.loads).toBe(loaded);
  await expect(line).toHaveValue("33");
  await staysEmpty(page, async () => {
    const kept = await page.evaluate(
      (key) => sessionStorage.getItem(key),
      RELOADED_FOR,
    );
    return kept === null ? [] : [kept];
  });

  // Putting the text back to what is stored ends the hold; the next poll reloads once.
  await line.fill(before);
  await expect(
    page.getByRole("button", {
      name: "Save Warn when free space is under (GB)",
    }),
  ).toHaveCount(0);
  await page.clock.fastForward(61_000);
  await expect.poll(() => counts.loads).toBe(loaded + 1);
  await expect(
    page.getByLabel("Warn when free space is under (GB)"),
  ).toBeVisible();
});
