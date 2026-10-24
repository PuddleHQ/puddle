// SPDX-License-Identifier: GPL-3.0-or-later
// App-level notices driven by events, on the fixture backend (scenario `lived-in`): they show on
// any screen, never take focus, and can be dismissed. Waits are on conditions, never on time.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeViolations } from "./support";

async function open(page: Page, backend: Backend, path = "/inbox") {
  await backend.control.reset("lived-in");
  await backend.signIn(page);
  await page.goto(path);
  await expect(
    page.getByRole("link", { name: "Skip to content" }),
  ).toBeAttached();
  // The stream is up once a harmless event goes through: wait for the shell's live state.
  await expect(page.getByRole("region", { name: "Notices" })).toBeAttached();
}

const notices = (page: Page) => page.getByRole("region", { name: "Notices" });

/** Emits until the page shows the notice: the stream may connect after the page loaded. */
async function emitUntilShown(
  backend: Backend,
  page: Page,
  event: Record<string, unknown>,
  text: RegExp,
) {
  await expect(async () => {
    await backend.control.emit(event);
    await expect(notices(page).getByText(text).first()).toBeVisible({
      timeout: 500,
    });
  }).toPass();
}

test("an out-of-memory kill shows a notice on any screen, without taking focus", async ({
  page,
  backend,
}) => {
  await open(page, backend, "/rules");
  await emitUntilShown(
    backend,
    page,
    { type: "oom_kill", workspace: "web-shop", pid: 4242, process: "node" },
    /web-shop ran out of memory/,
  );
  await expect(notices(page).getByText(/node \(process 4242\)/)).toBeVisible();
  expect(await page.evaluate(() => document.activeElement?.tagName)).toBe(
    "BODY",
  );
  expect(await axeViolations(page)).toEqual([]);
  await notices(page).getByRole("link", { name: "Workspace settings" }).click();
  await expect(page).toHaveURL(/\/workspaces\/web-shop\/settings$/);
});

test("a workspace that stops on its own is reported, and one that is stopped on purpose is not", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  // The stream is up once a first notice has come through; events before that would be lost.
  await emitUntilShown(
    backend,
    page,
    { type: "oom_kill", workspace: "data-tools", pid: 1, process: "ld" },
    /data-tools ran out of memory/,
  );
  // Asked for: stopping, then stopped.
  await backend.control.emit({
    type: "workspace_progress",
    workspace: "web-shop",
    step: "stopping",
    detail: null,
  });
  await backend.control.emit({
    type: "status_changed",
    workspace: "web-shop",
    status: "stopped",
  });
  await backend.control.emit({
    type: "status_changed",
    workspace: "web-shop",
    status: "running",
  });
  // Events arrive in order: once this one is shown, the three above were handled.
  await backend.control.emit({
    type: "oom_kill",
    workspace: "docs-site",
    pid: 2,
    process: "ld",
  });
  await expect(
    notices(page).getByText(/docs-site ran out of memory/),
  ).toBeVisible();
  await expect(
    notices(page).getByText(/stopped without being asked/),
  ).toHaveCount(0);
  // Not asked for: the scenario's own script.
  await expect(async () => {
    await backend.control.script("workspace-stopped");
    await expect(
      notices(page).getByText("web-shop stopped without being asked to."),
    ).toBeVisible({ timeout: 500 });
    await backend.control.emit({
      type: "status_changed",
      workspace: "web-shop",
      status: "running",
    });
  }).toPass();
  await backend.control.emit({
    type: "status_changed",
    workspace: "web-shop",
    status: "crashed",
  });
  await expect(notices(page).getByText("web-shop crashed.")).toBeVisible();
  // Running again withdraws the notice.
  await backend.control.emit({
    type: "status_changed",
    workspace: "web-shop",
    status: "running",
  });
  await expect(notices(page).getByText("web-shop crashed.")).toHaveCount(0);
});

test("notices can be dismissed by keyboard, and a repeat replaces the old notice", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  const oom = {
    type: "oom_kill",
    workspace: "docs-site",
    pid: 7,
    process: "cc1plus",
  };
  await emitUntilShown(backend, page, oom, /docs-site ran out of memory/);
  await backend.control.emit(oom);
  await backend.control.emit({ ...oom, workspace: "data-tools" });
  await expect(
    notices(page).getByText(/data-tools ran out of memory/),
  ).toBeVisible();
  await expect(
    notices(page).getByText(/docs-site ran out of memory/),
  ).toHaveCount(1);

  const dismiss = notices(page).getByRole("button", {
    name: /^Dismiss notice: docs-site/,
  });
  await dismiss.focus();
  await page.keyboard.press("Enter");
  await expect(
    notices(page).getByText(/docs-site ran out of memory/),
  ).toHaveCount(0);
  await expect(
    notices(page).getByRole("button", { name: /^Dismiss notice: data-tools/ }),
  ).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(notices(page).getByRole("button")).toHaveCount(0);
  await expect(page.locator("#main")).toBeFocused();
});

for (const scheme of ["light", "dark"] as const) {
  test(`axe finds nothing with several notices in ${scheme}`, async ({
    page,
    backend,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await open(page, backend);
    for (const workspace of ["web-shop", "docs-site", "data-tools"]) {
      await backend.control.emit({
        type: "oom_kill",
        workspace,
        pid: 1,
        process: "node",
      });
    }
    await backend.control.emit({
      type: "status_changed",
      workspace: "web-shop",
      status: "crashed",
    });
    await expect(notices(page).getByText("web-shop crashed.")).toBeVisible();
    await notices(page)
      .getByRole("button", { name: /^Show all/ })
      .click();
    expect(await axeViolations(page)).toEqual([]);
  });
}

test("a direct SSH failure shows its cause and a way to the workspace, and ends when the workspace stops", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  await emitUntilShown(
    backend,
    page,
    {
      type: "workspace_ssh_failed",
      workspace: "web-shop",
      reason:
        "the SSH endpoint stopped accepting connections (the listener closed); restart the workspace to get direct SSH back",
    },
    /Direct SSH to web-shop failed\./,
  );
  await expect(
    notices(page).getByText(/the listener closed\); restart the workspace/),
  ).toBeVisible();
  await expect(
    notices(page).getByText(/The workspace itself keeps running\./),
  ).toBeVisible();
  expect(await axeViolations(page)).toEqual([]);
  await notices(page).getByRole("link", { name: "Open workspace" }).click();
  await expect(page).toHaveURL(/\/workspaces\/web-shop$/);
  // The endpoint it reported on is gone once the workspace stops.
  await backend.control.emit({
    type: "status_changed",
    workspace: "web-shop",
    status: "stopped",
  });
  await expect(notices(page).getByText(/Direct SSH to web-shop/)).toHaveCount(
    0,
  );
});

test("an activity log that cannot be written is a notice with the reason, and ends with the next good write", async ({
  page,
  backend,
}) => {
  await open(page, backend);
  const key = "connection-records";
  await expect(async () => {
    await backend.control.step({
      do: "problem",
      key,
      title: "Puddle could not write to its activity log",
      detail:
        "the disk is full <b>now</b>. Connections still work, but the activity list is missing the ones made since this started. A full disk is the usual cause: free some space.",
    });
    await expect(
      notices(page).getByText("Puddle could not write to its activity log"),
    ).toBeVisible({ timeout: 500 });
  }).toPass();
  await expect(notices(page).getByText(/free some space/)).toBeVisible();
  // The reason is text, not markup.
  await expect(notices(page).locator("b")).toHaveCount(0);
  expect(await axeViolations(page)).toEqual([]);
  await backend.control.step({ do: "clear_problem", key });
  await expect(
    notices(page).getByText("Puddle could not write to its activity log"),
  ).toHaveCount(0);
});

/**
 * Answers the page's event stream with what the real service sends for a refused request: the
 * request goes to the fixture backend with one header changed, and its own answer comes back.
 */
async function refuseEvents(
  page: Page,
  change: (headers: Record<string, string>) => Record<string, string>,
) {
  const attempts = { count: 0 };
  await page.route("**/api/events*", async (route) => {
    attempts.count += 1;
    const response = await route.fetch({
      headers: change(route.request().headers()),
    });
    await route.fulfill({ response });
  });
  return attempts;
}

const refusals = [
  {
    kind: "a wrong key",
    change: (h: Record<string, string>) => ({
      ...h,
      authorization: "Bearer not-the-key",
    }),
    status: 401,
    text: "Live updates stopped: the service refused the event stream: missing or wrong bearer token (status 401). Restart Puddle's app to sign in again.",
  },
  {
    kind: "a wrong origin",
    change: (h: Record<string, string>) => ({
      ...h,
      origin: "http://evil.example",
    }),
    status: 403,
    text: "Live updates stopped: the service refused the event stream: requests from this origin are not allowed (status 403). Open Puddle from its own app window or tray icon, not from another address.",
  },
];

for (const refusal of refusals) {
  test(`${refusal.kind} stops the event stream for good, and the banner gives the service's reason and what to do`, async ({
    page,
    backend,
  }) => {
    await backend.control.reset("lived-in");
    await backend.signIn(page);
    const attempts = await refuseEvents(page, refusal.change);
    await page.goto("/rules");
    const banner = page.getByRole("status").filter({
      hasText: "Live updates stopped",
    });
    await expect(banner).toHaveText(refusal.text);
    // The first refusal ended it: nothing asked again before the banner could be read.
    expect(attempts.count).toBe(1);
  });
}

for (const scheme of ["light", "dark"] as const) {
  test(`the refusal banner is readable and fits a narrow window in ${scheme}`, async ({
    page,
    backend,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.setViewportSize({ width: 360, height: 700 });
    await backend.control.reset("lived-in");
    await backend.signIn(page);
    const origin = refusals[1];
    if (origin === undefined) throw new Error("the origin case is missing");
    await refuseEvents(page, origin.change);
    await page.goto("/rules");
    const banner = page.getByRole("status").filter({
      hasText: "Live updates stopped",
    });
    await expect(banner).toBeVisible();
    expect(await axeViolations(page)).toEqual([]);
    const box = await banner.evaluate((el) => {
      const style = getComputedStyle(el);
      const probe = document.createElement("span");
      probe.style.color = "var(--color-surface-raised)";
      document.body.append(probe);
      const surface = getComputedStyle(probe).color;
      probe.remove();
      return {
        fits: el.scrollWidth <= el.clientWidth,
        right: el.getBoundingClientRect().right,
        background: style.backgroundColor,
        surface,
        wrap: style.overflowWrap,
      };
    });
    // The whole sentence is on screen: no sideways scrolling, nothing cut off.
    expect(box.fits).toBe(true);
    expect(box.right).toBeLessThanOrEqual(360);
    expect(box.background).toBe(box.surface);
  });
}
