// SPDX-License-Identifier: AGPL-3.0-or-later
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { tick } from "svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";

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

import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import InboxPage from "./+page.svelte";

import { h } from "./inbox-harness.ts";
import {
  hostEl,
  press,
  queryHostEl,
  ready,
  rowOf,
  seed,
  shortcuts,
  shrinkSlices,
  installReset,
  showOptions,
  warmUp,
} from "./inbox-setup.ts";

const { inbox, source, request, agentApi, ask } = h;

installReset();
await warmUp();

describe("keyboard", () => {
  beforeEach(() => shortcuts(true));
  it("J and K move the current row and its focus, and stop at the ends", async () => {
    seed();
    await ready();
    await press("j");
    expect(rowOf("b.example.com")).toHaveAttribute("aria-current", "true");
    expect(rowOf("b.example.com")).toHaveFocus();
    await press("J");
    await press("j");
    expect(rowOf("registry.other.org")).toHaveFocus();
    await press("k");
    await press("K");
    await press("k");
    expect(rowOf("a.example.com")).toHaveFocus();
  });

  it("A allows and D denies the current row, and focus lands on the next row", async () => {
    seed();
    await ready();
    await press("a");
    await vi.waitFor(() => expect(queryHostEl("a.example.com")).toBeNull());
    expect(inbox.rules[0]).toMatchObject({
      effect: "allow",
      pattern: "a.example.com",
    });
    await vi.waitFor(() => expect(rowOf("b.example.com")).toHaveFocus());
    await press("D");
    await vi.waitFor(() =>
      expect(inbox.rules[1]).toMatchObject({
        effect: "deny",
        pattern: "b.example.com",
      }),
    );
    await vi.waitFor(() => expect(rowOf("registry.other.org")).toHaveFocus());
  });

  it("A and D keep acting on the row that was current when a newer request arrives above it", async () => {
    seed();
    await ready();
    expect(rowOf("a.example.com")).toHaveAttribute("aria-current", "true");
    source.emit({
      type: "pending_opened",
      request: request(99, { host: "evil.example.net", first_seen: 9_000 }),
      registrable_domain: "example.net",
    });
    await tick();
    expect(hostEl("evil.example.net")).toBeTruthy();
    await press("a");
    await vi.waitFor(() =>
      expect(inbox.rules[0]).toMatchObject({
        effect: "allow",
        pattern: "a.example.com",
      }),
    );
    expect(inbox.rules.map((r) => r.pattern)).not.toContain("evil.example.net");
  });

  it("the last decision moves focus to the heading", async () => {
    inbox.add(request(1), "example.com");
    await ready();
    await press("d");
    await vi.waitFor(() =>
      expect(screen.getByRole("heading", { level: 1 })).toHaveFocus(),
    );
  });

  it("Enter on a row opens its options; Escape closes them and returns focus to the trigger", async () => {
    seed();
    await ready();
    rowOf("a.example.com").focus();
    await press("Enter", rowOf("a.example.com"));
    const dialog = await showOptions();
    expect(dialog).toHaveAccessibleName("Choices for a.example.com");
    await fireEvent.keyDown(dialog, { key: "Escape" });
    await vi.waitFor(() =>
      expect(screen.queryByRole("dialog", { hidden: true })).toBeNull(),
    );
  });

  it("Enter opens the options again after they were closed", async () => {
    seed();
    await ready();
    await press("Enter");
    let dialog = await showOptions();
    await fireEvent.keyDown(dialog, { key: "Escape" });
    await vi.waitFor(() =>
      expect(screen.queryByRole("dialog", { hidden: true })).toBeNull(),
    );
    await press("Enter");
    dialog = await showOptions();
    expect(dialog).toHaveAccessibleName("Choices for a.example.com");
  });

  it("Enter on a button keeps its own meaning", async () => {
    seed();
    await ready();
    const allow = screen.getByRole("button", {
      name: "Allow a.example.com for demo",
    });
    allow.focus();
    await press("Enter", allow);
    expect(screen.queryByRole("dialog", { hidden: true })).toBeNull();
  });

  it("ignores keys with a modifier, in form fields, and while a dialog is open", async () => {
    seed();
    await ready();
    await fireEvent.keyDown(document.body, { key: "a", ctrlKey: true });
    await fireEvent.keyDown(document.body, { key: "a", metaKey: true });
    await fireEvent.keyDown(document.body, { key: "a", altKey: true });
    const field = document.createElement("input");
    document.body.append(field);
    await press("a", field);
    field.remove();
    await press("x");
    await press("Enter", document.body); // opens the options of the current row
    await showOptions();
    await press("d");
    await press("j");
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
  });

  it("does nothing without rows", async () => {
    render(InboxPage);
    await screen.findByRole("heading", { name: "All quiet" });
    await press("a");
    await press("j");
    await press("Enter");
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
  });

  it("A decides for this workspace only, never for every workspace", async () => {
    seed();
    await ready();
    await press("a");
    await vi.waitFor(() =>
      expect(inbox.rules[0]?.scope).toEqual({
        type: "workspace",
        workspace: "demo",
      }),
    );
  });
});

describe("local destinations", () => {
  beforeEach(() => shortcuts(true));

  async function local(toggleOn: boolean) {
    inbox.add(
      request(1, {
        host: "192.168.1.10",
        blocked_by: toggleOn ? null : "private",
      }),
      "192.168.1.10",
    );
    render(InboxPage);
    await screen.findByRole("heading", { name: "192.168.1.10" });
  }

  it("names the switched-off toggle, links to it, and offers only Deny", async () => {
    await local(false);
    await vi.waitFor(() =>
      expect(screen.getByText(/can't be approved yet/)).toBeInTheDocument(),
    );
    expect(
      screen.getByRole("link", { name: "Private networks" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Allow/ })).toBeNull();
    expect(screen.getByRole("button", { name: /^Deny/ })).toBeInTheDocument();
  });

  it("A explains instead of allowing, and Enter opens nothing", async () => {
    await local(false);
    await vi.waitFor(() =>
      expect(screen.getByText(/can't be approved yet/)).toBeInTheDocument(),
    );
    await press("a");
    expect(
      await screen.findByText(/Can't allow 192\.168\.1\.10 yet/),
    ).toBeInTheDocument();
    await press("Enter");
    expect(screen.queryByRole("dialog", { hidden: true })).toBeNull();
    expect(inbox.calls.filter((c) => c.startsWith("POST"))).toEqual([]);
  });

  it("D still denies it", async () => {
    await local(false);
    await vi.waitFor(() =>
      expect(screen.getByText(/can't be approved yet/)).toBeInTheDocument(),
    );
    await press("d");
    await vi.waitFor(() =>
      expect(inbox.rules[0]).toMatchObject({
        effect: "deny",
        pattern: "192.168.1.10",
      }),
    );
  });

  it("approves normally once its toggle is on", async () => {
    await local(true);
    await fireEvent.click(
      await screen.findByRole("button", { name: /^Allow/ }),
    );
    await vi.waitFor(() =>
      expect(inbox.rules[0]).toMatchObject({ effect: "allow" }),
    );
  });
});

describe("the page measures itself", () => {
  it("marks when data arrived and, after the rows were rendered, when they were painted", async () => {
    performance.clearMarks();
    seed();
    await ready();
    const painted = () => performance.getEntriesByName("puddle:inbox-painted");
    await vi.waitFor(() => expect(painted().length).toBeGreaterThan(0));
    const data = performance.getEntriesByName("puddle:inbox-data");
    expect(data).toHaveLength(1);
    await vi.waitFor(() =>
      expect(
        performance.getEntriesByName("puddle:inbox-all-painted").length,
      ).toBeGreaterThan(0),
    );
    expect(data[0]?.startTime).toBeLessThanOrEqual(
      painted().at(-1)?.startTime ?? 0,
    );
  });
});

describe("a long list is drawn in slices", () => {
  it("shows the first rows at once, then all of them, and counts every row in the group", async () => {
    shrinkSlices();
    // Hold the frames, so the next slice cannot be drawn between the heading appearing and the
    // first count (under load the frame and its timer fired first and 7 rows were there).
    const frames: FrameRequestCallback[] = [];
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
      frames.push(cb);
      return frames.length;
    });
    const rows = 9;
    for (let i = 1; i <= rows; i += 1) {
      inbox.add(
        request(i, { host: `h${i}.example.com`, first_seen: i }),
        "example.com",
      );
    }
    render(InboxPage);
    await screen.findByRole("heading", { name: "example.com" });
    expect(screen.getByText(`${rows} waiting`)).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
    await vi.waitFor(() => {
      frames.splice(0).forEach((cb) => cb(0));
      expect(screen.getAllByRole("listitem")).toHaveLength(rows);
    });
  });
});

describe("the SSH agent questions", () => {
  it("lists a waiting login with its workspace, host and key, and is not 'all quiet'", async () => {
    workspaces.list = [fakeWorkspace("demo")];
    agentApi.asks = [ask(1, { host: "build.example.com", attempts: 3 })];
    render(InboxPage);
    const region = await screen.findByRole("region", { name: "SSH agent" });
    expect(within(region).getByText("1 waiting")).toBeTruthy();
    expect(region).toHaveTextContent("build.example.com");
    expect(region).toHaveTextContent("ssh-ed25519");
    expect(region).toHaveTextContent("3 attempts");
    expect(screen.queryByText("All quiet")).toBeNull();
  });

  it("answers once, for this run, always or no, and the question goes", async () => {
    workspaces.list = [fakeWorkspace("demo")];
    agentApi.asks = [ask(1), ask(2), ask(3), ask(4)];
    render(InboxPage);
    const region = await screen.findByRole("region", { name: "SSH agent" });
    const press = async (id: number, name: RegExp) => {
      const item = region.querySelector(`[data-ask-id="${id}"]`) as HTMLElement;
      await fireEvent.click(within(item).getByRole("button", { name }));
    };
    await press(1, /^Allow once/);
    await press(2, /^Allow for this run/);
    await press(3, /^Always allow/);
    await press(4, /^Deny/);
    await vi.waitFor(() => expect(agentApi.answers).toHaveLength(4));
    expect(agentApi.answers).toEqual([
      { id: 1, answer: "once" },
      { id: 2, answer: "run" },
      { id: 3, answer: "always" },
      { id: 4, answer: "deny" },
    ]);
    await screen.findByText("All quiet");
  });

  it("says when the answer could not be given and keeps the question", async () => {
    workspaces.list = [fakeWorkspace("demo")];
    agentApi.asks = [ask(1)];
    render(InboxPage);
    const region = await screen.findByRole("region", { name: "SSH agent" });
    agentApi.nextStatus = 500;
    await fireEvent.click(
      within(region).getByRole("button", { name: /^Deny/ }),
    );
    await vi.waitFor(() =>
      expect(toasts.items.map((t) => t.message)).toContain(
        "Puddle couldn't take the answer.",
      ),
    );
    expect(region.querySelector('[data-ask-id="1"]')).not.toBeNull();
  });

  it("shows a question that arrives while the page is open", async () => {
    workspaces.list = [fakeWorkspace("demo")];
    seed();
    await ready();
    expect(screen.queryByRole("region", { name: "SSH agent" })).toBeNull();
    agentApi.asks = [ask(9, { host: "late.example.com" })];
    source.emit({ type: "ssh_agent_asks_changed", workspace: "demo" });
    expect(await screen.findByText("late.example.com")).toBeTruthy();
  });
});
