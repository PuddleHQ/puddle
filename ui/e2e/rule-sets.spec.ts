// SPDX-License-Identifier: AGPL-3.0-or-later
// Rule sets and System managed against the real API on the fixture backend (rules spec §7): the
// Rules screen's sections, a set's switches, and an inbox decision into a set. Every test starts
// from the `default` scenario (one pending request, demo -> registry.example.org).
import type { APIRequestContext, Page } from "@playwright/test";
import { type Backend, expect, test } from "./fixture";
import { SWATCHES } from "../src/lib/rules/fill.ts";
import { watchCsp, expectNoAxeViolations, staysEmpty } from "./support";

const auth = (backend: Backend) => ({
  Authorization: `Bearer ${backend.token}`,
});

interface RuleSetView {
  id: string;
  name: string;
  fill: { kind: string; color: string };
  global: boolean | null;
  overrides: { workspace: string; enabled: boolean }[];
  entries: { pattern: string; effect: string }[];
}

async function sets(
  request: APIRequestContext,
  backend: Backend,
): Promise<RuleSetView[]> {
  const response = await request.get("/api/rule-sets", {
    headers: auth(backend),
  });
  return ((await response.json()) as { sets: RuleSetView[] }).sets;
}

async function openRules(page: Page, backend: Backend): Promise<void> {
  await backend.signIn(page);
  await page.goto("/rules");
  await expect(
    page.getByRole("heading", { level: 2, name: "Rule sets" }),
  ).toBeVisible();
}

test.beforeEach(async ({ backend }) => {
  await backend.control.reset("default");
});

test("System managed lists what Puddle allows for the editor, with the reason", async ({
  page,
  backend,
}) => {
  const csp = await watchCsp(page);
  await openRules(page, backend);
  const system = page.getByRole("region", { name: "System managed" });
  await expect(system).toContainText("open-vsx.org");
  await expect(system).toContainText("bundled code-server");
  await expect(system).not.toContainText("marketplace.visualstudio.com");
  await expect(system).toContainText("add a deny rule to block one");
  await expectNoAxeViolations(page, "rules with sets");
  await staysEmpty(page, csp);
});

test("a built-in set ships off, and switches for one workspace", async ({
  page,
  request,
  backend,
}) => {
  await openRules(page, backend);
  const github = page.getByRole("article", { name: "GitHub" });
  await expect(
    github.getByRole("switch", { name: "On for every workspace" }),
  ).not.toBeChecked();
  await expect(github).toContainText("Off (built-in sets ship off)");
  await github.getByLabel("Workspace name").fill("demo");
  await github.getByRole("button", { name: "On there" }).click();
  await expect(github).toContainText("On in demo");
  const stored = (await sets(request, backend)).find(
    (s) => s.id === "builtin:github",
  );
  expect(stored?.overrides).toEqual([{ workspace: "demo", enabled: true }]);
  // Every workspace asks first.
  await github.getByRole("switch", { name: "On for every workspace" }).click();
  const confirm = page.getByRole("alertdialog", {
    name: "Turn on for every workspace?",
  });
  await expectNoAxeViolations(page, "confirm");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(
    github.getByRole("switch", { name: "On for every workspace" }),
  ).not.toBeChecked();
});

test("the mise tools set ships off, and is gone while the global tools switch is off", async ({
  page,
  request,
  backend,
}) => {
  await openRules(page, backend);
  const mise = page.getByRole("article", { name: "mise tools" });
  await expect(
    mise.getByRole("switch", { name: "On for every workspace" }),
  ).not.toBeChecked();
  await expect(mise).toContainText("Off (built-in sets ship off)");
  await expect(mise).toContainText("nodejs.org");

  const put = await request.put("/api/settings", {
    headers: auth(backend),
    data: { tools: { mise: false } },
  });
  expect(put.ok()).toBe(true);
  await page.goto("/rules");
  await expect(
    page.getByRole("heading", { level: 2, name: "Rule sets" }),
  ).toBeVisible();
  await expect(page.getByRole("article", { name: "GitHub" })).toBeVisible();
  await expect(page.getByRole("article", { name: "mise tools" })).toHaveCount(
    0,
  );
  expect((await sets(request, backend)).map((s) => s.id)).not.toContain(
    "builtin:mise-tools",
  );
});

test("a set of your own: make it, then approve a request into it from the inbox", async ({
  page,
  request,
  backend,
}) => {
  await openRules(page, backend);
  await page.getByRole("button", { name: "New rule set" }).click();
  const dialog = page.getByRole("dialog", { name: "New rule set" });
  await dialog.getByLabel("Name").fill("Client X");
  await expectNoAxeViolations(page, "new set");
  await dialog.getByRole("button", { name: "Make rule set" }).click();
  const card = page.getByRole("article", { name: "Client X" });
  await expect(card).toContainText("0 entries");
  // On everywhere would ask; keep it to demo so the inbox decision needs no confirm.
  await card.getByRole("switch", { name: "On for every workspace" }).click();
  await expect(card).toContainText("Off for every workspace");
  await card.getByLabel("Workspace name").fill("demo");
  await card.getByRole("button", { name: "On there" }).click();
  await expect(card).toContainText("On in demo");

  await page.goto("/inbox");
  const row = page.locator("li.req", { hasText: "registry.example.org" });
  await row.getByRole("button", { name: /^More choices for/ }).click();
  const options = page.getByRole("dialog", { name: /^Choices for/ });
  await options.getByRole("radio", { name: /Into rule set Client X/ }).check();
  await options.getByRole("button", { name: "Allow", exact: true }).click();
  await expect(row).toHaveCount(0);
  await expect(
    page.getByRole("region", { name: "Notifications" }).getByRole("status"),
  ).toContainText("Allowed registry.example.org in rule set Client X");
  const client = (await sets(request, backend)).find(
    (s) => s.name === "Client X",
  );
  expect(client?.entries).toEqual([
    expect.objectContaining({
      pattern: "registry.example.org",
      effect: "allow",
    }),
  ]);
});

test("every set has a colour, and the one you pick is kept", async ({
  page,
  request,
  backend,
}) => {
  await openRules(page, backend);
  // A built-in set arrives with its own colour and its name written out beside it.
  const github = page.getByRole("article", { name: "GitHub" });
  await expect(github.getByRole("heading", { name: "GitHub" })).toBeVisible();
  const before = await sets(request, backend);
  expect(before.every((s) => s.fill.kind === "solid")).toBe(true);

  await page.getByRole("button", { name: "New rule set" }).click();
  const dialog = page.getByRole("dialog", { name: "New rule set" });
  await dialog.getByLabel("Name").fill("Painted");
  await dialog.getByRole("button", { name: "#c04f7a" }).click();
  await expect(dialog.getByText(/#c04f7a, text contrast/)).toBeVisible();
  await expectNoAxeViolations(page, "new set with a colour");
  await dialog.getByRole("button", { name: "Make rule set" }).click();
  const card = page.getByRole("article", { name: "Painted" });
  await expect(card).toBeVisible();
  const stored = (await sets(request, backend)).find(
    (s) => s.name === "Painted",
  );
  expect(stored?.fill).toEqual({ kind: "solid", color: "#c04f7a" });
  await expectNoAxeViolations(page, "rules with coloured sets");

  // Editing starts from the colour the set has, and a new pick replaces it.
  await card.getByRole("button", { name: "Edit" }).click();
  const edit = page.getByRole("dialog", { name: "Edit rule set" });
  await expect(edit.getByRole("button", { name: "#c04f7a" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await edit.getByRole("button", { name: "#2aa1a8" }).click();
  await edit.getByRole("button", { name: "Save" }).click();
  await expect
    .poll(
      async () =>
        (await sets(request, backend)).find((s) => s.name === "Painted")?.fill,
    )
    .toEqual({ kind: "solid", color: "#2aa1a8" });
});

for (const scheme of ["light", "dark"] as const) {
  test(`axe finds nothing in ${scheme}: a set in every swatch, and the picker that offers them`, async ({
    page,
    request,
    backend,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    for (const [index, color] of SWATCHES.entries()) {
      const made = await request.post("/api/rule-sets", {
        headers: auth(backend),
        data: { name: `Swatch ${index + 1}`, fill: { kind: "solid", color } },
      });
      expect(made.status()).toBe(201);
    }
    await openRules(page, backend);
    for (const [index, color] of SWATCHES.entries()) {
      const chip = page
        .getByRole("article", { name: `Swatch ${index + 1}` })
        .getByRole("heading", { name: `Swatch ${index + 1}` });
      await expect(chip).toBeVisible();
      await expect(chip.locator("[style*='--set-fill']")).toHaveCSS(
        "background-color",
        rgb(color),
      );
    }
    await expectNoAxeViolations(page, `rules, every swatch, ${scheme}`);
    await page.getByRole("button", { name: "New rule set" }).click();
    const dialog = page.getByRole("dialog", { name: "New rule set" });
    for (const color of SWATCHES)
      await expect(dialog.getByRole("button", { name: color })).toBeVisible();
    await expectNoAxeViolations(page, `the picker, ${scheme}`);
  });
}

/** `#rrggbb` as the computed `rgb(r, g, b)` a browser reports. */
function rgb(hex: string): string {
  const n = Number.parseInt(hex.slice(1), 16);
  return `rgb(${n >> 16}, ${(n >> 8) & 255}, ${n & 255})`;
}
