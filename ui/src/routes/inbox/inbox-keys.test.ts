// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, screen, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

// The address of the page is the one in the window's history, as the browser has it.
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(window.location.href);
    },
  },
}));
vi.mock("#lib/stores/rule-sets.svelte.ts", async (original) =>
  (await import("./inbox-harness.ts")).ruleSetsMock(await original()),
);
vi.mock("#lib/stores/ssh-agent-asks.svelte.ts", async (original) =>
  (await import("./inbox-harness.ts")).sshAgentAsksMock(await original()),
);
vi.mock("#lib/stores/pending.svelte.ts", async (original) =>
  (await import("./inbox-harness.ts")).pendingMock(await original()),
);

import {
  FAKE_EFFECTIVE_LIMITS,
  FakeSettings,
} from "#lib/testing/fake-settings.ts";
import {
  INBOX_SHORTCUTS,
  type InboxShortcut,
  type InboxShortcutId,
} from "#lib/decision/inbox-shortcuts.ts";

import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { h } from "./inbox-harness.ts";
import {
  choice,
  moreChoices,
  press,
  ready,
  reset,
  rowOf,
  seed,
  shortcuts,
  installReset,
  showOptions,
  warmUp,
} from "./inbox-setup.ts";

const { inbox } = h;

installReset();
await warmUp();

describe("every workspace needs a confirm", () => {
  async function chooseGlobal(effect: "Allow" | "Deny") {
    seed();
    await ready();
    await fireEvent.click(moreChoices());
    const dialog = await showOptions();
    await fireEvent.click(choice(dialog, /^Every workspace/));
    await fireEvent.click(
      within(dialog).getByRole("button", { name: new RegExp(effect) }),
    );
  }

  it("asks first, names the pattern, and does nothing on Cancel", async () => {
    await chooseGlobal("Allow");
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveAccessibleName("Allow for every workspace?");
    expect(confirm).toHaveTextContent(
      "Allow a.example.com for every workspace, permanently",
    );
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
  });

  it("decides for every workspace only after Confirm", async () => {
    await chooseGlobal("Allow");
    await fireEvent.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Allow in every workspace",
      }),
    );
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(1));
    expect(inbox.rules[0]?.scope).toEqual({ type: "global" });
  });

  it("Ctrl+click on Allow asks first, then decides for every workspace", async () => {
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: /^Allow a\.example\.com for/ }),
      { ctrlKey: true },
    );
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveTextContent(
      "Allow a.example.com for every workspace, permanently",
    );
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Allow in every workspace" }),
    );
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(1));
    expect(inbox.rules[0]?.scope).toEqual({ type: "global" });
  });

  it("Ctrl+click decides at once, still sending the confirmation, when the Settings switch is off", async () => {
    const fake = new FakeSettings();
    fake.ui.confirm_global_rules = false;
    globalSettings.view = {
      effective: {} as never,
      ui: fake.ui,
      unknown_fields: [],
      revision: fake.revision(),
      vscode_server: fake.vscode,
      tools: fake.tools,
      mise_available: true,
      limits: fake.limits,
      effective_limits: FAKE_EFFECTIVE_LIMITS,
      workspace_defaults: fake.layer,
    };
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: /^Deny a\.example\.com for/ }),
      { ctrlKey: true },
    );
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(1));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(inbox.rules[0]).toMatchObject({
      effect: "deny",
      scope: { type: "global" },
    });
  });

  it("words a global deny as a deny", async () => {
    await chooseGlobal("Deny");
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveAccessibleName("Deny for every workspace?");
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Deny in every workspace" }),
    );
    await vi.waitFor(() =>
      expect(inbox.rules[0]).toMatchObject({
        effect: "deny",
        scope: { type: "global" },
      }),
    );
  });
});

describe("every line of the shortcuts list does what it says", () => {
  // One proof per line of the table the list is drawn from: a line without a proof does not
  // compile, and a line whose keys stop doing what it says fails here.
  const proofs: Record<InboxShortcutId, (s: InboxShortcut) => Promise<void>> = {
    allow: async (s) => {
      await press(singleKey(s));
      await vi.waitFor(() =>
        expect(inbox.rules[0]).toMatchObject({
          effect: "allow",
          pattern: "a.example.com",
          scope: { type: "workspace", workspace: "demo" },
        }),
      );
    },
    deny: async (s) => {
      await press(singleKey(s));
      await vi.waitFor(() =>
        expect(inbox.rules[0]).toMatchObject({
          effect: "deny",
          pattern: "a.example.com",
          scope: { type: "workspace", workspace: "demo" },
        }),
      );
    },
    next: async (s) => {
      await press(singleKey(s));
      expect(rowOf("b.example.com")).toHaveAttribute("aria-current", "true");
      expect(rowOf("b.example.com")).toHaveFocus();
    },
    previous: async (s) => {
      await press("j");
      await press("j");
      await press(singleKey(s));
      expect(rowOf("b.example.com")).toHaveAttribute("aria-current", "true");
      expect(rowOf("b.example.com")).toHaveFocus();
    },
    more: async (s) => {
      await press("j");
      await press(s.keys[0] ?? "", rowOf("b.example.com"));
      expect(await showOptions()).toHaveAccessibleName(
        "Choices for b.example.com",
      );
      expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
    },
    everywhere: async (s) => {
      expect(s.keys).toEqual(["Ctrl", "click"]);
      for (const effect of ["Allow", "Deny"] as const) {
        await fireEvent.click(
          screen.getAllByRole("button", {
            name: new RegExp(`^${effect} .* for demo$`),
          })[0]!,
          { ctrlKey: true },
        );
        const confirm = await screen.findByRole("alertdialog");
        await fireEvent.click(
          within(confirm).getByRole("button", {
            name: `${effect} in every workspace`,
          }),
        );
        await vi.waitFor(() =>
          expect(screen.queryByRole("alertdialog")).toBeNull(),
        );
      }
      await vi.waitFor(() => expect(inbox.rules).toHaveLength(2));
      expect(
        inbox.rules.map((r) => [r.effect, r.pattern, r.scope.type]),
      ).toEqual([
        ["allow", "a.example.com", "global"],
        ["deny", "b.example.com", "global"],
      ]);
    },
  };

  /** The letter a single-key line names, as the key event carries it. */
  const singleKey = (s: InboxShortcut) => {
    expect(s.singleKey).toBe(true);
    expect(s.keys).toHaveLength(1);
    return (s.keys[0] ?? "").toLowerCase();
  };

  for (const shortcut of INBOX_SHORTCUTS) {
    it(`${shortcut.keys.join("+")}: ${shortcut.does}`, async () => {
      seed();
      await ready();
      await proofs[shortcut.id](shortcut);
    });
  }
});

describe("the keys by default", () => {
  it("work without turning anything on, before the settings are read and with the switch unset", async () => {
    for (const setting of ["not read", null] as const) {
      cleanup();
      reset();
      if (setting !== "not read") shortcuts(setting);
      seed();
      await ready();
      await press("j");
      expect(rowOf("b.example.com")).toHaveFocus();
      await press("d");
      await vi.waitFor(() =>
        expect(inbox.rules.at(-1)).toMatchObject({
          effect: "deny",
          pattern: "b.example.com",
        }),
      );
    }
  });
});

describe("the keys while the setting is off", () => {
  it("leave A, D, J and K alone, and the shortcuts list says where to turn them on", async () => {
    shortcuts(false);
    seed();
    await ready();
    await press("a");
    await press("d");
    await press("j");
    await press("k");
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
    expect(rowOf("a.example.com")).not.toHaveFocus();
    await fireEvent.click(
      screen.getByRole("button", { name: "Keyboard shortcuts" }),
    );
    const list = await screen.findByRole("dialog", {
      name: "Keyboard shortcuts",
      hidden: true,
    });
    expect(
      [...list.querySelectorAll("[data-shortcut]")].map(
        (line) => (line as HTMLElement).dataset["shortcut"],
      ),
    ).toEqual(INBOX_SHORTCUTS.filter((s) => !s.singleKey).map((s) => s.id));
    expect(
      within(list).getByRole("link", { name: "Turn them on in Settings" }),
    ).toHaveAttribute("href", "/settings/notifications#set-confirm-global");
  });

  it("keep Enter opening the options of the current row", async () => {
    shortcuts(false);
    seed();
    await ready();
    await press("Enter", document.body);
    expect(await showOptions()).toBeInTheDocument();
  });
});
