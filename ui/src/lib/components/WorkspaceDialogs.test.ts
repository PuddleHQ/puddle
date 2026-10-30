// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = await vi.hoisted(async () => {
  const ws = await import("#lib/testing/fake-workspaces.ts");
  return {
    api: new ws.FakeWorkspaces(),
    workspace: ws.workspace,
    dirty: ws.dirtyCheck,
  };
});

vi.mock("#lib/stores/workspaces.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/workspaces.svelte.ts")>();
  return {
    ...mod,
    workspaces: new mod.WorkspaceStore({ api: h.api as never }),
  };
});

import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import WorkspaceDialogs from "./WorkspaceDialogs.svelte";

const { api, workspace, dirty } = h;

beforeEach(async () => {
  api.list = [workspace("demo", { status: "running" })];
  api.calls = [];
  api.check = null;
  await workspaces.refresh();
  actions.createOpen = false;
  actions.connectOpen = false;
  actions.trustOpen = false;
  actions.deleteOpen = false;
  actions.renameOpen = false;
  actions.renaming = null;
  actions.deleting = null;
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});
afterEach(cleanup);

describe("WorkspaceDialogs", () => {
  it("draws nothing until an action asks for a dialog", () => {
    render(WorkspaceDialogs);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("opens the create form from the action", async () => {
    render(WorkspaceDialogs);
    actions.openCreate();
    expect(
      await screen.findByRole("dialog", { name: "New workspace" }),
    ).toBeInTheDocument();
  });

  it("closes the create form when Escape is pressed", async () => {
    render(WorkspaceDialogs);
    actions.openCreate();
    const dialog = await screen.findByRole("dialog", { name: "New workspace" });
    await fireEvent.keyDown(dialog, { key: "Escape" });
    await vi.waitFor(() => expect(actions.createOpen).toBe(false));
  });

  it("renames from the dialog: starts from the current name, refuses a clash before asking, then saves and says so", async () => {
    api.list = [
      workspace("demo", { status: "running" }),
      workspace("docs", { display_name: "Handbook" }),
    ];
    await workspaces.refresh();
    render(WorkspaceDialogs);
    actions.openRename(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Rename workspace",
    });
    const field = within(dialog).getByRole("textbox", { name: "Name" });
    expect(field).toHaveValue("demo");
    expect(dialog).toHaveTextContent("Identifier: demo");
    await fireEvent.input(field, { target: { value: "handbook" } });
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      'Another workspace is already called "Handbook".',
    );
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(api.calls).not.toContain("PATCH /api/workspaces/{id}");
    await fireEvent.input(field, { target: { value: "" } });
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Give the workspace a name.",
    );
    await fireEvent.input(field, { target: { value: "x".repeat(61) } });
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Use at most 60 characters.",
    );
    await fireEvent.input(field, { target: { value: "Web shop" } });
    expect(within(dialog).queryByRole("alert")).toBeNull();
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await vi.waitFor(() =>
      expect(api.calls).toContain("PATCH /api/workspaces/{id}"),
    );
    await vi.waitFor(() => expect(actions.renameOpen).toBe(false));
    expect(workspaces.byName("demo")?.display_name).toBe("Web shop");
    expect(toasts.items.map((t) => t.message)).toEqual([
      "Renamed to Web shop.",
    ]);
  });

  it("closes the rename dialog on Cancel without saving", async () => {
    await workspaces.refresh();
    render(WorkspaceDialogs);
    actions.openRename(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Rename workspace",
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() => expect(actions.renameOpen).toBe(false));
    expect(api.calls).not.toContain("PATCH /api/workspaces/{id}");
  });

  it("shows the service's refusal in the dialog and keeps it open", async () => {
    await workspaces.refresh();
    api.refuse.set("PATCH /api/workspaces/{id}", {
      status: 409,
      message: "demo is deleting; rename it when that is done",
    });
    render(WorkspaceDialogs);
    actions.openRename(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Rename workspace",
    });
    await fireEvent.input(
      within(dialog).getByRole("textbox", { name: "Name" }),
      {
        target: { value: "Other" },
      },
    );
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "demo is deleting; rename it when that is done",
    );
    expect(actions.renameOpen).toBe(true);
    // Typing again clears the old refusal.
    await fireEvent.input(
      within(dialog).getByRole("textbox", { name: "Name" }),
      {
        target: { value: "Other 2" },
      },
    );
    expect(within(dialog).queryByRole("alert")).toBeNull();
  });

  it("offers the browser editor on top, and desktop VS Code only once direct SSH is on", async () => {
    render(WorkspaceDialogs);
    actions.connect(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Connect to demo",
    });
    const headings = within(dialog)
      .getAllByRole("heading", { level: 3 })
      .map((h) => h.textContent?.trim());
    expect(headings).toEqual(["In the browser", "On this computer"]);
    expect(
      within(dialog).getByRole("button", { name: "Open in the browser" }),
    ).toBeEnabled();
    expect(
      within(dialog).getByRole("checkbox", { name: "Allow direct SSH" }),
    ).not.toBeChecked();
    expect(
      within(dialog).getByRole("button", { name: "Open in VS Code" }),
    ).toBeDisabled();
    expect(dialog).toHaveTextContent("opens no SSH way in");
    expect(dialog).not.toHaveTextContent("Trusted");
  });

  it("opens the browser editor from the step without direct SSH and closes the step", async () => {
    render(WorkspaceDialogs);
    actions.connect(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Connect to demo",
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Open in the browser" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/attach"),
    );
    expect(api.bodies.at(-1)).toEqual({ mode: "browser" });
    await vi.waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Connect to demo" })).toBeNull(),
    );
  });

  it("says what direct SSH trusts when the switch is ticked, and changes nothing until the yes", async () => {
    render(WorkspaceDialogs);
    actions.connect(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Connect to demo",
    });
    await fireEvent.click(
      within(dialog).getByRole("checkbox", { name: "Allow direct SSH" }),
    );
    const trust = await screen.findByRole("alertdialog", {
      name: "Allow direct SSH to demo?",
    });
    expect(trust).toHaveTextContent("a trusted workspace");
    expect(trust).toHaveTextContent("signed-in GitHub token");
    expect(trust).toHaveTextContent(
      "The browser editor keeps the workspace isolated.",
    );
    expect(api.calls).not.toContain("PUT /api/settings/workspaces/{workspace}");
    await fireEvent.click(
      within(trust).getByRole("button", { name: "Allow direct SSH" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("PUT /api/settings/workspaces/{workspace}"),
    );
    // The step now shows the workspace as trusted and offers VS Code.
    await vi.waitFor(() =>
      expect(
        within(screen.getByRole("dialog")).getByRole("button", {
          name: "Open in VS Code",
        }),
      ).toBeEnabled(),
    );
    expect(screen.getByRole("dialog")).toHaveTextContent("Trusted");
  });

  it("cancelling the trust text leaves direct SSH off", async () => {
    render(WorkspaceDialogs);
    actions.connect(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Connect to demo",
    });
    await fireEvent.click(
      within(dialog).getByRole("checkbox", { name: "Allow direct SSH" }),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() => expect(actions.trustFor).toBeNull());
    expect(api.calls).not.toContain("PUT /api/settings/workspaces/{workspace}");
    expect(workspaces.byName("demo")?.direct_ssh).toBe(false);
  });

  it("turns direct SSH off from the step without asking, and opens desktop VS Code when on", async () => {
    api.list = [workspace("demo", { status: "running", direct_ssh: true })];
    await workspaces.refresh();
    render(WorkspaceDialogs);
    actions.connect(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Connect to demo",
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Open in VS Code" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("POST /api/workspaces/{id}/attach"),
    );
    expect(api.bodies.at(-1)).toEqual({ mode: "desktop" });
  });

  it("switching it off needs no question", async () => {
    api.list = [workspace("demo", { status: "running", direct_ssh: true })];
    await workspaces.refresh();
    render(WorkspaceDialogs);
    actions.connect(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("dialog", {
      name: "Connect to demo",
    });
    await fireEvent.click(
      within(dialog).getByRole("checkbox", { name: "Allow direct SSH" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("PUT /api/settings/workspaces/{workspace}"),
    );
    expect(screen.queryByRole("alertdialog")).toBeNull();
    await vi.waitFor(() =>
      expect(workspaces.byName("demo")?.direct_ssh).toBe(false),
    );
  });

  it("shows the delete check the action read, and deletes after the box is ticked", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    api.check = dirty("demo");
    await workspaces.refresh();
    render(WorkspaceDialogs);
    await actions.askDelete(workspaces.byName("demo")!);
    const dialog = await screen.findByRole("alertdialog", {
      name: "Delete demo?",
    });
    expect(dialog).toHaveTextContent("Uncommitted changes");
    await fireEvent.click(screen.getByRole("checkbox"));
    await fireEvent.click(
      screen.getByRole("button", { name: "Delete workspace" }),
    );
    await vi.waitFor(() =>
      expect(api.calls).toContain("DELETE /api/workspaces/{id}"),
    );
    expect(toasts.items.at(-1)?.message).toBe("Deleting demo.");
  });

  it("keeps the workspace when the user cancels the delete", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    api.check = dirty("demo");
    await workspaces.refresh();
    render(WorkspaceDialogs);
    await actions.askDelete(workspaces.byName("demo")!);
    await screen.findByRole("alertdialog", { name: "Delete demo?" });
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await vi.waitFor(() => expect(actions.deleteOpen).toBe(false));
    expect(api.calls).not.toContain("DELETE /api/workspaces/{id}");
  });
});
