// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FakeInbox, FakeSource, request } from "#lib/testing/fake-inbox.ts";
import { PendingStore } from "#lib/stores/pending.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import WaitingStrip from "./WaitingStrip.svelte";

const here = vi.hoisted(() => ({ path: "/workspaces" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://x${here.path}`);
    },
  },
}));

afterEach(() => {
  cleanup();
  here.path = "/workspaces";
  workspaces.list = [];
});

function storeWith(open: ReturnType<typeof request>[]) {
  const inbox = new FakeInbox();
  inbox.open = open.map((r) => ({ request: r, domain: "example.com" }));
  const store = new PendingStore({
    api: inbox as never,
    source: new FakeSource(),
    pollMs: 60_000,
  });
  return { inbox, store };
}

describe("WaitingStrip", () => {
  it("says it is quiet when nothing waits", async () => {
    const { store } = storeWith([]);
    render(WaitingStrip, { props: { store } });
    expect(await screen.findByText("Nothing is waiting for you.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Allow/ })).toBeNull();
  });

  it("counts by workspace and answers the newest request with an Allow and Deny pair", async () => {
    workspaces.list = [{ ...fakeWorkspace("Shop"), id: "demo" }];
    const { inbox, store } = storeWith([
      request(1, { host: "api.example.com" }),
      request(2, { host: "cdn.example.com" }),
    ]);
    render(WaitingStrip, { props: { store } });
    const strip = await screen.findByTestId("waiting-strip");
    expect(strip).toHaveTextContent("2 waiting");
    expect(strip).toHaveTextContent("Shop 2");
    expect(strip).toHaveTextContent("cdn.example.com");
    const buttons = screen
      .getAllByRole("button")
      .map((b) => b.textContent?.trim());
    expect(buttons).toEqual(["Allow", "Deny"]);
    await fireEvent.click(screen.getByRole("button", { name: /^Deny cdn/ }));
    await vi.waitFor(() => expect(inbox.calls.join()).toContain("deny"));
  });

  it("allows the newest request, and orders workspaces by count then name", async () => {
    workspaces.list = [
      { ...fakeWorkspace("Beta"), id: "b" },
      { ...fakeWorkspace("Alpha"), id: "a" },
      { ...fakeWorkspace("Gamma"), id: "g" },
    ];
    const { inbox, store } = storeWith([
      request(1, { workspace: "b" as never, first_seen: 5 }),
      request(2, { workspace: "a" as never, first_seen: 5 }),
      request(3, { workspace: "g" as never, first_seen: 5 }),
      request(4, { workspace: "g" as never, first_seen: 5 }),
    ]);
    render(WaitingStrip, { props: { store } });
    await screen.findByTestId("waiting-strip");
    const names = screen
      .getAllByRole("listitem")
      .map((li) => li.textContent?.trim());
    expect(names).toEqual(["Gamma 2", "Alpha 1", "Beta 1"]);
    await fireEvent.click(screen.getByRole("button", { name: /^Allow h4/ }));
    await vi.waitFor(() => expect(inbox.calls.join()).toContain("approve"));
  });

  it("offers only Deny for a blocked request", async () => {
    const { store } = storeWith([
      request(1, { host: "192.168.1.10", blocked_by: "private" }),
    ]);
    render(WaitingStrip, { props: { store } });
    await screen.findByTestId("waiting-strip");
    expect(screen.getAllByRole("button")).toHaveLength(1);
    expect(screen.getByRole("button", { name: /^Deny/ })).toBeTruthy();
  });

  it("only counts on the Inbox, where the list is right below", async () => {
    here.path = "/inbox";
    const { store } = storeWith([request(1)]);
    render(WaitingStrip, { props: { store } });
    const strip = await screen.findByTestId("waiting-strip");
    expect(strip).toHaveTextContent("1 waiting");
    expect(screen.queryByRole("button", { name: /Allow/ })).toBeNull();
  });
});
