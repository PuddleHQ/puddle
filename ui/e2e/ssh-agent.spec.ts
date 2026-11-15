// SPDX-License-Identifier: AGPL-3.0-or-later
// The filtered SSH agent in the UI: the first-use question in the inbox (once, for this run,
// always, deny) and the per-host switch on a workspace's SSH agent tab, against the real API on the
// fixture backend. The fixture's `ssh_agent_ask` step opens the question as the host's gate does.
import type { Backend } from "./fixture";
import { expect, test } from "./fixture";
import { expectNoAxeViolations } from "./support";

const KEY = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

/** A workspace's `ssh` waits for the first-use answer. */
async function wait(
  backend: Backend,
  workspace: string,
  host: string,
  fingerprint?: string,
): Promise<void> {
  await backend.control.step({
    do: "ssh_agent_ask",
    workspace,
    host,
    ...(fingerprint ? { fingerprint } : {}),
  });
}

async function openInbox(
  page: import("@playwright/test").Page,
  backend: Backend,
) {
  await backend.signIn(page);
  await page.goto("/inbox");
  await expect(
    page.getByRole("heading", { level: 1, name: "Inbox" }),
  ).toBeVisible();
}

const agentSection = (page: import("@playwright/test").Page) =>
  page.getByRole("region", { name: "SSH agent" });
const askFor = (page: import("@playwright/test").Page, host: string) =>
  agentSection(page).locator("li.ask", {
    has: page.locator(`.host:text-is(${JSON.stringify(host)})`),
  });

// The workspaces the questions are about must exist: `web-shop` is running in `lived-in`.
test.beforeEach(async ({ backend }) => {
  await backend.control.reset("lived-in");
});

test.describe("the first-use question", () => {
  test("shows who is logging in where and with which key, and counts in the badge", async ({
    page,
    backend,
  }) => {
    await wait(backend, "web-shop", "build.example.com");
    await openInbox(page, backend);
    const item = askFor(page, "build.example.com");
    await expect(item).toContainText("Workspace web-shop");
    await expect(item).toContainText("ssh-ed25519");
    await expect(agentSection(page)).toContainText("1 waiting");
    // The requests that wait and the question are in the badge together.
    const waiting = (await backend.control.state()).pending + 1;
    await expect(page.getByTestId("pending-badge")).toHaveText(
      new RegExp(String(waiting)),
    );
    await expectNoAxeViolations(page, "inbox with an SSH agent question");
  });

  test("a question that comes while the page is open appears without a reload", async ({
    page,
    backend,
  }) => {
    await openInbox(page, backend);
    await expect(agentSection(page)).toHaveCount(0);
    await wait(backend, "web-shop", "late.example.com");
    await expect(askFor(page, "late.example.com")).toBeVisible();
  });

  test("Always allow ends the question and later logins with that key ask nothing", async ({
    page,
    backend,
    request,
  }) => {
    await wait(backend, "web-shop", "build.example.com");
    await openInbox(page, backend);
    await askFor(page, "build.example.com")
      .getByRole("button", { name: /^Always allow/ })
      .click();
    await expect(askFor(page, "build.example.com")).toHaveCount(0);
    // The same workspace, host and key again: a standing yes covers it, so no question opens.
    await wait(backend, "web-shop", "build.example.com");
    // Another host and another key still ask, which is also the signal that the page saw the change.
    await wait(backend, "web-shop", "other.example.com", KEY);
    await expect(askFor(page, "other.example.com")).toBeVisible();
    await expect(askFor(page, "build.example.com")).toHaveCount(0);
    const list = await request.get("/api/ssh-agent-asks", {
      headers: auth(backend),
    });
    const asks = ((await list.json()) as { asks: { host: string }[] }).asks;
    expect(asks.map((a) => a.host)).toEqual(["other.example.com"]);
  });

  test("Allow once and Deny leave no standing answer", async ({
    page,
    backend,
  }) => {
    await wait(backend, "web-shop", "once.example.com");
    await wait(backend, "web-shop", "no.example.com");
    await openInbox(page, backend);
    await askFor(page, "once.example.com")
      .getByRole("button", { name: /^Allow once/ })
      .click();
    await askFor(page, "no.example.com")
      .getByRole("button", { name: /^Deny/ })
      .click();
    await expect(agentSection(page)).toHaveCount(0);
    await wait(backend, "web-shop", "once.example.com");
    await expect(askFor(page, "once.example.com")).toBeVisible();
  });

  test("Allow for this run is one answer among the four", async ({
    page,
    backend,
  }) => {
    await wait(backend, "web-shop", "run.example.com");
    await openInbox(page, backend);
    await askFor(page, "run.example.com")
      .getByRole("button", { name: /^Allow for this run/ })
      .click();
    await expect(agentSection(page)).toHaveCount(0);
  });
});

test.describe("the switch on the workspace's tab", () => {
  test("is off for every host, turns on for one, and off again", async ({
    page,
    backend,
  }) => {
    await backend.signIn(page);
    await page.goto("/workspaces/web-shop/ssh-agent");
    await expect(
      page.getByRole("heading", { level: 2, name: "SSH agent" }),
    ).toBeVisible();
    await expect(page.getByText("Off for every host.")).toBeVisible();
    await expectNoAxeViolations(page, "ssh agent tab, off");
    await page.getByLabel("Host").fill("Build.Example.com");
    await page.getByRole("button", { name: "Turn on" }).click();
    const row = page.locator("li", { hasText: "build.example.com" });
    await expect(row).toContainText("every key in your agent");
    await expectNoAxeViolations(page, "ssh agent tab, on");
    await page
      .getByRole("button", {
        name: "Turn off the SSH agent for build.example.com",
      })
      .click();
    await expect(page.getByText("Off for every host.")).toBeVisible();
  });

  test("a key that is not a fingerprint is refused before anything is sent", async ({
    page,
    backend,
  }) => {
    await backend.signIn(page);
    await page.goto("/workspaces/web-shop/ssh-agent");
    await page.getByLabel("Host").fill("build.example.com");
    await page
      .getByLabel("Only these keys (optional)")
      .fill("ssh-ed25519 AAAA");
    await page.getByRole("button", { name: "Turn on" }).click();
    await expect(page.getByRole("alert")).toContainText(
      "is not a SHA-256 key fingerprint",
    );
    await expect(page.getByText("Off for every host.")).toBeVisible();
  });

  test("turning a host off takes its waiting question off the inbox", async ({
    page,
    backend,
  }) => {
    await backend.signIn(page);
    await page.goto("/workspaces/web-shop/ssh-agent");
    await page.getByLabel("Host").fill("build.example.com");
    await page.getByLabel("Only these keys (optional)").fill(KEY);
    await page.getByRole("button", { name: "Turn on" }).click();
    await expect(
      page.locator("li", { hasText: "build.example.com" }),
    ).toContainText("1 key:");
    await wait(backend, "web-shop", "build.example.com");
    await page
      .getByRole("button", {
        name: "Turn off the SSH agent for build.example.com",
      })
      .click();
    await expect(page.getByText("Off for every host.")).toBeVisible();
    await page.goto("/inbox");
    await expect(
      page.getByRole("heading", { level: 1, name: "Inbox" }),
    ).toBeVisible();
    await expect(agentSection(page)).toHaveCount(0);
  });
});
