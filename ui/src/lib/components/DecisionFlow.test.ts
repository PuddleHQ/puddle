// SPDX-License-Identifier: GPL-3.0-or-later
// The confirmation before a decision that covers every workspace, and the Settings switch that
// turns it off. The rest of the decision flow is exercised through the inbox page's tests.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = await vi.hoisted(async () => {
  const m = await import("#lib/testing/fake-inbox.ts");
  const sets = await import("#lib/testing/fake-rule-sets.ts");
  return {
    inbox: new m.FakeInbox(),
    setsApi: new sets.FakeRuleSets(),
    source: new m.FakeSource(),
    request: m.request,
  };
});

vi.mock("#lib/stores/rule-sets.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/rule-sets.svelte.ts")>();
  return {
    ...mod,
    ruleSets: new mod.RuleSetsStore({
      api: h.setsApi as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});

vi.mock("#lib/stores/pending.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/pending.svelte.ts")>();
  return {
    ...mod,
    pending: new mod.PendingStore({
      api: h.inbox as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});

import { FakeSettings } from "#lib/testing/fake-settings.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { pending } from "#lib/stores/pending.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import InboxPage from "../../routes/inbox/+page.svelte";

const { inbox, setsApi, request } = h;

beforeEach(() => {
  inbox.open = [];
  inbox.rules = [];
  inbox.calls = [];
  pending.rows = [];
  pending.decided = [];
  pending.status = "loading";
  setsApi.sets = [];
  globalSettings.view = null;
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});
afterEach(cleanup);

/** The settings as the API would send them, with the confirmation switch set (or left unset). */
function settingsWith(confirm: boolean | null) {
  const fake = new FakeSettings();
  fake.ui.confirm_global_rules = confirm;
  globalSettings.view = {
    effective: {} as never,
    ui: fake.ui,
    unknown_fields: [],
    vscode_server: fake.vscode,
    workspace_defaults: fake.layer,
  };
}

async function chooseEveryWorkspace() {
  inbox.add(request(1, { host: "a.example.com" }), "example.com");
  render(InboxPage);
  await screen.findByRole("heading", { name: "example.com" });
  await fireEvent.click(
    screen.getByRole("button", { name: /^More choices for a\.example\.com/ }),
  );
  const dialog = await screen.findByRole("dialog", { hidden: true });
  if (dialog.parentElement) dialog.parentElement.style.visibility = "visible";
  await fireEvent.click(
    within(dialog).getByRole("radio", { name: /^Every workspace/ }),
  );
  await fireEvent.click(within(dialog).getByRole("button", { name: /Allow/ }));
}

describe("confirming a decision for every workspace", () => {
  it("asks when the switch was never set", async () => {
    settingsWith(null);
    await chooseEveryWorkspace();
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    expect(inbox.rules).toHaveLength(0);
  });

  it("asks when the settings have not been read yet", async () => {
    await chooseEveryWorkspace();
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
  });

  it("decides at once, with the confirmation the API needs, when the switch is off", async () => {
    settingsWith(false);
    await chooseEveryWorkspace();
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(1));
    expect(inbox.rules[0]?.scope).toEqual({ type: "global" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });
});
