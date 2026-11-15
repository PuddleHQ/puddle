// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeAll, describe, expect, it, vi } from "vitest";
import {
  emulatorDom,
  FakeScreen,
  FakeSocket,
} from "#lib/testing/fake-terminal.ts";
import { port } from "#lib/testing/fake-environment.ts";
import { MAX_IN_ALL, MAX_PER_WORKSPACE } from "#lib/terminal/wire.ts";
import {
  limitReason,
  setForwards,
  TerminalStore,
  terminals,
} from "./terminals.svelte.ts";

beforeAll(emulatorDom);

function make(post?: () => unknown) {
  const sockets: FakeSocket[] = [];
  const store = new TerminalStore(() => new FakeScreen(), {
    api: {
      POST: (async () =>
        post ? post() : { data: { terminal: 1, ticket: "aa" } }) as never,
    },
    origin: () => "http://h",
    socket: (url, protocols) => {
      const s = new FakeSocket(url, protocols);
      sockets.push(s);
      return s;
    },
  });
  return { store, sockets };
}

describe("limitReason", () => {
  it("names the limit that holds, the workspace's first", () => {
    expect(limitReason(0, 0)).toBeNull();
    expect(limitReason(MAX_PER_WORKSPACE, 9)).toMatch(
      /^This workspace has 8 terminals open/,
    );
    expect(limitReason(1, MAX_IN_ALL)).toMatch(/^32 terminals are open in all/);
    expect(limitReason(MAX_PER_WORKSPACE - 1, MAX_IN_ALL - 1)).toBeNull();
  });
});

describe("the sub-tabs", () => {
  it("numbers them per workspace and puts the new one in view", () => {
    const { store } = make();
    const a1 = store.add("a");
    const a2 = store.add("a");
    const b1 = store.add("b");
    expect([a1?.number, a2?.number, b1?.number]).toEqual([1, 2, 1]);
    expect(store.of("a")).toHaveLength(2);
    expect(store.selected("a")).toBe(a2);
    expect(store.selected("b")).toBe(b1);
    expect(store.selected("none")).toBeUndefined();
  });

  it("does not reuse a number after a close, and falls back to the last one in view", () => {
    const { store } = make();
    const first = store.add("a");
    const second = store.add("a");
    store.remove(second as never);
    expect(store.selected("a")).toBe(first);
    expect(store.add("a")?.number).toBe(3);
  });

  it("selecting a sub-tab clears its bell", () => {
    const { store } = make();
    const first = store.add("a");
    store.add("a");
    (first as NonNullable<typeof first>).bell = true;
    store.select("a", first as NonNullable<typeof first>);
    expect(first?.bell).toBe(false);
    expect(store.selected("a")).toBe(first);
  });

  it("refuses a ninth terminal in a workspace and says so", () => {
    const { store } = make();
    for (let i = 0; i < MAX_PER_WORKSPACE; i += 1)
      expect(store.add("a")).not.toBeNull();
    expect(store.limit("a")).toMatch(/8 terminals open/);
    expect(store.add("a")).toBeNull();
    expect(store.problem).toMatch(/8 terminals open/);
    expect(store.add("b")).not.toBeNull();
    expect(store.problem).toBeNull();
  });

  it("refuses the 33rd in all, and counts only terminals that are open", async () => {
    const { store, sockets } = make();
    for (let i = 0; i < MAX_IN_ALL; i += 1) {
      const s = store.add(`w${i % 5}-${Math.floor(i / 8)}`);
      expect(s).not.toBeNull();
      await s?.connect();
    }
    expect(store.limit("fresh")).toMatch(/32 terminals are open in all/);
    sockets[0]?.receive('{"type":"ended","reason":"exited","status":0}');
    expect(store.limit("fresh")).toBeNull();
  });
});

describe("connect", () => {
  it("opens the session with the measured size", async () => {
    const { store, sockets } = make();
    const session = store.add("a");
    await store.connect(session as never, { cols: 90, rows: 20 });
    expect(session?.state).toBe("connecting");
    expect(sockets).toHaveLength(1);
    expect(store.of("a")).toHaveLength(1);
  });

  it("drops a first terminal the host refused and shows why", async () => {
    const { store } = make(() => ({
      error: { error: "conflict", message: "the workspace is not running" },
    }));
    const session = store.add("a");
    await store.connect(session as never, { cols: 80, rows: 24 });
    expect(store.of("a")).toEqual([]);
    expect(store.problem).toBe("The workspace is not running.");
  });

  it("closing a sub-tab ends its session", async () => {
    const { store, sockets } = make();
    const session = store.add("a");
    await store.connect(session as never, { cols: 80, rows: 24 });
    store.remove(session as never);
    expect(sockets[0]?.closed).toBe(true);
    expect(store.sessions).toEqual([]);
  });
});

describe("the page's store", () => {
  it("builds real emulators whose links read the workspace's forwards", () => {
    const session = terminals.add("w-links");
    expect(session).not.toBeNull();
    setForwards("w-links", [port(3000, { forwarded: true, host_port: 3000 })]);
    const opened = vi.spyOn(globalThis, "open").mockImplementation(() => null);
    const handler = (
      session?.screen as unknown as {
        term: {
          options: {
            linkHandler: {
              activate(e: MouseEvent, t: string, r: unknown): void;
            };
          };
        };
      }
    ).term.options.linkHandler;
    const ctrl = new MouseEvent("click", { ctrlKey: true });
    handler.activate(ctrl, "http://localhost:3000/", {});
    handler.activate(ctrl, "http://localhost:4000/", {});
    expect(opened).toHaveBeenCalledTimes(1);
    expect(opened).toHaveBeenCalledWith(
      "http://localhost:3000/",
      "_blank",
      "noopener,noreferrer",
    );
    terminals.remove(session as never);
    opened.mockRestore();
  });
});
