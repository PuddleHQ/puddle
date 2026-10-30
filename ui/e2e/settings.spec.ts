// SPDX-License-Identifier: GPL-3.0-or-later
// The settings screen against the real API on the fixture backend (in-memory settings). Each
// test has a backend of its own, reset before it. Waits are on conditions (a response, a
// focus), never on key order or time.
import type { APIRequestContext, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { CPUS_MAX, CPUS_MIN } from "../src/lib/settings/cpus.ts";
import { watchCsp, expectNoAxeViolations, staysEmpty } from "./support";

const TERMS = "https://code.visualstudio.com/license/server";

interface Stored {
  workspace_defaults: Record<string, unknown> & {
    memory: number | null;
    local_toggles: Record<string, boolean | null>;
  };
  vscode_server: {
    server: string | null;
    telemetry: boolean | null;
    auto_update: boolean | null;
  };
  ui: Record<string, unknown>;
}

const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

async function stored(
  request: APIRequestContext,
  backend: Backend,
): Promise<Stored> {
  const r = await request.get("/api/settings", { headers: auth(backend) });
  return (await r.json()) as Stored;
}

async function consent(
  request: APIRequestContext,
  backend: Backend,
): Promise<{ state: string; terms_version?: string }> {
  const r = await request.get("/api/consents", { headers: auth(backend) });
  return ((await r.json()) as { vscode_server: { state: string } })
    .vscode_server;
}

async function openSettings(page: Page, backend: Backend) {
  await backend.signIn(page);
  await page.goto("/settings");
  await expect(
    page.getByRole("heading", { level: 2, name: "Appearance" }),
  ).toBeVisible();
}

/** Waits for a settings save to reach the API. */
const saved = (page: Page) =>
  page.waitForResponse(
    (r) => r.request().method() === "PUT" && r.url().endsWith("/api/settings"),
  );

/** Presses Save and waits until the API has stored the change. */
async function pressSave(page: Page) {
  const done = saved(page);
  await page.getByRole("button", { name: "Save" }).click();
  await done;
  await expect(page.getByText(/Settings saved/)).toBeVisible();
}

test("shows every section with Puddle's defaults, no CSP violations", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await openSettings(page, backend);
  for (const name of [
    "Notifications",
    "Workspaces",
    "Network",
    "Browser VS Code",
    "Git and credentials",
    "Privacy",
    "About",
  ]) {
    await expect(page.getByRole("heading", { level: 2, name })).toBeVisible();
  }
  await expect(page.getByLabel("Default memory")).toHaveValue("8192");
  await expect(page.getByLabel("Server", { exact: true })).toHaveValue(
    "code_server",
  );
  await expect(page.getByText(/Open VSX/)).toBeVisible();
  await expect(page.getByText(/sends nothing about you/)).toBeVisible();
  await staysEmpty(page, csp);
});

for (const scheme of ["light", "dark"] as const) {
  test(`axe finds nothing on the screen or in the popup in ${scheme}`, async ({
    page,
    backend,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await openSettings(page, backend);
    await expectNoAxeViolations(page, "screen");
    await page.getByLabel("Server", { exact: true }).selectOption("microsoft");
    await expect(page.getByRole("dialog")).toBeVisible();
    await expectNoAxeViolations(page, "popup");
  });
}

test("the global direct SSH default is off, asks the trust text, and is kept", async ({
  page,
  backend,
}) => {
  await openSettings(page, backend);
  const box = page.getByRole("checkbox", {
    name: "Allow direct SSH for new workspaces",
  });
  await expect(box).not.toBeChecked();
  await box.click();
  const trust = page.getByRole("alertdialog", {
    name: "Allow direct SSH for new workspaces?",
  });
  await expect(trust).toContainText("GitHub token");
  await expectNoAxeViolations(page, "trust text");
  await trust.getByRole("button", { name: "Cancel" }).click();
  await expect(box).not.toBeChecked();
  await box.click();
  await trust.getByRole("button", { name: "Allow for new workspaces" }).click();
  await expect(box).toBeChecked();
  await pressSave(page);
  await page.reload();
  await expect(box).toBeChecked();
});

test("the licences open on request and are scrollable by keyboard", async ({
  page,
  backend,
}) => {
  await openSettings(page, backend);
  await page.getByText("Third-party licences").click();
  const text = page.getByLabel("Third-party licences", { exact: true });
  await expect(text).toContainText("Third-party software shipped");
  await text.focus();
  await expect(text).toBeFocused();
  await expectNoAxeViolations(page);
});

test("the theme is kept by Puddle: it comes back after the page forgets everything", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend);
  await page.getByLabel("Theme", { exact: true }).selectOption("dark");
  // The theme shows at once and is stored only when Save is pressed.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  expect((await stored(request, backend)).ui["theme"]).toBeNull();
  await pressSave(page);
  expect((await stored(request, backend)).ui["theme"]).toBe("dark");

  // A new launch has a new origin: nothing in the page's storage survives.
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.getByLabel("Theme", { exact: true })).toHaveValue("dark");
});

test("changes wait for Save, are sent together and survive a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend);
  const save = page.getByRole("button", { name: "Save" });
  await expect(save).toBeDisabled();
  await page.getByLabel("Default memory").selectOption("16384");
  await page.getByLabel("Private networks").check();
  await page.getByLabel("Sound").check();
  await page.getByLabel("Closing the window").selectOption("quit");
  await expect(page.getByText(/You have unsaved changes/)).toBeVisible();
  // Nothing is stored before Save.
  const before = await stored(request, backend);
  expect(before.workspace_defaults.memory).toBeNull();
  expect(before.ui["sound"]).toBeNull();

  await pressSave(page);
  await expect(
    page.getByText(/Default memory apply when a workspace next starts/),
  ).toBeVisible();
  await expect(save).toBeDisabled();
  const now = await stored(request, backend);
  expect(now.workspace_defaults.memory).toBe(16_384);
  expect(now.workspace_defaults.local_toggles["private"]).toBe(true);
  expect(now.workspace_defaults.local_toggles["loopback"]).toBeNull();
  expect(now.ui["sound"]).toBe(true);
  expect(now.ui["close_behaviour"]).toBe("quit");

  await page.reload();
  await expect(page.getByLabel("Default memory")).toHaveValue("16384");
  await expect(page.getByLabel("Private networks")).toBeChecked();
  await expect(page.getByLabel("Sound")).toBeChecked();
  await expect(page.getByLabel("Closing the window")).toHaveValue("quit");
});

test.describe("default CPUs", () => {
  test("is reached by Tab after memory, changed from the keyboard, and kept after Save and a reload", async ({
    page,
    backend,
    request,
  }) => {
    await openSettings(page, backend);
    const cpus = page.getByLabel("Default CPUs");
    await expect(cpus).toHaveValue("1");
    await expect(cpus.locator("option")).toHaveCount(CPUS_MAX - CPUS_MIN + 1);
    await expect(cpus.locator("option").first()).toHaveText("1 CPU");
    await expect(cpus.locator("option").last()).toHaveText("32 CPUs");

    await page.getByLabel("Default memory").focus();
    await page.keyboard.press("Tab");
    await expect(cpus).toBeFocused();
    await expect
      .poll(() =>
        cpus.evaluate((el) => {
          const s = getComputedStyle(el);
          return { width: s.outlineWidth, style: s.outlineStyle };
        }),
      )
      .toEqual({ width: "2px", style: "solid" });
    await page.keyboard.press("ArrowDown");
    await expect(cpus).toHaveValue("2");
    await expect(
      page.getByText(/Default CPUs apply when a workspace next starts/),
    ).toBeVisible();
    // Nothing is stored before Save.
    expect(
      (await stored(request, backend)).workspace_defaults["cpus"],
    ).toBeNull();

    await pressSave(page);
    expect((await stored(request, backend)).workspace_defaults["cpus"]).toBe(2);
    await page.reload();
    await expect(page.getByLabel("Default CPUs")).toHaveValue("2");
  });

  test("says a running workspace keeps its count, in a control that is big enough and stays inside its card at phone width", async ({
    page,
    backend,
  }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await openSettings(page, backend);
    const cpus = page.getByLabel("Default CPUs");
    await expect(
      page.getByText(/A running workspace keeps the count it started with/),
    ).toBeVisible();
    const layout = await cpus.evaluate((el) => {
      const card = el.closest(".card") as HTMLElement;
      const box = el.getBoundingClientRect();
      const inside = card.getBoundingClientRect();
      return {
        height: box.height,
        left: box.left - inside.left,
        right: inside.right - box.right,
      };
    });
    expect(layout.height).toBeGreaterThanOrEqual(24);
    expect(layout.left).toBeGreaterThanOrEqual(0);
    expect(layout.right).toBeGreaterThanOrEqual(0);
    await expectNoAxeViolations(page);
  });
});

test("Discard drops the edits, and leaving with edits is held back", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend);
  await page.getByLabel("Sound").check();
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Rules/ })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "Save or discard them first",
  );
  await expect(page).toHaveURL(/\/settings$/);
  await page.getByRole("button", { name: "Discard" }).click();
  await expect(page.getByLabel("Sound")).not.toBeChecked();
  expect((await stored(request, backend)).ui["sound"]).toBeNull();
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Rules/ })
    .click();
  await expect(page).toHaveURL(/\/rules$/);
});

test("the Save bar stays on screen, its buttons are big enough, and the screen has no axe findings with unsaved edits", async ({
  page,
  backend,
}) => {
  await page.setViewportSize({ width: 1280, height: 600 });
  await openSettings(page, backend);
  await page.getByLabel("Sound").check();
  const save = page.getByRole("button", { name: "Save" });
  // Scrolled to the top of a long page, the bar is still inside the window.
  await expect(save).toBeInViewport();
  for (const name of ["Save", "Discard"]) {
    const box = await page.getByRole("button", { name }).boundingBox();
    expect(box?.height ?? 0, `${name} height`).toBeGreaterThanOrEqual(24);
    expect(box?.width ?? 0, `${name} width`).toBeGreaterThanOrEqual(24);
  }
  await expectNoAxeViolations(page);
});

test("the notification and inbox switches start at Puddle's defaults, are saved together and survive a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend);
  const labels = {
    notify_only_unfocused: "Only when Puddle is not the window in front",
    badge: "Show the number of waiting requests",
    notify_oom: "A workspace runs out of memory",
    notify_unexpected_stop: "A workspace stops on its own",
    confirm_global_rules: "Confirm before a rule affects every workspace",
  } as const;
  for (const label of Object.values(labels)) {
    await expect(page.getByLabel(label)).toBeChecked();
    await page.getByLabel(label).uncheck();
  }
  await pressSave(page);
  const now = (await stored(request, backend)).ui;
  for (const key of Object.keys(labels)) expect(now[key], key).toBe(false);
  await page.reload();
  for (const label of Object.values(labels)) {
    await expect(page.getByLabel(label)).not.toBeChecked();
  }
  // With system notifications off the focus choice has nothing to decide.
  await page.getByLabel("System notifications for new requests").uncheck();
  await expect(page.getByLabel(labels.notify_only_unfocused)).toBeDisabled();
});

test("with the confirmation switched off, a decision for every workspace goes through at once", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend);
  await page
    .getByLabel("Confirm before a rule affects every workspace")
    .uncheck();
  await pressSave(page);
  await backend.control.step({
    do: "request",
    workspace: "shop",
    host: "global.example.com",
  });
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Inbox/ })
    .click();
  const row = page.locator("li.req", { hasText: "global.example.com" });
  await row.getByRole("button", { name: /^More choices for/ }).click();
  const dialog = page.getByRole("dialog", { name: /^Choices for/ });
  await dialog.getByRole("radio", { name: /^Every workspace/ }).check();
  await dialog.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(row).toHaveCount(0);
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  const rules = (await (
    await request.get("/api/rules", { headers: auth(backend) })
  ).json()) as { rules: { pattern: string; scope: { type: string } }[] };
  expect(
    rules.rules.find((r) => r.pattern === "global.example.com"),
  ).toMatchObject({ scope: { type: "global" } });
});

test("the inbox keys do nothing until the switch is on, then A decides the current request", async ({
  page,
  backend,
  request,
}) => {
  await backend.signIn(page);
  await page.goto("/inbox");
  const row = page.locator("li.req").first();
  await expect(row).toBeVisible();
  const hint = page.getByRole("link", {
    name: /Turn on the A, D, J and K keys in Settings/,
  });
  await expect(hint).toBeVisible();
  await expectNoAxeViolations(page, "inbox with the keys off");
  await page.locator("main").focus();
  await page.keyboard.press("a");
  await expect(row).toBeVisible();
  const open = async () =>
    (
      (await (
        await request.get("/api/rules", { headers: auth(backend) })
      ).json()) as { rules: unknown[] }
    ).rules.length;
  // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
  expect(await open()).toBe(0);

  await hint.click();
  await expect(page).toHaveURL(/\/settings#s-inbox$/);
  await page.getByLabel("Keyboard shortcuts in the inbox").check();
  await pressSave(page);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Inbox/ })
    .click();
  await expect(page.locator("li.req").first()).toBeVisible();
  await expect(page.getByText("allow ·")).toBeVisible();
  await page.locator("main").focus();
  await page.keyboard.press("a");
  await expect(page.locator("li.req")).toHaveCount(0);
  await expect.poll(() => open()).toBe(1);
});

test("a reconnection grace outside the range is refused before anything is sent", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend);
  const grace = page.getByLabel("Reconnection grace (seconds)");
  await grace.fill("5");
  await grace.blur();
  await expect(page.getByText(/Use between 30 and 86400/)).toBeVisible();
  expect(
    (await stored(request, backend)).workspace_defaults["reconnection_grace"],
  ).toBeNull();
  await grace.fill("900");
  await grace.blur();
  await pressSave(page);
  expect(
    (await stored(request, backend)).workspace_defaults["reconnection_grace"],
  ).toBe(900);
});

test.describe("Microsoft's VS Code server", () => {
  test("declining or closing the popup keeps code-server and records nothing", async ({
    page,
    backend,
    request,
  }) => {
    await openSettings(page, backend);
    await page.getByLabel("Server", { exact: true }).selectOption("microsoft");
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText(
      "Puddle downloads the server from Microsoft",
    );
    await expect(
      dialog.getByRole("link", { name: /licence terms/ }),
    ).toHaveAttribute("href", TERMS);
    await expect(
      dialog.getByLabel("Allow the server to send telemetry to Microsoft"),
    ).not.toBeChecked();
    await dialog.getByRole("button", { name: "Keep code-server" }).click();
    await expect(dialog).toBeHidden();
    await expect(page.getByLabel("Server", { exact: true })).toHaveValue(
      "code_server",
    );
    expect((await consent(request, backend)).state).toBe("not_asked");
    expect((await stored(request, backend)).vscode_server.server).toBeNull();
  });

  test("Escape closes the popup and gives focus back to the choice", async ({
    page,
    backend,
    request,
  }) => {
    await openSettings(page, backend);
    await page.getByLabel("Server", { exact: true }).selectOption("microsoft");
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();
    // The dialog moves focus in after it opens; wait for that, then act.
    await expect(dialog.locator(":focus")).toHaveCount(1);
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(page.getByLabel("Server", { exact: true })).toBeFocused();
    expect((await consent(request, backend)).state).toBe("not_asked");
  });

  test("accepting records the consent with the terms and switches the server", async ({
    page,
    backend,
    request,
  }) => {
    await openSettings(page, backend);
    await page.getByLabel("Server", { exact: true }).selectOption("microsoft");
    const dialog = page.getByRole("dialog");
    await dialog
      .getByRole("button", { name: "Accept and use Microsoft's server" })
      .click();
    await expect(dialog).toBeHidden();
    // The toast says what happens next in plain words.
    await expect(
      page.getByText(/downloads the server from Microsoft the next time/),
    ).toBeVisible();
    await expect(page.getByLabel("Server", { exact: true })).toHaveValue(
      "microsoft",
    );
    await expect(page.getByLabel("Send Microsoft telemetry")).not.toBeChecked();
    const c = await consent(request, backend);
    expect(c.state).toBe("granted");
    expect(c.terms_version).toBe(TERMS);
    const s = await stored(request, backend);
    expect(s.vscode_server.server).toBe("microsoft");
    expect(s.vscode_server.telemetry).toBe(false);

    await page.reload();
    await expect(page.getByLabel("Server", { exact: true })).toHaveValue(
      "microsoft",
    );
    await expect(
      page.getByText(/You accepted Microsoft's terms on/),
    ).toBeVisible();
    // Switching back keeps the consent; switching again does not ask.
    await page
      .getByLabel("Server", { exact: true })
      .selectOption("code_server");
    await pressSave(page);
    await page.getByLabel("Server", { exact: true }).selectOption("microsoft");
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await pressSave(page);
  });

  test("telemetry is only on when the box was checked", async ({
    page,
    backend,
    request,
  }) => {
    await openSettings(page, backend);
    await page.getByLabel("Server", { exact: true }).selectOption("microsoft");
    const dialog = page.getByRole("dialog");
    await dialog
      .getByLabel("Allow the server to send telemetry to Microsoft")
      .check();
    await dialog
      .getByRole("button", { name: "Accept and use Microsoft's server" })
      .click();
    await expect(dialog).toBeHidden();
    await expect(page.getByLabel("Send Microsoft telemetry")).toBeChecked();
    expect((await stored(request, backend)).vscode_server.telemetry).toBe(true);
  });

  test("the API itself refuses Microsoft's server without consent", async ({
    backend,
    request,
  }) => {
    const r = await request.put("/api/settings", {
      headers: auth(backend),
      data: { vscode_server: { server: "microsoft" } },
    });
    expect(r.status()).toBe(422);
  });
});
