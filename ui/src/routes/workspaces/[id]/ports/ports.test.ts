// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
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
import { port } from "#lib/testing/fake-environment.ts";
import Page from "./+page.svelte";

const { api, source, workspace } = h;

beforeEach(() => {
  url.id = "web";
  workspaces.list = [
    workspace("web", { status: "running" }),
    workspace("docs", { status: "stopped" }),
  ];
  api.ports = {
    web: {
      running: true,
      ports: [
        port(3000),
        port(5432, { forwarded: true, host_port: 15432 }),
        port(8080, { forwarded: true, pinned: true, host_port: 8080 }),
      ],
    },
    docs: {
      running: false,
      ports: [
        port(4000, {
          listening: false,
          pinned: true,
          host_port: 4000,
          problem: "port 4000 on this computer is in use by node",
        }),
      ],
    },
  };
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.refuse = null;
});

afterEach(() => {
  cleanup();
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});

const rowOf = async (guest: number) => {
  const table = await screen.findByRole("table", { name: "Ports of web" });
  return within(table)
    .getAllByRole("row")
    .find((r) => r.getAttribute("data-port") === String(guest)) as HTMLElement;
};

describe("the table", () => {
  it("lists what listens, what is forwarded and what is kept", async () => {
    render(Page);
    expect(await rowOf(3000)).toHaveTextContent("Yes");
    expect(await rowOf(3000)).toHaveTextContent(/Forward/);
    expect(await rowOf(5432)).toHaveTextContent("Open on 127.0.0.1:15432");
    expect(await rowOf(8080)).toHaveTextContent("Open on 127.0.0.1:8080");
    expect(
      within(await rowOf(8080)).getByLabelText(/Keep the forward of 8080/),
    ).toBeChecked();
    expect(
      within(await rowOf(5432)).getByLabelText(/Keep the forward of 5432/),
    ).not.toBeChecked();
    expect(screen.getByText(/never on your network/)).toBeTruthy();
  });

  it("says a stopped workspace's kept forward is not open, with its problem, and offers no forward", async () => {
    url.id = "docs";
    render(Page);
    const table = await screen.findByRole("table", { name: "Ports of docs" });
    const row = within(table).getAllByRole("row")[1] as HTMLElement;
    expect(row).toHaveTextContent("Kept for 127.0.0.1:4000, not open");
    expect(row).toHaveTextContent("in use by node");
    expect(screen.getByRole("status")).toHaveTextContent("docs is not running");
    expect(screen.getByRole("button", { name: "Forward" })).toBeDisabled();
    expect(
      within(row).getByLabelText(/Keep the forward of 4000/),
    ).toBeDisabled();
    expect(
      within(row).getByRole("button", { name: "Stop forwarding 4000" }),
    ).toBeEnabled();
  });

  it("says when nothing listens", async () => {
    api.ports["web"] = { running: true, ports: [] };
    render(Page);
    expect(
      await screen.findByText(/Nothing listens in the workspace/),
    ).toBeTruthy();
    cleanup();
    api.ports["web"] = { running: false, ports: [] };
    render(Page);
    expect(await screen.findByText("No forward is kept.")).toBeTruthy();
  });

  it("says it is loading, and that it could not read", async () => {
    url.id = "nope";
    render(Page);
    expect(screen.getByText(/Loading ports/)).toBeTruthy();
    cleanup();
    url.id = "web";
    api.down = true;
    render(Page);
    expect(await screen.findByText(/Couldn't read the ports/)).toBeTruthy();
  });

  it("follows the events that change the list", async () => {
    render(Page);
    await rowOf(3000);
    api.ports["web"]!.ports.push(port(9000));
    source.emit({ type: "workspace_ports_changed", workspace: "web" });
    await waitFor(async () => expect(await rowOf(9000)).toBeTruthy());
  });
});

describe("forwarding", () => {
  it("forwards a port to the same number", async () => {
    render(Page);
    const row = await rowOf(3000);
    await fireEvent.click(
      within(row).getByRole("button", { name: "Forward 3000" }),
    );
    await waitFor(async () =>
      expect(await rowOf(3000)).toHaveTextContent("Open on 127.0.0.1:3000"),
    );
    expect(api.bodies.at(-1)).toEqual({ host_port: null, pinned: false });
    expect(toasts.items.at(-1)?.message).toBe(
      "3000 is forwarded to 127.0.0.1:3000.",
    );
  });

  it("forwards to another port and keeps it", async () => {
    render(Page);
    const row = await rowOf(3000);
    await fireEvent.input(
      within(row).getByLabelText("Port on this computer for 3000"),
      { target: { value: "13000" } },
    );
    await fireEvent.click(
      within(row).getByRole("button", { name: "Forward and keep 3000" }),
    );
    await waitFor(async () =>
      expect(await rowOf(3000)).toHaveTextContent("Open on 127.0.0.1:13000"),
    );
    expect(api.bodies.at(-1)).toEqual({ host_port: 13000, pinned: true });
    expect(toasts.items.at(-1)?.message).toBe(
      "3000 is forwarded to 127.0.0.1:13000 and kept.",
    );
  });

  it("refuses a port that is not a number before asking, and shows the service's refusal", async () => {
    render(Page);
    const row = await rowOf(3000);
    await fireEvent.input(
      within(row).getByLabelText("Port on this computer for 3000"),
      { target: { value: "70000" } },
    );
    await fireEvent.click(
      within(row).getByRole("button", { name: "Forward 3000" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "70000 is not a port: use a number from 1 to 65535.",
    );
    expect(api.calls.some((c) => c.startsWith("PUT"))).toBe(false);
    await fireEvent.input(
      within(row).getByLabelText("Port on this computer for 3000"),
      { target: { value: "80" } },
    );
    api.refuse = { status: 409, message: "port 80 is in use by nginx" };
    await fireEvent.click(
      within(row).getByRole("button", { name: "Forward 3000" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Port 80 is in use by nginx.",
      ),
    );
  });

  it("keeps and un-keeps an open forward", async () => {
    render(Page);
    const row = await rowOf(5432);
    await fireEvent.click(
      within(row).getByLabelText(/Keep the forward of 5432/),
    );
    await waitFor(() =>
      expect(api.ports["web"]!.ports[1]).toMatchObject({ pinned: true }),
    );
    expect(api.bodies.at(-1)).toEqual({ host_port: 15432, pinned: true });
    const kept = await rowOf(8080);
    await fireEvent.click(
      within(kept).getByLabelText(/Keep the forward of 8080/),
    );
    await waitFor(() =>
      expect(api.ports["web"]!.ports[2]).toMatchObject({ pinned: false }),
    );
  });

  it("says when keeping fails", async () => {
    render(Page);
    const row = await rowOf(5432);
    api.refuse = { status: 500, message: "x" };
    await fireEvent.click(
      within(row).getByLabelText(/Keep the forward of 5432/),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("X.");
  });

  it("stops a forward, and says when that fails", async () => {
    render(Page);
    const row = await rowOf(5432);
    api.refuse = { status: 500, message: "x" };
    await fireEvent.click(
      within(row).getByRole("button", { name: "Stop forwarding 5432" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle couldn't stop the forward.",
    );
    await fireEvent.click(
      within(await rowOf(5432)).getByRole("button", {
        name: "Stop forwarding 5432",
      }),
    );
    await waitFor(async () =>
      expect(await rowOf(5432)).toHaveTextContent("Forward and keep"),
    );
    expect(toasts.items.at(-1)?.message).toBe("Stopped forwarding 5432.");
  });
});

describe("forwarding another port", () => {
  const form = () => screen.findByLabelText("Forward another port");

  it("forwards a port nothing listens on yet", async () => {
    render(Page);
    await fireEvent.input(await form(), { target: { value: "6379" } });
    await fireEvent.input(screen.getByLabelText("On this computer"), {
      target: { value: "16379" },
    });
    await fireEvent.click(screen.getByLabelText("Keep it"));
    await fireEvent.click(
      within(
        screen.getByLabelText("Forward another port").closest("form")!,
      ).getByRole("button", { name: "Forward" }),
    );
    await waitFor(async () =>
      expect(await rowOf(6379)).toHaveTextContent("Open on 127.0.0.1:16379"),
    );
    expect(api.bodies.at(-1)).toEqual({ host_port: 16379, pinned: true });
    await waitFor(async () => expect(await form()).toHaveValue(""));
  });

  it("refuses a port that is not a number, focusing the box", async () => {
    render(Page);
    const field = await form();
    await fireEvent.input(field, { target: { value: "abc" } });
    await fireEvent.submit(field.closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Give the workspace's port as a number from 1 to 65535.",
    );
    expect(field).toHaveAttribute("aria-invalid", "true");
    expect(document.activeElement).toBe(field);
  });
});
