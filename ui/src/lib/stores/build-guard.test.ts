// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { BuildGuard } from "./build-guard.ts";

type Served = { data?: { ui_build_id: string | null } } | "throw";
let served: Served;
let calls: string[];
let reloads: number;
let kept: Map<string, string>;
const memory = {
  getItem: (key: string) => kept.get(key) ?? null,
  setItem: (key: string, value: string) => void kept.set(key, value),
};
const api = {
  GET: async (path: string) => {
    calls.push(path);
    if (served === "throw") throw new TypeError("down");
    return served;
  },
};
const location = { reload: () => void (reloads += 1) };
const make = (current = "build-1", extra = {}) =>
  new BuildGuard({ current, api: api as never, location, memory, ...extra });

beforeEach(() => {
  served = { data: { ui_build_id: "build-1" } };
  calls = [];
  reloads = 0;
  kept = new Map();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("BuildGuard.check", () => {
  it("leaves a page alone when the server serves its own build", async () => {
    expect(await make().check()).toBe(false);
    expect(calls).toEqual(["/api/health"]);
    expect(reloads).toBe(0);
  });

  it("reloads a page whose build is not the server's", async () => {
    served = { data: { ui_build_id: "build-2" } };
    expect(await make().check()).toBe(true);
    expect(reloads).toBe(1);
  });

  it("reloads once per server build, so a bundle that stays stale cannot loop", async () => {
    served = { data: { ui_build_id: "build-2" } };
    const guard = make();
    await guard.check();
    expect(await guard.check()).toBe(false);
    expect(reloads).toBe(1);
    served = { data: { ui_build_id: "build-3" } };
    expect(await guard.check()).toBe(true);
    expect(reloads).toBe(2);
  });

  it("has nothing to compare when the server serves no app", async () => {
    served = { data: { ui_build_id: null } };
    expect(await make().check()).toBe(false);
    expect(reloads).toBe(0);
  });

  it("has nothing to compare when the answer is an error or the server is down", async () => {
    served = {};
    expect(await make().check()).toBe(false);
    served = "throw";
    expect(await make().check()).toBe(false);
    expect(reloads).toBe(0);
  });

  it("still reloads when the tab keeps no session storage", async () => {
    served = { data: { ui_build_id: "build-2" } };
    expect(await make("build-1", { memory: undefined }).check()).toBe(true);
    expect(reloads).toBe(1);
  });

  it("still reloads when session storage refuses reads and writes", async () => {
    served = { data: { ui_build_id: "build-2" } };
    const broken = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };
    expect(await make("build-1", { memory: broken }).check()).toBe(true);
    expect(reloads).toBe(1);
  });

  it("uses the tab's session storage by default", async () => {
    served = { data: { ui_build_id: "build-2" } };
    const guard = new BuildGuard({
      current: "build-1",
      api: api as never,
      location,
    });
    expect(await guard.check()).toBe(true);
    expect(reloads).toBe(1);
    expect(window.sessionStorage.getItem("puddle.reloaded-for-build")).toBe(
      "build-2",
    );
    window.sessionStorage.clear();
  });

  it("goes without memory when session storage cannot be reached", async () => {
    served = { data: { ui_build_id: "build-2" } };
    vi.spyOn(window, "sessionStorage", "get").mockImplementation(() => {
      throw new Error("blocked");
    });
    const guard = new BuildGuard({
      current: "build-1",
      api: api as never,
      location,
    });
    expect(await guard.check()).toBe(true);
    expect(reloads).toBe(1);
    vi.restoreAllMocks();
  });
});

describe("BuildGuard.start", () => {
  it("checks at once, when the event stream reconnects, and on a slow poll", async () => {
    vi.useFakeTimers();
    const source = new FakeSource();
    const stop = make("build-1", { source, pollMs: 1000 }).start();
    expect(calls).toHaveLength(1);
    source.resync();
    expect(calls).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1000);
    expect(calls).toHaveLength(3);
    stop();
    source.resync();
    await vi.advanceTimersByTimeAsync(5000);
    expect(calls).toHaveLength(3);
    expect(source.listeners.size).toBe(0);
  });

  it("reloads when the server comes back with another build", async () => {
    vi.useFakeTimers();
    const source = new FakeSource();
    const stop = make("build-1", { source }).start();
    await vi.advanceTimersByTimeAsync(0);
    expect(reloads).toBe(0);
    served = { data: { ui_build_id: "build-2" } };
    source.resync();
    await vi.advanceTimersByTimeAsync(0);
    expect(reloads).toBe(1);
    stop();
  });

  it("works without an event source, on the poll alone", async () => {
    vi.useFakeTimers();
    const stop = make().start();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(calls).toHaveLength(2);
    stop();
  });
});
