// SPDX-License-Identifier: AGPL-3.0-or-later
// A guest chooses when a request arrives, and Allow is one unconfirmed click: nothing that
// arrives while the page is open may move a row the user has already been shown.
import { fireEvent, render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import { describe, expect, it, vi } from "vitest";

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

import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import { h } from "./inbox-harness.ts";
import { installReset, ready, seed, warmUp } from "./inbox-setup.ts";

const { agentApi, ask, inbox, request, source } = h;

installReset();
await warmUp();

/** Every host drawn in a request row, in document order. */
const drawnHosts = () =>
  [...document.querySelectorAll("li.req .host")].map((el) =>
    el.textContent?.replace(/:\d+$/, ""),
  );

/** What sits above the first request row in the document: anything here pushes the rows down. */
function aboveFirstRow(): Element[] {
  const first = document.querySelector("li.req");
  return [
    ...document.querySelectorAll(".full, [aria-label='SSH agent'], section"),
  ]
    .filter((el) => !el.contains(first))
    .filter(
      (el) =>
        el.compareDocumentPosition(first as Node) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    );
}

describe("what arrives while the page is open", () => {
  it("holds a new request behind a line, and draws it once the user asks", async () => {
    seed();
    await ready();
    const before = drawnHosts();
    source.emit({
      type: "pending_opened",
      request: request(9, { host: "evil.example.com", first_seen: 9_000 }),
      registrable_domain: "example.com",
    });
    await tick();
    expect(drawnHosts()).toEqual(before);
    const line = screen.getByRole("button", { name: "1 new request" });
    await fireEvent.click(line);
    await tick();
    expect(drawnHosts()).toEqual(["evil.example.com", ...before]);
    expect(screen.queryByRole("button", { name: /new request/ })).toBeNull();
  });

  it("counts several arrivals, including one the service lists on its next read", async () => {
    seed();
    await ready();
    inbox.add(
      request(8, { host: "one.new.test", first_seen: 8_000 }),
      "new.test",
    );
    source.emit({
      type: "pending_opened",
      request: request(8, { host: "one.new.test", first_seen: 8_000 }),
      registrable_domain: "new.test",
    });
    inbox.add(
      request(9, { host: "two.new.test", first_seen: 9_000 }),
      "new.test",
    );
    source.emit({ type: "pending_opened", request: request(9) });
    await vi.waitFor(() =>
      expect(
        screen.getByRole("button", { name: "2 new requests" }),
      ).toBeTruthy(),
    );
    expect(drawnHosts()).not.toContain("two.new.test");
  });

  it("draws the first request at once when nothing is drawn yet", async () => {
    render((await import("./+page.svelte")).default);
    await screen.findByText("All quiet");
    source.emit({
      type: "pending_opened",
      request: request(9, { host: "first.new.test" }),
      registrable_domain: "new.test",
    });
    await tick();
    expect(drawnHosts()).toEqual(["first.new.test"]);
    expect(screen.queryByRole("button", { name: /new request/ })).toBeNull();
  });

  it("puts an SSH agent question that arrives below the rows", async () => {
    workspaces.list = [fakeWorkspace("demo")];
    seed();
    await ready();
    agentApi.asks = [ask(9, { host: "late.example.com" })];
    source.emit({ type: "ssh_agent_asks_changed", workspace: "demo" });
    await screen.findByText("late.example.com");
    expect(aboveFirstRow()).toEqual([]);
  });

  it("puts the full-list notice below the rows", async () => {
    inbox.openLimit = 2;
    inbox.add(request(1, { host: "a.one.test" }), "one.test");
    render((await import("./+page.svelte")).default);
    await screen.findByRole("heading", { name: "one.test" });
    expect(document.querySelector(".full")).toBeNull();
    inbox.add(request(2, { host: "b.one.test" }), "one.test");
    source.emit({
      type: "pending_opened",
      request: request(2, { host: "b.one.test" }),
      registrable_domain: "one.test",
    });
    await vi.waitFor(() =>
      expect(document.querySelector(".full")).not.toBeNull(),
    );
    expect(aboveFirstRow()).toEqual([]);
  });
});
