// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ id: "w-web" }));
vi.mock("$app/state", () => ({
  page: {
    get params() {
      return { id: url.id };
    },
  },
}));

const h = await vi.hoisted(async () => {
  const fake = await import("#lib/testing/fake-ssh-agent.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  const inbox = await import("#lib/testing/fake-inbox.ts");
  return {
    api: new fake.FakeSshAgent(),
    source: new inbox.FakeSource(),
    key: fake.KEY_A,
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

const { api, source, key, workspace } = h;

beforeEach(() => {
  url.id = "w-web";
  workspaces.list = [workspace("web", { status: "running" })];
  api.hosts = {};
  api.calls = [];
  api.down = false;
  api.nextStatus = null;
});

afterEach(() => {
  cleanup();
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});

const hostBox = () =>
  screen.findByLabelText("Host") as Promise<HTMLInputElement>;
const keysBox = () =>
  screen.getByLabelText("Only these keys (optional)") as HTMLTextAreaElement;
const turnOn = () => screen.getByRole("button", { name: "Turn on" });

describe("the SSH agent tab", () => {
  it("says it is off for every host until one is turned on, and what it will never do", async () => {
    render(Page);
    await hostBox();
    expect(screen.getByText("Off for every host.")).toBeTruthy();
    expect(screen.getByText(/It never signs commits/)).toBeTruthy();
  });

  it("turns a host on for every key", async () => {
    render(Page);
    await fireEvent.input(await hostBox(), {
      target: { value: " Build.Example.com " },
    });
    await fireEvent.click(turnOn());
    const row = await screen.findByText("build.example.com");
    expect(row.closest("li")).toHaveTextContent("every key in your agent");
    expect(api.hosts["w-web"]).toHaveLength(1);
    expect(api.calls).toContain("PUT /api/workspaces/{id}/ssh-agent/{host}");
    await waitFor(() => expect(screen.getByLabelText("Host")).toHaveValue(""));
    expect(toasts.items.map((t) => t.message)).toContain(
      "Turned the SSH agent on for Build.Example.com.",
    );
  });

  it("limits a host to the keys typed, one fingerprint per line", async () => {
    render(Page);
    await fireEvent.input(await hostBox(), {
      target: { value: "build.example.com" },
    });
    await fireEvent.input(keysBox(), { target: { value: `${key}\n` } });
    await fireEvent.click(turnOn());
    const row = (await screen.findByText("build.example.com")).closest("li");
    expect(row).toHaveTextContent("1 key:");
    expect(row).toHaveTextContent(key);
    expect(api.hosts["w-web"]?.[0]?.keys).toEqual([key]);
  });

  it("refuses an empty host and a key that is not a fingerprint before asking the service", async () => {
    render(Page);
    await hostBox();
    await fireEvent.click(turnOn());
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Name the host the workspace logs in to.",
    );
    await fireEvent.input(await hostBox(), { target: { value: "h.example" } });
    await fireEvent.input(keysBox(), { target: { value: "ssh-ed25519 AAAA" } });
    await fireEvent.click(turnOn());
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "is not a SHA-256 key fingerprint",
    );
    expect(api.calls.filter((c) => c.startsWith("PUT"))).toEqual([]);
  });

  it("shows what the service refused and keeps what was typed", async () => {
    render(Page);
    await fireEvent.input(await hostBox(), { target: { value: "h.example" } });
    api.nextStatus = 422;
    await fireEvent.click(turnOn());
    expect(await screen.findByRole("alert")).toHaveTextContent("refused");
    expect(await hostBox()).toHaveValue("h.example");
  });

  it("turns a host off", async () => {
    api.hosts["w-web"] = [
      { host: "build.example.com", keys: null, changed_at: 1 },
    ];
    render(Page);
    await fireEvent.click(
      await screen.findByRole("button", {
        name: "Turn off the SSH agent for build.example.com",
      }),
    );
    await waitFor(() =>
      expect(toasts.items.map((t) => t.message)).toContain(
        "Turned the SSH agent off for build.example.com.",
      ),
    );
    expect(screen.getByText("Off for every host.")).toBeTruthy();
  });

  it("says when turning off failed", async () => {
    api.hosts["w-web"] = [
      { host: "build.example.com", keys: [key, key], changed_at: 1 },
    ];
    render(Page);
    const list = within(await screen.findByRole("list"));
    expect(list.getByText(/2 keys:/)).toBeTruthy();
    api.nextStatus = 500;
    await fireEvent.click(
      list.getByRole("button", { name: /Turn off the SSH agent/ }),
    );
    await waitFor(() =>
      expect(toasts.items.map((t) => t.message)).toContain(
        "Puddle couldn't turn the agent off.",
      ),
    );
    expect(list.getByText("build.example.com")).toBeTruthy();
  });

  it("follows a change made elsewhere", async () => {
    render(Page);
    await hostBox();
    api.hosts["w-web"] = [
      { host: "late.example.com", keys: null, changed_at: 1 },
    ];
    source.emit({ type: "workspace_git_changed", workspace: "w-web" });
    expect(await screen.findByText("late.example.com")).toBeTruthy();
  });

  it("says so while the service does not answer", async () => {
    api.down = true;
    render(Page);
    expect(
      await screen.findByText(/Couldn't read the SSH agent settings/),
    ).toBeTruthy();
  });

  it("waits while loading", async () => {
    workspaces.list = [];
    render(Page);
    expect(screen.getByText(/Loading the SSH agent settings/)).toBeTruthy();
  });
});
