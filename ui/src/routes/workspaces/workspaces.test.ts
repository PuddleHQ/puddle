// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = await vi.hoisted(async () => {
  const inbox = await import("#lib/testing/fake-inbox.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  return {
    inbox: new inbox.FakeInbox(),
    source: new inbox.FakeSource(),
    request: inbox.request,
    api: new ws.FakeWorkspaces(),
    workspace: ws.workspace,
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
vi.mock("#lib/stores/workspaces.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/workspaces.svelte.ts")>();
  return {
    ...mod,
    workspaces: new mod.WorkspaceStore({
      api: h.api as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
});

import { pending } from "#lib/stores/pending.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import Layout from "./+layout.svelte";
import ListPage from "./+page.svelte";

const { inbox, source, api, workspace, request } = h;

function reset() {
  inbox.open = [];
  inbox.calls = [];
  inbox.down = false;
  api.list = [];
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.refuse.clear();
  pending.rows = [];
  pending.status = "loading";
  workspaces.list = [];
  workspaces.status = "loading";
  workspaces.progress = {};
  workspaces.oom = {};
  workspaceActions.createOpen = false;
  workspaceActions.connectOpen = false;
  workspaceActions.renameOpen = false;
  workspaceActions.renaming = null;
  workspaceActions.trustOpen = false;
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
}

beforeEach(reset);
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

async function mount() {
  const view = render(ListPage);
  await workspaces.refresh();
  await pending.refresh();
  return view;
}

describe("the workspace list", () => {
  it("shows a loading line, then a row per workspace with a count", async () => {
    api.list = [
      workspace("web-shop", { status: "running" }),
      workspace("docs"),
    ];
    render(ListPage);
    expect(screen.getByText(/Loading workspaces/)).toBeInTheDocument();
    await workspaces.refresh();
    expect(
      await screen.findByRole("heading", { level: 1, name: "Workspaces" }),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("article")).toHaveLength(2);
    expect(screen.getByText("2 workspaces, 1 running")).toBeInTheDocument();
    expect(
      screen.getByRole("list", { name: "Workspaces" }),
    ).toBeInTheDocument();
  });

  it("says one workspace in the singular", async () => {
    api.list = [workspace("only")];
    await mount();
    expect(
      await screen.findByText("1 workspace, 0 running"),
    ).toBeInTheDocument();
  });

  it("says so when the service didn't answer", async () => {
    api.down = true;
    await mount();
    expect(
      await screen.findByText(
        /Couldn't read the workspaces\. Puddle's service isn't answering\./,
      ),
    ).toBeInTheDocument();
  });

  it("keeps the list but says it may be out of date when a refresh fails", async () => {
    api.list = [workspace("a-shop")];
    await mount();
    await screen.findByText("1 workspace, 0 running");
    api.down = true;
    await workspaces.refresh();
    expect(
      await screen.findByText(/Couldn't refresh the workspaces/),
    ).toBeInTheDocument();
    expect(screen.getByText("1 workspace, 0 running")).toBeInTheDocument();
  });

  it("shows the create form itself when there is none, not a box about it", async () => {
    await mount();
    expect(
      await screen.findByRole("textbox", { name: "Repository address" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("No workspaces yet")).toBeNull();
    expect(screen.queryByRole("button", { name: "New workspace" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "More options" }));
    expect(workspaceActions.createOpen).toBe(true);
  });

  it("creates the first workspace from the address alone, named after the repository", async () => {
    await mount();
    await fireEvent.input(
      await screen.findByRole("textbox", { name: "Repository address" }),
      { target: { value: "https://github.com/acme/ledger.git" } },
    );
    expect(screen.getByText("It will be called ledger.")).toBeInTheDocument();
    await fireEvent.click(
      screen.getByRole("button", { name: "Create workspace" }),
    );
    await vi.waitFor(() => expect(api.bodies).toHaveLength(1));
    expect(api.bodies[0]).toEqual({
      name: "ledger",
      repositories: [{ url: "https://github.com/acme/ledger.git" }],
    });
  });

  it("says why an address cannot be used, at the field", async () => {
    await mount();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Create workspace" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(/address/i);
    expect(api.bodies).toHaveLength(0);
  });

  it("opens the create dialog from the header button", async () => {
    api.list = [workspace("a")];
    await mount();
    await fireEvent.click(
      await screen.findByRole("button", { name: "New workspace" }),
    );
    expect(workspaceActions.createOpen).toBe(true);
  });

  it("starts a workspace from its row", async () => {
    api.list = [workspace("docs")];
    await mount();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Start docs" }),
    );
    await vi.waitFor(() =>
      expect(workspaces.byId("w-docs")?.busy).toBe("starting"),
    );
    expect(screen.getByRole("article")).toHaveTextContent("Starting");
  });

  it("asks to rename or delete a workspace from its row menu", async () => {
    const ask = vi
      .spyOn(workspaceActions, "askDelete")
      .mockResolvedValue(undefined);
    api.list = [workspace("docs")];
    await mount();
    const choose = async (item: string) => {
      await fireEvent.keyDown(
        await screen.findByRole("button", { name: "More about docs" }),
        { key: "Enter" },
      );
      await fireEvent.click(
        await screen.findByRole("menuitem", { name: item }),
      );
    };
    await choose("Rename docs");
    expect(workspaceActions.renameOpen).toBe(true);
    expect(workspaceActions.renaming?.id).toBe("w-docs");
    await choose("Delete docs");
    expect(ask).toHaveBeenCalledWith(expect.objectContaining({ id: "w-docs" }));
    ask.mockRestore();
  });

  it("shows each row's own waiting count and no banner above the list", async () => {
    api.list = [workspace("a"), workspace("b")];
    inbox.add(request(1, { workspace: "w-a" as never }), "example.com");
    inbox.add(request(2, { workspace: "w-b" as never }), "example.com");
    inbox.add(request(3, { workspace: "w-b" as never }), "example.org");
    await mount();
    expect(
      await screen.findByRole("link", { name: "1 waiting" }),
    ).toHaveAttribute("href", "/workspaces/w-a/network");
    expect(screen.getByRole("link", { name: "2 waiting" })).toHaveAttribute(
      "href",
      "/workspaces/w-b/network",
    );
    expect(screen.queryByText(/requests waiting/)).toBeNull();
  });

  it("shows the out-of-memory flag and a failure with a Dismiss", async () => {
    api.list = [workspace("a", { status: "running" })];
    await mount();
    workspaces.handleEvent({
      type: "oom_kill",
      workspace: "w-a",
      pid: 3,
      process: "node",
    });
    expect(await screen.findByText(/Out of memory/)).toBeInTheDocument();
    workspaces.handleEvent({
      type: "workspace_progress",
      workspace: "w-a",
      step: "failed",
      detail: "boom",
    });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Dismiss" }),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("ticks the out-of-memory age every 30 seconds", async () => {
    const start = Date.now();
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
    vi.setSystemTime(start);
    api.list = [workspace("a", { status: "running" })];
    await mount();
    workspaces.handleEvent({
      type: "oom_kill",
      workspace: "w-a",
      pid: 3,
      process: "node",
    });
    expect(
      await screen.findByText(/Out of memory just now/),
    ).toBeInTheDocument();
    vi.setSystemTime(start + 2 * 60_000);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(screen.getByText(/Out of memory 2 minutes ago/)).toBeInTheDocument();
  });
});

describe("the workspaces layout", () => {
  it("listens for the waiting requests of the section, shows the dialogs and toasts, and stops on leaving", async () => {
    api.list = [workspace("a", { status: "running" })];
    const start = vi.spyOn(workspaces, "start");
    const startPending = vi.spyOn(pending, "start");
    const view = render(Layout, { children: (() => {}) as never });
    // The shell reads the list (the sidebar counts it); this section adds the waiting requests.
    expect(start).not.toHaveBeenCalled();
    const stopList = workspaces.start();
    expect(startPending).toHaveBeenCalledOnce();
    expect(workspaces.onSettled).toBe(workspaceActions.settled);
    expect(
      screen.getByRole("region", { name: "Notifications" }),
    ).toBeInTheDocument();
    // The end of an operation reaches the user as a toast.
    await workspaces.refresh();
    source.emit({
      type: "workspace_progress",
      workspace: "w-a",
      step: "stopping",
      detail: null,
    });
    source.emit({
      type: "workspace_progress",
      workspace: "w-a",
      step: "done",
      detail: null,
    });
    await vi.waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("Stopped a."),
    );
    view.unmount();
    stopList();
    expect(workspaces.onSettled).toBeNull();
    start.mockRestore();
    startPending.mockRestore();
  });
});
