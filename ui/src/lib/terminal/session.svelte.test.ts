// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeScreen, FakeSocket } from "#lib/testing/fake-terminal.ts";
import {
  RESIZE_DELAY_MS,
  TerminalSession,
  type SessionDeps,
} from "./session.svelte.ts";
import { MAX_INPUT_BYTES, SUBPROTOCOL } from "./wire.ts";

type Post = SessionDeps["api"]["POST"];

function make(post?: Post) {
  const screen = new FakeScreen();
  const sockets: FakeSocket[] = [];
  const posts: unknown[] = [];
  const api = {
    POST: (async (path: string, init: unknown) => {
      posts.push([path, init]);
      return post
        ? (post as unknown as (p: string, i: unknown) => unknown)(path, init)
        : { data: { terminal: 7, ticket: "ab".repeat(32) } };
    }) as unknown as Post,
  };
  const session = new TerminalSession("demo", 1, screen, {
    api,
    origin: () => "http://127.0.0.1:9",
    socket: (url, protocols) => {
      const s = new FakeSocket(url, protocols);
      sockets.push(s);
      return s;
    },
  });
  return { session, screen, sockets, posts };
}

const bytes = (...n: number[]) => Uint8Array.from(n);

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("opening", () => {
  it("asks for a terminal of the measured size, then opens the socket with the ticket in the sub-protocol", async () => {
    const { session, sockets, posts } = make();
    expect(session.state).toBe("idle");
    expect(session.open).toBe(true);
    expect(session.label).toBe("Terminal 1");
    const done = session.connect({ cols: 100, rows: 30 });
    expect(session.state).toBe("connecting");
    expect(session.open).toBe(true);
    await done;
    expect(posts).toEqual([
      [
        "/api/workspaces/{id}/terminals",
        { params: { path: { id: "demo" } }, body: { cols: 100, rows: 30 } },
      ],
    ]);
    const socket = sockets[0] as FakeSocket;
    expect(socket.url).toBe("ws://127.0.0.1:9/api/workspaces/demo/terminals/7");
    expect(socket.protocols).toEqual([
      SUBPROTOCOL,
      `ticket.${"ab".repeat(32)}`,
    ]);
    expect(socket.binaryType).toBe("arraybuffer");
    socket.open();
    expect(session.state).toBe("live");
  });

  it("holds what is typed before the socket opens and sends it in order", async () => {
    const { session, screen, sockets } = make();
    await session.connect({ cols: 80, rows: 24 });
    screen.input("ls");
    screen.binary(bytes(200, 201));
    const socket = sockets[0] as FakeSocket;
    expect(socket.sent).toEqual([]);
    socket.open();
    expect(socket.sent).toEqual([
      new TextEncoder().encode("ls"),
      bytes(200, 201),
    ]);
  });

  it("splits a long paste into messages of at most 64 KiB", async () => {
    const { session, screen, sockets } = make();
    await session.connect();
    (sockets[0] as FakeSocket).open();
    screen.input("x".repeat(MAX_INPUT_BYTES + 3));
    const sent = (sockets[0] as FakeSocket).sent as Uint8Array[];
    expect(sent.map((m) => m.length)).toEqual([MAX_INPUT_BYTES, 3]);
  });

  it("drops what is typed in a terminal that ended", async () => {
    const { session, screen, sockets } = make();
    await session.connect();
    (sockets[0] as FakeSocket).open();
    (sockets[0] as FakeSocket).receive('{"type":"ended","reason":"stopped"}');
    screen.input("late");
    expect((sockets[0] as FakeSocket).sent).toEqual([]);
  });

  it("shows the host's refusal as the end and marks the session refused", async () => {
    const { session, screen, sockets } = make((async () => ({
      error: {
        error: "conflict",
        message: "this workspace has 8 terminals open",
      },
    })) as never);
    await session.connect();
    expect(sockets).toEqual([]);
    expect(session.refused).toBe(true);
    expect(session.state).toBe("ended");
    expect(session.end?.line).toBe("This workspace has 8 terminals open.");
    expect(screen.enabled).toBe(false);
  });

  it("says the service is not answering when the request throws or answers nothing", async () => {
    const thrown = make(async () => {
      throw new Error("offline");
    });
    await thrown.session.connect();
    expect(thrown.session.end?.line).toBe("Puddle's service isn't answering.");
    const empty = make((async () => ({})) as never);
    await empty.session.connect();
    expect(empty.session.end?.line).toBe("Puddle's service isn't answering.");
  });

  it("opens no socket for an attempt that was closed or replaced meanwhile", async () => {
    const closed = make();
    const pending = closed.session.connect();
    closed.session.close();
    await pending;
    expect(closed.sockets).toEqual([]);

    const replaced = make();
    const first = replaced.session.connect();
    const second = replaced.session.connect();
    await Promise.all([first, second]);
    expect(replaced.sockets).toHaveLength(1);
  });
});

describe("output and flow control", () => {
  async function live() {
    const made = make();
    await made.session.connect();
    const socket = made.sockets[0] as FakeSocket;
    socket.open();
    return { ...made, socket };
  }

  it("draws what arrives and acknowledges once it is drawn, in batches", async () => {
    const { session, screen, socket } = await live();
    expect(session.output).toBe(false);
    socket.receive(bytes(104, 105));
    expect(session.output).toBe(true);
    expect(screen.written[0]?.bytes).toEqual(bytes(104, 105));
    expect(socket.controls()).toEqual([]);
    (screen.written[0] as { done: () => void }).done();
    expect(socket.controls()).toEqual([]);
    vi.advanceTimersByTime(25);
    expect(socket.controls()).toEqual([{ type: "ack", bytes: 2 }]);

    socket.receive(new Uint8Array(20 * 1024));
    (screen.written[1] as { done: () => void }).done();
    expect(socket.controls().at(-1)).toEqual({ type: "ack", bytes: 20480 });
    vi.advanceTimersByTime(100);
    expect(socket.controls()).toHaveLength(2);
  });

  it("sends one acknowledgement when a batch fills while a small one waits, and drops pending timers at the end", async () => {
    const { session, screen, socket } = await live();
    socket.receive(bytes(1));
    (screen.written[0] as { done: () => void }).done();
    socket.receive(new Uint8Array(20 * 1024));
    (screen.written[1] as { done: () => void }).done();
    expect(socket.controls()).toEqual([{ type: "ack", bytes: 20481 }]);
    vi.advanceTimersByTime(100);
    expect(socket.controls()).toHaveLength(1);

    socket.receive(bytes(2));
    (screen.written[2] as { done: () => void }).done();
    screen.resize({ cols: 90, rows: 30 });
    socket.receive('{"type":"ended","reason":"stopped"}');
    vi.advanceTimersByTime(100);
    expect(socket.controls()).toHaveLength(1);
    expect(session.state).toBe("ended");
  });

  it("does not acknowledge on a socket that is gone, and ignores messages it does not know", async () => {
    const { screen, socket } = await live();
    socket.receive(bytes(1));
    socket.receive("{}");
    socket.onmessage?.({ data: 3 });
    socket.onclose?.();
    (screen.written[0] as { done: () => void }).done();
    vi.advanceTimersByTime(100);
    expect(socket.controls()).toEqual([]);
  });

  it("sends nothing for a drained counter", async () => {
    const { screen, socket } = await live();
    socket.receive(bytes(1));
    (screen.written[0] as { done: () => void }).done();
    vi.advanceTimersByTime(25);
    expect(socket.controls()).toHaveLength(1);
    vi.advanceTimersByTime(100);
    expect(socket.controls()).toHaveLength(1);
  });
});

describe("size", () => {
  it("sends the new size after 100 ms without change, once", async () => {
    const { session, screen, sockets } = make();
    await session.connect({ cols: 80, rows: 24 });
    const socket = sockets[0] as FakeSocket;
    socket.open();
    screen.resize({ cols: 90, rows: 30 });
    vi.advanceTimersByTime(RESIZE_DELAY_MS - 1);
    screen.resize({ cols: 100, rows: 40 });
    vi.advanceTimersByTime(RESIZE_DELAY_MS - 1);
    expect(socket.controls()).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(socket.controls()).toEqual([
      { type: "resize", cols: 100, rows: 40 },
    ]);
    screen.resize({ cols: 100, rows: 40 });
    vi.advanceTimersByTime(RESIZE_DELAY_MS);
    expect(socket.controls()).toHaveLength(1);
  });

  it("holds the limits and sends a size that changed while connecting once it opens", async () => {
    const { session, screen, sockets } = make();
    await session.connect({ cols: 80, rows: 24 });
    screen.resize({ cols: 900, rows: 0 });
    vi.advanceTimersByTime(RESIZE_DELAY_MS);
    const socket = sockets[0] as FakeSocket;
    expect(socket.sent).toEqual([]);
    socket.open();
    expect(socket.controls()).toEqual([{ type: "resize", cols: 500, rows: 1 }]);
  });
});

describe("the end", () => {
  async function live() {
    const made = make();
    await made.session.connect();
    const socket = made.sockets[0] as FakeSocket;
    socket.open();
    return { ...made, socket };
  }

  it("keeps the sub-tab and says why, from the host's message", async () => {
    const { session, screen, socket } = await live();
    socket.receive('{"type":"ended","reason":"exited","status":0}');
    expect(session.state).toBe("ended");
    expect(session.open).toBe(false);
    expect(session.end?.line).toBe("The shell exited with status 0.");
    expect(screen.enabled).toBe(false);
    expect(socket.closed).toBe(true);
    expect(screen.disposed).toBe(false);
  });

  it("says the connection broke when the socket closes or fails without an end", async () => {
    const closed = await live();
    closed.socket.onclose?.();
    expect(closed.session.end?.line).toBe(
      "The connection to Puddle broke; the terminal ended.",
    );
    const failed = await live();
    failed.socket.onerror?.();
    expect(failed.session.end?.action).toBe("new");
  });

  it("keeps the first end when the socket closes after it", async () => {
    const { session, socket } = await live();
    socket.receive('{"type":"ended","reason":"stopped"}');
    socket.onclose?.();
    expect(session.end?.action).toBe("start");
  });

  it("restarts in the same sub-tab under a separator and takes input again", async () => {
    const { session, screen, sockets } = await live();
    sockets[0]?.receive('{"type":"ended","reason":"exited","status":1}');
    await session.restart();
    expect(screen.separators).toBe(1);
    expect(screen.enabled).toBe(true);
    expect(session.state).toBe("connecting");
    expect(session.end).toBeNull();
    expect(sockets).toHaveLength(2);
    // The old socket's events no longer count.
    sockets[0]?.onclose?.();
    expect(session.state).toBe("connecting");
    sockets[0]?.open();
    sockets[0]?.receive(bytes(1));
    expect(session.state).toBe("connecting");
  });

  it("closing a sub-tab closes the socket and the emulator", async () => {
    const { session, screen, socket } = await live();
    session.close();
    expect(socket.closed).toBe(true);
    expect(screen.disposed).toBe(true);
    expect(session.state).toBe("ended");
  });
});

describe("title and bell", () => {
  it("labels the sub-tab with a cleaned title, else Terminal N", () => {
    const { session, screen } = make();
    screen.title(`  build ${String.fromCodePoint(0x202e)} ok `);
    expect(session.label).toBe("build  ok");
    screen.title(String.fromCodePoint(0x200b));
    expect(session.label).toBe("Terminal 1");
  });

  it("marks the bell", () => {
    const { session, screen } = make();
    expect(session.bell).toBe(false);
    screen.bell();
    expect(session.bell).toBe(true);
  });
});

describe("defaults", () => {
  it("opens a real WebSocket on the page's origin", async () => {
    const opened: unknown[][] = [];
    vi.stubGlobal("WebSocket", function (url: string, protocols: string[]) {
      opened.push([url, protocols]);
      return new FakeSocket(url, protocols);
    });
    const api = {
      POST: (async () => ({
        data: { terminal: 2, ticket: "cd".repeat(32) },
      })) as unknown as Post,
    };
    const session = new TerminalSession("w", 1, new FakeScreen(), { api });
    await session.connect();
    expect(opened).toHaveLength(1);
    expect(String(opened[0]?.[0])).toBe(
      `ws://${globalThis.location.host}/api/workspaces/w/terminals/2`,
    );
    vi.unstubAllGlobals();
  });
});
