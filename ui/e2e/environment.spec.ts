// SPDX-License-Identifier: GPL-3.0-or-later
// The Environment, Shell init and Ports tabs, Settings' global environment and the delete check's
// "what Puddle removes" line, against the real API on the fixture backend, scenario `environment`:
// web-shop runs (NODE_ENV, the secret NPM_TOKEN, a shell init, ports 3000 and 5432 listening) and
// docs-site is stopped (two variables, a secret and a rule). Waits are on conditions.
import type { Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { axeStops, watchCsp, staysEmpty } from "./support";

async function open(
  page: Page,
  backend: Backend,
  path: string,
  heading: string,
  workspace = "web-shop",
) {
  await backend.control.reset("environment");
  await backend.signIn(page);
  await page.goto(`/workspaces/${workspace}/${path}`);
  await expect(
    page.getByRole("heading", { level: 2, name: heading }),
  ).toBeVisible();
}

const table = (page: Page, workspace = "web-shop") =>
  page.getByRole("table", { name: `Environment of ${workspace}` });
const ports = (page: Page) =>
  page.getByRole("table", { name: "Ports of web-shop" });
const notices = (page: Page) => page.getByRole("region", { name: "Notices" });

/** Emits until the page shows the text: the event stream may connect after the page loaded. */
async function raise(
  backend: Backend,
  page: Page,
  event: Record<string, unknown>,
  where: ReturnType<Page["locator"]>,
  text: string | RegExp,
) {
  await expect(async () => {
    await backend.control.emit(event);
    await expect(where).toContainText(text, { timeout: 500 });
  }).toPass();
}

test.describe("environment", () => {
  test("lists the workspace's own first, then the global ones, and shows no secret value", async ({
    page,
    backend,
  }) => {
    const csp = await watchCsp(page);
    await open(page, backend, "environment", "Environment");
    const rows = table(page).getByRole("row");
    await expect(rows).toHaveCount(5);
    await expect(rows.nth(1)).toContainText("NODE_ENV");
    await expect(rows.nth(2)).toContainText("NPM_TOKEN");
    await expect(rows.nth(2)).toContainText("Stored in Puddle, never shown");
    await expect(rows.nth(2)).toContainText(
      "*.npmjs.org and registry.npmjs.org",
    );
    await expect(rows.nth(3)).toContainText("EDITOR");
    await expect(rows.nth(3)).toContainText("Every workspace");
    await expect(
      page.getByText(/Plain variables apply at the next start/),
    ).toBeVisible();
    // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
    expect(await page.content()).not.toContain("npm-seed-value");
    await staysEmpty(page, csp);
  });

  test("adds a variable and a secret; the secret's value is never shown and both survive a reload", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "environment", "Environment");
    await page.getByRole("button", { name: "Add variable or secret" }).click();
    let dialog = page.getByRole("dialog", { name: "Add a variable" });
    await dialog.getByLabel("Name", { exact: true }).fill("LOG_LEVEL");
    await dialog.getByLabel("Value", { exact: true }).fill("debug");
    await dialog.getByRole("button", { name: "Add" }).click();
    await expect(table(page)).toContainText("LOG_LEVEL");
    await expect(page.getByText("Saved LOG_LEVEL.")).toBeVisible();
    await expect(
      page.getByText("LOG_LEVEL: Plain variables apply at the next start"),
    ).toBeVisible();

    await page.getByRole("button", { name: "Add variable or secret" }).click();
    dialog = page.getByRole("dialog");
    await dialog.getByRole("radio", { name: /Secret:/ }).check();
    await expect(
      dialog.getByText(/Puddle will decrypt connections to each host/),
    ).toBeVisible();
    await dialog.getByLabel("Name", { exact: true }).fill("API_KEY");
    await dialog.getByLabel("Value", { exact: true }).fill("sk-typed-once");
    await dialog.getByLabel("Hosts it may be sent to").fill("api.example.com");
    await dialog.getByRole("button", { name: "Add" }).click();
    const row = table(page).getByRole("row", { name: /API_KEY/ });
    await expect(row).toContainText("Stored in Puddle, never shown");
    // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
    expect(await page.content()).not.toContain("sk-typed-once");

    await page.reload();
    await expect(table(page)).toContainText("LOG_LEVEL");
    await expect(table(page)).toContainText("API_KEY");
    // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
    expect(await page.content()).not.toContain("sk-typed-once");
  });

  test("a refused name is explained in the dialog and nothing is added", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "environment", "Environment");
    await page.getByRole("button", { name: "Add variable or secret" }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByLabel("Name", { exact: true }).fill("PATH");
    await dialog.getByLabel("Value", { exact: true }).fill("/x");
    await dialog.getByRole("button", { name: "Add" }).click();
    await expect(dialog.getByRole("alert")).toContainText(
      "PATH comes from the workspace's image",
    );
    await expect(dialog).toBeVisible();
  });

  test("edits a secret's hosts without asking for its value, and removing one asks first", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "environment", "Environment");
    await page.getByRole("button", { name: "Edit NPM_TOKEN" }).click();
    const dialog = page.getByRole("dialog", { name: "Edit NPM_TOKEN" });
    await expect(dialog.getByLabel("Value", { exact: true })).toHaveValue("");
    await dialog
      .getByLabel("Hosts it may be sent to")
      .fill("registry.npmjs.org");
    await dialog.getByRole("button", { name: "Save" }).click();
    await expect(
      table(page).getByRole("row", { name: /NPM_TOKEN/ }),
    ).not.toContainText("*.npmjs.org");

    await page.getByRole("button", { name: "Remove NPM_TOKEN" }).click();
    const sure = page.getByRole("alertdialog", { name: "Remove NPM_TOKEN?" });
    await sure.getByRole("button", { name: "Cancel" }).click();
    await expect(table(page)).toContainText("NPM_TOKEN");
    await page.getByRole("button", { name: "Remove NPM_TOKEN" }).click();
    await page
      .getByRole("alertdialog")
      .getByRole("button", { name: "Remove secret" })
      .click();
    await expect(table(page)).not.toContainText("NPM_TOKEN");
  });

  test("overrides a global variable for this workspace only", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "environment", "Environment");
    await page
      .getByRole("button", { name: "Override EDITOR for web-shop" })
      .click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByLabel("Name", { exact: true })).toHaveValue(
      "EDITOR",
    );
    await dialog.getByLabel("Value", { exact: true }).fill("nano");
    await dialog.getByRole("button", { name: "Add" }).click();
    await expect(
      table(page).getByRole("row", { name: /EDITOR nano/ }),
    ).toBeVisible();
    await expect(table(page)).toContainText(
      "Hidden here by this workspace's own EDITOR",
    );
  });

  test("a stopped workspace says both kinds are read at its start", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "environment", "Environment", "docs-site");
    await expect(
      page.getByText("Both kinds are read when the workspace starts."),
    ).toBeVisible();
    await expect(table(page, "docs-site")).toContainText("DOCS_TOKEN");
  });

  test("a change that did not reach the running workspace raises a notice that links to the tab", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "environment", "Environment");
    await raise(
      backend,
      page,
      {
        type: "workspace_env_failed",
        workspace: "web-shop",
        reason: "the credential store is locked",
      },
      notices(page),
      "the credential store is locked",
    );
    await expect(notices(page)).toContainText(
      "A change to the environment of web-shop did not reach it.",
    );
  });

  test("Settings holds the variables every workspace gets", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("environment");
    await backend.signIn(page);
    await page.goto("/settings");
    await expect(
      page.getByRole("heading", { level: 2, name: "Environment" }),
    ).toBeVisible();
    const global = page.getByRole("table", {
      name: "Environment of every workspace",
    });
    await expect(global).toContainText("EDITOR");
    await expect(global).toContainText("GHE_TOKEN");
    // eslint-disable-next-line no-restricted-syntax -- absence: the settled state is asserted above, and a poll of a negative passes on its first read too
    expect(await page.content()).not.toContain("ghe-global-value");
    await page.getByRole("button", { name: "Edit EDITOR" }).click();
    const dialog = page.getByRole("dialog", { name: "Edit EDITOR" });
    await expect(dialog).toContainText("Applies to every workspace.");
    await dialog.getByLabel("Value", { exact: true }).fill("emacs");
    await dialog.getByRole("button", { name: "Save" }).click();
    await expect(global).toContainText("emacs");
    await page.reload();
    await expect(
      page.getByRole("table", { name: "Environment of every workspace" }),
    ).toContainText("emacs");
  });
});

test.describe("shell init", () => {
  test("shows the saved script and saves a change after the workspace ran it", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "shell-init", "Shell init");
    const field = page.getByLabel("Script for web-shop");
    await expect(field).toHaveValue("export PAGER=less\n");
    await expect(page.getByRole("button", { name: "Save" })).toBeDisabled();
    await field.fill("export PAGER=less\nexport EDITOR=vim\n");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByRole("status").first()).toContainText(
      "Saved. Puddle ran it once in the workspace and it printed nothing.",
    );
    await page.reload();
    await expect(page.getByLabel("Script for web-shop")).toHaveValue(
      "export PAGER=less\nexport EDITOR=vim\n",
    );
  });

  test("a script that prints is refused with what it printed, and nothing is saved", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "shell-init", "Shell init");
    await backend.control.step({
      do: "shell_init_check",
      refuses: "it printed: hi",
    });
    await page.getByLabel("Script for web-shop").fill("echo hi\n");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByRole("alert")).toContainText("Not saved.");
    await expect(page.getByRole("alert")).toContainText("it printed: hi");
    await page.reload();
    await expect(page.getByLabel("Script for web-shop")).toHaveValue(
      "export PAGER=less\n",
    );
  });

  test("a stopped workspace's script is saved unchecked and says so", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "shell-init", "Shell init", "docs-site");
    await expect(page.getByText("Nothing saved yet.")).toBeVisible();
    await page.getByLabel("Script for docs-site").fill("export A=1\n");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByRole("status").first()).toContainText(
      "docs-site is not running; Puddle checks the script when it starts",
    );
    await page.getByRole("button", { name: "Remove the script" }).click();
    await expect(page.getByText("Nothing saved yet.")).toBeVisible();
    await expect(page.getByLabel("Script for docs-site")).toHaveValue(
      "export A=1\n",
    );
  });

  test("says when something in the workspace changed the file and Puddle put it back", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "shell-init", "Shell init");
    await raise(
      backend,
      page,
      { type: "workspace_shell_init_restored", workspace: "web-shop" },
      page.locator("main"),
      "Puddle put your saved text back",
    );
    await page.getByRole("button", { name: "Dismiss" }).click();
    await expect(
      page.getByText("Puddle put your saved text back"),
    ).toBeHidden();
  });
});

test.describe("ports", () => {
  test("lists what listens and forwards one to this computer, then stops it", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "ports", "Ports");
    await expect(ports(page).getByRole("row")).toHaveCount(3);
    await page
      .getByRole("button", { name: "Forward 3000", exact: true })
      .click();
    await expect(ports(page).getByRole("row", { name: /^3000/ })).toContainText(
      "Open on 127.0.0.1:3000",
    );
    await expect(
      page.getByText("3000 is forwarded to 127.0.0.1:3000."),
    ).toBeVisible();
    await page.getByRole("button", { name: "Stop forwarding 3000" }).click();
    await expect(
      ports(page).getByRole("row", { name: /^3000/ }),
    ).not.toContainText("Open on");
  });

  test("forwards to another port and keeps it, and a port taken by another workspace is refused by name", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "ports", "Ports");
    await page.getByLabel("Port on this computer for 5432").fill("15432");
    await page.getByRole("button", { name: "Forward and keep 5432" }).click();
    const row = ports(page).getByRole("row", { name: /^5432/ });
    await expect(row).toContainText("Open on 127.0.0.1:15432");
    await expect(
      row.getByLabel("Keep the forward of 5432 at every start"),
    ).toBeChecked();
    await page.getByLabel("Port on this computer for 3000").fill("15432");
    await page
      .getByRole("button", { name: "Forward 3000", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText(
      "Port 15432 on this computer is already in use",
    );
  });

  test("a port that starts listening shows up, and one can be forwarded before anything listens", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "ports", "Ports");
    await backend.control.step({
      do: "listening",
      workspace: "web-shop",
      ports: [3000, 5432, 8080],
    });
    await expect(ports(page).getByRole("row")).toHaveCount(4);
    await page.getByLabel("Forward another port").fill("6379");
    await page
      .getByRole("button", { name: "Forward", exact: true })
      .last()
      .click();
    await expect(ports(page).getByRole("row", { name: /^6379/ })).toContainText(
      "Open on 127.0.0.1:6379",
    );
  });

  test("a kept forward that could not open is a notice that links to the tab", async ({
    page,
    backend,
  }) => {
    await open(page, backend, "ports", "Ports");
    await raise(
      backend,
      page,
      {
        type: "workspace_port_forward_failed",
        workspace: "web-shop",
        guest_port: 3000,
        host_port: 3000,
        reason:
          "port 3000 on this computer is in use by node; choose another port",
      },
      notices(page),
      "in use by node",
    );
    await expect(notices(page)).toContainText(
      "Port 3000 of web-shop could not be forwarded to 127.0.0.1:3000.",
    );
  });
});

test("the delete check says what Puddle itself removes", async ({
  page,
  backend,
}) => {
  await backend.control.reset("environment");
  await backend.signIn(page);
  await page.goto("/workspaces/docs-site");
  await page.getByRole("button", { name: "Delete workspace…" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Delete docs-site?" });
  await expect(dialog).toContainText(
    "Puddle also removes what it keeps for docs-site: 2 variables, 1 secret and 1 rule.",
  );
});

// The states are scanned at most two to a test: a scan costs seconds on a loaded host.
const stops = axeStops([
  ["environment tab", "add dialog"],
  ["shell init", "shell init refused"],
  ["ports", "settings environment"],
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
      await open(page, backend, "environment", "Environment");
      await expect(table(page)).toBeVisible();
      await scan(page, "environment tab");
      await page
        .getByRole("button", { name: "Add variable or secret" })
        .click();
      await page.getByRole("radio", { name: /Secret:/ }).check();
      await expect(page.getByLabel("Hosts it may be sent to")).toBeVisible();
      await scan(page, "add dialog");
      await page.keyboard.press("Escape");
      await page.goto("/workspaces/web-shop/shell-init");
      await expect(page.getByLabel("Script for web-shop")).toBeVisible();
      await scan(page, "shell init");
      await backend.control.step({
        do: "shell_init_check",
        refuses: "it printed: hi",
      });
      await page.getByLabel("Script for web-shop").fill("echo hi\n");
      await page.getByRole("button", { name: "Save" }).click();
      await expect(page.getByRole("alert")).toBeVisible();
      await scan(page, "shell init refused");
      await page.goto("/workspaces/web-shop/ports");
      await expect(ports(page)).toBeVisible();
      await scan(page, "ports");
      await page.goto("/settings");
      await expect(
        page.getByRole("table", { name: "Environment of every workspace" }),
      ).toBeVisible();
      await scan(page, "settings environment");
      await staysEmpty(page, csp);
      scan.done();
    });
  }
}
