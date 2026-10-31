// SPDX-License-Identifier: GPL-3.0-or-later
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import {
  FakeRequestRules,
  requestRule,
} from "#lib/testing/fake-request-rules.ts";
import { RequestRulesStore } from "./request-rules.svelte.ts";

let api: FakeRequestRules;
let source: FakeSource;

const make = (pollMs = 60_000) =>
  new RequestRulesStore({ api: api as never, source, pollMs });

beforeEach(() => {
  api = new FakeRequestRules();
  source = new FakeSource();
});
afterEach(() => vi.useRealTimers());

describe("reading", () => {
  it("loads every rule", async () => {
    api.rules = [requestRule(1), requestRule(2)];
    const store = make();
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.rules.map((r) => r.id)).toEqual([1, 2]);
  });
  it("fails the first read quietly and keeps what it has after a later failure", async () => {
    api.down = true;
    const store = make();
    await store.refresh();
    expect(store.status).toBe("failed");
    expect(store.loadError).toBe("Puddle's service isn't answering.");
    api.down = false;
    api.rules = [requestRule(1)];
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.loadError).toBeNull();
    api.down = true;
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.rules).toHaveLength(1);
    expect(store.loadError).toBe("Puddle's service isn't answering.");
  });
  it("treats an error answer like a failure and says what it said", async () => {
    api.GET = async () =>
      ({
        error: { error: "internal", message: "the store is locked" },
        response: { status: 500, ok: false } as Response,
      }) as never;
    const store = make();
    await store.refresh();
    expect(store.status).toBe("failed");
    expect(store.loadError).toBe("The store is locked.");
  });
});

describe("a refused read after a good one", () => {
  it("keeps the rows and says why", async () => {
    api.rules = [requestRule(1)];
    const store = make();
    await store.refresh();
    api.GET = async () =>
      ({
        error: { error: "internal", message: "the store is locked" },
        response: { status: 500, ok: false } as Response,
      }) as never;
    await store.refresh();
    expect(store.status).toBe("ready");
    expect(store.rules).toHaveLength(1);
    expect(store.loadError).toBe("The store is locked.");
  });
});

describe("adding", () => {
  it("sends the rule and shows it at once", async () => {
    const store = make();
    const result = await store.add({
      host: "api.example.com",
      methods: ["delete"],
      path: "/repos/**",
      workspace: null,
    });
    expect(result.ok).toBe(true);
    expect(api.bodies.at(-1)).toEqual({
      host: "api.example.com",
      methods: ["delete"],
      path: "/repos/**",
      workspace: null,
    });
    expect(store.rules.map((r) => r.host)).toEqual(["api.example.com"]);
  });
  it("hands back a refusal with the field it belongs to", async () => {
    const store = make();
    api.refuse = { status: 422, message: "path: a path starts with `/`" };
    const result = await store.add({
      host: "a.example",
      methods: [],
      path: "x",
      workspace: null,
    });
    expect(result).toEqual({
      ok: false,
      field: "path",
      message: "A path starts with `/`.",
    });
    expect(store.rules).toEqual([]);
  });
  it("says the service is down when the call throws", async () => {
    const store = make();
    api.down = true;
    expect(
      await store.add({
        host: "a.example",
        methods: [],
        path: "/",
        workspace: null,
      }),
    ).toEqual({
      ok: false,
      field: "form",
      message: "Puddle's service isn't answering.",
    });
  });
  it("words an error with no message", async () => {
    const store = make();
    api.POST = async () =>
      ({ error: undefined, response: { status: 500, ok: false } }) as never;
    expect(
      await store.add({
        host: "a.example",
        methods: [],
        path: "/",
        workspace: null,
      }),
    ).toMatchObject({ ok: false, message: "Puddle refused the rule." });
  });
});

describe("deleting", () => {
  it("removes the rule", async () => {
    api.rules = [requestRule(1), requestRule(2)];
    const store = make();
    await store.refresh();
    expect(await store.remove(1)).toEqual({ ok: true });
    expect(store.rules.map((r) => r.id)).toEqual([2]);
    expect(api.calls).toContain("DELETE /api/request-rules/{id} 1");
  });
  it("counts a rule that is already gone as deleted", async () => {
    const store = make();
    expect(await store.remove(9)).toEqual({ ok: true });
  });
  it("says so when the service refuses or is down", async () => {
    api.rules = [requestRule(1)];
    const store = make();
    await store.refresh();
    api.refuse = { status: 500, message: "x" };
    expect(await store.remove(1)).toEqual({
      ok: false,
      message: "Puddle couldn't delete that rule.",
    });
    expect(store.rules).toHaveLength(1);
    api.down = true;
    expect(await store.remove(1)).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
  });
});

describe("staying current", () => {
  it("refetches on its event and on a resync, and ignores other events", async () => {
    const store = make();
    const stop = store.start();
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    api.rules = [requestRule(1)];
    source.emit({ type: "rules_changed" });
    await Promise.resolve();
    expect(store.rules).toHaveLength(0);
    source.emit({ type: "request_rules_changed" });
    await vi.waitFor(() => expect(store.rules).toHaveLength(1));
    api.rules = [requestRule(1), requestRule(2)];
    source.resync();
    await vi.waitFor(() => expect(store.rules).toHaveLength(2));
    stop();
  });
  it("polls as a safety net until stopped", async () => {
    vi.useFakeTimers();
    const store = make(1000);
    const stop = store.start();
    await vi.advanceTimersByTimeAsync(0);
    const first = api.calls.length;
    await vi.advanceTimersByTimeAsync(1000);
    expect(api.calls.length).toBe(first + 1);
    stop();
    await vi.advanceTimersByTimeAsync(5000);
    expect(api.calls.length).toBe(first + 1);
  });
  it("does not poll again when stopped while a poll's read is still going", async () => {
    vi.useFakeTimers();
    const store = make(1000);
    const stop = store.start();
    await vi.advanceTimersByTimeAsync(0);
    let release: () => void = () => undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const read = api.GET;
    api.GET = async (path: string) => {
      await held;
      return read(path);
    };
    await vi.advanceTimersByTimeAsync(1000);
    stop();
    release();
    const calls = api.calls.length;
    await vi.advanceTimersByTimeAsync(5000);
    expect(api.calls.length).toBe(calls + 1);
  });
  it("stops before the first read ends without polling", async () => {
    vi.useFakeTimers();
    const store = make(1000);
    const stop = store.start();
    stop();
    await vi.advanceTimersByTimeAsync(5000);
    expect(api.calls.length).toBe(1);
  });
});
