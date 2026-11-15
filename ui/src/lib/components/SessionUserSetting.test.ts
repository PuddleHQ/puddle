// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = await vi.hoisted(async () => {
  const ws = await import("#lib/testing/fake-workspaces.ts");
  return { api: new ws.FakeWorkspaces() };
});

vi.mock("#lib/api/client.ts", () => ({ api: h.api }));
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
import SessionUserSetting from "./SessionUserSetting.svelte";

const { api } = h;

beforeEach(() => {
  api.list = [];
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.refuse.clear();
});
afterEach(cleanup);

function mount(over: Partial<Workspace> = {}) {
  const ws = workspace("dev", over);
  api.list = [ws];
  workspaces.list = [ws];
  render(SessionUserSetting, { workspace: ws });
}

const select = () =>
  screen.getByRole("combobox", {
    name: "Terminals and the editor run as",
  }) as HTMLSelectElement;
const choose = (value: string) =>
  fireEvent.change(select(), { target: { value } });

describe("the session user setting", () => {
  it("shows the stored user and says sudo makes agent root", () => {
    mount({ session_user: "root" });
    expect(select()).toHaveValue("root");
    expect(screen.getByText(/becomes root with/)).toBeInTheDocument();
  });

  it("saves a change and says it applies at the next start of a running workspace", async () => {
    mount({ session_user: "root", status: "running" });
    await choose("agent");
    expect(
      await screen.findByText(
        "Saved. The workspace runs as agent from its next start.",
      ),
    ).toBeInTheDocument();
    expect(api.calls).toContain("PUT /api/workspaces/{id}/session-user");
    expect(api.bodies.at(-1)).toEqual({ session_user: "agent" });
    expect(workspaces.list[0]?.session_user).toBe("agent");
  });

  it("says a stopped workspace runs as the new user when it starts", async () => {
    mount();
    await choose("root");
    expect(
      await screen.findByText(
        "Saved. The workspace runs as root when it starts.",
      ),
    ).toBeInTheDocument();
  });

  it("shows a refusal and puts the choice back", async () => {
    mount();
    api.refuse.set("PUT /api/workspaces/{id}/session-user", {
      status: 409,
      message: "dev is deleting; change the user it runs as when that is done",
    });
    await choose("root");
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Dev is deleting; change the user it runs as when that is done.",
    );
    expect(select()).toHaveValue("agent");
  });

  it("says the service is not answering and puts the choice back", async () => {
    mount();
    api.down = true;
    await choose("root");
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle's service isn't answering.",
    );
    expect(select()).toHaveValue("agent");
  });
});
