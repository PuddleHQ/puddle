// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FakeInbox, FakeSource, request } from "#lib/testing/fake-inbox.ts";
import { PendingStore } from "#lib/stores/pending.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import WaitingStrip from "./WaitingStrip.svelte";

vi.mock("$app/state", () => ({
  page: { url: new URL("http://x/workspaces") },
}));

afterEach(() => {
  cleanup();
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
});
