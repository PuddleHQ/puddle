// SPDX-License-Identifier: AGPL-3.0-or-later
// The workspace screens against the real API on the fixture backend (fake workspace service).
// Every test starts from the `lived-in` scenario on its worker's own backend: web-shop running,
// docs-site stopped with unsaved work, data-tools never started, and requests waiting for each.
// Operations finish at once unless a test holds them (`hold_workspaces`) to look at the busy
// state; the page clock is the fixture's.
// The list, creating, start and stop, connecting.
import type { Locator, Page } from "@playwright/test";
import { expect, test } from "./fixture";
import { cpuLimits, RECONNECT_WAIT_MS, expectNoAxeViolations } from "./support";
import {
  auth,
  card,
  installReset,
  nameLink,
  openDetail,
  openList,
  pendingCount,
  stopFromRow,
  toast,
  workspaceList,
} from "./workspaces-support";

installReset();

test.describe("the list and the start screen", () => {
  test("the app starts on the workspaces, and the count of waiting requests shows in the sidebar, not as a banner", async ({
    page,
    request,
    backend,
  }) => {
    await backend.installClock(page);
    await backend.signIn(page);
    await page.goto("/");
    await expect(page).toHaveURL(/\/workspaces$/);
    const waiting = await pendingCount(request, backend);
    expect(waiting).toBeGreaterThan(3);
    await expect(page.getByTestId("pending-badge")).toContainText(
      String(waiting),
    );
    await expect(page.getByText("requests waiting")).toHaveCount(0);
  });

  test("a row per workspace with its state, repository, disk and waiting requests", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await expect(page.getByRole("article")).toHaveCount(3);
    const running = card(page, "web-shop");
    await expect(running).toContainText("Running");
    await expect(running).toContainText("acme/web-shop");
    await expect(running).toContainText("6 GiB / 32 GiB");
    await expect(running.getByRole("link", { name: /waiting$/ })).toBeVisible();
    await expect(card(page, "docs-site")).toContainText("Stopped");
    await expect(card(page, "data-tools")).toContainText("Not started");
    await expect(
      running.getByRole("button", { name: "Connect to web-shop" }),
    ).toBeEnabled();
    await expect(running).not.toContainText("Trusted");
    await expect(
      card(page, "docs-site").getByRole("button", { name: "Start docs-site" }),
    ).toBeEnabled();
  });

  test("no workspaces shows the create form itself", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("empty");
    await openList(page, backend);
    const form = page.getByRole("region", { name: "New workspace" });
    await expect(form).toBeVisible();
    await expect(form.getByLabel("Image")).toBeVisible();
    await expect(page.getByText("No workspaces yet")).toHaveCount(0);
    await expect(page.getByText("requests waiting")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "More options" }),
    ).toHaveCount(0);
  });

  test("the first workspace is made with the full form", async ({
    page,
    backend,
  }) => {
    await backend.control.reset("empty");
    await openList(page, backend);
    await page
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/ledger.git");
    await page.getByRole("button", { name: "Create workspace" }).click();
    await expect(card(page, "ledger")).toContainText("acme/ledger");
    await expect(page.getByLabel("Git repository (HTTPS)")).toHaveCount(0);
  });

  test("a row opens the details", async ({ page, backend }) => {
    await openList(page, backend);
    await nameLink(card(page, "docs-site"), "docs-site").click();
    await expect(page).toHaveURL(/\/workspaces\/docs-site$/);
    await expect(
      page.getByRole("heading", { level: 1, name: "docs-site" }),
    ).toBeVisible();
  });

  test("a request that arrives updates the sidebar count and the row live", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const before = await pendingCount(request, backend);
    const row = card(page, "data-tools");
    await expect(page.getByTestId("pending-badge")).toContainText(
      String(before),
    );
    const own = row.getByRole("link", { name: /^\d+ waiting$/ });
    const ownBefore = Number((await own.innerText()).trim());
    await backend.control.step({
      do: "request",
      workspace: "data-tools",
      host: "live.example.org",
    });
    await expect(page.getByTestId("pending-badge")).toContainText(
      String(before + 1),
    );
    await expect(own).toHaveText(String(ownBefore + 1));
  });
});

test.describe("creating", () => {
  async function openCreate(page: Page): Promise<Locator> {
    await page.getByRole("button", { name: "New workspace" }).first().click();
    const dialog = page.getByRole("region", { name: "New workspace" });
    await expect(dialog).toBeVisible();
    return dialog;
  }

  test("the create button stays where it is when the host note shows", async ({
    page,
    backend,
  }) => {
    // Hold the service's answer so the position before the note is read before it can arrive.
    let release: () => void = () => {};
    const held = new Promise<void>((done) => (release = done));
    await page.route("**/api/repository-hosts", async (route) => {
      await held;
      await route.continue();
    });
    await openList(page, backend);
    const dialog = await openCreate(page);
    const create = dialog.getByRole("button", { name: "Create workspace" });
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/Ledger_Service.git");
    await expect(dialog.getByLabel("Name")).toHaveValue("Ledger_Service");
    const before = await create.boundingBox();
    if (before === null) throw new Error("the create button has no box");
    release();
    await expect(dialog.getByTestId("hosts-new")).toContainText("github.com");
    await expect.poll(() => create.boundingBox()).toEqual(before);
  });

  test("creates from a repository URL, naming it after the repository", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({ do: "hold_workspaces" });
    const dialog = await openCreate(page);
    await expect(dialog.getByLabel("Git repository (HTTPS)")).toBeFocused();
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/Ledger_Service.git");
    await expect(dialog.getByLabel("Name")).toHaveValue("Ledger_Service");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog).toHaveCount(0);
    const created = card(page, "Ledger_Service");
    await expect(created).toContainText("Creating");
    // A workspace being created reads "created" but offers no Start until it is done.
    await expect(
      created.getByRole("button", { name: "Start Ledger_Service" }),
    ).toHaveCount(0);
    // The service reports where it is, with the bytes while an image comes down.
    await backend.control.emit({
      type: "workspace_progress",
      workspace: "ledger-service",
      step: "pulling_image",
      detail: null,
      bytes_done: 120 * 1024 * 1024,
      bytes_total: 480 * 1024 * 1024,
    });
    await expect(created).toContainText("Downloading the image");
    await expect(created).toContainText("120 MB of 480 MB, 25 %");
    await expect(
      created.getByRole("progressbar", { name: "Downloading the image" }),
    ).toHaveAttribute("value", "25");
    await backend.control.emit({
      type: "workspace_progress",
      workspace: "ledger-service",
      step: "cloning",
      detail: "https://github.com/acme/Ledger_Service.git",
    });
    await expect(created).toContainText("Cloning the repository");
    await expect(created).toContainText("step 3 of 3");
    await backend.control.step({ do: "release_workspaces" });
    await expect(toast(page, "Created Ledger_Service.")).toBeVisible();
    await expect(created).toContainText("Running");
    await expect(created).not.toContainText("Cloning");
    const made = (await workspaceList(request, backend)).find(
      (w) => w.name === "Ledger_Service",
    );
    expect(made?.busy).toBeNull();
    // The events above named it by its id; the name is only what is shown.
    expect(made?.id).toBe("ledger-service");
  });

  test("a typed name is kept; memory and image go to the API", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await dialog.getByLabel("Name").fill("my-name");
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://example.org/other.git");
    await expect(dialog.getByLabel("Name")).toHaveValue("my-name");
    await dialog.getByLabel("Memory (GiB)").fill("3");
    await dialog.getByLabel("Image").fill("ghcr.io/acme/dev:1");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(card(page, "my-name")).toBeVisible();
    const made = (await workspaceList(request, backend)).find(
      (w) => w.name === "my-name",
    );
    expect(made?.memory_mib).toBe(3072);
  });

  test("the CPU slider starts on the default, moves with the keyboard from 1 to the computer's maximum, and the count goes to the API", async ({
    page,
    request,
    backend,
  }) => {
    const limits = await cpuLimits(request, backend.token);
    await openList(page, backend);
    const dialog = await openCreate(page);
    await dialog.getByLabel("Name").fill("slider-ws");
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://example.org/slider.git");
    const slider = dialog.getByLabel("CPUs", { exact: true });
    await expect(slider).toHaveAttribute("max", String(limits.max_cpus));
    await expect(slider).toHaveValue(String(limits.default_cpus));
    await slider.focus();
    await page.keyboard.press("End");
    await expect(slider).toHaveValue(String(limits.max_cpus));
    await expect(dialog.locator("output")).toHaveText(String(limits.max_cpus));
    await page.keyboard.press("Home");
    await expect(slider).toHaveValue("1");
    await page.keyboard.press("End");
    const posted = page.waitForRequest(
      (r) => r.url().endsWith("/api/workspaces") && r.method() === "POST",
    );
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    expect((await posted).postDataJSON()).toMatchObject({
      name: "slider-ws",
      cpus: limits.max_cpus,
    });
    await expect(card(page, "slider-ws")).toBeVisible();
  });

  test("takes an SSH address of a Git host and clones it over HTTPS", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const dialog = await openCreate(page);
    await dialog.getByLabel("Name").fill("over-ssh");
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("git@github.com:Acme/Web-Shop.git");
    await expect(dialog.getByRole("alert")).toHaveCount(0);
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(card(page, "over-ssh")).toBeVisible();
    const made = (await workspaceList(request, backend)).find(
      (w) => w.name === "over-ssh",
    );
    expect(made?.repositories[0]?.url).toBe(
      "https://github.com/Acme/Web-Shop.git",
    );
  });

  test("checks the fields before asking; an SSH address of another host gets the message", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const before = (await workspaceList(request, backend)).length;
    const dialog = await openCreate(page);
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog.getByRole("alert")).toHaveCount(2);
    await expect(dialog.getByLabel("Git repository (HTTPS)")).toBeFocused();
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("git@git.example.com:acme/web-shop.git");
    await dialog.getByLabel("Name").fill("x".repeat(61));
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog.getByRole("alert").first()).toContainText(
      "Puddle reads SSH addresses of GitHub, GitLab, Bitbucket and Azure DevOps. For another host, use the repository's HTTPS URL.",
    );
    await expect(dialog.getByLabel("Name")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await expect(dialog.getByRole("alert").nth(1)).toContainText(
      "Use at most 60 characters.",
    );
    expect((await workspaceList(request, backend)).length).toBe(before);
  });

  test("a name another workspace has is refused at the name, ignoring case", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const before = (await workspaceList(request, backend)).length;
    const dialog = await openCreate(page);
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/web-shop.git");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog.getByLabel("Name")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    await expect(dialog.getByRole("alert")).toContainText(
      'Another workspace is already called "web-shop"',
    );
    await expect(dialog.getByLabel("Name")).toBeFocused();
    await dialog.getByLabel("Name").fill("WEB-SHOP");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(dialog.getByRole("alert")).toContainText("already called");
    expect((await workspaceList(request, backend)).length).toBe(before);
  });

  test("a clone that fails says why and leaves nothing behind", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({
      do: "fail_workspace",
      operation: "create",
      reason: "repository not found",
    });
    const dialog = await openCreate(page);
    await dialog
      .getByLabel("Git repository (HTTPS)")
      .fill("https://github.com/acme/missing.git");
    await dialog.getByRole("button", { name: "Create workspace" }).click();
    await expect(
      toast(page, /Creating missing failed: repository not found/),
    ).toBeVisible();
    await expect(card(page, "missing")).toHaveCount(0);
    expect(
      (await workspaceList(request, backend)).some((w) => w.name === "missing"),
    ).toBe(false);
  });

  test("leaving keeps what was typed, and Cancel forgets it", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const screen = await openCreate(page);
    await screen.getByLabel("Name").fill("half-done");
    await page.goBack();
    await expect(screen).toHaveCount(0);
    const again = await openCreate(page);
    await expect(again.getByLabel("Name")).toHaveValue("half-done");
    await again.getByRole("button", { name: "Cancel" }).click();
    await expect(again).toHaveCount(0);
    const fresh = await openCreate(page);
    await expect(fresh.getByLabel("Name")).toHaveValue("");
  });
});

test.describe("start and stop", () => {
  test("start shows progress, then the workspace runs", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({ do: "hold_workspaces" });
    const docs = card(page, "docs-site");
    await docs.getByRole("button", { name: "Start docs-site" }).click();
    await expect(docs).toContainText("Starting");
    await expect(docs.getByRole("status")).toBeVisible();
    await backend.control.emit({
      type: "workspace_progress",
      workspace: "docs-site",
      step: "syncing",
      detail: null,
    });
    await expect(docs).toContainText("Syncing your settings");
    await expect(docs).toContainText("step 3 of 3");
    await backend.control.step({ do: "release_workspaces" });
    await expect(toast(page, "docs-site is running.")).toBeVisible();
    await expect(docs).toContainText("Running");
    await expect(
      docs.getByRole("button", { name: "Connect to docs-site" }),
    ).toBeEnabled();
    expect(
      (await workspaceList(request, backend)).find(
        (w) => w.name === "docs-site",
      )?.status,
    ).toBe("running");
  });

  test("a start that fails ends crashed and shows the reason until dismissed", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({
      do: "fail_workspace",
      operation: "start",
      reason: "the VM did not boot",
    });
    const docs = card(page, "docs-site");
    await docs.getByRole("button", { name: "Start docs-site" }).click();
    await expect(
      toast(page, "Starting docs-site failed: the VM did not boot"),
    ).toBeVisible();
    await expect(docs).toContainText("Crashed");
    const failure = docs.getByRole("alert");
    await expect(failure).toContainText("Starting failed");
    await expect(failure).toContainText("the VM did not boot");
    await expect(
      docs.getByRole("button", { name: "Start docs-site" }),
    ).toBeEnabled();
    await failure.getByRole("button", { name: "Dismiss" }).click();
    await expect(docs.getByRole("alert")).toHaveCount(0);
  });

  test("stop shows progress and ends stopped; a stop that fails stays running", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const shop = card(page, "web-shop");
    await backend.control.step({ do: "hold_workspaces" });
    await stopFromRow(page, shop, "web-shop");
    await expect(shop).toContainText("Stopping");
    await shop.getByRole("button", { name: "More about web-shop" }).click();
    await expect(
      page.getByRole("menuitem", { name: "Stop web-shop" }),
    ).toBeDisabled();
    await page.keyboard.press("Escape");
    await backend.control.step({ do: "release_workspaces" });
    await expect(toast(page, "Stopped web-shop.")).toBeVisible();
    await expect(shop).toContainText("Stopped");

    await backend.control.step({ do: "release_workspaces" });
    await shop.getByRole("button", { name: "Start web-shop" }).click();
    await expect(shop).toContainText("Running");
    await backend.control.step({
      do: "fail_workspace",
      operation: "stop",
      reason: "the guest did not answer",
    });
    await stopFromRow(page, shop, "web-shop");
    await expect(
      toast(page, "Stopping web-shop failed: the guest did not answer"),
    ).toBeVisible();
    await expect(shop).toContainText("Running");
  });

  test("the row menu opens the terminal tab", async ({ page, backend }) => {
    await openList(page, backend);
    const shop = card(page, "web-shop");
    await shop.getByRole("button", { name: "More about web-shop" }).click();
    await page.getByRole("menuitem", { name: "Terminal of web-shop" }).click();
    await expect(page).toHaveURL(/\/workspaces\/web-shop\/terminal$/);
  });

  test("the row menu renames a workspace without leaving the list", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const shop = card(page, "web-shop");
    await shop.getByRole("button", { name: "More about web-shop" }).click();
    await page.getByRole("menuitem", { name: "Rename web-shop" }).click();
    const form = page.getByRole("dialog", { name: "Rename workspace" });
    await form.getByLabel("Name").fill("Web shop (prod)");
    await form.getByRole("button", { name: "Save" }).click();
    await expect(toast(page, "Renamed to Web shop (prod).")).toBeVisible();
    await expect(card(page, "Web shop (prod)")).toBeVisible();
    await expect(page).toHaveURL(/\/workspaces$/);
  });

  test("the row menu deletes a stopped workspace after the check", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    const tools = card(page, "data-tools");
    await tools.getByRole("button", { name: "More about data-tools" }).click();
    await page.getByRole("menuitem", { name: "Delete data-tools" }).click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("found nothing unsaved");
    await dialog.getByRole("checkbox", { name: /Delete data-tools/ }).click();
    await dialog.getByRole("button", { name: "Delete workspace" }).click();
    await expect(toast(page, "Deleted data-tools.")).toBeVisible();
    await expect(tools).toHaveCount(0);
  });

  test("a workspace stopped from elsewhere changes the card at once", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    // Stopped for real: the card takes the actions it offers from the record read after the
    // event, so an event alone, with the host still saying running, offers nothing.
    await request.post("/api/workspaces/web-shop/stop", {
      headers: auth(backend),
    });
    await expect(card(page, "web-shop")).toContainText("Stopped");
    await expect(
      card(page, "web-shop").getByRole("button", { name: "Start web-shop" }),
    ).toBeEnabled();
  });

  test("a restarted service is picked up again", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.restart();
    await request.post("/api/workspaces/web-shop/stop", {
      headers: auth(backend),
    });
    await expect(card(page, "web-shop")).toContainText("Stopped", {
      timeout: RECONNECT_WAIT_MS,
    });
  });
});

test.describe("connecting", () => {
  test("the step puts the browser editor first and keeps direct SSH off until allowed", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await card(page, "web-shop")
      .getByRole("button", { name: "Connect to web-shop" })
      .click();
    const step = page.getByRole("dialog", { name: "Connect to web-shop" });
    await expect(step.getByRole("heading", { level: 3 })).toHaveText([
      "Isolated VS Code",
      "On this computer",
    ]);
    await expect(
      step.getByRole("button", { name: "Open isolated VS Code" }),
    ).toBeEnabled();
    await expect(
      step.getByRole("checkbox", { name: "Allow direct SSH" }),
    ).not.toBeChecked();
    await expect(
      step.getByRole("button", { name: "Open in VS Code" }),
    ).toBeDisabled();
  });

  test("a browser editor that could not be installed is said in the step and on the page, with the reason as text, and goes away when it works", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await backend.control.step({
      do: "problem",
      key: "editor-install:web-shop",
      workspace: "web-shop",
      title:
        "The browser editor could not be installed in web-shop: the download does not match <img src=x onerror=alert(1)>",
      detail:
        "Puddle downloaded the editor server and refused it. Terminals and desktop VS Code are not affected.",
    });
    await card(page, "web-shop")
      .getByRole("button", { name: "Connect to web-shop" })
      .click();
    const step = page.getByRole("dialog", { name: "Connect to web-shop" });
    const note = step.getByRole("alert");
    await expect(note).toContainText(
      "The browser editor could not be installed in web-shop: the download does not match",
    );
    await expect(step.locator("img")).toHaveCount(0);
    await expectNoAxeViolations(page, "connect step");
    // Trying anyway is still the user's call.
    await expect(
      step.getByRole("button", { name: "Open isolated VS Code" }),
    ).toBeEnabled();
    await step.getByRole("button", { name: "Close" }).click();

    await nameLink(card(page, "web-shop"), "web-shop").click();
    const main = page.getByRole("main");
    await expect(main.getByRole("alert")).toContainText(
      "could not be installed in web-shop",
    );
    await expect(main).toContainText(
      "Terminals and desktop VS Code are not affected",
    );
    await expectNoAxeViolations(page, "workspace page");
    await backend.control.step({
      do: "clear_problem",
      key: "editor-install:web-shop",
    });
    await expect(main.getByRole("alert")).toHaveCount(0);

    // A newer build that could not be taken is its own line, beside the install one, and the
    // editor still opens.
    await backend.control.step({
      do: "problem",
      key: "editor-update:web-shop",
      workspace: "web-shop",
      title:
        "The browser editor in web-shop could not update: a newer build was refused <img src=x onerror=alert(1)>",
      detail: "The editor keeps working on the build it has.",
    });
    await expect(main.getByRole("alert")).toContainText(
      "could not update: a newer build was refused",
    );
    await expect(main).toContainText("keeps working on the build it has");
    await expect(main.locator("img")).toHaveCount(0);
    await expectNoAxeViolations(page, "workspace page with an update problem");
    await backend.control.step({
      do: "clear_problem",
      key: "editor-update:web-shop",
    });
    await expect(main.getByRole("alert")).toHaveCount(0);

    // A server that runs as root for want of the session user is its own line too.
    await backend.control.step({
      do: "problem",
      key: "editor-session-user:web-shop",
      workspace: "web-shop",
      title:
        "The browser editor in web-shop runs as root: its user agent is missing in the guest <img src=x onerror=alert(1)>",
      detail: "The editor opens and works, but its files belong to root.",
    });
    await expect(main.getByRole("alert")).toContainText(
      "runs as root: its user agent is missing",
    );
    await expect(main.locator("img")).toHaveCount(0);
    await expectNoAxeViolations(page, "workspace page with a root problem");
    await backend.control.step({
      do: "clear_problem",
      key: "editor-session-user:web-shop",
    });
    await expect(main.getByRole("alert")).toHaveCount(0);
  });

  test("opening in the browser needs no direct SSH and says the window is opening", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await card(page, "web-shop")
      .getByRole("button", { name: "Connect to web-shop" })
      .click();
    const step = page.getByRole("dialog", { name: "Connect to web-shop" });
    await step.getByRole("button", { name: "Open isolated VS Code" }).click();
    await expect(
      toast(page, "Opening web-shop in isolated VS Code."),
    ).toBeVisible();
    await expect(step).toHaveCount(0);
  });

  test("allowing direct SSH says what it trusts, then marks the workspace trusted", async ({
    page,
    request,
    backend,
  }) => {
    await openList(page, backend);
    const shop = card(page, "web-shop");
    await shop.getByRole("button", { name: "Connect to web-shop" }).click();
    const step = page.getByRole("dialog", { name: "Connect to web-shop" });
    await step.getByRole("checkbox", { name: "Allow direct SSH" }).click();
    const trust = page.getByRole("alertdialog", {
      name: "Allow direct SSH to web-shop?",
    });
    await expect(trust).toContainText("trusted workspace");
    await expect(trust).toContainText("GitHub token");
    await expect(trust.getByRole("button", { name: "Cancel" })).toBeFocused();
    await trust.getByRole("button", { name: "Cancel" }).click();
    await expect(
      step.getByRole("checkbox", { name: "Allow direct SSH" }),
    ).not.toBeChecked();
    expect(
      (await workspaceList(request, backend)).find((w) => w.name === "web-shop")
        ?.direct_ssh,
    ).toBe(false);

    await step.getByRole("checkbox", { name: "Allow direct SSH" }).click();
    await trust.getByRole("button", { name: "Allow direct SSH" }).click();
    await expect(
      toast(page, "Direct SSH is on for web-shop: it is trusted now."),
    ).toBeVisible();
    await expect(
      step.getByRole("checkbox", { name: "Allow direct SSH" }),
    ).toBeChecked();
    await expect(step).toContainText("Trusted");
    await expect(shop).toContainText("Trusted");

    await step.getByRole("button", { name: "Open in VS Code" }).click();
    await expect(toast(page, "Opening web-shop in VS Code.")).toBeVisible();
  });

  test("turning it off needs no question and drops the trusted mark", async ({
    page,
    request,
    backend,
  }) => {
    await openDetail(page, backend, "web-shop");
    await page.getByRole("button", { name: "Connect to web-shop" }).click();
    const step = page.getByRole("dialog", { name: "Connect to web-shop" });
    await step.getByRole("checkbox", { name: "Allow direct SSH" }).click();
    await page
      .getByRole("alertdialog")
      .getByRole("button", { name: "Allow direct SSH" })
      .click();
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(page.getByText("Trusted").first()).toBeVisible();
    await step.getByRole("checkbox", { name: "Allow direct SSH" }).click();
    await expect(page.getByRole("alertdialog")).toHaveCount(0);
    await expect(toast(page, "Direct SSH is off for web-shop.")).toBeVisible();
    await expect
      .poll(
        async () =>
          (await workspaceList(request, backend)).find(
            (w) => w.name === "web-shop",
          )?.direct_ssh,
      )
      .toBe(false);
  });
});
