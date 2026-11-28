// SPDX-License-Identifier: AGPL-3.0-or-later
// What every rules page test file does around its tests: reset the fakes and stores, seed rules,
// read the table, open a row menu. Import it after the file's vi.mock calls.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, expect } from "vitest";
import { ruleSets } from "#lib/stores/rule-sets.svelte.ts";
import { requestRulesStore } from "#lib/stores/request-rules.svelte.ts";
import { rulesStore } from "#lib/stores/rules.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import RulesPage from "./+page.svelte";
import { h } from "./rules-harness.ts";

const { api, setsApi, requestApi, rule } = h;

export function installReset() {
  beforeEach(() => {
    api.rules = [];
    api.calls = [];
    api.bodies = [];
    api.down = false;
    api.refuse = null;
    rulesStore.rules = [];
    rulesStore.status = "loading";
    requestApi.rules = [];
    requestApi.calls = [];
    requestApi.bodies = [];
    requestApi.down = false;
    requestApi.refuse = null;
    requestRulesStore.rules = [];
    requestRulesStore.status = "loading";
    setsApi.sets = [];
    setsApi.system = [];
    setsApi.calls = [];
    setsApi.bodies = [];
    setsApi.down = false;
    ruleSets.sets = [];
    ruleSets.system = [];
    ruleSets.status = "loading";
    for (const t of [...toasts.items]) toasts.dismiss(t.id);
  });
  afterEach(cleanup);
}

export function seed() {
  api.rules = [
    rule(1, { pattern: "registry.npmjs.org", created_at: 100 }),
    rule(2, {
      pattern: ".github.com",
      pattern_kind: "suffix",
      scope: { type: "workspace", workspace: "shop" as never },
      effect: "deny",
      created_at: 200,
    }),
    rule(3, {
      pattern: "old.example.com",
      expires_at: Date.now() - 60_000,
      created_at: 300,
    }),
  ];
}

export const rows = () => screen.getAllByRole("row").slice(1);
export const hosts = () =>
  rows().map((r) => within(r).getAllByRole("cell")[0]!.textContent?.trim());

export async function loaded() {
  render(RulesPage);
  await waitFor(() => expect(rulesStore.status).toBe("ready"));
}

/** Opens the row menu of the rule an item name points at ("Delete rule: <rule>") and picks that item. */
export async function choose(item: string | RegExp) {
  const source = typeof item === "string" ? item : item.source;
  const found = /^\^?(?:Edit rule|Change expiry|Delete rule)(?:: (.*))?$/.exec(
    source,
  );
  const rest = found?.[1] ?? "";
  const trigger = screen.getByRole("button", {
    name: new RegExp(
      `^More about ${typeof item === "string" ? rest.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") : rest}`,
    ),
  });
  await fireEvent.keyDown(trigger, { key: "Enter" });
  await fireEvent.click(await screen.findByRole("menuitem", { name: item }));
}

// The first render of this page, its first open dialog and its first role queries build what every
// later test reuses. That cost fell on the first test, where a loaded host (load 91, 12 s loop lag)
// turned a 0.4 s test into 7.4 s against the 5 s limit. A top-level await has no test clock, so the
// file pays it here once, as vitest.setup.ts does for what every page shares.
export async function warmUp() {
  seed();
  await loaded();
  await fireEvent.click(screen.getByRole("button", { name: /Add rule/ }));
  await screen.findByRole("dialog");
  screen.getAllByRole("option", { hidden: true });
  rows();
  cleanup();
  api.rules = [];
  rulesStore.rules = [];
  rulesStore.status = "loading";
  ruleSets.status = "loading";
  requestRulesStore.status = "loading";
}
