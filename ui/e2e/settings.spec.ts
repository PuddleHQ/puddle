// SPDX-License-Identifier: AGPL-3.0-or-later
// The settings screen against the real API on the fixture backend (in-memory settings). Each
// test has a backend of its own, reset before it. Waits are on conditions (a response, a
// focus), never on key order or time.
import type { APIRequestContext, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { cpuLabel, CPUS_MIN } from "../src/lib/settings/cpus.ts";
import { SETTINGS } from "../src/lib/settings/pages.ts";
import {
  cpuLimits,
  watchCsp,
  expectNoAxeViolations,
  staysEmpty,
} from "./support";

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
  tools: { mise: boolean | null };
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

type Where =
  | "appearance"
  | "notifications"
  | "workspaces"
  | "network"
  | "editor-and-git"
  | "this-computer";

async function openSettings(
  page: Page,
  backend: Backend,
  where: Where = "appearance",
) {
  await backend.signIn(page);
  await page.goto(`/settings/${where}`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Settings" }),
  ).toBeVisible();
  await expect(
    page.getByRole("navigation", { name: "Settings pages" }),
  ).toBeVisible();
}

/** Waits until a change has been stored: every setting says "Saved." beside itself. */
async function pressSave(page: Page) {
  await expect(page.getByText("Saved.").first()).toBeVisible();
}

/** Presses the Save beside a typed value and waits until it is stored. */
async function saveField(page: Page, label: string) {
  await page
    .getByRole("button", { name: `Save ${label}`, exact: true })
    .click();
  await pressSave(page);
}

const SECTIONS: Record<Where, string[]> = {
  appearance: ["Appearance"],
  notifications: ["Notifications", "Also notify when", "Inbox"],
  workspaces: ["Workspaces", "Disk space", "Environment"],
  network: ["Proxy", "Local destinations", "Decrypting traffic"],
  "editor-and-git": ["Browser VS Code", "Git and SSH"],
  "this-computer": [
    "Startup and window",
    "Devices and sign-in",
    "Back up",
    "Activity log",
    "Checks and errors",
    "Privacy and about",
  ],
};

test("shows every page with Puddle's defaults, no CSP violations", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await openSettings(page, backend);
  for (const [where, names] of Object.entries(SECTIONS)) {
    await page.goto(`/settings/${where}`);
    for (const name of names) {
      await expect(page.getByRole("heading", { level: 2, name })).toBeVisible();
    }
  }
  await page.goto("/settings/workspaces");
  await expect(page.getByLabel("Default memory")).toHaveValue("8192");
  await page.goto("/settings/this-computer");
  await expect(
    page.getByLabel("Start Puddle when I sign in"),
  ).not.toBeChecked();
  await expect(page.getByText(/sends nothing about you/)).toBeVisible();
  await page.goto("/settings/editor-and-git");
  await expect(page.getByLabel("Server", { exact: true })).toHaveValue(
    "code_server",
  );
  await expect(page.getByText(/Open VSX/)).toBeVisible();
  await staysEmpty(page, csp);
});

test("no settings page is longer than two screens at 1440 px", async ({
  page,
  backend,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openSettings(page, backend);
  for (const where of Object.keys(SECTIONS)) {
    await page.goto(`/settings/${where}`);
    await expect(page.getByRole("heading", { level: 2 }).first()).toBeVisible();
    await expect
      .poll(() => page.evaluate(() => document.documentElement.scrollHeight), {
        message: `${where} is at most two screens tall`,
      })
      .toBeLessThanOrEqual(1800);
  }
});

// One test per setting: the catalogue grows, and a single test that walked all of it took 18 s in
// chromium and 32 s in webkit on the host, so it grew past the test limit with every setting added.
for (const entry of SETTINGS) {
  test(`the search finds ${entry.id} and the result opens it`, async ({
    page,
    backend,
  }) => {
    await openSettings(page, backend);
    const mise = (await (
      await page.request.get("/api/settings", { headers: auth(backend) })
    ).json()) as { mise_available: boolean };
    const noMise = entry.id === "set-mise" && !mise.mise_available;
    test.skip(noMise, "no mise here");
    const needsMicrosoft = ["set-ms-telemetry", "set-withdraw"].includes(
      entry.id,
    );
    test.skip(needsMicrosoft, "only while Microsoft's server is chosen");
    await page.goto("/settings/appearance");
    await page
      .getByRole("searchbox", { name: "Search settings" })
      .fill(entry.label);
    await page
      .getByRole("region", { name: "Search results" })
      .locator(`a[href="/settings/${entry.page}#${entry.id}"]`)
      .click();
    await expect(page, entry.id).toHaveURL(
      new RegExp(`/settings/${entry.page}#${entry.id}$`),
    );
    await expect(page.locator(`#${entry.id}`), entry.id).toBeVisible();
  });
}

for (const scheme of ["light", "dark"] as const) {
  test(`axe finds nothing on the screen or in the popup in ${scheme}`, async ({
    page,
    backend,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await openSettings(page, backend, "editor-and-git");
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
  await openSettings(page, backend, "workspaces");
  const box = page.getByRole("switch", {
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
  await pressSave(page);
  await expect(box).toBeChecked();
  await page.reload();
  await expect(box).toBeChecked();
});

test("the licences open on request and are scrollable by keyboard", async ({
  page,
  backend,
}) => {
  await openSettings(page, backend, "this-computer");
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
  // The theme shows at once and is stored at once.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await pressSave(page);
  expect((await stored(request, backend)).ui["theme"]).toBe("dark");

  // A new launch has a new origin: nothing in the page's storage survives.
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.getByLabel("Theme", { exact: true })).toHaveValue("dark");
});

test("each change is stored at once, says Saved, and survives a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "workspaces");
  await page.getByLabel("Default memory").selectOption("16384");
  await pressSave(page);
  await expect(
    page.getByRole("status").getByText("Applies when a workspace next starts."),
  ).toHaveCount(1);
  await page.goto("/settings/network");
  await page.getByLabel("Private networks").check();
  await pressSave(page);
  await page.goto("/settings/notifications");
  await page.getByLabel("Sound").check();
  await pressSave(page);
  await page.goto("/settings/this-computer");
  await page.getByLabel("Closing the window").selectOption("quit");
  await pressSave(page);

  const now = await stored(request, backend);
  expect(now.workspace_defaults.memory).toBe(16_384);
  expect(now.workspace_defaults.local_toggles["private"]).toBe(true);
  expect(now.workspace_defaults.local_toggles["loopback"]).toBeNull();
  expect(now.ui["sound"]).toBe(true);
  expect(now.ui["close_behaviour"]).toBe("quit");

  await page.reload();
  await expect(page.getByLabel("Closing the window")).toHaveValue("quit");
  await page.goto("/settings/workspaces");
  await expect(page.getByLabel("Default memory")).toHaveValue("16384");
});

test("a change can be undone from the row", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "notifications");
  await page.getByLabel("Sound").check();
  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(page.getByText("Undone.")).toBeVisible();
  await expect(page.getByLabel("Sound")).not.toBeChecked();
  expect((await stored(request, backend)).ui["sound"]).toBe(false);
});

test("the global mise switch is on, is kept after Save, and takes the tool switches off every workspace's Settings tab", async ({
  page,
  backend,
  request,
}) => {
  // The default scenario has no workspace; this one has web-shop.
  await backend.control.reset("lived-in");
  await openSettings(page, backend, "workspaces");
  const global = page.getByLabel("Manage tools with mise in workspaces");
  const view0 = await request.get("/api/settings", { headers: auth(backend) });
  if (!((await view0.json()) as { mise_available: boolean }).mise_available) {
    // The v0.1 build leaves the mise layer out: no switch here, no Tools section on a workspace.
    await expect(global).toHaveCount(0);
    await page.goto("/workspaces/web-shop/settings");
    await expect(
      page.getByRole("table", { name: "Settings of web-shop" }),
    ).toBeVisible();
    await expect(page.getByRole("columnheader", { name: "Tools" })).toHaveCount(
      0,
    );
    await expect(
      page.getByLabel("Manage this workspace's tools with mise"),
    ).toHaveCount(0);
    return;
  }
  await expect(global).toBeChecked();
  expect((await stored(request, backend)).tools.mise).toBeNull();

  // On: the workspace tab has the two switches.
  await page.goto("/workspaces/web-shop/settings");
  const mise = page.getByLabel("Manage this workspace's tools with mise", {
    exact: true,
  });
  const code = page.getByLabel("Let mise run the repository's own code", {
    exact: true,
  });
  await expect(mise).toHaveValue("on");
  await expect(code).toHaveValue("on");
  // A workspace value is kept while the global switch is off, and never turns it back on.
  await code.selectOption("off");
  await expect(page.getByText("Running repository code saved.")).toBeVisible();

  await page.goto("/settings/workspaces");
  await expect(global).toBeChecked();
  await global.uncheck();
  await pressSave(page);
  expect((await stored(request, backend)).tools.mise).toBe(false);
  await page.reload();
  await expect(global).not.toBeChecked();

  // Off: the workspace tab loses both switches (and its section).
  await page.goto("/workspaces/web-shop/settings");
  await expect(
    page.getByRole("table", { name: "Settings of web-shop" }),
  ).toBeVisible();
  await expect(mise).toHaveCount(0);
  await expect(code).toHaveCount(0);
  await expect(page.getByRole("columnheader", { name: "Tools" })).toHaveCount(
    0,
  );
  const view = await request.get("/api/settings/workspaces/web-shop", {
    headers: auth(backend),
  });
  expect(
    ((await view.json()) as { tools: unknown }).tools,
    "the stored workspace switches stay",
  ).toEqual({ mise: null, repo_code: false });

  // Back on: the switches return as they were.
  await page.goto("/settings/workspaces");
  await global.check();
  await pressSave(page);
  await page.goto("/workspaces/web-shop/settings");
  await expect(mise).toHaveValue("on");
  await expect(code).toHaveValue("off");
});

test.describe("default CPUs", () => {
  test("is reached by Tab after memory, changed from the keyboard, and kept after Save and a reload", async ({
    page,
    backend,
    request,
  }) => {
    await openSettings(page, backend, "workspaces");
    const limits = await cpuLimits(request, backend.token);
    // The arrow key below needs one count above the default; a one-core computer has none.
    test.skip(limits.max_cpus <= limits.default_cpus, "a one-core computer");
    const cpus = page.getByLabel("Default CPUs", { exact: true });
    await expect(cpus).toHaveValue(String(limits.default_cpus));
    await expect(cpus.locator("option")).toHaveCount(
      limits.max_cpus - CPUS_MIN + 1,
    );
    await expect(cpus.locator("option").first()).toHaveText("1 CPU");
    await expect(cpus.locator("option").last()).toHaveText(
      cpuLabel(limits.max_cpus),
    );

    await page.getByLabel("Default memory").focus();
    // The row's "How this works" comes first in the page order, then the control.
    await page.keyboard.press("Tab");
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
    const next = limits.default_cpus + 1;
    await expect(cpus).toHaveValue(String(next));
    await expect(
      page
        .getByRole("status")
        .getByText("Applies when a workspace next starts."),
    ).toHaveCount(1);
    await pressSave(page);
    expect((await stored(request, backend)).workspace_defaults["cpus"]).toBe(
      next,
    );
    await page.reload();
    await expect(page.getByLabel("Default CPUs", { exact: true })).toHaveValue(
      String(next),
    );
  });

  test("says a running workspace keeps its count, in a control that is big enough and stays inside its card at phone width", async ({
    page,
    backend,
  }) => {
    await page.setViewportSize({ width: 390, height: 800 });
    await openSettings(page, backend, "workspaces");
    const cpus = page.getByLabel("Default CPUs", { exact: true });
    await page
      .locator("summary")
      .filter({ hasText: "for Default CPUs" })
      .click();
    await expect(
      page.getByText(/A running workspace keeps the count it started with/),
    ).toBeVisible();
    const layout = await cpus.evaluate((el) => {
      const card = el.closest(".group") as HTMLElement;
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

test("a typed value's Save is big enough and the page has no axe findings with a value waiting", async ({
  page,
  backend,
}) => {
  await page.setViewportSize({ width: 1280, height: 600 });
  await openSettings(page, backend, "workspaces");
  await page
    .getByLabel("Warn when free space is under (GB)", { exact: true })
    .fill("30");
  const save = page.getByRole("button", {
    name: "Save Warn when free space is under (GB)",
  });
  await expect(save).toBeVisible();
  const box = await save.boundingBox();
  expect(box?.height ?? 0, "Save height").toBeGreaterThanOrEqual(24);
  expect(box?.width ?? 0, "Save width").toBeGreaterThanOrEqual(24);
  await expectNoAxeViolations(page);
});

test("the disk warning threshold starts at 95, is saved with the other settings and refuses a value outside 50 to 100", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "notifications");
  const field = page.getByLabel("Warn when a disk is this full (%)", {
    exact: true,
  });
  await expect(field).toHaveValue("95");
  await field.fill("120");
  await expect(page.getByRole("alert").filter({ hasText: /50/ })).toBeVisible();
  await field.fill("80");
  await saveField(page, "Warn when a disk is this full (%)");
  expect((await stored(request, backend)).ui.disk_warning_percent).toBe(80);
  await page.reload();
  await expect(field).toHaveValue("80");
});

test("the notification and inbox switches start at Puddle's defaults, are each saved and survive a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "notifications");
  const labels = {
    notify_only_unfocused: "Only when Puddle is not the window in front",
    badge: "Show the number of waiting requests",
    notify_oom: "A workspace runs out of memory",
    notify_disk_full: "A workspace's disk is nearly full",
    notify_unexpected_stop: "A workspace stops on its own",
    confirm_global_rules: "Confirm before a rule affects every workspace",
  } as const;
  for (const label of Object.values(labels)) {
    await expect(page.getByLabel(label)).toBeChecked();
    await page.getByLabel(label).uncheck();
  }
  for (const key of Object.keys(labels)) {
    await expect
      .poll(async () => (await stored(request, backend)).ui[key], key)
      .toBe(false);
  }
  await page.reload();
  for (const label of Object.values(labels)) {
    await expect(page.getByLabel(label)).not.toBeChecked();
  }
  // With system notifications off the focus choice has nothing to decide.
  await page.getByLabel("System notifications for new requests").uncheck();
  await expect(page.getByLabel(labels.notify_only_unfocused)).toBeDisabled();
});

test("the start-at-sign-in switch is off at first, is saved and survives a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "this-computer");
  const box = page.getByLabel("Start Puddle when I sign in");
  await expect(box).not.toBeChecked();
  await box.check();
  await pressSave(page);
  expect((await stored(request, backend)).ui.start_at_login).toBe(true);
  await page.reload();
  await expect(box).toBeChecked();
  await box.uncheck();
  await pressSave(page);
  expect((await stored(request, backend)).ui.start_at_login).toBe(false);
});

test("with the confirmation switched off, a decision for every workspace goes through at once", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "notifications");
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

test("the inbox keys are on by default; switched off from the shortcuts list they do nothing until switched on again", async ({
  page,
  backend,
  request,
}) => {
  await backend.signIn(page);
  await page.goto("/inbox");
  const row = page.locator("li.req").first();
  await expect(row).toBeVisible();
  const keysButton = page.getByRole("button", { name: "Keyboard shortcuts" });
  const keys = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  await keysButton.click();
  await expect(keys.locator('[data-shortcut="allow"]')).toBeVisible();
  await keys
    .getByRole("link", { name: "Turn the single keys off in Settings" })
    .click();
  await expect(page).toHaveURL(/\/settings\/notifications#set-confirm-global$/);
  const box = page.getByLabel("Keyboard shortcuts in the inbox");
  await expect(box).toBeChecked();
  await box.uncheck();
  await pressSave(page);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Inbox/ })
    .click();
  await expect(row).toBeVisible();
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

  await keysButton.click();
  await expect(keys.locator('[data-shortcut="allow"]')).toHaveCount(0);
  await expectNoAxeViolations(page, "inbox shortcuts list with the keys off");
  await keys.getByRole("link", { name: "Turn them on in Settings" }).click();
  await expect(page).toHaveURL(/\/settings\/notifications#set-confirm-global$/);
  await box.check();
  await pressSave(page);
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: /^Inbox/ })
    .click();
  await expect(page.locator("li.req").first()).toBeVisible();
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
  await openSettings(page, backend, "editor-and-git");
  const grace = page.getByLabel("Reconnection grace (seconds)", {
    exact: true,
  });
  await grace.fill("5");
  await expect(page.getByText(/Use between 30 and 86400/)).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Save Reconnection grace (seconds)" }),
  ).toBeDisabled();
  expect(
    (await stored(request, backend)).workspace_defaults["reconnection_grace"],
  ).toBeNull();
  await grace.fill("900");
  await saveField(page, "Reconnection grace (seconds)");
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
    await openSettings(page, backend, "editor-and-git");
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
    await openSettings(page, backend, "editor-and-git");
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
    await openSettings(page, backend, "editor-and-git");
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
    await openSettings(page, backend, "editor-and-git");
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

test("the disk space lines start at 20 and 5 GB, are checked before saving and survive a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "workspaces");
  const warn = page.getByLabel("Warn when free space is under (GB)", {
    exact: true,
  });
  const fail = page.getByLabel("Fail when free space is under (GB)", {
    exact: true,
  });
  await expect(warn).toHaveValue("20");
  await expect(fail).toHaveValue("5");

  // A failure line above the warning line cannot be saved.
  await fail.fill("30");
  await expect(
    page.getByText(
      "The failure line (30 GB) cannot be above the warning line (20 GB).",
    ),
  ).toBeVisible();
  await expect(
    page.getByRole("button", {
      name: "Save Fail when free space is under (GB)",
    }),
  ).toBeDisabled();
  await warn.fill("0");
  await expect(page.getByText("Use between 1 and 10000 GB.")).toBeVisible();

  await warn.fill("60");
  await saveField(page, "Warn when free space is under (GB)");
  await fail.fill("12");
  await saveField(page, "Fail when free space is under (GB)");
  await page.goto("/settings/notifications");
  const notice = page.getByLabel(
    "The drive that holds Puddle's data runs low on space",
  );
  await expect(notice).toBeChecked();
  await notice.uncheck();
  await expect
    .poll(async () => (await stored(request, backend)).ui.notify_low_disk_space)
    .toBe(false);
  const ui = (await stored(request, backend)).ui;
  expect(ui.disk_warn_free_gb).toBe(60);
  expect(ui.disk_fail_free_gb).toBe(12);
  await page.reload();
  await expect(notice).not.toBeChecked();
  await page.goto("/settings/workspaces");
  await expect(warn).toHaveValue("60");
  await expect(fail).toHaveValue("12");
});

test("the SSH agent pipe is checked before saving, stored trimmed and survives a reload", async ({
  page,
  backend,
  request,
}) => {
  await openSettings(page, backend, "editor-and-git");
  const field = page.getByLabel("SSH agent pipe or socket", { exact: true });
  await expect(field).toHaveValue("");

  await field.fill("/run/a\tb");
  await expect(
    page.getByText("A pipe or socket path cannot hold a control character."),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Save SSH agent pipe or socket" }),
  ).toBeDisabled();

  await field.fill(String.raw`  \\.\pipe\pageant.alice  `);
  await saveField(page, "SSH agent pipe or socket");
  expect((await stored(request, backend)).ui.ssh_agent_pipe).toBe(
    String.raw`\\.\pipe\pageant.alice`,
  );
  await page.reload();
  await expect(field).toHaveValue(String.raw`\\.\pipe\pageant.alice`);

  await field.fill("");
  await saveField(page, "SSH agent pipe or socket");
  expect((await stored(request, backend)).ui.ssh_agent_pipe).toBeNull();
});
