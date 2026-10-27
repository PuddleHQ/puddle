// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ id: "web" }));
vi.mock("$app/state", () => ({
  page: {
    get params() {
      return { id: url.id };
    },
  },
}));

const h = await vi.hoisted(async () => {
  const fake = await import("#lib/testing/fake-environment.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  const inbox = await import("#lib/testing/fake-inbox.ts");
  return {
    api: new fake.FakeEnvApi(),
    source: new inbox.FakeSource(),
    workspace: ws.workspace,
  };
});
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));
vi.mock("#lib/stores/live.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/live.svelte.ts")>();
  return { ...mod, live: h.source };
});

import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import Page from "./+page.svelte";

const { api, source, workspace } = h;

beforeEach(() => {
  url.id = "web";
  workspaces.list = [workspace("web", { status: "running" })];
  api.shell = { web: { content: "export A=1\n", changed_at: 10 } };
  api.shellChecked = { checked: true, reason: null };
  api.calls = [];
  api.down = false;
  api.refuse = null;
});

afterEach(() => {
  cleanup();
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});

const box = () =>
  screen.findByLabelText("Script for web") as Promise<HTMLTextAreaElement>;
const button = (name: string) => screen.getByRole("button", { name });

describe("the editor", () => {
  it("shows the saved script and keeps Save for a change", async () => {
    render(Page);
    expect(await box()).toHaveValue("export A=1\n");
    expect(screen.getByText(/must print nothing/)).toBeTruthy();
    expect(button("Save")).toBeDisabled();
    expect(button("Discard changes")).toBeDisabled();
    expect(button("Remove the script")).toBeEnabled();
    expect(screen.getByText(/Saved\./)).toBeTruthy();
  });

  it("says nothing is saved yet, and can't remove what is not there", async () => {
    api.shell = {};
    render(Page);
    expect(await box()).toHaveValue("");
    expect(screen.getByText(/Nothing saved yet/)).toBeTruthy();
    expect(button("Remove the script")).toBeDisabled();
  });

  it("saves a change and says the running workspace ran it once", async () => {
    render(Page);
    const field = await box();
    await fireEvent.input(field, { target: { value: "export A=2\n" } });
    expect(button("Save")).toBeEnabled();
    await fireEvent.click(button("Save"));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Saved. Puddle ran it once in the workspace and it printed nothing.",
    );
    expect(api.shell["web"]?.content).toBe("export A=2\n");
    expect(field).toHaveValue("export A=2\n");
    expect(button("Save")).toBeDisabled();
    expect(toasts.items.at(-1)?.message).toBe("Saved the shell init.");
  });

  it("says a stopped workspace's script is checked when it starts", async () => {
    api.shellChecked = { checked: false, reason: "The workspace is stopped." };
    render(Page);
    await fireEvent.input(await box(), { target: { value: "ls" } });
    await fireEvent.click(button("Save"));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Saved. The workspace is stopped.",
    );
    api.shellChecked = { checked: false, reason: null };
    await fireEvent.input(await box(), { target: { value: "ls -l" } });
    await fireEvent.click(button("Save"));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(
        "Puddle checks the script when it starts",
      ),
    );
  });

  it("shows what the script printed when the save is refused, and saves nothing", async () => {
    render(Page);
    const field = await box();
    await fireEvent.input(field, { target: { value: "echo hello" } });
    api.refuse = { status: 422, message: "it printed:\nhello" };
    await fireEvent.click(button("Save"));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Not saved. it printed: hello");
    expect(api.shell["web"]?.content).toBe("export A=1\n");
    expect(field).toHaveValue("echo hello");
    expect(field).toHaveAttribute("aria-invalid", "true");
    await fireEvent.input(field, { target: { value: "ls" } });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("discards what was typed", async () => {
    render(Page);
    const field = await box();
    await fireEvent.input(field, { target: { value: "other" } });
    await fireEvent.click(button("Discard changes"));
    expect(field).toHaveValue("export A=1\n");
  });

  it("doesn't save an empty script: removing is its own button", async () => {
    render(Page);
    await fireEvent.input(await box(), { target: { value: "" } });
    expect(button("Save")).toBeDisabled();
  });

  it("removes the script and keeps its text in the box so Save puts it back", async () => {
    render(Page);
    const field = await box();
    await fireEvent.click(button("Remove the script"));
    await waitFor(() => expect(api.shell["web"]).toBeUndefined());
    await waitFor(() => expect(field).toHaveValue("export A=1\n"));
    await waitFor(() => expect(button("Save")).toBeEnabled());
    expect(toasts.items.at(-1)?.message).toContain(
      "Its text is still in the box",
    );
    await fireEvent.click(button("Save"));
    await waitFor(() => expect(api.shell["web"]?.content).toBe("export A=1\n"));
  });

  it("says when the removal fails", async () => {
    render(Page);
    await box();
    api.refuse = { status: 500, message: "x" };
    await fireEvent.click(button("Remove the script"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle couldn't remove the script.",
    );
  });
});

describe("what happens behind the page", () => {
  it("tells the user when Puddle put the saved text back, until dismissed", async () => {
    render(Page);
    await box();
    expect(screen.queryByText(/Puddle put your saved text back/)).toBeNull();
    source.emit({ type: "workspace_shell_init_restored", workspace: "web" });
    expect(
      await screen.findByText(/Puddle put your saved text back/),
    ).toBeTruthy();
    await fireEvent.click(button("Dismiss"));
    expect(screen.queryByText(/Puddle put your saved text back/)).toBeNull();
  });

  it("follows a save made elsewhere when the box is untouched", async () => {
    render(Page);
    const field = await box();
    api.shell["web"] = { content: "export B=1\n", changed_at: 20 };
    source.emit({ type: "workspace_shell_init_changed", workspace: "web" });
    await waitFor(() => expect(field).toHaveValue("export B=1\n"));
    expect(screen.queryByText(/changed while you were editing/)).toBeNull();
  });

  it("keeps the user's edits when the saved script changes meanwhile, until they load it", async () => {
    render(Page);
    const field = await box();
    await fireEvent.input(field, { target: { value: "my edit" } });
    api.shell["web"] = { content: "export B=1\n", changed_at: 20 };
    source.emit({ type: "workspace_shell_init_changed", workspace: "web" });
    expect(
      await screen.findByText(/changed while you were editing/),
    ).toBeTruthy();
    expect(field).toHaveValue("my edit");
    await fireEvent.click(button("Load the saved script"));
    expect(field).toHaveValue("export B=1\n");
    expect(screen.queryByText(/changed while you were editing/)).toBeNull();
  });
});

describe("loading", () => {
  it("says it is loading, and that it could not read", async () => {
    url.id = "nope";
    render(Page);
    expect(screen.getByText(/Loading shell init/)).toBeTruthy();
    cleanup();
    url.id = "web";
    api.down = true;
    render(Page);
    expect(
      await screen.findByText(/Couldn't read the shell init/),
    ).toBeTruthy();
  });
});
