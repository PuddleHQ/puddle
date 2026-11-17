// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import type { EventStreamOptions } from "../api/sse.ts";
import { LiveState } from "./live.svelte.ts";

function fakeApi(result: () => unknown) {
  return { GET: vi.fn(async (_path?: string) => result()) } as never;
}

const ok = (n: number) => ({
  data: { requests: Array.from({ length: n }, () => ({})) },
  response: { status: 200 },
});

describe("LiveState.refresh", () => {
  it("counts the open requests and clears a problem", async () => {
    const live = new LiveState({ api: fakeApi(() => ok(3)) });
    live.problem = "unreachable";
    await live.refresh();
    expect(live.pending).toBe(3);
    expect(live.problem).toBeNull();
  });

  it("names a 401 as unauthorized and keeps the last count", async () => {
    let answer: unknown = ok(2);
    const live = new LiveState({ api: fakeApi(() => answer) });
    await live.refresh();
    answer = { error: { error: "unauthorized" }, response: { status: 401 } };
    await live.refresh();
    expect(live.problem).toBe("unauthorized");
    expect(live.pending).toBe(2);
  });

  it("tells a failing service from an unreachable one", async () => {
    const live = new LiveState({
      api: fakeApi(() => ({ error: {}, response: { status: 500 } })),
    });
    await live.refresh();
    expect(live.problem).toBe("failing");
    expect(live.problemDetail).toBe(
      "Puddle's service hit an error (status 500).",
    );
    const thrown = new LiveState({
      api: fakeApi(() => {
        throw new TypeError("down");
      }),
    });
    await thrown.refresh();
    expect(thrown.problem).toBe("unreachable");
  });
});

describe("LiveState.start", () => {
  it("refreshes on start, on a timer and on resync, mirrors the stream state, and stops cleanly", async () => {
    vi.useFakeTimers();
    try {
      const api = fakeApi(() => ok(1));
      // The count of requests; the SSH agent questions are read beside it.
      const pendingReads = () =>
        (api as { GET: ReturnType<typeof vi.fn> }).GET.mock.calls.filter(
          (call) => call[0] === "/api/pending",
        ).length;
      let options: EventStreamOptions | undefined;
      const live = new LiveState({
        api,
        pollMs: 1000,
        events: (o) => {
          options = o;
          return (async function* () {
            yield { type: "status_changed" };
          })();
        },
      });
      const stop = live.start();
      await vi.advanceTimersByTimeAsync(0);
      expect(pendingReads()).toBe(1);
      expect(live.pending).toBe(1);

      await vi.advanceTimersByTimeAsync(1000);
      expect(pendingReads()).toBe(2);

      options?.onState?.("reconnecting");
      expect(live.stream).toBe("reconnecting");
      options?.onError?.("the event stream answered 500");
      expect(live.streamError).toBe("the event stream answered 500");
      options?.onState?.("connected");
      expect(live.streamError).toBeNull();
      options?.onResync?.("lagged");
      await vi.advanceTimersByTimeAsync(0);
      expect(pendingReads()).toBe(3);

      stop();
      expect(options?.signal.aborted).toBe(true);
      await vi.advanceTimersByTimeAsync(5000);
      expect(pendingReads()).toBe(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("opens the stream again when the service answers after refusing its key", async () => {
    vi.useFakeTimers();
    try {
      const opened: EventStreamOptions[] = [];
      let answer: unknown = ok(0);
      const live = new LiveState({
        api: fakeApi(() => answer),
        pollMs: 1000,
        events: (o) => {
          opened.push(o);
          return (async function* () {})();
        },
      });
      const stop = live.start();
      await vi.advanceTimersByTimeAsync(0);
      // A poll that works while the stream is fine opens nothing new.
      await vi.advanceTimersByTimeAsync(1000);
      expect(opened).toHaveLength(1);
      const refusal = { reason: "the service said no", advice: "Restart." };
      opened[0]?.onRefused?.(refusal);
      opened[0]?.onState?.("refused");
      expect(live.stream).toBe("refused");
      expect(live.streamRefusal).toEqual(refusal);
      // The poll answering all along does not reopen a stream whose key was refused.
      await vi.advanceTimersByTimeAsync(2000);
      expect(opened).toHaveLength(1);
      // The service fails the poll too, then answers again: that is a restart, so reopen.
      answer = { error: {}, response: { status: 401 } };
      await vi.advanceTimersByTimeAsync(1000);
      expect(live.problem).toBe("unauthorized");
      expect(opened).toHaveLength(1);
      answer = ok(0);
      await vi.advanceTimersByTimeAsync(1000);
      expect(opened).toHaveLength(2);
      expect(opened[0]?.signal.aborted).toBe(true);
      expect(live.stream).toBe("connecting");
      // Late words from the old stream change nothing.
      opened[0]?.onRefused?.({ reason: "late", advice: "late" });
      opened[0]?.onState?.("refused");
      expect(live.stream).toBe("connecting");
      expect(live.streamRefusal).toEqual(refusal);
      // A connection that works clears what the refusal said.
      opened[1]?.onState?.("connected");
      expect(live.streamRefusal).toBeNull();
      stop();
      expect(opened[1]?.signal.aborted).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("uses the app's client, the real stream and a 15 s poll by default", () => {
    const live = new LiveState();
    expect(live.pending).toBe(0);
    expect(live.stream).toBe("connecting");
    expect(live.problem).toBeNull();
  });
});

describe("LiveState.subscribe", () => {
  it("hands every event and every resync to the listeners until they unsubscribe", async () => {
    vi.useFakeTimers();
    try {
      let options: EventStreamOptions | undefined;
      let push: (event: unknown) => void = () => undefined;
      const live = new LiveState({
        api: fakeApi(() => ok(0)),
        pollMs: 60_000,
        events: (o) => {
          options = o;
          return (async function* () {
            const queue: unknown[] = [];
            push = (e) => queue.push(e);
            while (!o.signal.aborted) {
              const next = queue.shift();
              if (next !== undefined) yield next;
              else await new Promise((r) => setTimeout(r, 10));
            }
          })();
        },
      });
      const event = vi.fn();
      const resync = vi.fn();
      const unsubscribe = live.subscribe({ event, resync });
      live.subscribe({});
      const stop = live.start();
      await vi.advanceTimersByTimeAsync(0);
      push({ type: "pending_opened" });
      await vi.advanceTimersByTimeAsync(20);
      expect(event).toHaveBeenCalledWith({ type: "pending_opened" });
      options?.onResync?.("lagged");
      expect(resync).toHaveBeenCalledOnce();
      unsubscribe();
      options?.onResync?.("reconnect");
      push({ type: "x" });
      await vi.advanceTimersByTimeAsync(20);
      expect(resync).toHaveBeenCalledOnce();
      expect(event).toHaveBeenCalledOnce();
      stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the stream going when a listener throws", async () => {
    vi.useFakeTimers();
    try {
      let options: EventStreamOptions | undefined;
      let push: (event: unknown) => void = () => undefined;
      const live = new LiveState({
        api: fakeApi(() => ok(0)),
        pollMs: 60_000,
        events: (o) => {
          options = o;
          return (async function* () {
            const queue: unknown[] = [];
            push = (e) => queue.push(e);
            while (!o.signal.aborted) {
              const next = queue.shift();
              if (next !== undefined) yield next as never;
              else await new Promise((r) => setTimeout(r, 10));
            }
          })();
        },
      });
      const seen: unknown[] = [];
      let resynced = 0;
      live.subscribe({
        event: () => {
          throw new Error("a screen broke");
        },
        resync: () => {
          throw new Error("a screen broke");
        },
      });
      live.subscribe({
        event: (e) => seen.push(e),
        resync: () => (resynced += 1),
      });
      const stop = live.start();
      await vi.advanceTimersByTimeAsync(0);
      push({ type: "first" });
      await vi.advanceTimersByTimeAsync(20);
      push({ type: "second" });
      await vi.advanceTimersByTimeAsync(20);
      options?.onResync?.("lagged");
      expect(seen).toEqual([{ type: "first" }, { type: "second" }]);
      expect(resynced).toBe(1);
      stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("lets a screen that knows the count tell the badge at once", () => {
    const live = new LiveState({ api: fakeApi(() => ok(0)) });
    live.setPending(7);
    expect(live.pending).toBe(7);
  });
});

describe("the SSH agent questions in the badge", () => {
  const routed = (asks: () => unknown) =>
    ({
      GET: vi.fn(async (path: string) =>
        path === "/api/ssh-agent-asks" ? asks() : ok(2),
      ),
    }) as never;
  const some = (n: number) => ({
    data: { asks: Array.from({ length: n }, () => ({})) },
    response: { status: 200 },
  });

  it("adds the waiting questions to the requests", async () => {
    const live = new LiveState({ api: routed(() => some(3)) });
    await live.refresh();
    await vi.waitFor(() => expect(live.asks).toBe(3));
    expect(live.pending).toBe(2);
    expect(live.open).toBe(5);
    live.setAsks(1);
    expect(live.open).toBe(3);
  });

  it("keeps the last count when the questions cannot be read", async () => {
    let answer: unknown = some(4);
    const live = new LiveState({ api: routed(() => answer) });
    await live.refreshAsks();
    expect(live.asks).toBe(4);
    answer = { error: {}, response: { status: 500 } };
    await live.refreshAsks();
    answer = undefined;
    await live.refreshAsks();
    expect(live.asks).toBe(4);
  });

  it("reads them again when the stream says they changed, and for nothing else", async () => {
    let n = 1;
    const api = routed(() => some(n));
    const live = new LiveState({
      api,
      pollMs: 60_000,
      events: () =>
        (async function* () {
          yield { type: "status_changed" };
          yield null;
          n = 2;
          yield { type: "ssh_agent_asks_changed", workspace: "demo" };
        })(),
    });
    const stop = live.start();
    await vi.waitFor(() => expect(live.asks).toBe(2));
    stop();
  });
});
