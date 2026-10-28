// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import { decryptAgain } from "#lib/interception/learned.ts";
import type { LiveListener } from "./live.svelte.ts";
import { LearnedLimitsStore, LearnedList } from "./learned.svelte.ts";

type Reply = {
  data?: unknown;
  error?: { message: string };
  response: { status: number; ok: boolean };
};

class Stub {
  hosts = [
    { host: "pinned.example", expires_in_secs: 600 },
    { host: "other.example", expires_in_secs: 3000 },
  ];
  limits = { ttl_secs: 43_200, cap: 1024 };
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  refuse: { status: number; message: string } | null = null;

  private answer(data: unknown, call: string, body?: unknown): Reply {
    this.calls.push(call);
    if (body !== undefined) this.bodies.push(body);
    if (this.down) throw new TypeError("fetch failed");
    if (this.refuse) {
      const { status, message } = this.refuse;
      return { error: { message }, response: { status, ok: false } };
    }
    return { data, response: { status: 200, ok: true } };
  }

  async GET(path: string) {
    return path === "/api/interception/learned-limits"
      ? this.answer(this.limits, `GET ${path}`)
      : this.answer({ hosts: this.hosts }, `GET ${path}`);
  }

  async PUT(path: string, init: { body: { ttl_secs: number; cap: number } }) {
    const reply = this.answer(init.body, `PUT ${path}`, init.body);
    if (reply.data) this.limits = init.body;
    return reply;
  }

  async DELETE(path: string, init: { params: { path: { host?: string } } }) {
    const host = init.params.path.host;
    const reply = this.answer(undefined, `DELETE ${path} ${host ?? "*"}`);
    if (!reply.error) {
      this.hosts = host ? this.hosts.filter((h) => h.host !== host) : [];
      return { ...reply, response: { status: 204, ok: true } };
    }
    return reply;
  }
}

class Source {
  listener: LiveListener | null = null;
  stopped = false;
  subscribe(listener: LiveListener) {
    this.listener = listener;
    return () => {
      this.stopped = true;
    };
  }
}

function held() {
  const waiting: Array<{
    ok: (r: unknown) => void;
    fail: (e: unknown) => void;
  }> = [];
  const call = () =>
    new Promise((ok, fail) => {
      waiting.push({ ok, fail });
    });
  return { api: { GET: call, PUT: call }, waiting };
}

const reply = (data: unknown) => ({
  data,
  error: undefined,
  response: { status: 200, ok: true },
});

let api: Stub;
beforeEach(() => {
  api = new Stub();
});

describe("LearnedList", () => {
  it("reads the hosts", async () => {
    const list = new LearnedList(api as never);
    expect(list.status).toBe("loading");
    await list.load("web");
    expect(list.status).toBe("ready");
    expect(list.hosts.map((h) => h.host)).toEqual([
      "pinned.example",
      "other.example",
    ]);
    expect(api.calls).toEqual([
      "GET /api/workspaces/{id}/interception/learned",
    ]);
  });

  it("decrypts one host again and shows the list the API holds", async () => {
    const list = new LearnedList(api as never);
    await list.load("web");
    expect(await list.decryptAgain("pinned.example")).toEqual({ ok: true });
    expect(list.hosts.map((h) => h.host)).toEqual(["other.example"]);
    expect(await list.decryptAll()).toEqual({ ok: true });
    expect(list.hosts).toEqual([]);
    expect(api.calls.filter((c) => c.startsWith("DELETE"))).toEqual([
      "DELETE /api/workspaces/{id}/interception/learned/{host} pinned.example",
      "DELETE /api/workspaces/{id}/interception/learned *",
    ]);
  });

  it("says why a refusal happened and keeps the list", async () => {
    const list = new LearnedList(api as never);
    await list.load("web");
    api.refuse = { status: 500, message: "the list is locked" };
    expect(await list.decryptAgain("pinned.example")).toEqual({
      ok: false,
      message: "The list is locked.",
    });
    expect(await list.decryptAll()).toEqual({
      ok: false,
      message: "The list is locked.",
    });
    expect(list.hosts).toHaveLength(2);
    api.refuse = null;
    api.down = true;
    expect((await list.decryptAgain("pinned.example")).ok).toBe(false);
    expect((await list.decryptAll()).ok).toBe(false);
  });

  it("fails to load without a service or with a refusal, and keeps an old list on a quiet reload", async () => {
    const list = new LearnedList(api as never);
    api.down = true;
    await list.load("web");
    expect(list.status).toBe("failed");
    api.down = false;
    await list.load("web");
    expect(list.status).toBe("ready");
    api.refuse = { status: 404, message: "no such workspace" };
    await list.load("web", true);
    expect(list.status).toBe("ready");
    expect(list.hosts).toHaveLength(2);
    await list.load("web");
    expect(list.status).toBe("failed");
    expect(list.reason).toBe("No such workspace.");
  });

  it("reads again on the event of its own workspace and on a resync, and stops listening", async () => {
    const source = new Source();
    const list = new LearnedList(api as never, source);
    const stop = list.start("web");
    await Promise.resolve();
    expect(api.calls).toHaveLength(1);
    api.hosts.push({ host: "new.example", expires_in_secs: 5 });
    source.listener?.event?.({
      type: "pass_through_learned",
      workspace: "other-workspace",
      host: "x.example",
    });
    source.listener?.event?.({ type: "rules_changed" });
    expect(api.calls).toHaveLength(1);
    source.listener?.event?.({
      type: "pass_through_learned",
      workspace: "web",
      host: "new.example",
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(list.hosts.map((h) => h.host)).toContain("new.example");
    source.listener?.resync?.();
    await new Promise((r) => setTimeout(r, 0));
    expect(api.calls).toHaveLength(3);
    stop();
    expect(source.stopped).toBe(true);
  });

  it("reads again when the notice decrypts a host in its workspace, and not once stopped", async () => {
    const stubApi = {
      DELETE: async () => ({ response: { ok: true, status: 204 } }),
    };
    const list = new LearnedList(api as never, new Source());
    const stop = list.start("web");
    await Promise.resolve();
    expect(api.calls).toHaveLength(1);
    await decryptAgain(stubApi as never, "other", "x.example");
    expect(api.calls).toHaveLength(1);
    await decryptAgain(stubApi as never, "web", "x.example");
    await new Promise((r) => setTimeout(r, 0));
    expect(api.calls).toHaveLength(2);
    stop();
    await decryptAgain(stubApi as never, "web", "x.example");
    await new Promise((r) => setTimeout(r, 0));
    expect(api.calls).toHaveLength(2);
  });

  it("ignores a slow read that a newer one has overtaken, whether it answers or fails", async () => {
    const { api: slow, waiting } = held();
    const list = new LearnedList(slow as never, undefined);
    const stale = list.load("a");
    const failing = list.load("b");
    const fresh = list.load("c");
    waiting[2]!.ok(
      reply({ hosts: [{ host: "new.example", expires_in_secs: 1 }] }),
    );
    await fresh;
    waiting[0]!.ok(reply({ hosts: [] }));
    waiting[1]!.fail(new TypeError("fetch failed"));
    await stale;
    await failing;
    expect(list.hosts.map((h) => h.host)).toEqual(["new.example"]);
    expect(list.status).toBe("ready");
    expect(list.reason).toBeNull();
  });
});

describe("LearnedLimitsStore", () => {
  it("reads and saves the limits and shows what the API answers", async () => {
    const store = new LearnedLimitsStore(api as never);
    await store.load();
    expect(store.status).toBe("ready");
    expect(store.limits).toEqual({ ttl_secs: 43_200, cap: 1024 });
    expect(await store.save({ ttl_secs: 3600, cap: 8 })).toEqual({ ok: true });
    expect(api.bodies).toEqual([{ ttl_secs: 3600, cap: 8 }]);
    expect(store.limits).toEqual({ ttl_secs: 3600, cap: 8 });
  });

  it("says why a save was refused and keeps the limits", async () => {
    const store = new LearnedLimitsStore(api as never);
    await store.load();
    api.refuse = { status: 422, message: "the size must be 1 to 10000 hosts" };
    expect(await store.save({ ttl_secs: 3600, cap: 0 })).toEqual({
      ok: false,
      message: "The size must be 1 to 10000 hosts.",
    });
    expect(store.limits).toEqual({ ttl_secs: 43_200, cap: 1024 });
    api.refuse = null;
    api.down = true;
    expect((await store.save({ ttl_secs: 3600, cap: 8 })).ok).toBe(false);
  });

  it("fails to load", async () => {
    const store = new LearnedLimitsStore(api as never);
    api.down = true;
    await store.load();
    expect(store.status).toBe("failed");
    api.down = false;
    api.refuse = { status: 500, message: "broken" };
    await store.load();
    expect(store.reason).toBe("Broken.");
  });

  it("ignores a slow read that a newer one has overtaken, whether it answers or fails", async () => {
    const { api: slow, waiting } = held();
    const store = new LearnedLimitsStore(slow as never);
    const stale = store.load();
    const failing = store.load();
    const fresh = store.load();
    waiting[2]!.ok(reply({ ttl_secs: 60, cap: 2 }));
    await fresh;
    waiting[0]!.ok(reply({ ttl_secs: 1, cap: 1 }));
    waiting[1]!.fail(new TypeError("fetch failed"));
    await stale;
    await failing;
    expect(store.limits).toEqual({ ttl_secs: 60, cap: 2 });
    expect(store.status).toBe("ready");
    expect(store.reason).toBeNull();
  });

  it("keeps the limits of a newer read when a slow save answers after it", async () => {
    const { api: slow, waiting } = held();
    const store = new LearnedLimitsStore(slow as never);
    const saving = store.save({ ttl_secs: 5, cap: 5 });
    const reading = store.load();
    waiting[1]!.ok(reply({ ttl_secs: 60, cap: 2 }));
    await reading;
    waiting[0]!.ok(reply({ ttl_secs: 5, cap: 5 }));
    expect(await saving).toEqual({ ok: true });
    expect(store.limits).toEqual({ ttl_secs: 60, cap: 2 });
  });
});
