// SPDX-License-Identifier: AGPL-3.0-or-later
import { fireEvent, render, screen, within } from "@testing-library/svelte";
import { tick } from "svelte";
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

import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import { pending } from "#lib/stores/pending.svelte.ts";
import { INBOX_SHORTCUTS } from "#lib/decision/inbox-shortcuts.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import InboxPage from "./+page.svelte";

import { h } from "./inbox-harness.ts";
import {
  choice,
  hostEl,
  moreChoices,
  queryHostEl,
  ready,
  reset,
  rowOf,
  seed,
  shrinkSlices,
  installReset,
  showOptions,
  warmUp,
} from "./inbox-setup.ts";

const { inbox, setsApi, source, request, mine } = h;

installReset();
await warmUp();

describe("the inbox page", () => {
  it("shows a loading line, then the groups with their rows", async () => {
    seed();
    render(InboxPage);
    expect(screen.getByText(/Loading requests/)).toBeInTheDocument();
    await screen.findByRole("heading", { level: 2, name: "example.com" });
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    expect(
      screen.getByRole("heading", { level: 2, name: "other.org" }),
    ).toBeInTheDocument();
    expect(screen.getByText("2 waiting")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
    expect(rowOf("a.example.com")).toHaveAttribute("aria-current", "true");
  });

  it("makes the request a notification named current, and focuses it", async () => {
    seed();
    window.history.replaceState(null, "", "/inbox?request=3");
    try {
      render(InboxPage);
      await screen.findByRole("heading", { level: 2, name: "example.com" });
      expect(rowOf("registry.other.org")).toHaveAttribute(
        "aria-current",
        "true",
      );
      await vi.waitFor(() =>
        expect(document.querySelector('[data-request-id="3"]')).toHaveFocus(),
      );
    } finally {
      window.history.replaceState(null, "", "/");
    }
  });

  it("keeps the first request current for a request that is gone or a link that is no id", async () => {
    seed();
    for (const query of ["?request=99", "?request=abc", "?request=-2"]) {
      window.history.replaceState(null, "", `/inbox${query}`);
      const view = render(InboxPage);
      await screen.findByRole("heading", { level: 2, name: "example.com" });
      expect(rowOf("a.example.com")).toHaveAttribute("aria-current", "true");
      view.unmount();
    }
    window.history.replaceState(null, "", "/");
  });

  it("says so when the service didn't answer, and shows an empty state when nothing waits", async () => {
    inbox.down = true;
    const view = render(InboxPage);
    await screen.findByText(
      /Couldn't read the requests\. Puddle's service isn't answering\./,
    );
    view.unmount();
    reset();
    render(InboxPage);
    await screen.findByRole("heading", { name: "All quiet" });
  });

  it("keeps the list but says it may be out of date when a refresh fails", async () => {
    seed();
    await ready();
    inbox.down = true;
    await pending.refresh();
    expect(
      await screen.findByText(/Couldn't refresh the requests/),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("listitem").length).toBeGreaterThan(0);
  });

  it("says a request stays until the user decides, and what the tool is told meanwhile", async () => {
    seed();
    await ready();
    const sub = document.querySelector(".sub");
    expect(sub).toHaveTextContent(/stays here until you decide/);
    expect(sub).toHaveTextContent(/waiting for approval/);
    expect(sub).not.toHaveTextContent(/workspace waits/);
  });

  it("has no keyboard panel under the heading; a button beside it lists the shortcuts, Ctrl+click among them", async () => {
    seed();
    await ready();
    expect(document.querySelector(".head")).not.toHaveTextContent(
      /Tab to a request|more choices|Turn on the A, D, J and K keys/,
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Keyboard shortcuts" }),
    );
    const list = await screen.findByRole("dialog", {
      name: "Keyboard shortcuts",
      hidden: true,
    });
    const lines = [...list.querySelectorAll("[data-shortcut]")].map(
      (line) => line.textContent?.replace(/\s+/g, " ").trim() ?? "",
    );
    expect(lines).toEqual(
      INBOX_SHORTCUTS.map((s) => `${s.keys.join("+")} ${s.does}`),
    );
    expect(lines).toContain(
      "Ctrl+click On Allow or Deny: allow or deny for all workspaces",
    );
    expect(
      within(list).getByRole("link", {
        name: "Turn the single keys off in Settings",
      }),
    ).toHaveAttribute("href", "/settings/notifications#set-confirm-global");
  });

  it("never holds a workspace's requests back: a long list has no held-back line (R-44)", async () => {
    shrinkSlices();
    for (let n = 1; n <= 70; n++) {
      inbox.add(request(n, { host: `h${n}.flood.example.net` }), "example.net");
    }
    render(InboxPage);
    await screen.findByRole("heading", { name: "example.net" });
    expect(document.querySelector(".held")).toBeNull();
    expect(pending.count).toBe(70);
    expect(inbox.calls.some((c) => c.includes("suppression"))).toBe(false);
  });

  it("says in a quiet line how many requests rules allowed today, and says nothing for none", async () => {
    inbox.allowedToday = 214;
    inbox.add(request(1, { host: "a.one.test" }), "one.test");
    const view = render(InboxPage);
    expect(await screen.findByTestId("allowed-today")).toHaveTextContent(
      "214 requests allowed by rules today.",
    );
    view.unmount();
    reset();
    inbox.add(request(1, { host: "a.one.test" }), "one.test");
    render(InboxPage);
    await screen.findByRole("heading", { name: "one.test" });
    expect(screen.queryByTestId("allowed-today")).toBeNull();
  });

  it("says when a workspace's list is full, and stops saying it once some are answered (R-45)", async () => {
    inbox.openLimit = 3;
    inbox.add(request(1, { host: "a.one.test" }), "one.test");
    inbox.add(request(2, { host: "b.one.test" }), "one.test");
    inbox.add(request(3, { host: "c.one.test" }), "one.test");
    inbox.add(request(4, { workspace: "other" as never }), "other.test");
    render(InboxPage);
    await vi.waitFor(() =>
      expect(document.querySelector(".full")).not.toBeNull(),
    );
    const notice = document.querySelector(".full");
    expect(notice).toHaveTextContent(
      "demo has 3 waiting requests, the most Puddle lists.",
    );
    expect(notice).toHaveTextContent("listed once you answer some");
    expect(notice).not.toHaveTextContent("other");
    source.emit({ type: "pending_closed", id: 1, state: "denied", rule_id: 9 });
    await vi.waitFor(() => expect(document.querySelector(".full")).toBeNull());
  });

  it("updates live: a new row, an updated count, a closed row, without a refetch", async () => {
    seed();
    await ready();
    const fetched = inbox.calls.length;
    // A new row also schedules a refetch 150 ms later, which replaces the rows with the service's
    // (no row 9 here). Each step below waits on a Svelte flush, a microtask, so that timer cannot
    // fire between them however long the worker is starved; waiting on the clock instead let it.
    source.emit({
      type: "pending_opened",
      request: request(9, { host: "new.example.com", first_seen: 9_000 }),
      registrable_domain: "example.com",
    });
    await tick();
    expect(queryHostEl("new.example.com")).toBeNull();
    await fireEvent.click(
      screen.getByRole("button", { name: "1 new request" }),
    );
    expect(hostEl("new.example.com")).toBeInTheDocument();
    source.emit({
      type: "pending_updated",
      id: 9,
      attempts: 4,
      last_seen: 9_500,
    });
    await tick();
    expect(rowOf("new.example.com")).toHaveTextContent("4 tries");
    source.emit({
      type: "pending_closed",
      id: 9,
      state: "allowed",
      rule_id: 1,
    });
    await tick();
    expect(screen.queryByText("new.example.com")).toBeNull();
    expect(
      inbox.calls.slice(fetched).filter((c) => c === "GET /api/inbox"),
    ).toEqual([]);
  });

  it("refreshes relative times every 30 seconds", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    vi.setSystemTime(1_000_000 + 60_000);
    inbox.add(
      request(1, { first_seen: 1_000_000, last_seen: 1_000_000 }),
      "example.com",
    );
    render(InboxPage);
    await vi.waitFor(() =>
      expect(screen.getByRole("listitem")).toHaveTextContent("1 minute ago"),
    );
    await vi.advanceTimersByTimeAsync(30_000);
    expect(screen.getByRole("listitem")).toHaveTextContent("1 minute ago");
    await vi.advanceTimersByTimeAsync(120_000);
    expect(screen.getByRole("listitem")).toHaveTextContent("3 minutes ago");
  });
});

describe("deciding with the buttons", () => {
  it("names a workspace by its display name, never its id, in Allow, Deny and Undo", async () => {
    // The request's workspace is the id; the name is what a person chose.
    workspaces.list = [{ ...fakeWorkspace("Shop"), id: "g07jxx5q" }];
    inbox.add(
      request(1, { host: "a.example.com", workspace: "g07jxx5q" }),
      "example.com",
    );
    await ready();
    expect(
      screen.getByRole("button", { name: "Deny a.example.com for Shop" }),
    ).toBeInTheDocument();
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow a.example.com for Shop" }),
    );
    const undo = await screen.findByRole("button", {
      name: /^Undo: Allowed a\.example\.com for workspace Shop, permanently/,
    });
    expect(undo.getAttribute("aria-label")).not.toContain("g07jxx5q");
  });

  it("adds the id only when two workspaces share a name", async () => {
    workspaces.list = [
      { ...fakeWorkspace("Shop"), id: "g07jxx5q" },
      { ...fakeWorkspace("Shop"), id: "h19kyy6r" },
    ];
    inbox.add(
      request(1, { host: "a.example.com", workspace: "g07jxx5q" }),
      "example.com",
    );
    await ready();
    expect(
      screen.getByRole("button", {
        name: "Allow a.example.com for Shop (g07jxx5q)",
      }),
    ).toBeInTheDocument();
  });

  it("allows one host for one workspace with one click, announces it, and closes the row", async () => {
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow a.example.com for demo" }),
    );
    await vi.waitFor(() => expect(queryHostEl("a.example.com")).toBeNull());
    expect(inbox.rules[0]).toMatchObject({
      effect: "allow",
      pattern: "a.example.com",
      scope: { type: "workspace" },
    });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Allowed a.example.com for workspace demo, permanently.",
    );
    expect(
      within(
        screen.getByRole("region", { name: "Decided just now" }),
      ).getByText("allowed"),
    ).toBeInTheDocument();
    expect(pending.count).toBe(2);
  });

  it("denies with one click", async () => {
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: "Deny b.example.com for demo" }),
    );
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(1));
    expect(inbox.rules[0]).toMatchObject({
      effect: "deny",
      pattern: "b.example.com",
    });
  });

  it("undoes from the toast: the rule is deleted and the request is listed again", async () => {
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow a.example.com for demo" }),
    );
    await fireEvent.click(await screen.findByRole("button", { name: "Undo" }));
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(0));
    expect(
      await screen.findByText(/Undone: Allowed a\.example\.com/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("region", { name: "Decided just now" }),
    ).toBeNull();
    expect(
      await screen.findByRole("button", {
        name: "Allow a.example.com for demo",
      }),
    ).toBeInTheDocument();
  });

  it("undoes from the Decided just now list", async () => {
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: "Deny a.example.com for demo" }),
    );
    const undo = await screen.findByRole("button", {
      name: /^Undo: Denied a\.example\.com/,
    });
    await fireEvent.click(undo);
    await vi.waitFor(() => expect(inbox.rules).toHaveLength(0));
  });

  it("says when an undo fails", async () => {
    seed();
    await ready();
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow a.example.com for demo" }),
    );
    vi.spyOn(inbox, "POST").mockResolvedValue({
      response: { status: 500, ok: false },
    } as never);
    await fireEvent.click(
      await screen.findByRole("button", { name: /^Undo: / }),
    );
    expect(
      await screen.findByText("Puddle couldn't undo that."),
    ).toBeInTheDocument();
  });

  it("puts the rule into one of your sets that is on for the workspace", async () => {
    setsApi.sets = [
      mine(4, { name: "Client X" }),
      mine(5, {
        name: "Elsewhere",
        overrides: [{ workspace: "demo" as never, enabled: false }],
      }),
    ];
    inbox.add(request(1, { host: "a.example.com" }), "example.com");
    await ready();
    await vi.waitFor(() =>
      expect(setsApi.calls).toContain("GET /api/rule-sets"),
    );
    await fireEvent.click(moreChoices());
    const dialog = await showOptions();
    expect(within(dialog).queryByLabelText(/Elsewhere/)).toBeNull();
    await fireEvent.click(choice(dialog, /Into rule set Client X/));
    await fireEvent.click(
      within(dialog).getByRole("button", { name: /Allow/ }),
    );
    // On for every workspace, so it asks first.
    const confirm = await screen.findByRole("alertdialog");
    expect(confirm).toHaveTextContent(
      "Allow a.example.com in rule set Client X (which is on for every workspace), permanently",
    );
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Allow in every workspace" }),
    );
    await vi.waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "Allowed a.example.com in rule set Client X",
      ),
    );
  });

  it("mentions the other requests the rule closed", async () => {
    inbox.add(request(1, { host: "a.example.com" }), "example.com");
    inbox.add(request(2, { host: "b.example.com" }), "example.com");
    await ready();
    await fireEvent.click(moreChoices());
    const dialog = await showOptions();
    await fireEvent.click(choice(dialog, /^Everything under/));
    await fireEvent.click(
      within(dialog).getByRole("button", { name: /Allow/ }),
    );
    await vi.waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "also closed 1 other request",
      ),
    );
    expect(
      await screen.findByRole("heading", { name: "All quiet" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/· also closed 1 other/)).toBeInTheDocument();
  });

  it("shows a refusal and refetches when the request was already decided elsewhere", async () => {
    seed();
    await ready();
    inbox.nextDecisionStatus = 409;
    inbox.open = inbox.open.filter((o) => o.request.id !== 1);
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow a.example.com for demo" }),
    );
    expect(
      await screen.findByText(/already decided or has gone away/),
    ).toBeInTheDocument();
    await vi.waitFor(() => expect(queryHostEl("a.example.com")).toBeNull());
  });

  it("shows a refusal from the service as an error toast", async () => {
    seed();
    await ready();
    inbox.nextDecisionStatus = 500;
    await fireEvent.click(
      screen.getByRole("button", { name: "Allow a.example.com for demo" }),
    );
    expect(await screen.findByText("refused")).toBeInTheDocument();
    expect(rowOf("a.example.com")).toBeInTheDocument();
  });
});
