// SPDX-License-Identifier: AGPL-3.0-or-later
// The "Tools:" line on the workspace card, fed by `workspace_tools` events (the background install
// after a start). Every state, with the accessibility scan, and the same card at 390 px.
import type { Locator, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { expectNoAxeViolations, staysEmpty } from "./support";

const card = (page: Page, name: string): Locator =>
  page.getByRole("article", { name });

async function openList(page: Page, backend: Backend) {
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto("/workspaces");
  await expect(card(page, "web-shop")).toBeVisible();
}

/** Emits until the card shows the text: the event stream may connect after the page loaded. */
async function raise(
  backend: Backend,
  page: Page,
  state: Record<string, unknown>,
  text: string,
) {
  await expect(async () => {
    await backend.control.emit({
      type: "workspace_tools",
      workspace: "web-shop",
      state,
    });
    await expect(card(page, "web-shop")).toContainText(text, { timeout: 500 });
  }).toPass();
}

/** The elements that reach past the viewport's right edge; none means no horizontal scroll. */
const overflowing = (page: Page) => () =>
  page.evaluate(() => {
    const width = document.documentElement.clientWidth;
    return [...document.body.querySelectorAll("*")]
      .filter((el) => el.getBoundingClientRect().right > width + 0.5)
      .slice(0, 6)
      .map((el) => `${el.tagName.toLowerCase()}.${String(el.className)}`);
  });

test.beforeEach(async ({ backend }) => {
  await backend.control.reset("lived-in");
});

test("a card says nothing about tools until an install ran", async ({
  page,
  backend,
}) => {
  await openList(page, backend);
  await expect(card(page, "web-shop")).not.toContainText("Tools:");
});

test("shows the install's progress, then ready", async ({ page, backend }) => {
  await openList(page, backend);
  await raise(
    backend,
    page,
    { phase: "installing", done: 1, total: 3, tools: ["node", "java"] },
    "Tools: installing 1 of 3 (node, java)",
  );
  await expectNoAxeViolations(page, "tools installing");
  await raise(backend, page, { phase: "ready" }, "Tools: ready");
  await expect(card(page, "web-shop")).not.toContainText("installing");
  await expectNoAxeViolations(page, "tools ready");
});

test("a failed install names the tools and links the hosts it waits on to the Network tab", async ({
  page,
  backend,
}) => {
  await openList(page, backend);
  await raise(
    backend,
    page,
    { phase: "failed", tools: ["node"], waiting_on: ["nodejs.org"] },
    "Tools: failed to install node",
  );
  await expectNoAxeViolations(page, "tools failed");
  await card(page, "web-shop")
    .getByRole("link", { name: "Waiting for you to allow nodejs.org" })
    .click();
  await expect(page).toHaveURL(/\/workspaces\/web-shop\/network$/);
});

test.describe("at 390 px wide", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("the failed line with long hosts fits without a horizontal scroll", async ({
    page,
    backend,
  }) => {
    await openList(page, backend);
    await raise(
      backend,
      page,
      {
        phase: "failed",
        tools: ["node", "java", "python", "terraform"],
        waiting_on: [
          "registry.npmjs.org",
          "api.adoptium.net",
          "github.com",
          "releases.hashicorp.com",
        ],
      },
      "Tools: failed to install node, java, python, terraform",
    );
    await staysEmpty(page, overflowing(page), "overflow");
    await expectNoAxeViolations(page, "tools failed at 390 px");
    await raise(
      backend,
      page,
      {
        phase: "installing",
        done: 2,
        total: 4,
        tools: ["python", "terraform"],
      },
      "Tools: installing 2 of 4",
    );
    await staysEmpty(page, overflowing(page), "overflow");
  });
});
