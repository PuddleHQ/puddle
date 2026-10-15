// SPDX-License-Identifier: GPL-3.0-or-later
// The inbox when a workspace has as many waiting requests as Puddle lists (1000): the notice
// says so, a new host past the limit gets no row until the user answers some, and nothing
// else changes. Against the real API on the fixture backend, with the real store's limit.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";

const LIMIT = 1000;
const DOMAIN = "flood.example.net";

const rows = (page: Page) => page.locator("li.req", { hasText: `.${DOMAIN}` });

async function openInbox(page: Page, backend: Backend): Promise<void> {
  await backend.signIn(page);
  await page.goto("/inbox");
  await expect(
    page.getByRole("heading", { level: 1, name: "Inbox" }),
  ).toBeVisible();
}

test.describe("a workspace with a full list of waiting requests", () => {
  test("says so, lists a new host once some are answered, and says so no more when it has room", async ({
    page,
    backend,
  }) => {
    // One more than the limit: the last host asks while the list is full.
    await backend.control.step({
      do: "bulk",
      workspace: "flood",
      count: LIMIT + 1,
      domain: DOMAIN,
    });
    await openInbox(page, backend);

    const notice = page.getByRole("status").filter({ hasText: "flood has" });
    await expect(notice).toContainText(
      `flood has ${LIMIT} waiting requests, the most Puddle lists`,
    );
    await expect(rows(page)).toHaveCount(LIMIT);
    await expect(rows(page).filter({ hasText: `n${LIMIT}.` })).toHaveCount(0);
    // Only the workspace that is full gets the notice.
    await expect(
      page.getByRole("status").filter({ hasText: "demo has" }),
    ).toHaveCount(0);

    // Answering one makes room: the notice goes, and the next request that asks is listed.
    await rows(page)
      .first()
      .getByRole("button", { name: /^Deny/ })
      .first()
      .click();
    await expect(rows(page)).toHaveCount(LIMIT - 1);
    await expect(notice).toHaveCount(0);
    await backend.control.step({
      do: "request",
      workspace: "flood",
      host: `n${LIMIT}.${DOMAIN}`,
    });
    await expect(rows(page)).toHaveCount(LIMIT);
    await expect(rows(page).filter({ hasText: `n${LIMIT}.` })).toHaveCount(1);
    await expect(notice).toContainText(`flood has ${LIMIT} waiting requests`);
  });
});
