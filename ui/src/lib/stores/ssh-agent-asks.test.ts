// SPDX-License-Identifier: AGPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import { FakeSshAgent, ask } from "#lib/testing/fake-ssh-agent.ts";
import { live } from "./live.svelte.ts";
import { SshAgentAsksStore } from "./ssh-agent-asks.svelte.ts";

let api: FakeSshAgent;
let source: FakeSource;
let counts: number[];
const make = () =>
  new SshAgentAsksStore(api as never, source, (n) => counts.push(n));

beforeEach(() => {
  api = new FakeSshAgent();
  source = new FakeSource();
  counts = [];
});

describe("reading", () => {
  it("tells the app's badge by default", async () => {
    api.asks = [ask(1), ask(2), ask(3)];
    await new SshAgentAsksStore(api as never, source).load();
    expect(live.asks).toBe(3);
    live.setAsks(0);
  });

  it("reads the open questions and tells the badge how many", async () => {
    api.asks = [ask(1), ask(2)];
    const store = make();
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.asks.map((a) => a.id)).toEqual([1, 2]);
    expect(counts).toEqual([2]);
  });

  it("fails the first read, and keeps the list on a quiet one", async () => {
    const store = make();
    api.down = true;
    await store.load();
    expect(store.status).toBe("failed");
    api.down = false;
    api.asks = [ask(1)];
    await store.load();
    api.down = true;
    await store.load(true);
    expect(store.status).toBe("ready");
    expect(store.asks).toHaveLength(1);
  });

  it("treats a refusal like a stopped service on the first read only", async () => {
    const store = make();
    api.GET = (async () => ({
      response: { status: 500 } as Response,
    })) as never;
    await store.load();
    expect(store.status).toBe("failed");
    store.status = "ready";
    await store.load(true);
    expect(store.status).toBe("ready");
  });

  it("never lets a slow answer overwrite a newer one", async () => {
    const store = make();
    const real = api.GET;
    let release: (() => void) | undefined;
    api.GET = (async (path: string, init: never) => {
      if (release === undefined) {
        await new Promise<void>((resolve) => (release = resolve));
      }
      return real(path, init);
    }) as never;
    const slow = store.load();
    api.asks = [ask(7)];
    await store.load(true);
    api.asks = [];
    release?.();
    await slow;
    expect(store.asks.map((a) => a.id)).toEqual([7]);
  });

  it("follows a change on the stream, a resync, and stops", async () => {
    const store = make();
    const stop = store.start();
    await vi.waitFor(() => expect(store.status).toBe("ready"));
    api.asks = [ask(1)];
    source.emit({ type: "something_else" });
    source.emit("not an event");
    source.emit(null);
    await Promise.resolve();
    expect(store.asks).toHaveLength(0);
    source.emit({ type: "ssh_agent_asks_changed", workspace: "demo" });
    await vi.waitFor(() => expect(store.asks).toHaveLength(1));
    api.asks = [ask(1), ask(2)];
    source.resync();
    await vi.waitFor(() => expect(store.asks).toHaveLength(2));
    stop();
    api.asks = [];
    source.emit({ type: "ssh_agent_asks_changed", workspace: "demo" });
    source.resync();
    await Promise.resolve();
    expect(store.asks).toHaveLength(2);
  });
});

describe("answering", () => {
  it("sends the answer to the question's workspace and drops the question at once", async () => {
    api.asks = [ask(1, { workspace: "shop" as never }), ask(2)];
    const store = make();
    await store.load();
    const first = store.asks[0];
    if (!first) throw new Error("no question");
    const result = await store.answer(first, "always");
    expect(result).toEqual({ ok: true });
    expect(api.answers).toEqual([{ id: 1, answer: "always" }]);
    expect(api.calls).toContain(
      "POST /api/workspaces/{id}/ssh-agent-asks/{ask}/answer",
    );
    expect(store.asks.map((a) => a.id)).toEqual([2]);
  });

  it("counts a question that is already gone as answered", async () => {
    api.asks = [ask(1)];
    const store = make();
    await store.load();
    const first = store.asks[0];
    if (!first) throw new Error("no question");
    for (const status of [404, 409]) {
      api.nextStatus = status;
      expect(await store.answer(first, "once")).toEqual({ ok: true });
    }
    expect(api.answers).toEqual([]);
  });

  it("says when the service refused or is down, and keeps the question", async () => {
    api.asks = [ask(1)];
    const store = make();
    await store.load();
    const first = store.asks[0];
    if (!first) throw new Error("no question");
    api.nextStatus = 500;
    expect(await store.answer(first, "deny")).toEqual({
      ok: false,
      message: "Puddle couldn't take the answer.",
    });
    expect(store.asks).toHaveLength(1);
    api.down = true;
    expect(await store.answer(first, "deny")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
    expect(store.asks).toHaveLength(1);
  });
});
