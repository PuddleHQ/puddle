// SPDX-License-Identifier: AGPL-3.0-or-later
// The workspace screens against the real API on the fixture backend (fake workspace service).
// Every test starts from the `lived-in` scenario on its worker's own backend: web-shop running,
// docs-site stopped with unsaved work, data-tools never started, and requests waiting for each.
// Operations finish at once unless a test holds them (`hold_workspaces`) to look at the busy
// state; the page clock is the fixture's.
// The settings tab, keyboard use, accessibility and the disk cases.
import { cpuLabel, CPUS_MIN } from "../src/lib/settings/cpus.ts";
import { expect, test } from "./fixture";
import {
  axeStops,
  cpuLimits,
  watchCsp,
  expectNoAxeViolations,
  staysEmpty,
} from "./support";
import {
  auth,
  card,
  installReset,
  nameLink,
  openDetail,
  openList,
  toast,
} from "./workspaces-support";

installReset();

test.describe("the settings tab", () => {
  test("memory: choose a size, it is kept and applies at the next start", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const memory = page.getByLabel("Memory", { exact: true });
    await expect(memory.locator("option")).not.toContainText([
      /Use the global/,
    ]);
    await expect(
      page.getByText(/A change applies at the next restart/),
    ).toBeVisible();
    await memory.selectOption({ label: "16 GiB" });
    await expect(page.getByText("Memory saved.")).toBeVisible();
    await expect(
      page.getByText(
        "The new memory applies the next time the workspace starts.",
      ),
    ).toBeVisible();
    await expect(
      page.locator('[data-setting="memory"] .meta', {
        hasText: "This workspace",
      }),
    ).toBeVisible();
    const stored = await request.get("/api/settings/workspaces/web-shop", {
      headers: auth(backend),
    });
    const view = (await stored.json()) as {
      overrides: { memory: number | null };
    };
    expect(view.overrides.memory).toBe(16_384);
    await page.reload();
    await expect(page.getByLabel("Memory", { exact: true })).toHaveValue(
      "16384",
    );
    await page
      .getByRole("button", { name: "Use the global setting again for Memory" })
      .click();
    await expect(page.getByText("Memory saved.")).toBeVisible();
    const again = await request.get("/api/settings/workspaces/web-shop", {
      headers: auth(backend),
    });
    expect(
      ((await again.json()) as { overrides: { memory: unknown } }).overrides
        .memory,
    ).toBeNull();
  });

  test("cpus: choose a count from the keyboard, it is kept, says a running workspace keeps its count, and the API takes 1 up to the computer's cores only", async ({
    page,
    request,
    backend,
  }) => {
    const limits = await cpuLimits(request, backend.token);
    // The arrow key below picks the first count; a count of one would not change the select.
    test.skip(limits.max_cpus < 2, "a one-core computer");
    await openDetail(page, backend, "web-shop", "settings");
    const cpus = page.getByLabel("CPUs", { exact: true });
    await expect(cpus).toHaveValue(String(limits.default_cpus));
    await expect(cpus.locator("option").first()).toHaveText(cpuLabel(CPUS_MIN));
    await expect(cpus.locator("option")).toHaveCount(
      limits.max_cpus - CPUS_MIN + 1,
    );
    await expect(
      page.getByText(/A running workspace keeps the count it started with/),
    ).toBeVisible();

    const up = limits.default_cpus < limits.max_cpus;
    const chosen = limits.default_cpus + (up ? 1 : -1);
    await page.getByLabel("Memory", { exact: true }).focus();
    await page.keyboard.press("Tab");
    await expect(cpus).toBeFocused();
    await page.keyboard.press(up ? "ArrowDown" : "ArrowUp");
    await expect(page.getByText("CPUs saved.")).toBeVisible();
    await expect(
      page.getByText(/The new CPU count applies the next time/),
    ).toBeVisible();
    const url = "/api/settings/workspaces/web-shop";
    const read = async () =>
      (await (await request.get(url, { headers: auth(backend) })).json()) as {
        overrides: { cpus: number | null };
      };
    expect((await read()).overrides.cpus).toBe(chosen);
    await expect(page.locator('[data-setting="cpus"] .meta')).toHaveText(
      "This workspace",
    );

    await page.reload();
    await expect(page.getByLabel("CPUs", { exact: true })).toHaveValue(
      String(chosen),
    );
    await page
      .getByRole("button", { name: "Use the global setting again for CPUs" })
      .click();
    await expect(page.getByText("CPUs saved.")).toBeVisible();
    expect((await read()).overrides.cpus).toBeNull();

    // The limits the page offers are the ones the API enforces.
    const { overrides } = await read();
    const put = async (count: number) =>
      (
        await request.put(url, {
          headers: auth(backend),
          data: { overrides: { ...overrides, cpus: count } },
        })
      ).ok();
    // Each put is an action whose answer is the thing under test, so it is made once, not polled.
    const accepted = {
      belowMin: await put(CPUS_MIN - 1),
      aboveMax: await put(limits.max_cpus + 1),
      min: await put(CPUS_MIN),
      max: await put(limits.max_cpus),
    };
    expect(accepted).toEqual({
      belowMin: false,
      aboveMax: false,
      min: true,
      max: true,
    });
  });

  test("local destinations and clipboard follow the global setting until set here", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const loopback = page.getByLabel("This computer (loopback)", {
      exact: true,
    });
    await expect(loopback).toHaveValue("off");
    await expect(loopback.locator("option")).toHaveText([
      "Allowed",
      "Not allowed",
    ]);
    await loopback.selectOption("on");
    await expect(
      page.getByText("This computer (loopback) saved."),
    ).toBeVisible();
    const clipboard = page.getByLabel("Pages reading the clipboard", {
      exact: true,
    });
    await expect(clipboard).toHaveValue("allow");
    await clipboard.selectOption("deny");
    await expect(page.getByText("Clipboard saved.")).toBeVisible();
    const stored = (await (
      await request.get("/api/settings/workspaces/web-shop", {
        headers: auth(backend),
      })
    ).json()) as {
      overrides: {
        local_toggles: { loopback: boolean | null; private: boolean | null };
        clipboard_read: string | null;
      };
      effective: {
        local_toggles: { loopback: { source: string; value: boolean } };
      };
    };
    expect(stored.overrides.local_toggles.loopback).toBe(true);
    expect(stored.overrides.local_toggles.private).toBeNull();
    expect(stored.overrides.clipboard_read).toBe("deny");
    expect(stored.effective.local_toggles.loopback).toEqual({
      value: true,
      source: "workspace",
    });
    await page
      .getByRole("button", {
        name: "Use the global setting again for This computer (loopback)",
      })
      .click();
    await expect(
      page.getByText("This computer (loopback) saved."),
    ).toBeVisible();
  });

  test("login capture is on by default, can be turned off for the workspace and goes back to the global default", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const capture = page.getByLabel("Keep logins made in this workspace", {
      exact: true,
    });
    await expect(capture).toHaveValue("on");
    await expect(capture.locator("option")).toHaveText(["On", "Off"]);
    await capture.selectOption("off");
    await expect(page.getByText("Login capture saved.")).toBeVisible();
    const stored = (await (
      await request.get("/api/settings/workspaces/web-shop", {
        headers: auth(backend),
      })
    ).json()) as {
      overrides: { capture_logins: boolean | null };
      effective: { capture_logins: { source: string; value: boolean } };
    };
    expect(stored.overrides.capture_logins).toBe(false);
    expect(stored.effective.capture_logins).toEqual({
      value: false,
      source: "workspace",
    });
    await page.reload();
    await expect(
      page.getByLabel("Keep logins made in this workspace", { exact: true }),
    ).toHaveValue("off");
    await page
      .getByRole("button", {
        name: "Use the global setting again for Keep logins made in this workspace",
      })
      .click();
    await expect(page.getByText("Login capture saved.")).toBeVisible();
  });

  test("an install that never chose is offered the choice once, and Settings keeps the global mode and learning", async ({
    page,
    backend,
    request,
  }) => {
    // The fixture's install has been through setup; a notice flag that is off is an install from
    // before the choice existed.
    const reset = await request.put("/api/interception", {
      headers: auth(backend),
      data: { notice_seen: false },
    });
    expect(reset.ok()).toBe(true);
    await backend.signIn(page);
    await page.goto("/workspaces");
    const notice = page
      .getByRole("region", { name: "Notices" })
      .getByText("decrypt every HTTPS host");
    await expect(notice).toBeVisible();
    await page.getByRole("button", { name: "Keep it as it is" }).click();
    await expect(notice).toHaveCount(0);
    await page.reload();
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(notice).toHaveCount(0);

    await page.goto("/settings/network");
    const mode = page.getByLabel("Which hosts to decrypt");
    await expect(mode).toHaveValue("known_only");
    await mode.selectOption("all_except_curated");
    await expect(page.getByText("Saved.")).toBeVisible();
    await page.getByLabel("Learn from rejections").uncheck();
    await expect(page.getByText("Saved.")).toHaveCount(2);
    await page.reload();
    await expect(page.getByLabel("Which hosts to decrypt")).toHaveValue(
      "all_except_curated",
    );
    await expect(page.getByLabel("Learn from rejections")).not.toBeChecked();
  });

  test("a workspace follows the global interception mode by default; its own mode and the never-decrypt names are kept", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const mode = page.getByRole("combobox", { name: "Which hosts to decrypt" });
    await expect(mode).toHaveValue("known_only");
    await mode.selectOption("all");
    await expect(page.getByText("Interception mode saved.")).toBeVisible();
    const names = page.getByLabel("Never decrypt in this workspace");
    await names.fill("Bank.example\n*.corp.test");
    await page
      .getByRole("button", { name: "Save Never decrypt in this workspace" })
      .click();
    await expect(page.getByText("Saved.", { exact: true })).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("combobox", { name: "Which hosts to decrypt" }),
    ).toHaveValue("all");
    await expect(
      page.getByLabel("Never decrypt in this workspace"),
    ).toHaveValue("*.corp.test\nbank.example");
    await names.fill("10.0.0.1");
    await page
      .getByRole("button", { name: "Save Never decrypt in this workspace" })
      .click();
    await expect(page.getByRole("alert")).toContainText("10.0.0.1");
  });

  test("a host a tool keeps refusing is listed with its history, announced from the third time and made permanent", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const notice = page.getByRole("region", { name: "Notices" });
    const group = page.getByRole("group", {
      name: "Hosts a tool refused Puddle's certificate for",
    });
    // The event stream may connect after the page loaded, so ask again until it is heard.
    await expect(async () => {
      await backend.control.step({
        do: "learned",
        workspace: "web-shop",
        host: "pinned.example",
        expires_in_secs: 600,
      });
      await expect(group).toContainText("pinned.example", { timeout: 500 });
    }).toPass();
    await expect(group).toContainText("refused once");
    await expect(group).toContainText("in 10 minutes");
    await expect(notice).not.toContainText("pinned.example");
    await expect(async () => {
      await backend.control.step({
        do: "learned",
        workspace: "web-shop",
        host: "pinned.example",
        expires_in_secs: 600,
        count: 3,
      });
      await expect(notice).toContainText(
        "A tool keeps refusing Puddle's certificate for pinned.example",
        { timeout: 500 },
      );
    }).toPass();
    await expect(group).toContainText("refused 3 times");
    await notice.getByRole("button", { name: "Never decrypt it here" }).click();
    await expect(group).toContainText("None.");
    await expect(notice).not.toContainText("pinned.example");
    await expect(
      page.getByLabel("Never decrypt in this workspace"),
    ).toHaveValue("pinned.example");
  });

  test("a refused host is tried again from the list and made permanent everywhere", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    const group = page.getByRole("group", {
      name: "Hosts a tool refused Puddle's certificate for",
    });
    await expect(async () => {
      await backend.control.step({
        do: "learned",
        workspace: "web-shop",
        host: "pinned.example",
        expires_in_secs: 600,
        count: 2,
      });
      await expect(group).toContainText("pinned.example", { timeout: 500 });
    }).toPass();
    await group
      .getByRole("button", { name: "Try decrypting pinned.example again" })
      .click();
    await expect(group).toContainText("pinned.example");
    await expect(group).toContainText("refused 2 times");
    await expect(
      group.getByRole("button", {
        name: "Try decrypting pinned.example again",
      }),
    ).toHaveCount(0);
    await group
      .getByRole("button", {
        name: "Never decrypt pinned.example in any workspace",
      })
      .click();
    await expect(group).toContainText("None.");
    await expect(
      page.getByText(/Never decrypted in any workspace/),
    ).toContainText("pinned.example");
  });

  test("a refused save says why and puts the choice back", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop", "settings");
    await page.route("**/api/settings/workspaces/web-shop", async (route) => {
      if (route.request().method() === "PUT") {
        await route.fulfill({
          status: 422,
          contentType: "application/json",
          body: JSON.stringify({
            error: "invalid",
            message: "memory is too small",
          }),
        });
      } else await route.continue();
    });
    const memory = page.getByLabel("Memory", { exact: true });
    const before = await memory.inputValue();
    await memory.selectOption({ label: "4 GiB" });
    await expect(page.getByRole("alert")).toContainText("memory is too small");
    await expect(memory).toHaveValue(before);
  });
});

test.describe("keyboard only", () => {
  test("create, start, and delete without a pointer", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await page.getByRole("button", { name: "New workspace" }).first().focus();
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("region", { name: "New workspace" });
    await expect(dialog.getByLabel("Git repository (HTTPS)")).toBeFocused();
    await page.keyboard.type("https://github.com/acme/keys.git");
    await page.keyboard.press("Enter");
    await expect(dialog).toHaveCount(0);
    const created = card(page, "keys");
    // A create ends running: stop it, then start it again, all from the keyboard.
    await expect(created).toContainText("Running");
    await created.getByRole("button", { name: "More about keys" }).focus();
    await page.keyboard.press("Enter");
    await page.getByRole("menuitem", { name: "Stop keys" }).focus();
    await page.keyboard.press("Enter");
    await expect(created).toContainText("Stopped");

    await created.getByRole("button", { name: "Start keys" }).focus();
    await page.keyboard.press("Enter");
    await expect(created).toContainText("Running");
    await created.getByRole("button", { name: "More about keys" }).focus();
    await page.keyboard.press("Enter");
    await page.getByRole("menuitem", { name: "Stop keys" }).focus();
    await page.keyboard.press("Enter");
    await expect(created).toContainText("Stopped");

    await nameLink(created, "keys").focus();
    await page.keyboard.press("Enter");
    await page
      .getByRole("navigation", { name: "Workspace sections" })
      .getByRole("link", { name: "Settings" })
      .focus();
    await page.keyboard.press("Enter");
    const del = page.getByRole("button", { name: "Delete workspace…" });
    await del.focus();
    await page.keyboard.press("Enter");
    const confirm = page.getByRole("alertdialog");
    await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(confirm.getByRole("checkbox")).toBeFocused();
    await page.keyboard.press("Space");
    await expect(confirm.getByRole("checkbox")).toBeChecked();
    await page.keyboard.press("Tab"); // Cancel again: the safe button comes before Delete
    await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(
      confirm.getByRole("button", { name: "Delete workspace" }),
    ).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/workspaces$/);
    await expect(card(page, "keys")).toHaveCount(0);
  });

  test("Escape closes the delete dialog and focus returns to its button", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site", "settings");
    const del = page.getByRole("button", { name: "Delete workspace…" });
    await del.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("alertdialog")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("alertdialog")).toHaveCount(0);
    await expect(del).toBeFocused();
  });
});

test.describe("accessibility", () => {
  // A scan costs seconds on a loaded host: each test scans a group of at most two stops.
  const stops = axeStops([
    ["list", "create with errors"],
    ["connect step", "direct SSH trust text"],
    ["failure and toast"],
  ]);
  for (const scheme of ["light", "dark"] as const) {
    for (const group of stops.groups) {
      test(`axe finds nothing in ${scheme}: ${group.join(", ")}`, async ({
        page,
        backend,
      }) => {
        const scan = stops.scanner(group);
        await page.emulateMedia({ colorScheme: scheme });
        const csp = await watchCsp(page);
        await openList(page, backend);
        await scan(page, "list");

        await page
          .getByRole("button", { name: "New workspace" })
          .first()
          .click();
        const create = page.getByRole("region", { name: "New workspace" });
        await create.getByRole("button", { name: "Create workspace" }).click();
        await expect(create.getByRole("alert")).toHaveCount(2);
        await scan(page, "create with errors");
        await create.getByRole("button", { name: "Cancel" }).click();

        const docs = card(page, "docs-site");
        await docs.getByRole("button", { name: "Start docs-site" }).click();
        await expect(docs).toContainText("Running");
        await docs
          .getByRole("button", { name: "Connect to docs-site" })
          .click();
        await scan(page, "connect step");
        await page.getByRole("checkbox", { name: "Allow direct SSH" }).click();
        await scan(page, "direct SSH trust text");
        await page.keyboard.press("Escape");
        await page.keyboard.press("Escape");

        await backend.control.step({
          do: "fail_workspace",
          operation: "start",
          reason: "boom",
        });
        await card(page, "data-tools")
          .getByRole("button", { name: "Start data-tools" })
          .click();
        await expect(card(page, "data-tools").getByRole("alert")).toBeVisible();
        await scan(page, "failure and toast");
        await staysEmpty(page, csp);
        scan.done();
      });
    }

    test(`axe finds nothing in ${scheme}: the overview with a notice and the delete dialog`, async ({
      page,
      backend,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      await openDetail(page, backend, "web-shop");
      await backend.control.script("oom");
      await expect(page.getByRole("alert")).toBeVisible();
      await expectNoAxeViolations(page, "overview with the notice");

      await page.goto("/workspaces/data-tools/settings");
      await expect(
        page.getByRole("heading", { level: 1, name: "data-tools" }),
      ).toBeVisible();
      await page.getByRole("button", { name: "Delete workspace…" }).click();
      const confirm = page.getByRole("alertdialog");
      await expect(confirm).toBeVisible();
      await expectNoAxeViolations(page, "delete clean");
      await page.keyboard.press("Escape");
      await staysEmpty(page, csp);
    });

    for (const tab of [
      "network",
      "git",
      "environment",
      "shell-init",
      "ssh-agent",
      "ports",
      "settings",
    ]) {
      test(`axe finds nothing in ${scheme}: the ${tab} tab`, async ({
        page,
        backend,
      }) => {
        await page.emulateMedia({ colorScheme: scheme });
        const csp = await watchCsp(page);
        // The notice is up when the tab opens, as it is in use.
        await openDetail(page, backend, "web-shop");
        await backend.control.script("oom");
        await expect(page.getByRole("alert")).toBeVisible();
        await page.goto(`/workspaces/web-shop/${tab}`);
        await expect(
          page.getByRole("heading", { level: 1, name: "web-shop" }),
        ).toBeVisible();
        await expect(page.getByText("Loading")).toHaveCount(0);
        await expectNoAxeViolations(page, tab);
        await staysEmpty(page, csp);
      });
    }
  }

  test("the delete dialog with a long list passes axe too", async ({
    page,
    backend,
  }) => {
    await openDetail(page, backend, "docs-site", "settings");
    await page.getByRole("button", { name: "Delete workspace…" }).click();
    await expect(page.getByRole("alertdialog")).toContainText("stash@{0}");
    await expectNoAxeViolations(page, "delete with losses");
  });

  test("the buttons are at least 24 by 24 CSS pixels (WCAG 2.5.8)", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    // The heading comes before the list: `all()` counts what is there now and waits for nothing,
    // so wait until the card has its buttons (and the header's) before taking them.
    const shop = card(page, "web-shop");
    await expect(shop).toBeVisible();
    await expect(shop.getByRole("button")).not.toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "New workspace" }),
    ).not.toHaveCount(0);
    const buttons = await shop.getByRole("button").all();
    expect(buttons.length).toBeGreaterThanOrEqual(2);
    for (const button of [
      ...buttons,
      ...(await page.getByRole("button", { name: "New workspace" }).all()),
    ]) {
      const box = await button.boundingBox();
      expect(box?.width ?? 0).toBeGreaterThanOrEqual(24);
      expect(box?.height ?? 0).toBeGreaterThanOrEqual(24);
    }
  });
});

test.describe("a workspace whose volume is gone", () => {
  test("says so on the card and the page, and deleting it is allowed and loses nothing", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("volume-missing");
    await openList(page, backend);
    const lost = card(page, "lost-disk");
    await expect(lost).toContainText("Volume missing");
    await expect(lost).toContainText("Restore its volume");
    await lost.getByText("Volume name").click();
    await expect(lost).toContainText("ws-lost-disk");
    await expect(
      lost.getByRole("button", { name: "Start lost-disk" }),
    ).toBeEnabled();
    await expectNoAxeViolations(page, "list");

    await nameLink(lost, "lost-disk").click();
    await expect(page.getByText("Its disk is gone")).toBeVisible();
    await page
      .getByRole("navigation", { name: "Workspace sections" })
      .getByRole("link", { name: "Settings" })
      .click();
    await page.getByRole("button", { name: "Delete workspace…" }).click();
    const confirm = page.getByRole("alertdialog");
    await expect(confirm).toContainText("nothing is lost");
    await expectNoAxeViolations(page, "delete dialog");
    await confirm.getByRole("checkbox", { name: "Delete lost-disk" }).check();
    await confirm.getByRole("button", { name: "Delete workspace" }).click();
    await expect(page).toHaveURL(/\/workspaces$/);
    await expect(card(page, "lost-disk")).toHaveCount(0);
  });
});

test.describe("a workspace whose root disk is nearly full, made bigger", () => {
  test("makes the root disk bigger from the warning, refuses a smaller size, and clears the warning", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("disks-full");
    await openDetail(page, backend, "full-root");
    const root = page.getByTestId("disk-warning-root");
    await expect(root).toContainText("Root disk nearly full");
    await root.getByRole("button", { name: "Make root disk bigger…" }).click();
    const dialog = page.getByRole("dialog");
    const size = dialog.getByRole("textbox", { name: /New size in GiB/ });
    await expect(size).toHaveValue("20");
    await expect(dialog).toContainText("has the room when it starts next");
    await expectNoAxeViolations(page, "grow dialog");
    await size.fill("8");
    await expect(dialog.getByRole("alert")).toContainText(
      "A root disk only grows: choose more than 10 GiB.",
    );
    await expectNoAxeViolations(page, "grow dialog with a refusal");
    await size.fill("24");
    await dialog.getByRole("button", { name: "Make bigger" }).click();
    await expect(dialog).toHaveCount(0);
    await expect(
      toast(page, "Made the root disk of full-root bigger."),
    ).toBeVisible();
    await expect(page.getByTestId("disk-warning-root")).toHaveCount(0);
    await page
      .getByRole("navigation", { name: "Workspace sections" })
      .getByRole("link", { name: "Settings" })
      .click();
    await expect(page.getByText("9.7 GiB of 24 GiB used")).toBeVisible();
    await expectNoAxeViolations(page, "after the grow");
  });

  test.describe("at 390 px wide", () => {
    test.use({ viewport: { width: 390, height: 844 } });

    test("the warning and the dialog fit without a horizontal scroll", async ({
      page,
      backend,
    }) => {
      await backend.control.reset("disks-full");
      await openDetail(page, backend, "full-root");
      const overflow = () =>
        page.evaluate(() => {
          const width = document.documentElement.clientWidth;
          const culprits = [...document.body.querySelectorAll("*")]
            .filter((el) => el.getBoundingClientRect().right > width + 0.5)
            .slice(0, 6)
            .map(
              (el) =>
                `${el.tagName.toLowerCase()}.${String(el.className)} right=${Math.round(el.getBoundingClientRect().right)}`,
            );
          return {
            by: document.documentElement.scrollWidth - width,
            culprits,
          };
        });
      const before = await overflow();
      expect(before.by, before.culprits.join(", ")).toBe(0);
      await page
        .getByTestId("disk-warning-root")
        .getByRole("button", { name: "Make root disk bigger…" })
        .click();
      await expect(page.getByRole("dialog")).toBeVisible();
      const during = await overflow();
      expect(during.by, during.culprits.join(", ")).toBe(0);
      await expectNoAxeViolations(page, "grow dialog at 390 px");
    });
  });
});

test.describe("a workspace whose disk is nearly full", () => {
  test("warns on the page and the card for the root disk and the volume, and resets only the root disk", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("disks-full");
    await openList(page, backend);
    await expect(card(page, "full-root")).toContainText(/\d+ % full/);
    await expect(card(page, "full-volume")).toContainText(/\d+ % full/);
    await expect(card(page, "roomy")).not.toContainText(/\d+ % full/);

    await openDetail(page, backend, "roomy");
    await expect(page.getByTestId("disk-warning-root")).toHaveCount(0);
    await expect(page.getByTestId("disk-warning-volume")).toHaveCount(0);

    await openDetail(page, backend, "full-volume");
    const volume = page.getByTestId("disk-warning-volume");
    await expect(volume).toContainText("Volume nearly full");
    await expect(page.getByTestId("disk-warning-root")).toHaveCount(0);
    await expect(
      volume.getByRole("button", { name: /Reset root disk/ }),
    ).toHaveCount(0);

    await openDetail(page, backend, "full-root");
    const root = page.getByTestId("disk-warning-root");
    await expect(root).toContainText("Root disk nearly full");
    await expect(root).toContainText("volume and your work are untouched");
    await expectNoAxeViolations(page, "disk warning");
    await root.getByRole("button", { name: /Reset root disk/ }).click();
    const confirm = page.getByRole("alertdialog");
    await expect(confirm).toContainText("The volume, with your repositories");
    await expectNoAxeViolations(page, "reset dialog");
    await confirm.getByRole("button", { name: "Reset root disk" }).click();
    await expect(page.getByTestId("disk-warning-root")).toHaveCount(0);
  });
});
