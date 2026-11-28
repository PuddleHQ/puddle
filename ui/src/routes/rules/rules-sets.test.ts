// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

vi.mock("#lib/stores/rule-sets.svelte.ts", async (original) =>
  (await import("./rules-harness.ts")).ruleSetsMock(await original()),
);
vi.mock("#lib/stores/request-rules.svelte.ts", async (original) =>
  (await import("./rules-harness.ts")).requestRulesMock(await original()),
);
vi.mock("#lib/stores/rules.svelte.ts", async (original) =>
  (await import("./rules-harness.ts")).rulesMock(await original()),
);

import { ruleSets } from "#lib/stores/rule-sets.svelte.ts";
import { requestRulesStore } from "#lib/stores/request-rules.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import RulesPage from "./+page.svelte";
import { h } from "./rules-harness.ts";
import { choose, installReset, loaded, warmUp } from "./rules-setup.ts";

const {
  api,
  setsApi,
  source,
  rule,
  builtIn,
  mine,
  systemHost,
  requestApi,
  requestRule,
} = h;

installReset();
await warmUp();

describe("rule sets and System managed (rules spec §7)", () => {
  async function withSets() {
    setsApi.sets = [
      builtIn("github", { name: "GitHub" }),
      mine(4, {
        name: "Client X",
        entries: [
          {
            pattern: "x.example",
            pattern_kind: "exact",
            effect: "deny",
            note: "",
            enabled: true,
            rule_id: 9,
            expires_at: null,
            precedence: 5,
          },
        ],
      }),
    ];
    setsApi.system = [systemHost("open-vsx.org")];
    api.rules = [
      rule(1, { pattern: "mine.example" }),
      rule(9, { pattern: "x.example", scope: { type: "set", set: 4 } }),
    ];
    await loaded();
    await waitFor(() => expect(ruleSets.status).toBe("ready"));
  }
  const section = (name: string) =>
    screen.getByRole("region", { name }) as HTMLElement;

  it("keeps a set's entries out of your rules and shows them under the set", async () => {
    await withSets();
    const table = screen.getAllByRole("table")[0]!;
    expect(within(table).getByText("mine.example")).toBeInTheDocument();
    expect(within(table).queryByText("x.example")).not.toBeInTheDocument();
    expect(
      screen.getByText("1 rule", { selector: ".count" }),
    ).toBeInTheDocument();
    const sets = section("Rule sets");
    const card = within(sets).getByRole("article", { name: "Client X" });
    expect(within(card).getByText("x.example")).toBeInTheDocument();
    const system = section("System managed");
    expect(within(system).getByText("open-vsx.org")).toBeInTheDocument();
    expect(
      within(system).getByText(/bundled code-server/, { selector: "b" }),
    ).toBeInTheDocument();
  });

  it("asks before turning a set on for every workspace", async () => {
    await withSets();
    const card = within(section("Rule sets")).getByRole("article", {
      name: "GitHub",
    });
    await fireEvent.click(
      within(card).getByRole("switch", { name: "On for every workspace" }),
    );
    const confirm = await screen.findByRole("alertdialog", {
      name: "Turn on for every workspace?",
    });
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Turn on everywhere" }),
    );
    await waitFor(() =>
      expect(setsApi.calls).toContain("PUT /api/rule-sets/{id}/switch"),
    );
    expect(setsApi.bodies.at(-1)).toEqual({ workspace: null, enabled: true });
  });

  it("refuses a rule into a set that was deleted while the form was open", async () => {
    await withSets();
    await fireEvent.click(
      screen.getAllByRole("button", { name: "Add rule" })[0]!,
    );
    const dialog = await screen.findByRole("dialog", { name: "Add a rule" });
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /In rule set Client X/ }),
    );
    setsApi.sets = [];
    await ruleSets.refresh();
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: "y.example" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "That rule set no longer exists.",
    );
    expect(api.calls).not.toContain("POST /api/rules");
  });

  it("adds an entry into a set and deletes one", async () => {
    await withSets();
    const card = within(section("Rule sets")).getByRole("article", {
      name: "Client X",
    });
    await fireEvent.click(
      within(card).getByRole("button", { name: "Add entry" }),
    );
    const dialog = await screen.findByRole("dialog", { name: "Add a rule" });
    expect(
      within(dialog).getByRole("radio", { name: /In rule set Client X/ }),
    ).toBeChecked();
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: "y.example" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    // The set is on for every workspace, so it asks first.
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveTextContent(
      "Allow y.example in rule set Client X, which is on for every workspace",
    );
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Allow in every workspace" }),
    );
    await waitFor(() =>
      expect(api.bodies.at(-1)).toMatchObject({
        pattern: "y.example",
        scope: { type: "set", set: 4 },
      }),
    );
    // Or choose the set in a plain Add rule.
    await fireEvent.click(
      screen.getAllByRole("button", { name: "Add rule" })[0]!,
    );
    const plain = await screen.findByRole("dialog", { name: "Add a rule" });
    await fireEvent.click(
      within(plain).getByRole("radio", { name: /In rule set Client X/ }),
    );
    expect(
      within(plain).getByRole("radio", { name: /In rule set Client X/ }),
    ).toBeChecked();
    await fireEvent.click(
      within(plain).getByRole("button", { name: "Cancel" }),
    );
    await fireEvent.click(
      within(card).getByRole("button", {
        name: "Delete x.example from Client X",
      }),
    );
    await waitFor(() => expect(api.calls).toContain("DELETE /api/rules/{id}"));
  });

  it("moves a rule into a set by editing it, and the set shows it afterwards", async () => {
    await withSets();
    const readsBefore = setsApi.calls.length;
    await choose("Edit rule: allow mine.example for every workspace");
    const dialog = await screen.findByRole("dialog", { name: "Edit rule" });
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /In rule set Client X/ }),
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Save rule" }),
    );
    // The rule was already for every workspace and so is the set: nothing to ask.
    await waitFor(() =>
      expect(api.bodies.at(-1)).toMatchObject({
        pattern: "mine.example",
        scope: { type: "set", set: 4 },
      }),
    );
    expect(api.calls).toContain("PUT /api/rules/{id}");
    await waitFor(() =>
      expect(setsApi.calls.length).toBeGreaterThan(readsBefore),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe(
        "Saved: allow mine.example in a rule set.",
      ),
    );
  });
});

describe("method and path rules", () => {
  const section = () => screen.getByRole("region", { name: "Method and path" });

  it("sits on the rules screen with what a rule does, and an empty state", async () => {
    render(RulesPage);
    const region = await screen.findByRole("region", {
      name: "Method and path",
    });
    expect(region).toHaveTextContent("only refuses");
    expect(region).toHaveTextContent("decrypts the host");
    expect(
      await within(region).findByText(/No rules on method and path yet/),
    ).toBeInTheDocument();
  });

  it("selects the rule the address names, as the Activity screen links to it", async () => {
    Element.prototype.scrollIntoView = vi.fn();
    window.history.replaceState({}, "", "/rules?request-rule=2");
    requestApi.rules = [requestRule(1), requestRule(2)];
    render(RulesPage);
    await waitFor(() => expect(requestRulesStore.status).toBe("ready"));
    const rows = within(section()).getAllByRole("row").slice(1);
    await waitFor(() =>
      expect(rows[1]).toHaveAttribute("aria-current", "true"),
    );
    expect(rows[0]).not.toHaveAttribute("aria-current");
    window.history.replaceState({}, "", "/rules");
  });

  it("opens the new-rule form with the host and workspace the address names, as Activity links to it", async () => {
    workspaces.list = [{ ...fakeWorkspace("shop"), id: "shop" }];
    window.history.replaceState(
      {},
      "",
      "/rules?new-rule=ads.example.net&workspace=shop",
    );
    render(RulesPage);
    const dialog = await screen.findByRole("dialog", { name: "Add a rule" });
    expect(within(dialog).getByLabelText(/^Host/)).toHaveValue(
      "ads.example.net",
    );
    expect(
      within(dialog).getByRole("combobox", { name: "Workspace" }),
    ).toHaveValue("shop");
    window.history.replaceState({}, "", "/rules");
  });

  it("opens the new-rule form with only the host when the address names no workspace", async () => {
    window.history.replaceState({}, "", "/rules?new-rule=ads.example.net");
    render(RulesPage);
    const dialog = await screen.findByRole("dialog", { name: "Add a rule" });
    expect(within(dialog).getByLabelText(/^Host/)).toHaveValue(
      "ads.example.net",
    );
    window.history.replaceState({}, "", "/rules");
  });

  it("ignores a rule named in the address that is not a number", async () => {
    window.history.replaceState({}, "", "/rules?request-rule=abc");
    requestApi.rules = [requestRule(1)];
    render(RulesPage);
    await waitFor(() => expect(requestRulesStore.status).toBe("ready"));
    expect(within(section()).getAllByRole("row")[1]).not.toHaveAttribute(
      "aria-current",
    );
    window.history.replaceState({}, "", "/rules");
  });

  it("lists the rules and deletes one after asking", async () => {
    requestApi.rules = [
      requestRule(1, {
        host: "api.example.com",
        methods: ["DELETE", "PUT"],
        path: "/repos/**",
      }),
    ];
    render(RulesPage);
    await waitFor(() => expect(requestRulesStore.status).toBe("ready"));
    expect(section()).toHaveTextContent("DELETE, PUT");
    await fireEvent.click(
      within(section()).getByRole("button", { name: /^Delete rule: refuse/ }),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Delete rule" }),
    );
    await waitFor(() =>
      expect(requestApi.calls).toContain("DELETE /api/request-rules/{id} 1"),
    );
    expect(
      await within(section()).findByText(/No rules on method and path yet/),
    ).toBeInTheDocument();
  });

  it("switches a rule off and on with a toast", async () => {
    requestApi.rules = [requestRule(1, { host: "api.example.com" })];
    render(RulesPage);
    await waitFor(() => expect(requestRulesStore.status).toBe("ready"));
    const toggle = within(section()).getByRole("switch", {
      name: /^Rule on: refuse/,
    });
    await fireEvent.click(toggle);
    await waitFor(() => expect(toggle).not.toBeChecked());
    expect(requestApi.calls).toContain("PUT /api/request-rules/{id}/enabled 1");
    expect(toasts.items.at(-1)?.message).toMatch(/is off\.$/);
    await fireEvent.click(toggle);
    await waitFor(() => expect(toggle).toBeChecked());
    expect(toasts.items.at(-1)?.message).toMatch(/is on\.$/);
  });

  it("says why when a switch is refused", async () => {
    requestApi.rules = [requestRule(1)];
    render(RulesPage);
    await waitFor(() => expect(requestRulesStore.status).toBe("ready"));
    requestApi.refuse = { status: 500, message: "x" };
    await fireEvent.click(
      within(section()).getByRole("switch", { name: /^Rule on: refuse/ }),
    );
    await waitFor(() => expect(toasts.items.at(-1)?.message).toBe("X."));
  });

  it("reads a rule that was added elsewhere when the event arrives", async () => {
    render(RulesPage);
    await waitFor(() => expect(requestRulesStore.status).toBe("ready"));
    requestApi.rules = [requestRule(2, { host: "late.example.com" })];
    source.emit({ type: "request_rules_changed" });
    await waitFor(() =>
      expect(section()).toHaveTextContent("late.example.com"),
    );
  });
});
