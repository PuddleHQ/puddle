// SPDX-License-Identifier: GPL-3.0-or-later
// Rules on method and path against the real API on the fixture backend. Every test starts from the
// `request-rules` scenario on its worker's own backend: a global rule that refuses DELETE on
// /repos/** at api.example.com, a rule of docs-site's own, and one request the first rule refused
// (it shows on the Activity screen). Waits are on conditions, never on time.
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations, staysEmpty, watchCsp } from "./support";

interface RequestRule {
  id: number;
  host: string;
  methods: string[];
  path: string;
  workspace: string | null;
  warning: string | null;
}

const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

async function requestRules(
  request: APIRequestContext,
  backend: Backend,
): Promise<RequestRule[]> {
  const response = await request.get("/api/request-rules", {
    headers: auth(backend),
  });
  return ((await response.json()) as { rules: RequestRule[] }).rules;
}

test.beforeEach(async ({ backend }) => {
  await backend.control.reset("request-rules");
});

const section = (page: Page): Locator =>
  page.getByRole("region", { name: "Method and path" });

const rowFor = (page: Page, host: string): Locator =>
  section(page)
    .locator("tbody tr")
    .filter({ has: page.locator("td.host .mono", { hasText: host }) });

async function openRules(page: Page, backend: Backend): Promise<void> {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto("/rules");
  await expect(
    page.getByRole("heading", { level: 1, name: "Rules" }),
  ).toBeVisible();
  await expect(section(page).locator("tbody tr")).toHaveCount(2);
}

async function openNetwork(
  page: Page,
  backend: Backend,
  workspace: string,
): Promise<Locator> {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto(`/workspaces/${workspace}/network`);
  const region = page.getByRole("region", { name: "Rules on method and path" });
  await expect(region).toBeVisible();
  return region;
}

const addButton = (page: Page) =>
  page.getByRole("button", { name: "Add method and path rule" });
const addDialog = (page: Page) =>
  page.getByRole("dialog", { name: "Add a method and path rule" });

test.describe("the list", () => {
  test("shows each rule's host, methods, path and workspace, and says a rule only refuses", async ({
    page,
    backend,
  }) => {
    await openRules(page, backend);
    await expect(section(page)).toContainText("only refuses");
    await expect(section(page)).toContainText("decrypts the host");
    const global = rowFor(page, "api.example.com");
    await expect(global).toContainText("DELETE");
    await expect(global).toContainText("/repos/**");
    await expect(global).toContainText("Every workspace");
    const own = rowFor(page, "*.example.org");
    await expect(own).toContainText("and subdomains");
    await expect(own).toContainText("POST, PUT");
    await expect(own).toContainText("/upload/*");
    await expect(own).toContainText("docs-site");
  });

  test("a rule on a host the never-decrypt list names carries the API's warning in full", async ({
    page,
    request,
    backend,
  }) => {
    const put = await request.put("/api/interception/never-decrypt", {
      headers: auth(backend),
      data: { never_decrypt: ["*.example.com"] },
    });
    expect(put.ok()).toBe(true);
    await openRules(page, backend);
    const warned = rowFor(page, "api.example.com");
    await expect(warned).toContainText(
      "api.example.com is decrypted for this rule although the never-decrypt list names it: *.example.com (every workspace). A tool there that does not trust Puddle's certificate will fail on it.",
    );
    await expect(rowFor(page, "*.example.org")).not.toContainText("Note:");
  });

  test("a rule made elsewhere shows at once, and one deleted elsewhere goes", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    const made = await request.post("/api/request-rules", {
      headers: auth(backend),
      data: {
        host: "late.example.net",
        methods: ["PATCH"],
        path: "/v1/**",
        workspace: null,
      },
    });
    expect(made.status()).toBe(201);
    await expect(rowFor(page, "late.example.net")).toBeVisible();
    const { id } = (await made.json()) as { id: number };
    const gone = await request.delete(`/api/request-rules/${id}`, {
      headers: auth(backend),
    });
    expect(gone.ok()).toBe(true);
    await expect(rowFor(page, "late.example.net")).toHaveCount(0);
  });

  test("the empty state says what a rule is for", async ({ page, backend }) => {
    await backend.control.reset("empty");
    await backend.installClock(page);
    await backend.signIn(page);
    await page.goto("/rules");
    await expect(section(page)).toContainText(
      "No rules on method and path yet",
    );
    await expect(section(page).getByRole("table")).toHaveCount(0);
  });

  test("the refused request is in the Activity screen and names the rule", async ({
    page,
    backend,
  }) => {
    await backend.installClock(page);
    await backend.signIn(page);
    await page.goto("/activity");
    const row = page.getByRole("row").filter({ hasText: "api.example.com" });
    await expect(row.first()).toContainText("DELETE /repos/acme/web-shop");
    await expect(row.first()).toContainText("method and path rule 1");
  });
});

test.describe("adding", () => {
  test("adds a rule for one workspace and the API holds it", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).click();
    const dialog = addDialog(page);
    await dialog.getByLabel("Host").fill("registry.example.com");
    await dialog.getByLabel("Methods").fill("delete, put");
    await dialog.getByLabel("Path").fill("/packages/*/versions");
    await dialog.getByLabel("Workspace name").fill("web-shop");
    await dialog.getByRole("button", { name: "Add rule" }).click();
    await expect(dialog).toHaveCount(0);
    const row = rowFor(page, "registry.example.com");
    await expect(row).toContainText("DELETE, PUT");
    await expect(row).toContainText("/packages/*/versions");
    await expect(row).toContainText("web-shop");
    await expect(page.getByRole("status")).toContainText(
      "Added: refuse DELETE, PUT /packages/*/versions on registry.example.com for workspace web-shop.",
    );
    const held = (await requestRules(request, backend)).find(
      (r) => r.host === "registry.example.com",
    );
    expect(held).toMatchObject({
      methods: ["DELETE", "PUT"],
      path: "/packages/*/versions",
      workspace: "web-shop",
    });
  });

  test("a rule without methods refuses every method", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).click();
    const dialog = addDialog(page);
    await dialog.getByLabel("Host").fill("admin.example.com");
    await dialog.getByLabel("Path").fill("/admin/**");
    await dialog.getByLabel("Workspace name").fill("docs-site");
    await dialog.getByRole("button", { name: "Add rule" }).click();
    await expect(rowFor(page, "admin.example.com")).toContainText(
      "Every method",
    );
    const held = (await requestRules(request, backend)).find(
      (r) => r.host === "admin.example.com",
    );
    expect(held?.methods).toEqual([]);
  });

  test("a rule for every workspace asks first; Cancel returns to the form with what was typed", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).click();
    const dialog = addDialog(page);
    await dialog.getByLabel("Host").fill("everywhere.example.com");
    await dialog.getByLabel("Methods").fill("DELETE");
    await dialog.getByLabel("Path").fill("/**");
    await dialog.getByLabel(/Every workspace/).check();
    await dialog.getByRole("button", { name: "Add rule" }).click();
    const confirm = page.getByRole("alertdialog", {
      name: "Refuse in every workspace?",
    });
    await expect(confirm).toContainText(
      "Refuse DELETE /** on everywhere.example.com for every workspace",
    );
    await expect(confirm).toContainText("decrypts the host for all of them");
    await confirm.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog.getByLabel("Host")).toHaveValue(
      "everywhere.example.com",
    );
    expect(
      (await requestRules(request, backend)).some(
        (r) => r.host === "everywhere.example.com",
      ),
    ).toBe(false);
    await dialog.getByRole("button", { name: "Add rule" }).click();
    await confirm
      .getByRole("button", { name: "Refuse in every workspace" })
      .click();
    await expect(rowFor(page, "everywhere.example.com")).toContainText(
      "Every workspace",
    );
  });

  test("a bad entry is refused at its field before anything is sent", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).click();
    const dialog = addDialog(page);
    await dialog.getByLabel("Host").fill("10.0.0.1");
    await dialog.getByLabel("Methods").fill("GE(T)");
    await dialog.getByLabel("Path").fill("/a*");
    await dialog.getByRole("button", { name: "Add rule" }).click();
    await expect(dialog.getByRole("alert")).toHaveCount(4);
    await expect(dialog.getByLabel("Host")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await expect(dialog).toContainText("Rules name hosts, not IP addresses.");
    await expect(dialog).toContainText("is not an HTTP method");
    await expect(dialog).toContainText("A * stands for a whole segment");
    await expect(dialog).toContainText("Name the workspace");
    // The alerts above are shown, so the refusal is final; the API still holds only the two seeds.
    await expect
      .poll(async () => (await requestRules(request, backend)).length)
      .toBe(2);
  });

  test("a refusal from the API is shown at the host field in its words, and the form stays", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).click();
    const dialog = addDialog(page);
    await dialog.getByLabel("Host").fill("bad_name.example.com");
    await dialog.getByLabel("Path").fill("/**");
    await dialog.getByLabel("Workspace name").fill("web-shop");
    await dialog.getByRole("button", { name: "Add rule" }).click();
    await expect(dialog.getByRole("alert")).toContainText(
      '"bad_name.example.com" is not a host name',
    );
    await expect(dialog.getByLabel("Host")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await expect(dialog.getByLabel("Path")).toHaveValue("/**");
    // The alerts above are shown, so the refusal is final; the API still holds only the two seeds.
    await expect
      .poll(async () => (await requestRules(request, backend)).length)
      .toBe(2);
  });
});

test.describe("deleting", () => {
  test("asks, names the rule, and deletes on confirm; Cancel keeps it", async ({
    page,
    request,
    backend,
  }) => {
    await openRules(page, backend);
    const row = rowFor(page, "api.example.com");
    await row.getByRole("button", { name: /^Delete rule/ }).click();
    const confirm = page.getByRole("alertdialog", {
      name: "Delete this rule?",
    });
    await expect(confirm).toContainText(
      "refuse DELETE /repos/** on api.example.com for every workspace",
    );
    await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
    await confirm.getByRole("button", { name: "Cancel" }).click();
    await expect(row).toBeVisible();

    await row.getByRole("button", { name: /^Delete rule/ }).click();
    await confirm.getByRole("button", { name: "Delete rule" }).click();
    await expect(row).toHaveCount(0);
    await expect(page.getByRole("status")).toContainText(
      "Deleted: refuse DELETE /repos/** on api.example.com",
    );
    expect(
      (await requestRules(request, backend)).some(
        (r) => r.host === "api.example.com",
      ),
    ).toBe(false);
    // Focus moved to the neighbouring row's button, not to the page.
    await expect
      .poll(() =>
        page.evaluate(
          () => document.activeElement?.closest("tr")?.dataset["requestRuleId"],
        ),
      )
      .toBe("2");
  });

  test("the last rule going leaves the empty state and focus on the section heading", async ({
    page,
    backend,
  }) => {
    await openRules(page, backend);
    for (const host of ["api.example.com", "*.example.org"]) {
      await rowFor(page, host)
        .getByRole("button", { name: /^Delete rule/ })
        .click();
      await page
        .getByRole("alertdialog")
        .getByRole("button", { name: "Delete rule" })
        .click();
      await expect(rowFor(page, host)).toHaveCount(0);
    }
    await expect(section(page)).toContainText(
      "No rules on method and path yet",
    );
    await expect(
      section(page).getByRole("heading", { name: "Method and path" }),
    ).toBeFocused();
  });
});

test.describe("a workspace's Network tab", () => {
  test("lists the rules that apply to it, read-only, and links to the Rules screen", async ({
    page,
    backend,
  }) => {
    const region = await openNetwork(page, backend, "web-shop");
    await expect(region.locator("tbody tr")).toHaveCount(1);
    await expect(region).toContainText("api.example.com");
    await expect(region).not.toContainText("*.example.org");
    await expect(region.getByRole("button")).toHaveCount(0);
    await expect(
      region.getByRole("link", { name: "All method and path rules" }),
    ).toHaveAttribute("href", "/rules");
  });

  test("a workspace's own rule is listed with the global ones", async ({
    page,
    backend,
  }) => {
    const region = await openNetwork(page, backend, "docs-site");
    await expect(region.locator("tbody tr")).toHaveCount(2);
    await expect(region).toContainText("*.example.org");
    await expect(region).toContainText("api.example.com");
  });

  test("says when no rule applies", async ({ page, backend }) => {
    await backend.control.reset("lived-in");
    const region = await openNetwork(page, backend, "web-shop");
    await expect(region).toContainText(
      "No rule on method and path applies to web-shop.",
    );
  });
});

test.describe("keyboard only", () => {
  test("add for every workspace and delete without a pointer", async ({
    page,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).focus();
    await page.keyboard.press("Enter");
    const dialog = addDialog(page);
    await expect(dialog.getByLabel("Host")).toBeFocused();
    await page.keyboard.type("keys.example.org");
    await page.keyboard.press("Tab");
    await expect(dialog.getByLabel("Methods")).toBeFocused();
    await page.keyboard.type("post");
    await page.keyboard.press("Tab");
    await expect(dialog.getByLabel("Path")).toBeFocused();
    await page.keyboard.type("/upload/**");
    await page.keyboard.press("Tab"); // the checked radio of the group (One workspace)
    await page.keyboard.press("ArrowDown"); // Every workspace
    await expect(dialog.getByLabel(/Every workspace/)).toBeChecked();
    await dialog.getByRole("button", { name: "Add rule" }).focus();
    await page.keyboard.press("Enter");
    const confirm = page.getByRole("alertdialog", {
      name: "Refuse in every workspace?",
    });
    await expect(confirm.getByRole("button", { name: "Cancel" })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(
      confirm.getByRole("button", { name: "Refuse in every workspace" }),
    ).toBeFocused();
    await page.keyboard.press("Enter");
    const row = rowFor(page, "keys.example.org");
    await expect(row).toContainText("POST");
    await expect(addButton(page)).toBeFocused();

    await row.getByRole("button", { name: /^Delete rule/ }).focus();
    await page.keyboard.press("Enter");
    const ask = page.getByRole("alertdialog", { name: "Delete this rule?" });
    await expect(ask.getByRole("button", { name: "Cancel" })).toBeFocused();
    await page.keyboard.press("Tab");
    await page.keyboard.press("Enter");
    await expect(row).toHaveCount(0);
    await expect(page.locator("body")).not.toBeFocused();
  });

  test("Escape closes the form and gives focus back to Add", async ({
    page,
    backend,
  }) => {
    await openRules(page, backend);
    await addButton(page).focus();
    await page.keyboard.press("Enter");
    await expect(addDialog(page)).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(addDialog(page)).toHaveCount(0);
    await expect(addButton(page)).toBeFocused();
  });
});

test.describe("accessibility", () => {
  for (const scheme of ["light", "dark"] as const) {
    test(`axe finds nothing in ${scheme}: the list with a warning and the add dialog with errors`, async ({
      page,
      request,
      backend,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      await request.put("/api/interception/never-decrypt", {
        headers: auth(backend),
        data: { never_decrypt: ["*.example.com"] },
      });
      await openRules(page, backend);
      await expect(rowFor(page, "api.example.com")).toContainText("Note:");
      await expectNoAxeViolations(page, "list");
      await addButton(page).click();
      const dialog = addDialog(page);
      await dialog.getByRole("button", { name: "Add rule" }).click();
      await expect(dialog.getByRole("alert")).toHaveCount(3);
      await expectNoAxeViolations(page, "add with errors");
      await staysEmpty(page, csp);
    });

    test(`axe finds nothing in ${scheme}: the delete dialog and the toast`, async ({
      page,
      backend,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      await openRules(page, backend);
      await rowFor(page, "api.example.com")
        .getByRole("button", { name: /^Delete rule/ })
        .click();
      await expectNoAxeViolations(page, "delete");
      await page
        .getByRole("button", { name: "Delete rule", exact: true })
        .click();
      await expect(page.getByRole("status")).toContainText("Deleted");
      await expectNoAxeViolations(page, "toast");
      await staysEmpty(page, csp);
    });

    test(`axe finds nothing in ${scheme}: the Network tab and the empty state`, async ({
      page,
      backend,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      const csp = await watchCsp(page);
      const region = await openNetwork(page, backend, "docs-site");
      await expect(region.locator("tbody tr")).toHaveCount(2);
      await expectNoAxeViolations(page, "network tab");
      await backend.control.reset("empty");
      await backend.signIn(page);
      await page.goto("/rules");
      await expect(section(page)).toContainText(
        "No rules on method and path yet",
      );
      await expectNoAxeViolations(page, "empty");
      await staysEmpty(page, csp);
    });
  }

  test("the buttons are at least 24 by 24 CSS pixels (WCAG 2.5.8)", async ({
    page,
    backend,
  }) => {
    await openRules(page, backend);
    const buttons = [
      ...(await section(page).getByRole("button").all()),
      ...(await section(page)
        .locator("tbody tr")
        .first()
        .getByRole("button")
        .all()),
    ];
    expect(buttons.length).toBeGreaterThanOrEqual(3);
    for (const button of buttons) {
      const box = await button.boundingBox();
      expect(box?.width ?? 0).toBeGreaterThanOrEqual(24);
      expect(box?.height ?? 0).toBeGreaterThanOrEqual(24);
    }
  });
});
