// SPDX-License-Identifier: AGPL-3.0-or-later
// A stream that answers 200 and then sends nothing is a dead connection the browser never reports.
// The page treats 45 s without a byte as dead, says so in the banner and reconnects with a
// resync. The page clock is frozen and moved by hand, so no test waits 45 s; the fixture backend
// sends its own keep-alive only every 15 s of real time, which a test this short never reaches.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";

const SILENCE_MS = 45_000;
const FIRST_BACKOFF_MS = 1_000;

test("a stream that goes silent for 45 s shows the banner, then reconnects and resyncs", async ({
  page,
  backend,
}) => {
  const streamRequests = countStreamRequests(page);
  await backend.control.reset("lived-in");
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto("/inbox");
  const notices = page.getByRole("region", { name: "Notices" });
  const banner = page.getByRole("status").filter({
    hasText: "Live updates paused: the event stream went silent for 45 s",
  });

  // The stream is up once an emitted event shows its notice.
  await expect(async () => {
    await backend.control.emit({
      type: "oom_kill",
      workspace: "web-shop",
      pid: 1,
      process: "before",
    });
    await expect(
      notices.getByText(/web-shop ran out of memory/).first(),
    ).toBeVisible({ timeout: 500 });
  }).toPass();
  await expect(banner).toHaveCount(0);
  expect(streamRequests()).toBe(1);

  // Freeze time so the reconnect's wait cannot pass on its own between the two checks below.
  const now = await page.evaluate(() => Date.now());
  await page.clock.pauseAt(now + 1_000);
  await page.clock.fastForward(SILENCE_MS);
  await expect(banner).toBeVisible();
  expect(streamRequests()).toBe(1);

  // The backoff ends: the page reconnects, the banner clears and events flow again.
  await page.clock.runFor(FIRST_BACKOFF_MS + 500);
  await expect(banner).toHaveCount(0);
  await expect.poll(streamRequests).toBe(2);
  await expect(async () => {
    await backend.control.emit({
      type: "oom_kill",
      workspace: "data-tools",
      pid: 2,
      process: "after",
    });
    await expect(
      notices.getByText(/data-tools ran out of memory/).first(),
    ).toBeVisible({ timeout: 500 });
  }).toPass();
});

/** How many times the page has asked for the event stream. */
function countStreamRequests(page: Page): () => number {
  let count = 0;
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/api/events") count += 1;
  });
  return () => count;
}
