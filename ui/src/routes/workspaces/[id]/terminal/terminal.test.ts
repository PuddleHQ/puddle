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
  const fake = await import("#lib/testing/fake-environment.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  const inbox = await import("#lib/testing/fake-inbox.ts");
  class TerminalApi extends fake.FakeEnvApi {
    posts: unknown[] = [];
    answer: () => unknown = () => ({
      data: { terminal: this.posts.length, ticket: "aa" },
    });
    POST = async (_path: string, init: unknown) => {
      this.posts.push(init);
      return this.answer();
    };
  }
  return {
    api: new TerminalApi(),
    source: new inbox.FakeSource(),
    workspace: ws.workspace,
    sockets: [] as import("#lib/testing/fake-terminal.ts").FakeSocket[],
    start: undefined as unknown as ReturnType<typeof import("vitest").vi.fn>,
  };
});
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));
vi.mock("#lib/stores/live.svelte.ts", async (original) => {
  const mod = await original<typeof import("#lib/stores/live.svelte.ts")>();
  return { ...mod, live: h.source };
});
vi.mock("#lib/terminal/xterm-screen.ts", async () => {
  const { FakeScreen } = await import("#lib/testing/fake-terminal.ts");
  return { XtermScreen: class extends FakeScreen {} };
});
vi.mock("#lib/stores/workspace-actions.svelte.ts", () => ({
  workspaceActions: { start: vi.fn(async () => {}) },
}));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { terminals } from "#lib/stores/terminals.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { port } from "#lib/testing/fake-environment.ts";
import { FakeScreen, FakeSocket } from "#lib/testing/fake-terminal.ts";
import Page from "./+page.svelte";

const { api, workspace, sockets } = h;

// Terminal numbers are never reused while the page lives, so each test gets workspaces of its own.
let run = 0;
let web = "";
let docs = "";
let boot = "";

beforeEach(() => {
  run += 1;
  [web, docs, boot] = [`web${run}`, `docs${run}`, `boot${run}`];
  url.id = `w-${web}`;
  workspaces.list = [
    workspace(web, { status: "running" }),
    workspace(docs, { status: "stopped" }),
    workspace(boot, { status: "starting", busy: "starting" }),
  ];
  api.ports = {
    [`w-${web}`]: {
      running: true,
      ports: [port(3000, { forwarded: true, host_port: 3000 })],
    },
  };
  api.posts = [];
  api.answer = () => ({ data: { terminal: api.posts.length, ticket: "aa" } });
  sockets.length = 0;
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  }));
  vi.stubGlobal("WebSocket", function (u: string, p: string[]) {
    const socket = new FakeSocket(u, p);
    sockets.push(socket);
    return socket;
  });
});

afterEach(() => {
  cleanup();
  for (const s of [...terminals.sessions]) terminals.remove(s);
  terminals.problem = null;
  terminals.screenReader = false;
  vi.unstubAllGlobals();
  vi.mocked(workspaceActions.start).mockClear();
});

const newTerminal = () => screen.getByRole("button", { name: /New terminal/ });

describe("a workspace that is not running", () => {
  it("says to start it and offers a Start button", async () => {
    url.id = `w-${docs}`;
    render(Page);
    expect(
      await screen.findByText("Start the workspace to open a terminal."),
    ).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(workspaceActions.start).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: /New terminal/ })).toBeNull();
  });

  it("keeps a stopped workspace's ended terminals, with New terminal off", async () => {
    render(Page);
    await fireEvent.click(newTerminal());
    await waitFor(() => expect(sockets).toHaveLength(1));
    sockets[0]?.open();
    workspaces.list = [workspace(web, { status: "stopped" })];
    sockets[0]?.receive('{"type":"ended","reason":"stopped"}');
    expect(
      await screen.findByText("The workspace stopped; the terminal ended."),
    ).toBeInTheDocument();
    expect(newTerminal()).toBeDisabled();
    expect(
      screen.getAllByText("Start the workspace to open a terminal."),
    ).toHaveLength(1);
    await fireEvent.click(screen.getByRole("button", { name: "Start" }));
    expect(workspaceActions.start).toHaveBeenCalledTimes(1);
  });
});

describe("opening and closing", () => {
  it("opens a terminal, connects it at the measured size and labels the sub-tab", async () => {
    render(Page);
    expect(screen.getByText(/No terminal is open/)).toBeInTheDocument();
    await fireEvent.click(newTerminal());
    const tab = await screen.findByRole("tab", { name: "Terminal 1" });
    expect(tab).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tabpanel")).toBeInTheDocument();
    await waitFor(() =>
      expect(api.posts).toEqual([
        { params: { path: { id: `w-${web}` } }, body: { cols: 80, rows: 24 } },
      ]),
    );
    expect(sockets[0]?.protocols[0]).toBe("puddle-terminal.v1");
  });

  it("shows several sub-tabs, one in view, and closes one", async () => {
    render(Page);
    await fireEvent.click(newTerminal());
    await fireEvent.click(newTerminal());
    const tabs = await screen.findAllByRole("tab");
    expect(tabs).toHaveLength(2);
    expect(tabs[1]).toHaveAttribute("aria-selected", "true");
    await fireEvent.click(tabs[0] as HTMLElement);
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
    await fireEvent.click(
      screen.getByRole("button", { name: "Close Terminal 1" }),
    );
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "Terminal 2" })).toHaveFocus(),
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Close Terminal 2" }),
    );
    expect(screen.queryByRole("tab")).toBeNull();
    expect(newTerminal()).toHaveFocus();
  });

  it("moves between sub-tabs with the arrow keys, Home and End", async () => {
    render(Page);
    for (let i = 0; i < 3; i += 1) await fireEvent.click(newTerminal());
    const [one, two, three] = await screen.findAllByRole("tab");
    three?.focus();
    await fireEvent.keyDown(three as HTMLElement, { key: "ArrowRight" });
    await waitFor(() => expect(one).toHaveFocus());
    expect(one).toHaveAttribute("aria-selected", "true");
    await fireEvent.keyDown(one as HTMLElement, { key: "ArrowLeft" });
    await waitFor(() => expect(three).toHaveFocus());
    await fireEvent.keyDown(three as HTMLElement, { key: "Home" });
    await waitFor(() => expect(one).toHaveFocus());
    await fireEvent.keyDown(one as HTMLElement, { key: "End" });
    await waitFor(() => expect(three).toHaveFocus());
    await fireEvent.keyDown(three as HTMLElement, { key: "x" });
    expect(three).toHaveFocus();
    await fireEvent.keyDown(two as HTMLElement, { key: "End" });
    expect(three).toHaveFocus();
  });

  it("turns New terminal off at the limit and says which one holds", async () => {
    render(Page);
    for (let i = 0; i < 8; i += 1) await fireEvent.click(newTerminal());
    await waitFor(() => expect(newTerminal()).toBeDisabled());
    expect(
      screen.getByText(/This workspace has 8 terminals open/),
    ).toBeInTheDocument();
    expect(newTerminal()).toHaveAccessibleDescription(/8 terminals open/);
  });

  it("shows why the host refused a terminal and leaves no sub-tab", async () => {
    api.answer = () => ({
      error: { error: "conflict", message: "this workspace is not running" },
    });
    render(Page);
    await fireEvent.click(newTerminal());
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "This workspace is not running.",
    );
    expect(screen.queryByRole("tab")).toBeNull();
  });

  it("opens in a workspace that is still starting and says it waits", async () => {
    url.id = `w-${boot}`;
    render(Page);
    await fireEvent.click(newTerminal());
    await waitFor(() => expect(sockets).toHaveLength(1));
    sockets[0]?.open();
    expect(
      await screen.findByText("Waiting for the workspace to finish starting"),
    ).toBeInTheDocument();
    sockets[0]?.receive(Uint8Array.of(36, 32));
    await waitFor(() =>
      expect(
        screen.queryByText("Waiting for the workspace to finish starting"),
      ).toBeNull(),
    );
  });
});

describe("the sub-tab", () => {
  async function open() {
    render(Page);
    await fireEvent.click(newTerminal());
    await waitFor(() => expect(sockets).toHaveLength(1));
    const socket = sockets[0] as FakeSocket;
    socket.open();
    return {
      socket,
      session: terminals.sessions[0] as (typeof terminals.sessions)[number],
    };
  }

  it("shows a program's title, still named Terminal N to a screen reader, and marks the bell", async () => {
    const { session } = await open();
    const fake = session.screen as FakeScreen;
    fake.title("npm run dev");
    const tab = await screen.findByRole("tab", { name: "Terminal 1" });
    expect(within(tab).getByText("npm run dev")).toBeInTheDocument();
    const other = terminals.add(`w-${web}`) as NonNullable<
      ReturnType<typeof terminals.add>
    >;
    (session as { bell: boolean }).bell = true;
    expect(await screen.findByText(/Bell rang\./)).toBeInTheDocument();
    terminals.select(`w-${web}`, other);
  });

  it("says an ended terminal ended", async () => {
    const { socket } = await open();
    socket.receive('{"type":"ended","reason":"exited","status":1}');
    expect(
      await screen.findByText("The shell exited with status 1."),
    ).toBeInTheDocument();
    expect(screen.getByText(/Ended\./)).toBeInTheDocument();
  });

  it("restarts a terminal whose shell exited, in the same sub-tab", async () => {
    const { socket, session } = await open();
    socket.receive('{"type":"ended","reason":"exited","status":0}');
    await fireEvent.click(
      await screen.findByRole("button", { name: "Restart" }),
    );
    await waitFor(() => expect(sockets).toHaveLength(2));
    expect((session.screen as FakeScreen).separators).toBe(1);
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  it("offers Start for a crash, with the reason", async () => {
    const { socket } = await open();
    socket.receive(
      '{"type":"ended","reason":"crashed","message":"out of memory"}',
    );
    expect(await screen.findByText("out of memory")).toBeInTheDocument();
    workspaces.list = [workspace(web, { status: "crashed" })];
    const start = await screen.findByRole("button", { name: "Start" });
    await fireEvent.click(start);
    expect(workspaceActions.start).toHaveBeenCalledTimes(1);
  });

  it("points a failed boot at the Overview", async () => {
    const { socket } = await open();
    socket.receive(
      '{"type":"ended","reason":"boot_failed","message":"the boot hook failed"}',
    );
    expect(await screen.findByText("the boot hook failed")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Overview" })).toHaveAttribute(
      "href",
      `/workspaces/w-${web}`,
    );
  });

  it("offers New terminal when the connection broke", async () => {
    const { socket } = await open();
    socket.onclose?.();
    expect(
      await screen.findByText(
        "The connection to Puddle broke; the terminal ended.",
      ),
    ).toBeInTheDocument();
    await fireEvent.click(
      screen.getAllByRole("button", { name: /New terminal/ })[1] as HTMLElement,
    );
    expect(screen.getAllByRole("tab")).toHaveLength(2);
  });

  it("F6 in the terminal moves focus to the sub-tab row", async () => {
    const { session } = await open();
    (session.screen as FakeScreen).onLeave();
    expect(
      await screen.findByRole("tab", { name: "Terminal 1" }),
    ).toHaveFocus();
  });

  it("the Screen reader mode switch reaches the emulator", async () => {
    const { session } = await open();
    await fireEvent.click(
      screen.getByRole("checkbox", { name: "Screen reader mode" }),
    );
    await waitFor(() =>
      expect((session.screen as FakeScreen).reader).toBe(true),
    );
  });
});

describe("a workspace that is gone", () => {
  it("says it is loading", () => {
    url.id = "w-nope";
    render(Page);
    expect(screen.getByText(/Loading/)).toBeInTheDocument();
  });
});
