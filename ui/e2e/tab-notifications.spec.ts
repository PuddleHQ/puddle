// SPDX-License-Identifier: AGPL-3.0-or-later
// Notifications in open browser tabs, on the fixture backend (scenario `lived-in`): a tab reports
// itself, the service picks who is told of a new request, and a picked tab shows a Web
// Notification that opens the request. The page's `Notification` is replaced by a recorder, so
// what is asserted is exactly what the page asked the browser to show.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";

interface Shown {
  title: string;
  body: string;
  tag: string;
}

/** Records every notification the page makes, and lets a test click one. */
async function recordNotifications(page: Page, switchOn: boolean) {
  await page.addInitScript((on) => {
    const shown: unknown[] = [];
    const made: { onclick: (() => void) | null }[] = [];
    class Recorder {
      static permission = "granted";
      static requestPermission = async () => "granted";
      onclick: (() => void) | null = null;
      constructor(title: string, options: { body: string; tag: string }) {
        shown.push({ title, body: options.body, tag: options.tag });
        made.push(this);
      }
      close() {}
    }
    Object.assign(window, {
      Notification: Recorder,
      __shown: shown,
      __click: (n: number) => made[n]?.onclick?.(),
    });
    localStorage.setItem("puddle.tab-notifications", on ? "on" : "off");
  }, switchOn);
}

const shownOn = (page: Page) =>
  page.evaluate(() => (window as unknown as { __shown: Shown[] }).__shown);

async function open(page: Page, backend: Backend, switchOn = true) {
  await backend.control.reset("lived-in");
  await recordNotifications(page, switchOn);
  await backend.signIn(page);
  const reported = page.waitForResponse(
    (r) => r.url().endsWith("/api/presence") && r.request().method() === "POST",
  );
  await page.goto("/workspaces");
  expect((await reported).status()).toBe(204);
}

test("a new request shows one notification in the tab the service picked, and a click opens the request", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await backend.control.step({
    do: "request",
    workspace: "web-shop",
    host: "registry.example.org",
  });
  await expect
    .poll(async () => (await shownOn(page)).length, { timeout: 15_000 })
    .toBe(1);
  const [notice] = await shownOn(page);
  expect(notice?.title).toMatch(/ is asking for access$/);
  expect(notice?.body).toBe(
    "registry.example.org:443 is waiting for your decision.",
  );
  expect(notice?.tag).toMatch(/^puddle-request-\d+$/);
  const id = notice?.tag.replace("puddle-request-", "");

  await page.evaluate(() =>
    (window as unknown as { __click: (n: number) => void }).__click(0),
  );
  await expect(page).toHaveURL(new RegExp(`/inbox\\?request=${id}$`));
  await expect(page.locator(`[data-request-id="${id}"]`)).toBeFocused();
});

test("a tab with its switch off is not told", async ({ page, backend }) => {
  await open(page, backend, false);
  await backend.control.step({
    do: "request",
    workspace: "web-shop",
    host: "quiet.example.org",
  });
  // The request is in the inbox, which is the proof that the event went past the page.
  await page.goto("/inbox");
  await expect(page.locator("li.req .host").first()).toBeVisible();
  expect(await shownOn(page)).toEqual([]);
});

test("the settings switch asks the browser and tells the service at once", async ({
  page,
  backend,
}) => {
  await open(page, backend, false);
  await page.goto("/settings");
  const box = page.getByLabel("Notifications in this browser");
  await expect(box).not.toBeChecked();
  const reported = page.waitForRequest(
    (r) =>
      r.url().endsWith("/api/presence") &&
      (r.postDataJSON() as { notify?: boolean }).notify === true,
  );
  await box.check();
  await reported;
  await expect(box).toBeChecked();
  expect(
    await page.evaluate(() => localStorage.getItem("puddle.tab-notifications")),
  ).toBe("on");
});
