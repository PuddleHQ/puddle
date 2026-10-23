// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = await vi.hoisted(async () => {
  const ws = await import("#lib/testing/fake-workspaces.ts");
  const inbox = await import("#lib/testing/fake-inbox.ts");
  return { api: new ws.FakeWorkspaces(), source: new inbox.FakeSource() };
});

vi.mock("#lib/api/client.ts", () => ({ api: h.api }));
vi.mock("#lib/stores/live.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/live.svelte.ts")>();
  return { ...mod, live: h.source };
});
vi.mock("#lib/stores/workspaces.svelte.ts", async (original) => {
  const mod =
    await original<typeof import("#lib/stores/workspaces.svelte.ts")>();
  return {
    ...mod,
    workspaces: new mod.WorkspaceStore({ api: h.api as never, pollMs: 60_000 }),
  };
});

import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import StartFolderSetting from "./StartFolderSetting.svelte";

const { api } = h;

beforeEach(() => {
  api.list = [];
  api.calls = [];
  api.bodies = [];
  api.problems = [];
  api.down = false;
  api.refuse.clear();
});
afterEach(cleanup);

function mount(over: Partial<Workspace> = {}) {
  const ws = workspace("dev", { status: "running", ...over });
  api.list = [ws];
  workspaces.list = [ws];
  render(StartFolderSetting, { workspace: ws });
}

const field = () =>
  screen.getByRole("textbox", { name: "Start folder" }) as HTMLInputElement;
const type = (value: string) => fireEvent.input(field(), { target: { value } });
const save = () =>
  fireEvent.click(screen.getByRole("button", { name: "Save" }));

describe("the start folder setting", () => {
  it("starts from the stored value and says where terminals land", () => {
    mount({ start_folder: "api/services/web" });
    expect(field()).toHaveValue("api/services/web");
    expect(
      screen.getByText(/land in \/workspaces\/dev\/api\/services\/web/),
    ).toBeInTheDocument();
  });

  it("names the workspace root when nothing is set", () => {
    mount();
    expect(screen.getByText(/land in the workspace root/)).toBeInTheDocument();
  });

  it("checks the path as typed and sends nothing while it is wrong", async () => {
    mount();
    await type("../escape");
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Leave out \. and \.\./,
    );
    await save();
    expect(api.calls).not.toContain("PUT /api/workspaces/{id}/start-folder");
    expect(field()).toHaveFocus();
  });

  it("saves the trimmed path and says the folder exists", async () => {
    const ws = workspace("dev", { status: "running" });
    api.list = [ws];
    workspaces.list = [ws];
    const view = render(StartFolderSetting, { workspace: ws });
    await type("  api/web  ");
    await save();
    expect(await screen.findByText("Start folder saved.")).toBeInTheDocument();
    expect(api.bodies.at(-1)).toEqual({ path: "api/web" });
    // The page passes the refreshed workspace down.
    await view.rerender({ workspace: { ...ws, start_folder: "api/web" } });
    expect(
      await screen.findByText("The folder exists in the workspace."),
    ).toBeInTheDocument();
  });

  it("clears the setting when the field is emptied", async () => {
    mount({ start_folder: "api" });
    await type("");
    await save();
    await waitFor(() => expect(api.bodies.at(-1)).toEqual({ path: null }));
  });

  it("shows the service's refusal as an alert and keeps the text", async () => {
    mount();
    api.refuse.set("PUT /api/workspaces/{id}/start-folder", {
      status: 400,
      message: "the start folder is inside .puddle",
    });
    await type("ok/path");
    await save();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The start folder is inside .puddle.",
    );
    expect(field()).toHaveValue("ok/path");
  });

  it("says the service is not answering", async () => {
    mount();
    api.down = true;
    await type("ok");
    await save();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle's service isn't answering.",
    );
  });

  it("warns that the folder does not exist when the host lists that problem", async () => {
    api.problems = [
      {
        key: "start-folder:dev",
        title: "Start folder missing",
        detail: "api/web is not in the workspace",
        workspace: "dev",
      },
    ];
    mount({ start_folder: "api/web" });
    expect(
      await screen.findByText("The folder does not exist."),
    ).toBeInTheDocument();
  });

  it("says a stopped workspace is checked at its start", () => {
    mount({ status: "stopped", start_folder: "api" });
    expect(
      screen.getByText(
        "Puddle looks for the folder when the workspace starts.",
      ),
    ).toBeInTheDocument();
  });

  it("follows a change from another client until the user types", async () => {
    const ws = workspace("dev", { status: "running", start_folder: "a" });
    api.list = [ws];
    const view = render(StartFolderSetting, { workspace: ws });
    expect(field()).toHaveValue("a");
    await view.rerender({ workspace: { ...ws, start_folder: "b" } });
    await waitFor(() => expect(field()).toHaveValue("b"));
    await type("mine");
    await view.rerender({ workspace: { ...ws, start_folder: "c" } });
    expect(field()).toHaveValue("mine");
  });

  it("follows the host's list of problems while open", async () => {
    mount({ start_folder: "api/web" });
    await waitFor(() => expect(api.calls).toContain("GET /api/problems"));
    expect(screen.queryByText("The folder does not exist.")).toBeNull();
    api.problems = [
      {
        key: "start-folder:dev",
        title: "Start folder missing",
        detail: "gone",
        workspace: "dev",
      },
    ];
    h.source.emit({ type: "problems_changed" });
    expect(
      await screen.findByText("The folder does not exist."),
    ).toBeInTheDocument();
    api.problems = [];
    h.source.emit({ type: "status_changed", workspace: "dev" });
    h.source.resync();
    await waitFor(() =>
      expect(screen.queryByText("The folder does not exist.")).toBeNull(),
    );
  });
});
