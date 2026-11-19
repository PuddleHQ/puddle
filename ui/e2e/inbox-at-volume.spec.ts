// SPDX-License-Identifier: AGPL-3.0-or-later
// The Inbox and Rules pages at volume and across workspaces, against the real API on the fixture
// backend: Ctrl+click on Allow, the reason a UDP attempt was refused, the keys the buttons name,
// workspaces whose id differs from their name (50 waiting requests: names everywhere, never ids),
// counts that equal the rows, a deny that clears the list, and 1000 rules.
import type { APIRequestContext, Locator, Page } from "@playwright/test";
import { slowdown } from "./cpu-throttle.ts";
import { type Backend, expect, test } from "./fixture";
import { availableParallelism, loadavg } from "node:os";
import { loadFactor } from "../scripts/load-budget.ts";
import { staysEmpty } from "./support";

const SLOWDOWN = slowdown();

interface Rule {
  id: number;
  effect: "allow" | "deny";
  pattern: string;
  pattern_kind: "exact" | "suffix";
  scope: { type: "global" } | { type: "workspace"; workspace: string };
}

const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

async function rules(
  request: APIRequestContext,
  backend: Backend,
): Promise<Rule[]> {
  const response = await request.get("/api/rules", { headers: auth(backend) });
  return ((await response.json()) as { rules: Rule[] }).rules;
}

async function ask(
  backend: Backend,
  workspace: string,
  host: string,
): Promise<void> {
  await backend.control.step({ do: "request", workspace, host });
}

const rowFor = (page: Page, host: string): Locator =>
  page.locator("li.req", {
    has: page.locator(`.host:text-is(${JSON.stringify(host)})`),
  });

async function openInbox(page: Page, backend: Backend): Promise<void> {
  await backend.signIn(page);
  await page.goto("/inbox");
  await expect(
    page.getByRole("heading", { level: 1, name: "Inbox" }),
  ).toBeVisible();
}

/**
 * Makes workspaces through the API, as the app does: the id is made from the name, so a name with
 * capitals and spaces gets a different id. Returns name and id, and fails if they are the same,
 * because then a test could not tell the one from the other.
 */
async function makeWorkspaces(
  request: APIRequestContext,
  backend: Backend,
  names: readonly string[],
): Promise<{ name: string; id: string }[]> {
  for (const name of names) {
    const made = await request.post("/api/workspaces", {
      headers: auth(backend),
      data: {
        name,
        repositories: [{ url: "https://github.com/acme/volume.git" }],
      },
    });
    expect(made.status(), `creating ${name}`).toBe(202);
  }
  const listed = (await (
    await request.get("/api/workspaces", { headers: auth(backend) })
  ).json()) as { workspaces: { id: string; name: string }[] };
  return names.map((name) => {
    const found = listed.workspaces.find((w) => w.name === name);
    expect(found, `workspace ${name} is listed`).toBeDefined();
    expect(found?.id, `the id of ${name} differs from its name`).not.toBe(name);
    return { name, id: found!.id };
  });
}

// Counts and first rows are exact: start from a scenario with nothing waiting.
test.beforeEach(async ({ backend }) => {
  await backend.control.reset("empty");
});

test.describe("Allow and Deny, and the keys they name", () => {
  test("Ctrl+click on Allow opens 'Allow for every workspace?' and a confirm writes a global allow", async ({
    page,
    request,
    backend,
  }) => {
    await ask(backend, "shop", "ctrlallow.example.com");
    await openInbox(page, backend);
    const row = rowFor(page, "ctrlallow.example.com");
    await row
      .getByRole("button", { name: /^Allow ctrlallow\.example\.com/ })
      .click({ modifiers: ["Control"] });
    const confirm = page.getByRole("alertdialog", {
      name: "Allow for every workspace?",
    });
    await expect(confirm).toContainText(
      "Allow ctrlallow.example.com for every workspace, permanently",
    );
    // Nothing is written before the confirm.
    expect(
      (await rules(request, backend)).some(
        (r) => r.pattern === "ctrlallow.example.com",
      ),
    ).toBe(false);
    await confirm
      .getByRole("button", { name: "Allow in every workspace" })
      .click();
    await expect(row).toHaveCount(0);
    expect(
      (await rules(request, backend)).find(
        (r) => r.pattern === "ctrlallow.example.com",
      ),
    ).toMatchObject({ effect: "allow", scope: { type: "global" } });
  });

  test("each row button's title names the key, and pressing that key does what the button does", async ({
    page,
    request,
    backend,
  }) => {
    for (const host of ["t0.example.com", "t1.example.com", "t2.example.com"]) {
      await ask(backend, "shop", host);
    }
    await openInbox(page, backend);
    await expect(page.locator("li.req")).toHaveCount(3);
    const hosts = (await page.locator("li.req .host").allTextContents()).filter(
      (h) => /^t\d\.example\.com$/.test(h),
    );
    // flake-ok: the three rows were counted by the web-first assertion above
    expect(hosts).toHaveLength(3);

    /** The key a button's first title line names in brackets: "... permanently (A)". */
    const keyOf = async (button: Locator): Promise<string> => {
      const title = (await button.getAttribute("title")) ?? "";
      const key = /\(([A-Za-z]+)\)\s*$/m.exec(title.split("\n")[0] ?? "")?.[1];
      expect(key, `a key in the title ${JSON.stringify(title)}`).toBeDefined();
      return key!;
    };
    const buttons = (host: string) => ({
      allow: rowFor(page, host).getByRole("button", { name: /^Allow / }),
      deny: rowFor(page, host).getByRole("button", { name: /^Deny / }),
      more: rowFor(page, host).getByRole("button", {
        name: /^More choices for /,
      }),
    });
    const keys = {
      allow: await keyOf(buttons(hosts[0]!).allow),
      deny: await keyOf(buttons(hosts[0]!).deny),
      more: await keyOf(buttons(hosts[0]!).more),
    };
    // The same key on every row: the title is not specific to the first.
    for (const host of hosts) {
      await expect(buttons(host).allow).toHaveAttribute(
        "title",
        new RegExp(`\\(${keys.allow}\\)`, "m"),
      );
      await expect(buttons(host).deny).toHaveAttribute(
        "title",
        new RegExp(`\\(${keys.deny}\\)`, "m"),
      );
      await expect(buttons(host).more).toHaveAttribute(
        "title",
        new RegExp(`\\(${keys.more}\\)`, "m"),
      );
    }
    expect(new Set(Object.values(keys)).size, "three different keys").toBe(3);

    // The named key, pressed with the row focused, decides or opens that button's choice.
    await rowFor(page, hosts[0]!).locator(".host").click();
    await expect(rowFor(page, hosts[0]!)).toBeFocused();
    await page.keyboard.press(keys.allow);
    await expect(rowFor(page, hosts[0]!)).toHaveCount(0);
    await expect(rowFor(page, hosts[1]!)).toBeFocused();
    await page.keyboard.press(keys.more);
    await expect(
      page.getByRole("dialog", { name: `Choices for ${hosts[1]}` }),
    ).toBeVisible();
    await page.keyboard.press("Escape");
    await rowFor(page, hosts[1]!).locator(".host").click();
    await page.keyboard.press(keys.deny);
    await expect(rowFor(page, hosts[1]!)).toHaveCount(0);
    const made = (await rules(request, backend))
      .filter((r) => hosts.includes(r.pattern))
      .map((r) => [r.effect, r.pattern]);
    expect(made.sort()).toEqual(
      [
        ["allow", hosts[0]],
        ["deny", hosts[1]],
      ].sort(),
    );
  });
});

test("a UDP attempt the agent reported shows 'UDP is not supported' in Activity, and never waits in the Inbox", async ({
  page,
  backend,
}) => {
  await backend.control.step({
    do: "connection",
    workspace: "shop",
    host: "voice.example.com",
    port: 3478,
    decision: "blocked",
    reason: "udp_unsupported",
  });
  await backend.installClock(page);
  await backend.signIn(page);
  await page.goto("/activity");
  await expect(
    page.getByRole("heading", { level: 1, name: "Activity" }),
  ).toBeVisible();
  const row = page.locator("tbody tr.row", { hasText: "voice.example.com" });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("UDP is not supported");
  await page.goto("/inbox");
  await expect(
    page.getByRole("heading", { level: 1, name: "Inbox" }),
  ).toBeVisible();
  await expect(
    page.locator("li.req", { hasText: "voice.example.com" }),
  ).toHaveCount(0);
});

test.describe("five workspaces whose id differs from the name, 50 waiting requests", () => {
  const NAMES = [
    "Shop One",
    "Billing Two",
    "Docs Three",
    "Mobile Four",
    "Search Five",
  ] as const;
  const DOMAIN = "volume.example.net";
  /** Longer than the clearing takes on an idle machine by a wide margin; grows with a declared slowdown. */
  const CLEAR_BAR_MS = 5_000 * SLOWDOWN;
  test.setTimeout(90_000 * SLOWDOWN);

  async function fifty(
    request: APIRequestContext,
    backend: Backend,
  ): Promise<{ name: string; id: string }[]> {
    const spaces = await makeWorkspaces(request, backend, NAMES);
    for (let i = 0; i < 50; i += 1) {
      await ask(backend, spaces[i % 5]!.id, `n${i}.${DOMAIN}`);
    }
    return spaces;
  }

  const rows = (page: Page) =>
    page.locator("li.req", { hasText: `.${DOMAIN}` });

  /** Every text, accessible name and tooltip the page shows in its content, joined. */
  const shown = (page: Page) =>
    page
      .locator("main")
      .evaluate((main) =>
        [
          main.textContent ?? "",
          ...[...main.querySelectorAll("[aria-label], [title]")].flatMap(
            (el) => [
              el.getAttribute("aria-label") ?? "",
              el.getAttribute("title") ?? "",
            ],
          ),
        ].join("\n"),
      );

  test("every row shows the workspace's name, never its id, and the counts equal the rows", async ({
    page,
    request,
    backend,
  }) => {
    const spaces = await fifty(request, backend);
    await openInbox(page, backend);
    await expect(rows(page)).toHaveCount(50, { timeout: CLEAR_BAR_MS });
    // Each workspace's rows say its name (10 each).
    for (const { name } of spaces) {
      await expect(
        rows(page).filter({
          has: page.locator(".facts b", { hasText: new RegExp(`^${name}$`) }),
        }),
      ).toHaveCount(10);
    }
    await staysEmpty(
      page,
      async () => {
        const text = await shown(page);
        return spaces.filter(({ id }) => text.includes(id)).map(({ id }) => id);
      },
      "ids shown in the page's text, names or tooltips",
    );
    // Title, nav badge and rows agree.
    await expect(page).toHaveTitle("Inbox (50) - Puddle");
    await expect(page.getByTestId("pending-badge")).toContainText("50 pending");
    // One decided row moves all three together.
    await rows(page)
      .first()
      .getByRole("button", { name: /^Deny / })
      .click();
    await expect(rows(page)).toHaveCount(49);
    await expect(page).toHaveTitle("Inbox (49) - Puddle");
    await expect(page.getByTestId("pending-badge")).toContainText("49 pending");
    // The same count as the service's.
    const pending = (await (
      await request.get("/api/pending", { headers: auth(backend) })
    ).json()) as { requests: unknown[] };
    expect(pending.requests).toHaveLength(49);
  });

  test("a suffix deny for every workspace clears all 50 at once", async ({
    page,
    request,
    backend,
  }) => {
    await fifty(request, backend);
    await openInbox(page, backend);
    await expect(rows(page)).toHaveCount(50, { timeout: CLEAR_BAR_MS });
    await rows(page)
      .first()
      .getByRole("button", { name: /^More choices for / })
      .click();
    const dialog = page.getByRole("dialog", { name: /^Choices for/ });
    await dialog.getByRole("radio", { name: /^Every workspace/ }).check();
    await dialog
      .getByRole("radio", { name: /^Everything under example\.net/ })
      .check();
    await dialog.getByRole("button", { name: "Deny", exact: true }).click();
    await page.getByRole("button", { name: "Deny in every workspace" }).click();
    await expect(rows(page)).toHaveCount(0, { timeout: CLEAR_BAR_MS });
    await expect(page).toHaveTitle("Inbox - Puddle");
    await expect(
      page.getByRole("heading", { name: "All quiet" }),
    ).toBeVisible();
    expect(
      (await rules(request, backend)).filter((r) => r.effect === "deny"),
    ).toMatchObject([
      {
        pattern: ".example.net",
        pattern_kind: "suffix",
        scope: { type: "global" },
      },
    ]);
  });

  test("a suffix deny for one workspace clears that workspace's 10 and leaves the other 40", async ({
    page,
    request,
    backend,
  }) => {
    const spaces = await fifty(request, backend);
    await openInbox(page, backend);
    await expect(rows(page)).toHaveCount(50, { timeout: CLEAR_BAR_MS });
    const mine = rows(page).filter({
      has: page.locator(".facts b", {
        hasText: new RegExp(`^${spaces[0]!.name}$`),
      }),
    });
    await mine
      .first()
      .getByRole("button", { name: /^More choices for / })
      .click();
    const dialog = page.getByRole("dialog", { name: /^Choices for/ });
    await dialog
      .getByRole("radio", { name: /^Everything under example\.net/ })
      .check();
    await dialog.getByRole("button", { name: "Deny", exact: true }).click();
    await expect(rows(page)).toHaveCount(40, { timeout: CLEAR_BAR_MS });
    await expect(mine).toHaveCount(0);
    await expect(page).toHaveTitle("Inbox (40) - Puddle");
  });

  test("a deny for every workspace of one host that all five asked about clears its five rows", async ({
    page,
    request,
    backend,
  }) => {
    const spaces = await makeWorkspaces(request, backend, NAMES);
    for (const { id } of spaces) await ask(backend, id, `same.${DOMAIN}`);
    await openInbox(page, backend);
    const same = page.locator("li.req", {
      has: page.locator(`.host:text-is("same.${DOMAIN}")`),
    });
    await expect(same).toHaveCount(5, { timeout: CLEAR_BAR_MS });
    await same
      .first()
      .getByRole("button", { name: /^Deny / })
      .click({
        modifiers: ["Control"],
      });
    await page.getByRole("button", { name: "Deny in every workspace" }).click();
    await expect(same).toHaveCount(0, { timeout: CLEAR_BAR_MS });
    await expect(page).toHaveTitle("Inbox - Puddle");
  });

  test("the 'full list' banner names the workspace, never its id", async ({
    page,
    request,
    backend,
  }) => {
    const [flood] = await makeWorkspaces(request, backend, ["Flood Zone"]);
    await backend.control.step({
      do: "bulk",
      workspace: flood!.id,
      count: 1000,
      domain: DOMAIN,
    });
    await openInbox(page, backend);
    const banner = page.getByRole("status").filter({ hasText: "has 1000" });
    await expect(banner).toContainText("Flood Zone has 1000 waiting requests", {
      timeout: CLEAR_BAR_MS,
    });
    await expect(banner).not.toContainText(flood!.id);
  });
});

test.describe("1000 rules", () => {
  test("seeded through the API they render within the bar, the page's count equals the API's, and 20 adds and deletes keep it right", async ({
    page,
    request,
    backend,
  }) => {
    // 20 adds and 20 deletes on a 1000-row page: seconds each on a host with every lane busy (the 4 min this had ran out at the 18th delete).
    test.setTimeout(900_000 * SLOWDOWN);
    const spaces = await makeWorkspaces(request, backend, [
      "Rules One",
      "Rules Two",
      "Rules Three",
    ]);
    for (let i = 0; i < 1000; i += 1) {
      const made = await request.post("/api/rules", {
        headers: auth(backend),
        data: {
          pattern: `host${i}.volume.example.org`,
          effect: i % 2 === 0 ? "allow" : "deny",
          scope:
            i % 3 === 0
              ? { type: "global" }
              : { type: "workspace", workspace: spaces[i % 3]!.id },
        },
      });
      expect(made.status(), `rule ${i}`).toBe(201);
    }
    // Making a workspace adds rules of its own, so the API's count is the reference, not 1000.
    const seeded = (await rules(request, backend)).length;
    expect(seeded).toBeGreaterThanOrEqual(1000);

    await backend.installClock(page);
    await backend.signIn(page);
    // The first load also boots the app, and a shared host takes seconds over that. The bar is
    // the page's own work: leave for the Inbox and come back through the navigation, five times,
    // and judge the best (the way inbox-bulk.spec.ts judges its loads: a regression costs
    // seconds in every one, a busy machine only in some).
    await page.goto("/rules");
    await expect(page.locator("p.count")).toHaveText(`${seeded} rules`, {
      timeout: 60_000 * SLOWDOWN,
    });
    const nav = page.getByRole("navigation", { name: "Main" });
    const loads: number[] = [];
    for (let load = 0; load < 5; load += 1) {
      await nav.getByRole("link", { name: /^Inbox/ }).click();
      // Leaving the 1000-row page is the page's own work (not judged here, the Rules load below is):
      // it took over 5 s on the shared host, in chromium and webkit alike (two of two runs).
      await expect(
        page.getByRole("heading", { level: 1, name: "Inbox" }),
      ).toBeVisible({ timeout: 60_000 * SLOWDOWN });
      const began = Date.now();
      await nav.getByRole("link", { name: /^Rules/ }).click();
      await expect(page.locator("p.count")).toHaveText(`${seeded} rules`, {
        timeout: 60_000 * SLOWDOWN,
      });
      loads.push(Date.now() - began);
    }
    const best = Math.min(...loads);
    test.info().annotations.push({
      type: "rules-1000",
      description: `Rules page, 1000 rules, best of 5: ${best} ms (${loads.join(", ")})`,
    });
    // Both browsers meet the same bar: the table draws its first rows at once and the rest a chunk
    // per frame (RuleTable.svelte), because WebKit took 2849 ms to draw all 1000 in one go. The bar
    // grows with the machine's load the way every test's time limit does (scripts/load-budget.ts):
    // the best of five loads on the host with every lane busy took 3337 ms in Chromium with nothing
    // wrong with the page. An idle machine keeps 2 s; the load read when the test starts, at most
    // four times that.
    const load = loadFactor(loadavg()[0] ?? 0, availableParallelism());
    const bar = 2000 * SLOWDOWN * load;
    expect(best, "the page lists 1000 rules in time").toBeLessThan(bar);
    // Deferred is not dropped: every rule ends up as a row, reachable by keyboard and screen reader.
    await expect(page.locator("tbody tr")).toHaveCount(seeded, {
      timeout: 60_000 * SLOWDOWN,
    });

    // 20 adds through the form, 20 deletes through the rows; the count follows each.
    for (let i = 0; i < 20; i += 1) {
      await page.getByRole("button", { name: "Add rule" }).first().click();
      const dialog = page.getByRole("dialog", { name: "Add a rule" });
      await dialog.getByLabel("Host").fill(`added${i}.volume.example.org`);
      await dialog
        .getByLabel("Workspace", { exact: true })
        .selectOption({ label: spaces[0]!.name });
      await dialog.getByRole("button", { name: "Add rule" }).click();
      await expect(dialog).toHaveCount(0);
      await expect(page.locator("p.count")).toHaveText(
        `${seeded + 1 + i} rules`,
      );
    }
    expect((await rules(request, backend)).length).toBe(seeded + 20);
    for (let i = 0; i < 20; i += 1) {
      const row = page.locator("tbody tr", {
        has: page.locator(
          `td.host span.mono:text-is("added${i}.volume.example.org")`,
        ),
      });
      await row.getByRole("button", { name: /^Delete rule/ }).click();
      await page
        .getByRole("alertdialog", { name: "Delete this rule?" })
        .getByRole("button", { name: "Delete rule" })
        .click();
      await expect(row).toHaveCount(0);
      await expect(page.locator("p.count")).toHaveText(
        `${seeded + 19 - i} rules`,
      );
    }
    const left = await rules(request, backend);
    expect(left.length).toBe(seeded);
    expect(left.some((r) => r.pattern.startsWith("added"))).toBe(false);
    await expect(page.locator("p.count")).toHaveText(`${seeded} rules`);
  });
});
