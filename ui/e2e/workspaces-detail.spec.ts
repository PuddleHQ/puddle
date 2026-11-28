// SPDX-License-Identifier: AGPL-3.0-or-later
// The workspace screens against the real API on the fixture backend (fake workspace service).
// Every test starts from the `lived-in` scenario on its worker's own backend: web-shop running,
// docs-site stopped with unsaved work, data-tools never started, and requests waiting for each.
// Operations finish at once unless a test holds them (`hold_workspaces`) to look at the busy
// state; the page clock is the fixture's.
// Deleting, the detail page, renaming and the network tab.
import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";
import {
  auth,
  card,
  installReset,
  nameLink,
  openDetail,
  openList,
  pendingCount,
  toast,
  workspaceList,
} from "./workspaces-support";

installReset();

test.describe("deleting", () => {
  test("lists what would be lost and needs the box ticked", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site", "settings");
    await page.getByRole("button", { name: "Delete workspace…" }).click();
    const dialog = page.getByRole("alertdialog", { name: "Delete docs-site?" });
    await expect(dialog).toContainText("Uncommitted changes");
    await expect(dialog).toContainText("M content/intro.md");
    await expect(dialog).toContainText("?? drafts/launch.md");
    await expect(dialog).toContainText("3f2a9c1 Rewrite the intro");
    await expect(dialog).toContainText("stash@{0}");
    await expect(dialog).toContainText("scratch");
    await expect(dialog.getByRole("note")).toHaveText(
      "The unsaved work check ignores git-ignored files.",
    );
    await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();
    const remove = dialog.getByRole("button", { name: "Delete workspace" });
    await expect(remove).toBeDisabled();
    await dialog
      .getByRole("checkbox", { name: /I understand this work will be lost/ })
      .click();
    await expect(remove).toBeEnabled();
    await remove.click();
    await expect(toast(page, "Deleted docs-site.")).toBeVisible();
    await expect(page).toHaveURL(/\/workspaces$/);
    await expect(page.getByRole("article")).toHaveCount(2);
    expect(
      (await workspaceList(request, backend)).some(
        (w) => w.name === "docs-site",
      ),
    ).toBe(false);
  });

  test("Cancel deletes nothing and returns to the button", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site", "settings");
    const button = page.getByRole("button", { name: "Delete workspace…" });
    await button.click();
    await page
      .getByRole("alertdialog")
      .getByRole("button", { name: "Cancel" })
      .click();
    await expect(page.getByRole("alertdialog")).toHaveCount(0);
    await expect(button).toBeFocused();
    expect((await workspaceList(request, backend)).length).toBe(3);
  });

  test("a clean workspace says so, and still asks", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "data-tools", "settings");
    await page.getByRole("button", { name: "Delete workspace…" }).click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("found nothing unsaved");
    await expect(
      dialog.getByRole("button", { name: "Delete workspace" }),
    ).toBeDisabled();
    await dialog.getByRole("checkbox", { name: /Delete data-tools/ }).click();
    await dialog.getByRole("button", { name: "Delete workspace" }).click();
    await expect(toast(page, "Deleted data-tools.")).toBeVisible();
  });

  test("a running workspace can't be deleted", async ({ page, backend }) => {
    await openDetail(page, backend, "web-shop", "settings");
    await expect(
      page.getByRole("button", { name: "Delete workspace…" }),
    ).toBeDisabled();
    await expect(
      page.getByText("Stop the workspace before deleting it."),
    ).toBeVisible();
  });

  test("a delete that fails says why and the workspace stays", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "data-tools", "settings");
    await backend.control.step({
      do: "fail_workspace",
      operation: "delete",
      reason: "the volume is in use",
    });
    await page.getByRole("button", { name: "Delete workspace…" }).click();
    const dialog = page.getByRole("alertdialog");
    await dialog.getByRole("checkbox").click();
    await dialog.getByRole("button", { name: "Delete workspace" }).click();
    await expect(
      toast(page, "Deleting data-tools failed: the volume is in use"),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { level: 1, name: "data-tools" }),
    ).toBeVisible();
    expect(
      (await workspaceList(request, backend)).some(
        (w) => w.name === "data-tools",
      ),
    ).toBe(true);
  });

  test("deleted from elsewhere takes the open page back to the list", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "data-tools");
    const check = await request.get("/api/workspaces/data-tools/delete-check", {
      headers: auth(backend),
    });
    const { fingerprint } = (await check.json()) as { fingerprint: string };
    await request.delete("/api/workspaces/data-tools", {
      headers: auth(backend),
      data: { confirm: true, fingerprint },
    });
    // Only the event can do this: the list polls every 60 s. 15 s is the starved browser's ceiling; the
    // whole test took 5 to 10 s there.
    await expect(page).toHaveURL(/\/workspaces$/, { timeout: 15_000 });
    await expect(page.getByRole("article")).toHaveCount(2);
  });

  test("an unknown workspace says so", async ({ page, backend }) => {
    await backend.installClock(page);
    await backend.signIn(page);
    await page.goto("/workspaces/nope");
    await expect(
      page.getByRole("heading", { name: "No such workspace" }),
    ).toBeVisible();
    await page.getByRole("link", { name: "Back to the workspaces" }).click();
    await expect(page).toHaveURL(/\/workspaces$/);
  });
});

test.describe("the detail page", () => {
  test("overview: five tabs, what waits and what the workspace holds, with no id and no state row", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    const tabs = page.getByRole("navigation", { name: "Workspace sections" });
    await expect(tabs.getByRole("link")).toHaveText([
      /Overview/,
      /Terminal/,
      /Network/,
      /Git/,
      /Settings/,
    ]);
    await expect(tabs.getByRole("link", { name: /Overview/ })).toHaveAttribute(
      "aria-current",
      "page",
    );
    await expect(
      page.getByRole("heading", { name: /Waiting for you/ }),
    ).toBeVisible();
    await expect(page.getByText("Workspace id")).toHaveCount(0);
    await expect(page.getByText("ws-web-shop")).toHaveCount(0);
    await expect(page.getByText("State", { exact: true })).toHaveCount(0);
    await expect(
      page.getByRole("heading", { name: "Delete workspace" }),
    ).toHaveCount(0);
  });

  test("the settings tab is one table with Advanced and Danger groups, and a dropdown that shows only its value", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const table = page.getByRole("table", { name: "Settings of web-shop" });
    await expect(table).toBeVisible();
    await expect(
      table.getByRole("columnheader", { name: "Advanced" }),
    ).toBeVisible();
    await expect(
      table.getByRole("columnheader", { name: "Danger" }),
    ).toBeVisible();
    const link = page.getByRole("link", { name: "Ports" });
    await expect(link).toBeVisible();
    const memory = page.getByLabel("Memory", { exact: true });
    await expect(memory).toBeVisible();
    await expect(memory.locator("option")).not.toContainText([
      /Use the global/,
    ]);
    // The text of a value is not cut off: the select is as wide as its longest option needs.
    const box = await page
      .getByLabel("Private networks", { exact: true })
      .boundingBox();
    expect(box?.width ?? 0).toBeGreaterThan(90);
  });

  test("a kill by the out-of-memory killer shows a notice that links to the memory setting", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    await backend.control.script("oom");
    const notice = page.getByRole("alert");
    await expect(notice).toContainText("Out of memory");
    await expect(notice).toContainText("node");
    await expect(notice).toContainText("process 4242");
    await notice.getByRole("link", { name: "Change memory" }).click();
    await expect(page).toHaveURL(/\/workspaces\/web-shop\/settings$/);
    await page.getByRole("link", { name: "Overview" }).click();
    await expect(page.getByRole("alert")).toContainText("Out of memory");
    await page.getByRole("button", { name: "Dismiss", exact: true }).click();
    await expect(page.getByRole("alert")).toHaveCount(0);
  });

  test("the card on the list flags the kill too", async ({ page, backend }) => {
    await openList(page, backend);
    await backend.control.script("oom");
    await expect(card(page, "web-shop")).toContainText("Out of memory");
  });

  test("reclaim space runs and reports", async ({ page, backend }) => {
    await openDetail(page, backend, "web-shop", "settings");
    await backend.control.step({ do: "hold_workspaces" });
    await page.getByRole("button", { name: "Reclaim space" }).click();
    await expect(
      page.getByRole("status").filter({ hasText: "Reclaiming" }),
    ).toBeVisible();
    await backend.control.step({ do: "release_workspaces" });
    await expect(
      toast(page, "Reclaimed free space on web-shop."),
    ).toBeVisible();
  });

  test("Environment, Shell init, SSH agent and Ports are pages under Settings, and the old addresses lead to them", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    for (const name of ["Environment", "Shell init", "SSH agent", "Ports"]) {
      await page
        .getByRole("navigation", { name: "Workspace settings" })
        .getByRole("link", { name })
        .click();
      await expect(page.getByRole("heading", { level: 2, name })).toBeVisible();
      await expect(
        page
          .getByRole("navigation", { name: "Workspace settings" })
          .getByRole("link", { name }),
      ).toHaveAttribute("aria-current", "page");
      await expect(
        page
          .getByRole("navigation", { name: "Workspace sections" })
          .getByRole("link", { name: "Settings" }),
      ).toHaveAttribute("aria-current", "page");
    }
    await page.goto("/workspaces/web-shop/ports");
    await expect(page).toHaveURL(/\/workspaces\/web-shop\/settings\/ports$/);
    await page.goto("/workspaces/web-shop/environment");
    await expect(page).toHaveURL(
      /\/workspaces\/web-shop\/settings\/environment$/,
    );
  });

  test("start and stop work from the detail page", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site");
    await page.getByRole("button", { name: "Start docs-site" }).click();
    await expect(toast(page, "docs-site is running.")).toBeVisible();
    await page.getByRole("button", { name: "Stop docs-site" }).click();
    await expect(toast(page, "Stopped docs-site.")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Start docs-site" }),
    ).toBeEnabled();
  });
});

test.describe("renaming", () => {
  const rename = (page: Page) => page.getByRole("button", { name: /^Rename / });
  const dialog = (page: Page) =>
    page.getByRole("dialog", { name: "Rename workspace" });

  test("a running workspace is renamed from its page and keeps its id, name, state and disk", async ({
    page,
    backend,
    request,
  }) => {
    await openDetail(page, backend, "web-shop");
    await rename(page).click();
    const form = dialog(page);
    await expect(form.getByLabel("Name")).toHaveValue("web-shop");
    await expect(form).toContainText("The new name shows everywhere");
    await form.getByLabel("Name").fill("  Web shop (prod) ");
    await form.getByRole("button", { name: "Save" }).click();
    await expect(form).toHaveCount(0);
    await expect(toast(page, "Renamed to Web shop (prod).")).toBeVisible();
    await expect(
      page.getByRole("heading", { level: 1, name: "Web shop (prod)" }),
    ).toBeVisible();
    // Still running, still the same address, and the id is on the overview.
    await expect(page).toHaveURL(/\/workspaces\/web-shop$/);
    await expect(page.getByText("Running").first()).toBeVisible();
    await expect(page.getByText("Workspace id")).toHaveCount(0);
    await expect(page.getByText("ws-web-shop")).toHaveCount(0);
    const found = (await workspaceList(request, backend)).find(
      (w) => w.id === "web-shop",
    );
    expect(found).toMatchObject({
      id: "web-shop",
      name: "Web shop (prod)",
      status: "running",
      busy: null,
    });
    // The list shows it too.
    await page
      .locator("#main")
      .getByRole("link", { name: "Workspaces", exact: true })
      .click();
    await expect(card(page, "Web shop (prod)")).toBeVisible();
    await expect(card(page, "web-shop")).toHaveCount(0);
    await expect(
      card(page, "Web shop (prod)").getByRole("button", {
        name: "Connect to Web shop (prod)",
      }),
    ).toBeVisible();
  });

  test("the name goes into the words the page says about the workspace", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site");
    await rename(page).click();
    await dialog(page).getByLabel("Name").fill("Handbook");
    await dialog(page).getByRole("button", { name: "Save" }).click();
    await expect(toast(page, "Renamed to Handbook.")).toBeVisible();
    await page.getByRole("button", { name: "Start Handbook" }).click();
    await expect(toast(page, "Handbook is running.")).toBeVisible();
    await page.getByRole("button", { name: "Stop Handbook" }).click();
    await expect(toast(page, "Stopped Handbook.")).toBeVisible();
  });

  test("a name another workspace has is refused as you type, ignoring case, and by the service if it slips through", async ({
    page,
    backend,
    request,
  }) => {
    await openDetail(page, backend, "web-shop");
    await rename(page).click();
    const form = dialog(page);
    await form.getByLabel("Name").fill("DOCS-SITE");
    await expect(form.getByRole("alert")).toContainText(
      'Another workspace is already called "docs-site".',
    );
    await form.getByRole("button", { name: "Save" }).click();
    await expect(form).toBeVisible();
    // Another window names a workspace the page has not heard of yet: the service says no, in
    // the dialog, and the page then reads the list again.
    const raced = await request.patch("/api/workspaces/data-tools", {
      headers: auth(backend),
      data: { name: "Racing" },
    });
    expect(raced.status()).toBe(200);
    await form.getByLabel("Name").fill("racing");
    // Normally the page has not heard yet and the service refuses; if its poll ran first, the form
    // refuses as you type. Either way the dialog says so and stays open.
    await form.getByRole("button", { name: "Save" }).click();
    await expect(form.getByRole("alert")).toContainText(
      /already called "?Racing/i,
    );
    await expect(form).toBeVisible();
    // The workspace's own name is free for it.
    await form.getByLabel("Name").fill("web-shop");
    await expect(form.getByRole("alert")).toHaveCount(0);
  });

  test("a name of spaces, capitals and other scripts is created, shown and renamed, and every address keeps the id", async ({
    page,
    backend,
    request,
  }) => {
    await openList(page, backend);
    await page.getByRole("button", { name: "New workspace" }).first().click();
    const create = page.getByRole("region", { name: "New workspace" });
    await create
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/shop.git");
    await create.getByLabel("Name").fill("Shop API \u00fc");
    await create.getByRole("button", { name: "Create workspace" }).click();
    await expect(create).toHaveCount(0);
    await expect(toast(page, "Created Shop API \u00fc.")).toBeVisible();
    await expect(card(page, "Shop API \u00fc")).toBeVisible();
    const made = (await workspaceList(request, backend)).find(
      (w) => w.name === "Shop API \u00fc",
    );
    expect(made?.id).toBe("shop-api");
    // The page's address and the overview use the id; the heading uses the name.
    await card(page, "Shop API \u00fc")
      .getByRole("link", { name: "Shop API \u00fc", exact: true })
      .click();
    await expect(page).toHaveURL(/\/workspaces\/shop-api$/);
    await expect(
      page.getByRole("heading", { level: 1, name: "Shop API \u00fc" }),
    ).toBeVisible();
    await expect(page.getByText("Workspace id")).toHaveCount(0);
    await expect(page.getByText("shop-api", { exact: true })).toHaveCount(0);
    // Renaming changes the name and nothing else, and the address survives.
    await rename(page).click();
    const form = dialog(page);
    await expect(form.getByLabel("Name")).toHaveValue("Shop API \u00fc");
    await form.getByLabel("Name").fill("  Kiosk \u00c4pp 2  ");
    await form.getByRole("button", { name: "Save" }).click();
    await expect(toast(page, "Renamed to Kiosk \u00c4pp 2.")).toBeVisible();
    await expect(
      page.getByRole("heading", { level: 1, name: "Kiosk \u00c4pp 2" }),
    ).toBeVisible();
    await expect(page).toHaveURL(/\/workspaces\/shop-api$/);
    const renamed = (await workspaceList(request, backend)).find(
      (w) => w.id === "shop-api",
    );
    expect(renamed).toMatchObject({ id: "shop-api", name: "Kiosk \u00c4pp 2" });
    expect(renamed).not.toHaveProperty("display_name");
    // The old name is free at once, and the same name in other case is a clash.
    await rename(page).click();
    await dialog(page).getByLabel("Name").fill("DOCS-SITE");
    await expect(dialog(page).getByRole("alert")).toContainText(
      'Another workspace is already called "docs-site".',
    );
  });

  test("a rename is not offered while a workspace is being created", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({ do: "hold_workspaces" });
    await page.getByRole("button", { name: "New workspace" }).first().click();
    const create = page.getByRole("region", { name: "New workspace" });
    await create
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/slow.git");
    await create.getByRole("button", { name: "Create workspace" }).click();
    await expect(card(page, "slow")).toContainText("Creating");
    await nameLink(card(page, "slow"), "slow").click();
    await expect(
      page.getByRole("button", { name: "Rename slow" }),
    ).toBeDisabled();
    await backend.control.step({ do: "release_workspaces" });
    await expect(
      page.getByRole("button", { name: "Rename slow" }),
    ).toBeEnabled();
  });

  test("keyboard only: Enter opens it, Enter saves, Escape cancels and focus returns to the button", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    await rename(page).focus();
    await page.keyboard.press("Enter");
    await expect(dialog(page).getByLabel("Name")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(dialog(page)).toHaveCount(0);
    await expect(rename(page)).toBeFocused();
    await page.keyboard.press("Enter");
    // Focus moves into the field a moment after the dialog is drawn; keys sent before that land on
    // the Rename button (a space clicks it) and the typed text is lost.
    await expect(dialog(page).getByLabel("Name")).toBeFocused();
    await page.keyboard.press("ControlOrMeta+A");
    await page.keyboard.type("Shop two");
    await page.keyboard.press("Enter");
    await expect(
      page.getByRole("heading", { level: 1, name: "Shop two" }),
    ).toBeVisible();
  });

  for (const scheme of ["light", "dark"] as const) {
    test(`axe finds nothing in ${scheme}: the rename dialog, with and without a refusal`, async ({
      page,
      backend,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await openDetail(page, backend, "web-shop");
      await rename(page).click();
      await expect(dialog(page)).toBeVisible();
      await expectNoAxeViolations(page, "rename dialog");
      await dialog(page).getByLabel("Name").fill("docs-site");
      await expect(dialog(page).getByRole("alert")).toBeVisible();
      await expectNoAxeViolations(page, "rename refused");
    });
  }
});

test.describe("the network tab", () => {
  test("shows this workspace's waiting requests and the rules that apply", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    const waiting = page.locator("li.req");
    await expect(waiting.first()).toBeVisible();
    for (const text of await waiting.allTextContents())
      expect(text).toContain("web-shop");
    await expect(
      page.getByText("api.stripe-sandbox.example.com").first(),
    ).toBeVisible();
    await expect(
      page
        .getByRole("navigation", { name: "Workspace sections" })
        .getByRole("link", { name: /Overview/ }),
    ).toContainText(String(await waiting.count()));
    await page
      .getByRole("navigation", { name: "Workspace sections" })
      .getByRole("link", { name: "Network" })
      .click();
    await expect(page.locator("li.req")).toHaveCount(0);
    await expect(page.getByText(/waiting for web-shop/)).toBeVisible();
    const rules = page.getByRole("table");
    await expect(rules).toContainText("registry.npmjs.org"); // for every workspace
    await expect(rules).toContainText("api.example.com");
    await expect(rules).not.toContainText("ads.example.net"); // docs-site's rule
    await expect(rules.getByRole("button")).toHaveCount(0);
  });

  test("a decision made here removes the row, counts down the tab and can be undone", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site");
    const rows = page.locator("li.req");
    // The heading shows before the requests are read: count only once a row is there.
    await expect(rows.first()).toBeVisible();
    const n = await rows.count();
    const total = await pendingCount(request, backend);
    await rows
      .first()
      .getByRole("button", { name: /^Allow/ })
      .first()
      .click();
    await expect(rows).toHaveCount(n - 1);
    await expect(toast(page, /^Allowed/)).toBeVisible();
    await expect.poll(() => pendingCount(request, backend)).toBe(total - 1);
    await page.getByRole("button", { name: "Undo" }).click();
    await expect(toast(page, /Undone/)).toBeVisible();
  });

  test("Allow all decides every request of the workspace after a confirm and leaves the others", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    const rows = page.locator("li.req");
    // web-shop has three requests waiting in the default scenario.
    const mine = 3;
    await expect(rows).toHaveCount(mine);
    const total = await pendingCount(request, backend);
    await page.getByRole("button", { name: "Allow all" }).click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText(`Allow all ${mine} requests?`);
    await dialog.getByRole("button", { name: "Allow all" }).click();
    await expect(
      page.getByText("Nothing is waiting for web-shop."),
    ).toBeVisible();
    await expect(
      toast(page, `Allowed ${mine} requests for web-shop.`),
    ).toBeVisible();
    await expect.poll(() => pendingCount(request, backend)).toBe(total - mine);
  });

  test("Deny all decides every request of the workspace after a confirm", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    const rows = page.locator("li.req");
    const mine = 3;
    await expect(rows).toHaveCount(mine);
    const total = await pendingCount(request, backend);
    await page.getByRole("button", { name: "Deny all" }).click();
    await page
      .getByRole("alertdialog")
      .getByRole("button", { name: "Deny all" })
      .click();
    await expect(
      toast(page, `Denied ${mine} requests for web-shop.`),
    ).toBeVisible();
    await expect.poll(() => pendingCount(request, backend)).toBe(total - mine);
  });

  test("nothing waiting says so", async ({ page, backend }) => {
    await backend.control.reset("empty");
    await backend.control.reset("lived-in");
    await openDetail(page, backend, "data-tools");
    // data-tools has requests in this scenario; decide them all.
    const rows = page.locator("li.req");
    await expect(rows.first()).toBeVisible();
    // Wait for each row to go before the next click: a row that is still on its way out can
    // be counted but not clicked, and the click then waits for a button that never returns.
    for (let left = await rows.count(); left > 0; left -= 1) {
      await rows.first().getByRole("button", { name: /^Deny/ }).click();
      await expect(rows).toHaveCount(left - 1);
    }
    await expect(
      page.getByText("Nothing is waiting for data-tools."),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Waiting for you" }),
    ).toBeFocused();
  });
});
