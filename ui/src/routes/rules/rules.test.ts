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

import { rulesStore } from "#lib/stores/rules.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import RulesPage from "./+page.svelte";
import { expectNoIdsShown } from "#lib/testing/no-ids.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import { chooseWorkspace } from "#lib/testing/choose-workspace.ts";
import { h } from "./rules-harness.ts";
import {
  choose,
  hosts,
  installReset,
  loaded,
  rows,
  seed,
  warmUp,
} from "./rules-setup.ts";

const { api, setsApi, source, rule, builtIn } = h;

installReset();
await warmUp();

describe("workspace ids", () => {
  it("stay behind the names in the table, the filter and the add form", async () => {
    workspaces.list = [{ ...fakeWorkspace("Shop"), id: "g07jxx5q" }];
    api.rules = [
      rule(1, {
        pattern: "a.example.com",
        scope: { type: "workspace", workspace: "g07jxx5q" as never },
      }),
    ];
    setsApi.sets = [
      builtIn("github", {
        overrides: [{ workspace: "g07jxx5q" as never, enabled: true }],
      }),
    ];
    await loaded();
    await fireEvent.click(screen.getByRole("button", { name: /Add rule/ }));
    await screen.findByRole("dialog");
    // The filter and the form read "Shop" and carry the id as the value; the rule set switch reads it too.
    const shop = screen.getAllByRole("option", { name: "Shop", hidden: true });
    expect(shop.length).toBeGreaterThanOrEqual(2);
    expect(
      screen.getByRole("group", { name: "github in Shop" }),
    ).toBeInTheDocument();
    for (const option of shop) expect(option).toHaveValue("g07jxx5q");
    expect(within(rows()[0]!).getByText("Shop")).toBeInTheDocument();
    expectNoIdsShown(document.body, ["g07jxx5q"]);
    workspaces.list = [];
  });
  it("show an example host in the host filter", async () => {
    seed();
    await loaded();
    expect(screen.getByLabelText("Host contains")).toHaveAttribute(
      "placeholder",
      expect.stringContaining("github.com"),
    );
  });
});

describe("the list", () => {
  it("says it is loading, then shows an empty state with a way forward", async () => {
    render(RulesPage);
    expect(screen.getByText(/loading rules/i)).toBeInTheDocument();
    expect(await screen.findByText("No rules yet")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Inbox" })).toHaveAttribute(
      "href",
      "/inbox",
    );
    expect(screen.queryByRole("search")).not.toBeInTheDocument();
  });
  it("says so when the service can't be read", async () => {
    api.down = true;
    render(RulesPage);
    expect(
      await screen.findByText(
        /Couldn't read the rules\. Puddle's service isn't answering\./,
      ),
    ).toBeInTheDocument();
  });
  it("keeps the table but says it may be out of date when a refresh fails", async () => {
    seed();
    await loaded();
    api.down = true;
    await rulesStore.refresh();
    expect(
      await screen.findByText(/Couldn't refresh the rules/),
    ).toBeInTheDocument();
    expect(hosts().length).toBeGreaterThan(0);
  });
  it("shows each rule's host, effect, workspace and expiry, newest first", async () => {
    seed();
    await loaded();
    expect(hosts()).toEqual([
      "old.example.com",
      "*.github.com",
      "registry.npmjs.org",
    ]);
    const old = rows()[0]!;
    expect(within(old).getByText("Expired")).toBeInTheDocument();
    expect(within(old).queryByText("no longer applies")).toBeNull();
    expect(within(old).queryByRole("switch")).toBeNull();
    expect(within(rows()[1]!).getByText("Deny")).toBeInTheDocument();
    expect(within(rows()[1]!).getByText("shop")).toBeInTheDocument();
    expect(within(rows()[2]!).getByText("Every workspace")).toBeInTheDocument();
    expect(within(rows()[2]!).getByText("Never")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { level: 1, name: "Rules" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/most specific rule wins/i)).toBeInTheDocument();
  });
  it("sorts by a column and flips on a second click", async () => {
    seed();
    await loaded();
    await fireEvent.click(screen.getByRole("button", { name: "Sort by host" }));
    expect(hosts()[0]).toBe("*.github.com");
    expect(screen.getByRole("columnheader", { name: /Host/ })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );
    await fireEvent.click(screen.getByRole("button", { name: "Sort by host" }));
    expect(screen.getByRole("columnheader", { name: /Host/ })).toHaveAttribute(
      "aria-sort",
      "descending",
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Sort by expires" }),
    );
    expect(
      screen.getByRole("columnheader", { name: /Expires/ }),
    ).toHaveAttribute("aria-sort", "descending");
  });
});

describe("filters", () => {
  it("narrows by host, workspace, effect and state, counts, and clears", async () => {
    seed();
    await loaded();
    expect(
      screen.getByText(/rules?\b/, { selector: ".count" }),
    ).toHaveTextContent("3 rules");
    await fireEvent.input(screen.getByLabelText("Host contains"), {
      target: { value: "GITHUB" },
    });
    expect(hosts()).toHaveLength(1);
    expect(
      screen.getByText(/rules?\b/, { selector: ".count" }),
    ).toHaveTextContent("1 of 3 rules");
    await fireEvent.click(
      screen.getByRole("button", { name: "Clear filters" }),
    );
    expect(rows()).toHaveLength(3);
    await fireEvent.change(screen.getByLabelText("Workspace"), {
      target: { value: "global" },
    });
    expect(rows()).toHaveLength(2);
    await fireEvent.change(screen.getByLabelText("Workspace"), {
      target: { value: "shop" },
    });
    expect(rows()).toHaveLength(1);
    await fireEvent.change(screen.getByLabelText("Workspace"), {
      target: { value: "any" },
    });
    await fireEvent.change(screen.getByLabelText("Effect"), {
      target: { value: "allow" },
    });
    expect(rows()).toHaveLength(2);
    await fireEvent.change(screen.getByLabelText("State"), {
      target: { value: "expired" },
    });
    expect(hosts()).toEqual(["old.example.com"]);
  });
  it("says when nothing matches", async () => {
    seed();
    await loaded();
    await fireEvent.input(screen.getByLabelText("Host contains"), {
      target: { value: "zzz" },
    });
    expect(screen.getByText("No rule matches")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });
});

describe("adding", () => {
  async function open() {
    await loaded();
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    return screen.findByRole("dialog", { name: "Add a rule" });
  }
  const fill = async (
    dialog: HTMLElement,
    host: string,
    workspace = "shop",
  ) => {
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: host },
    });
    if (workspace)
      await chooseWorkspace(
        within(dialog).getByLabelText("Workspace"),
        workspace,
      );
  };

  it("sends what is chosen last when the user changes their mind about effect and who", async () => {
    seed();
    const dialog = await open();
    await fill(dialog, "changed.example.org");
    await fireEvent.click(within(dialog).getByLabelText("Deny"));
    await fireEvent.click(within(dialog).getByLabelText("Allow"));
    await fireEvent.click(within(dialog).getByLabelText(/Every workspace/));
    await fireEvent.click(within(dialog).getByLabelText("One workspace"));
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    await waitFor(() => expect(api.bodies).toHaveLength(1));
    expect(api.bodies[0]).toMatchObject({
      pattern: "changed.example.org",
      effect: "allow",
      scope: { type: "workspace", workspace: "shop" },
    });
  });

  it("adds a workspace rule with an expiry and says so", async () => {
    seed();
    const dialog = await open();
    await fill(dialog, "  *.example.org ");
    await fireEvent.click(within(dialog).getByLabelText("Deny"));
    await fireEvent.change(within(dialog).getByLabelText("Expires"), {
      target: { value: "3600" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    const body = api.bodies[0] as {
      expires_at: number;
      scope: unknown;
      pattern: string;
      effect: string;
    };
    expect(body).toMatchObject({
      pattern: "*.example.org",
      effect: "deny",
      scope: { type: "workspace", workspace: "shop" },
    });
    expect(body.expires_at).toBeGreaterThan(Date.now());
    expect(toasts.items[0]?.message).toBe(
      "Added: deny *.example.org for workspace shop.",
    );
    expect(hosts()).toContain("*.example.org");
  });
  it("checks the form before asking the server", async () => {
    const dialog = await open();
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    expect(
      within(dialog)
        .getAllByRole("alert")
        .map((a) => a.textContent),
    ).toEqual([
      expect.stringMatching(/enter a host/i),
      expect.stringMatching(/choose the workspace/i),
    ]);
    expect(within(dialog).getByLabelText("Host")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
    expect(api.bodies).toHaveLength(0);
    // The dropdown holds only real workspaces, so once one is chosen the form is complete.
    await fill(dialog, "a.example.com");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
  });
  it("shows the server's refusal under the host and keeps what was typed", async () => {
    const dialog = await open();
    api.refuse = { status: 422, message: "`.com` is a public suffix" };
    await fill(dialog, "*.com");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("`.com` is a public suffix.");
    const host = within(dialog).getByLabelText("Host");
    expect(host).toHaveAttribute("aria-invalid", "true");
    expect(host.getAttribute("aria-describedby")).toContain(alert.id);
    expect(host).toHaveValue("*.com");
  });
  it("shows other failures for the whole form", async () => {
    const dialog = await open();
    api.down = true;
    await fill(dialog, "a.example.com");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      /isn't answering/,
    );
  });
  it("asks before a rule for every workspace, and Cancel returns to the form", async () => {
    const dialog = await open();
    await fireEvent.click(within(dialog).getByLabelText(/Every workspace/));
    expect(
      within(dialog).queryByLabelText("Workspace"),
    ).not.toBeInTheDocument();
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: "cdn.example.com" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    const confirm = await screen.findByRole("alertdialog", {
      name: "Allow for every workspace?",
    });
    expect(api.bodies).toHaveLength(0);
    expect(confirm).toHaveTextContent(
      "Allow cdn.example.com for every workspace",
    );
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Cancel" }),
    );
    const again = await screen.findByRole("dialog", { name: "Add a rule" });
    expect(within(again).getByLabelText("Host")).toHaveValue("cdn.example.com");
    await fireEvent.click(
      within(again).getByRole("button", { name: "Add rule" }),
    );
    const second = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(second).getByRole("button", { name: "Allow in every workspace" }),
    );
    await waitFor(() => expect(api.bodies).toHaveLength(1));
    expect(api.bodies[0]).toMatchObject({ scope: { type: "global" } });
    await waitFor(() => expect(toasts.items.length).toBeGreaterThan(0));
  });
  it("brings the form back with the refusal when the confirmed rule is refused", async () => {
    const dialog = await open();
    await fireEvent.click(within(dialog).getByLabelText(/Every workspace/));
    await fireEvent.click(within(dialog).getByLabelText("Deny"));
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: "*.co.uk" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    const confirm = await screen.findByRole("alertdialog", {
      name: "Deny for every workspace?",
    });
    api.refuse = { status: 422, message: "co.uk is a public suffix" };
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Deny in every workspace" }),
    );
    const again = await screen.findByRole("dialog", { name: "Add a rule" });
    expect(await within(again).findByRole("alert")).toHaveTextContent(
      "Co.uk is a public suffix.",
    );
  });
  it("starts empty the next time, and Cancel discards", async () => {
    const dialog = await open();
    await fill(dialog, "typed.example.com");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(
      within(await screen.findByRole("dialog")).getByLabelText("Host"),
    ).toHaveValue("");
  });
});

describe("editing a rule in place", () => {
  async function open(name: RegExp) {
    if (!screen.queryByRole("table")) await loaded();
    await choose(name);
    return screen.findByRole("dialog", { name: "Edit rule" });
  }
  const save = (dialog: HTMLElement) =>
    fireEvent.click(within(dialog).getByRole("button", { name: "Save rule" }));

  it("opens the add form filled with the rule, and saves the changes over the same rule", async () => {
    seed();
    const dialog = await open(
      /Edit rule: deny \*\.github\.com for workspace shop/,
    );
    expect(within(dialog).getByLabelText("Host")).toHaveValue("*.github.com");
    expect(within(dialog).getByLabelText("Deny")).toBeChecked();
    expect(within(dialog).getByLabelText("One workspace")).toBeChecked();
    expect(
      within(dialog).getByRole("combobox", { name: "Workspace" }),
    ).toHaveValue("shop");
    expect(within(dialog).getByLabelText("Expires")).toHaveDisplayValue(
      /^Unchanged/,
    );
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: "api.github.com" },
    });
    await fireEvent.click(within(dialog).getByLabelText("Allow"));
    await save(dialog);
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(api.calls).toContain("PUT /api/rules/{id}");
    expect(api.calls).not.toContain("POST /api/rules");
    expect(api.bodies).toEqual([
      {
        pattern: "api.github.com",
        effect: "allow",
        scope: { type: "workspace", workspace: "shop" },
        expires_at: null,
      },
    ]);
    expect(toasts.items.at(-1)?.message).toBe(
      "Saved: allow api.github.com for workspace shop.",
    );
    expect(hosts()).toContain("api.github.com");
    expect(hosts()).not.toContain("*.github.com");
    expect(rows()).toHaveLength(3);
  });

  it("keeps the expiry unless another is chosen, and can make the rule permanent", async () => {
    seed();
    const later = Date.now() + 3_600_000;
    api.rules[0]!.expires_at = later;
    const dialog = await open(/Edit rule: allow registry/);
    await save(dialog);
    await waitFor(() => expect(api.bodies).toHaveLength(1));
    expect(api.bodies[0]).toMatchObject({ expires_at: later });
    const again = await open(/Edit rule: allow registry/);
    await fireEvent.change(within(again).getByLabelText("Expires"), {
      target: { value: "0" },
    });
    await save(again);
    await waitFor(() => expect(api.bodies).toHaveLength(2));
    expect(api.bodies[1]).toMatchObject({ expires_at: null });
  });

  it("shows the server's refusal under the host and keeps what was typed", async () => {
    seed();
    const dialog = await open(/Edit rule: allow registry/);
    await fireEvent.input(within(dialog).getByLabelText("Host"), {
      target: { value: "*.com" },
    });
    api.refuse = { status: 422, message: "pattern is a public suffix" };
    await save(dialog);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Pattern is a public suffix.",
    );
    expect(within(dialog).getByLabelText("Host")).toHaveValue("*.com");
    expect(rulesStore.rules.find((r) => r.id === 1)?.pattern).toBe(
      "registry.npmjs.org",
    );
  });

  it("asks before it widens a rule to every workspace, and not for one that already is", async () => {
    seed();
    const dialog = await open(
      /Edit rule: deny \*\.github\.com for workspace shop/,
    );
    await fireEvent.click(within(dialog).getByLabelText(/Every workspace/));
    await save(dialog);
    const confirm = await screen.findByRole("alertdialog", {
      name: "Deny for every workspace?",
    });
    expect(api.bodies).toHaveLength(0);
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Cancel" }),
    );
    const back = await screen.findByRole("dialog", { name: "Edit rule" });
    expect(within(back).getByLabelText("Host")).toHaveValue("*.github.com");
    await save(back);
    const second = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(second).getByRole("button", { name: "Deny in every workspace" }),
    );
    await waitFor(() => expect(api.bodies).toHaveLength(1));
    expect(api.bodies[0]).toMatchObject({ scope: { type: "global" } });
    expect(api.calls).toContain("PUT /api/rules/{id}");
    // Rule 1 is already for every workspace: saving it asks nothing.
    const global = await open(/Edit rule: allow registry/);
    await save(global);
    await waitFor(() => expect(api.bodies).toHaveLength(2));
    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
  });

  it("starts the add form empty after an edit, and Cancel discards the edit", async () => {
    seed();
    const dialog = await open(/Edit rule: allow registry/);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(api.bodies).toHaveLength(0);
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    const add = await screen.findByRole("dialog", { name: "Add a rule" });
    expect(within(add).getByLabelText("Host")).toHaveValue("");
    expect(within(add).getByRole("button", { name: "Add rule" })).toBeVisible();
  });
});

describe("changing the expiry", () => {
  async function open(name: RegExp) {
    await loaded();
    await choose(name);
    return screen.findByRole("dialog", { name: "Change expiry" });
  }
  it("makes a rule permanent, or ends it later", async () => {
    seed();
    api.rules[0]!.expires_at = Date.now() + 3_600_000;
    const dialog = await open(/Change expiry: allow registry/);
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(within(dialog).getByRole("alert")).toHaveTextContent(/choose when/i);
    await fireEvent.change(within(dialog).getByLabelText("Ends"), {
      target: { value: "0" },
    });
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(api.bodies[0]).toEqual({ expires_at: null });
    expect(toasts.items[0]?.message).toBe(
      "registry.npmjs.org never expires now.",
    );
    expect(
      within(
        rows().find((r) => r.textContent?.includes("registry"))!,
      ).getByText("Never"),
    ).toBeInTheDocument();
  });
  it("sets a new time, and shows the server's refusal", async () => {
    seed();
    const dialog = await open(/Change expiry: deny \*\.github/);
    expect(dialog).toHaveTextContent(/never expires now/i);
    api.refuse = { status: 422, message: "expiry must be in the future" };
    await fireEvent.change(within(dialog).getByLabelText("Ends"), {
      target: { value: "3600" },
    });
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Expiry must be in the future.",
    );
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(toasts.items.at(-1)?.message).toBe(
      "Changed the expiry of *.github.com.",
    );
  });
  it("Cancel closes it without a call", async () => {
    seed();
    const dialog = await open(/Change expiry: allow registry/);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(api.bodies).toHaveLength(0);
  });
});

describe("switching a rule", () => {
  it("switches a rule off and on again with a toast, and the row shows it", async () => {
    seed();
    await loaded();
    const name = "Rule on: allow registry.npmjs.org for every workspace";
    const toggle = screen.getByRole("switch", { name });
    expect(toggle).toBeChecked();
    await fireEvent.click(toggle);
    await waitFor(() => expect(toggle).not.toBeChecked());
    expect(api.calls).toContain("PUT /api/rules/{id}/enabled");
    expect(api.bodies.at(-1)).toEqual({ enabled: false });
    expect(toasts.items.at(-1)?.message).toBe("registry.npmjs.org is off.");
    expect(toggle.closest("tr")).toHaveTextContent("off");
    await fireEvent.click(toggle);
    await waitFor(() => expect(toggle).toBeChecked());
    expect(toasts.items.at(-1)?.message).toBe("registry.npmjs.org is on.");
  });
  it("says why when Puddle refuses, and leaves the switch as it was", async () => {
    seed();
    await loaded();
    api.refuse = { status: 500, message: "could not save" };
    const toggle = screen.getByRole("switch", {
      name: "Rule on: allow registry.npmjs.org for every workspace",
    });
    await fireEvent.click(toggle);
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("Could not save."),
    );
    expect(toggle).toBeChecked();
  });
});

describe("deleting", () => {
  it("asks first, names the rule, and Cancel keeps it", async () => {
    seed();
    await loaded();
    await choose("Delete rule: allow registry.npmjs.org for every workspace");
    const dialog = await screen.findByRole("alertdialog", {
      name: "Delete this rule?",
    });
    expect(dialog).toHaveTextContent(
      "allow registry.npmjs.org for every workspace",
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(rows()).toHaveLength(3);
    expect(api.calls.some((c) => c.startsWith("DELETE"))).toBe(false);
  });
  it("deletes on confirm, and puts focus on the next row", async () => {
    seed();
    await loaded();
    await choose(/Delete rule: allow old.example.com/);
    const dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete rule" }),
    );
    await waitFor(() => expect(rows()).toHaveLength(2));
    expect(toasts.items[0]?.message).toBe(
      "Deleted: allow old.example.com for every workspace.",
    );
    await waitFor(() =>
      expect(document.activeElement?.closest("tr")).toBe(rows()[0]),
    );
  });
  it("reports a failure and keeps the rule", async () => {
    seed();
    await loaded();
    api.refuse = { status: 500, message: "x" };
    await choose(/Delete rule: allow old.example.com/);
    await fireEvent.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Delete rule",
      }),
    );
    await waitFor(() => expect(toasts.items[0]?.tone).toBe("error"));
    expect(rows()).toHaveLength(3);
  });
  it("focuses the heading when the last rule goes", async () => {
    api.rules = [rule(1)];
    await loaded();
    await choose(/Delete rule/);
    await fireEvent.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Delete rule",
      }),
    );
    await screen.findByText("No rules yet");
    await waitFor(() =>
      expect(screen.getByRole("heading", { level: 1 })).toHaveFocus(),
    );
  });
});

describe("live", () => {
  it("shows a rule another client added when rules_changed arrives", async () => {
    seed();
    await loaded();
    api.rules = [
      ...api.rules,
      rule(9, { pattern: "late.example.com", created_at: 999 }),
    ];
    source.emit({ type: "rules_changed" });
    await waitFor(() => expect(hosts()[0]).toBe("late.example.com"));
  });
  it("shows an expiry that passes without a reload, at the next 30 s tick", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    api.rules = [rule(1, { expires_at: Date.now() + 10_000 })];
    try {
      render(RulesPage);
      await vi.waitFor(() =>
        expect(screen.getByRole("table")).toBeInTheDocument(),
      );
      const table = within(screen.getByRole("table"));
      expect(table.queryByText("Expired")).not.toBeInTheDocument();
      await vi.advanceTimersByTimeAsync(30_000);
      expect(table.getByText("Expired")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});
